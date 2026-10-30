// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI

/// Root visual navigation coordinator of the Arca iOS application.
///
/// The state is the one the scene owns and injects: "Open with Arca" (`onOpenURL`) and the screens
/// must talk to the same `AppState`, so the view does not create its own.
public struct RootView: View {
    @EnvironmentObject private var state: AppState

    public init() {}

    public var body: some View {
        ZStack {
            state.theme.background.ignoresSafeArea()

            switch state.currentScreen {
            case .idle:
                DropZoneView()
                    .environmentObject(state)
                    .transition(.opacity)

            case .scanning(let fileName):
                ScanningView(fileName: fileName)
                    .environmentObject(state)
                    .transition(.opacity)

            case .report(let report, let diagnostics):
                XRayReportView(report: report, diagnostics: diagnostics)
                    .environmentObject(state)
                    .transition(.asymmetric(insertion: .move(edge: .trailing), removal: .move(edge: .leading)))

            case .viewer(let report, let meta):
                DocumentViewer(report: report, meta: meta)
                    .environmentObject(state)
                    .transition(.asymmetric(insertion: .move(edge: .trailing), removal: .move(edge: .leading)))

            case .about:
                AboutPassportView()
                    .environmentObject(state)
                    .transition(.move(edge: .bottom))
            }
        }
        .animation(.arcaSpring, value: state.currentScreen)
        // Password Prompt Modal
        .alert("Документ зашифрован", isPresented: $state.isPasswordPromptVisible) {
            SecureField("Пароль", text: $state.passwordInput)
            Button("Разблокировать") {
                state.submitPassword()
            }
            Button("Отмена", role: .cancel) {
                state.closeDocument()
            }
        } message: {
            Text("Введите пароль для безопасного разбора содержимого в песочнице.")
        }
        // Error Alert
        .alert("Ошибка безопасности", isPresented: Binding(
            get: { state.activeError != nil },
            set: { if !$0 { state.activeError = nil } }
        )) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(state.activeError?.errorDescription ?? "")
        }
        // Macro Inspector Sheet
        .sheet(item: $state.inspectedMacro) { macro in
            MacroViewer(macro: macro)
                .environmentObject(state)
        }
        // Image Viewer Fullscreen Cover
        .fullScreenCover(item: Binding(
            get: { state.inspectedImage.map { IdentifiableImage(image: $0) } },
            set: { state.inspectedImage = $0?.image }
        )) { wrapper in
            ImageViewer(image: wrapper.image)
                .environmentObject(state)
        }
        // Sanitize Sheet
        .sheet(item: Binding(
            get: { state.sanitizedDocument.map { IdentifiableSanitized(doc: $0) } },
            set: { state.sanitizedDocument = $0?.doc }
        )) { wrapper in
            SanitizeSheet(sanitized: wrapper.doc)
                .environmentObject(state)
        }
    }
}

// MARK: - Scanning Animation View

private struct ScanningView: View {
    let fileName: String
    @EnvironmentObject private var state: AppState
    @State private var isPulsing = false

    var body: some View {
        VStack(spacing: 24) {
            ArcaShieldShape()
                .stroke(state.theme.accent, lineWidth: 3)
                .frame(width: 80, height: 96)
                .scaleEffect(isPulsing ? 1.08 : 0.95)
                .overlay(
                    ArcaGlyph(.shield, size: 36, color: state.theme.accent)
                )
                .animation(.easeInOut(duration: 1.0).repeatForever(autoreverses: true), value: isPulsing)
                .onAppear { isPulsing = true }

            VStack(spacing: 8) {
                Text("Рентген безопасности…")
                    .font(.system(size: 20, weight: .bold))
                    .foregroundColor(state.theme.textPrimary)

                Text(fileName)
                    .font(.system(size: 14, weight: .medium, design: .monospaced))
                    .foregroundColor(state.theme.textSecondary)
                    .lineLimit(1)
            }

            Button {
                state.closeDocument()
            } label: {
                Text("Отмена")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundColor(state.theme.textSecondary)
                    .padding(.horizontal, 20)
                    .padding(.vertical, 8)
                    .background(state.theme.surfaceSecondary)
                    .clipShape(Capsule())
            }
            .padding(.top, 16)
        }
    }
}

// Identifiable Wrappers for sheets
private struct IdentifiableImage: Identifiable {
    var id: String { image.facts.format + "\(image.facts.width)x\(image.facts.height)" }
    let image: DecodedImage
}

private struct IdentifiableSanitized: Identifiable {
    var id: String { doc.info.verification.sha256 }
    let doc: SanitizedDocument
}
