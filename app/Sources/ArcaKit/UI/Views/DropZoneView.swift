// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI
import UniformTypeIdentifiers

/// Initial intake screen supporting file import, drag-and-drop, and system pickers.
public struct DropZoneView: View {
    @EnvironmentObject private var state: AppState
    @State private var isImporterPresented = false
    @State private var isTargeted = false

    public init() {}

    public var body: some View {
        ZStack {
            state.theme.background.ignoresSafeArea()

            VStack(spacing: 32) {
                // Top Bar
                HStack {
                    Spacer()
                    Button {
                        state.currentScreen = .about
                        ArcaHaptics.selection()
                    } label: {
                        HStack(spacing: 6) {
                            ArcaGlyph(.passport, size: 16, color: state.theme.accent)
                            Text("Паспорт")
                                .font(.system(size: 14, weight: .semibold))
                                .foregroundColor(state.theme.accent)
                        }
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(state.theme.surfaceSecondary)
                        .clipShape(Capsule())
                    }
                }
                .padding(.horizontal, 24)
                .padding(.top, 12)

                Spacer()

                // Hero Shield & Title
                VStack(spacing: 20) {
                    ArcaShieldShape()
                        .stroke(state.theme.accent, lineWidth: 3)
                        .frame(width: 80, height: 96)
                        .overlay(
                            ArcaGlyph(.shieldCheck, size: 40, color: state.theme.accent)
                        )
                        .shadow(color: state.theme.accent.opacity(0.2), radius: 20)

                    VStack(spacing: 8) {
                        Text("Arca")
                            .font(.system(size: 36, weight: .bold, design: .rounded))
                            .foregroundColor(state.theme.textPrimary)

                        Text("Параноидально безопасный просмотр\nв одноразовой Wasm-песочнице")
                            .font(.system(size: 15, weight: .medium))
                            .multilineTextAlignment(.center)
                            .foregroundColor(state.theme.textSecondary)
                            .lineSpacing(4)
                    }
                }

                // Drag and Drop Zone
                Button {
                    isImporterPresented = true
                    ArcaHaptics.selection()
                } label: {
                    VStack(spacing: 14) {
                        ArcaGlyph(.document, size: 32, color: state.theme.accent)

                        Text("Нажмите для выбора файла")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundColor(state.theme.textPrimary)

                        Text("или перетащите DOCX, XLSX, PPTX, PDF, RTF сюда")
                            .font(.system(size: 13, weight: .regular))
                            .foregroundColor(state.theme.textSecondary)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 36)
                    .background(isTargeted ? state.theme.surfaceSecondary : state.theme.surface)
                    .clipShape(RoundedRectangle(cornerRadius: 20))
                    .overlay(
                        RoundedRectangle(cornerRadius: 20)
                            .strokeBorder(
                                isTargeted ? state.theme.accent : state.theme.surfaceSecondary,
                                style: StrokeStyle(lineWidth: 2, dash: [8, 6])
                            )
                    )
                }
                .buttonStyle(.plain)
                .padding(.horizontal, 24)
                .onDrop(of: [.item, .content, .data], isTargeted: $isTargeted) { providers in
                    guard let provider = providers.first else { return false }
                    _ = provider.loadObject(ofClass: URL.self) { url, _ in
                        if let fileURL = url {
                            DispatchQueue.main.async {
                                state.openDocument(from: fileURL)
                            }
                        }
                    }
                    return true
                }

                // Security Floor Badges
                HStack(spacing: 20) {
                    Badge(text: "Zero Network", glyph: .shield)
                    Badge(text: "Safe Rust", glyph: .check)
                    Badge(text: "Wasm Sandbox", glyph: .lock)
                }

                Spacer()
            }
        }
        .fileImporter(
            isPresented: $isImporterPresented,
            allowedContentTypes: [.item, .content, .data],
            allowsMultipleSelection: false
        ) { result in
            switch result {
            case .success(let urls):
                if let url = urls.first {
                    state.openDocument(from: url)
                }
            case .failure:
                break
            }
        }
    }
}

private struct Badge: View {
    let text: String
    let glyph: ArcaGlyphKind
    @EnvironmentObject private var state: AppState

    var body: some View {
        HStack(spacing: 6) {
            ArcaGlyph(glyph, size: 12, color: state.theme.textSecondary)
            Text(text)
                .font(.system(size: 12, weight: .medium))
                .foregroundColor(state.theme.textSecondary)
        }
    }
}
