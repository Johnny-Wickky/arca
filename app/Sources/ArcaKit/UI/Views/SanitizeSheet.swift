// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI
import UIKit

/// Modal presenting the verified sterile document copy with sharing capabilities.
public struct SanitizeSheet: View {
    public let sanitized: SanitizedDocument
    @EnvironmentObject private var state: AppState
    @Environment(\.dismiss) private var dismiss
    @State private var isSharePresented = false

    public init(sanitized: SanitizedDocument) {
        self.sanitized = sanitized
    }

    public var body: some View {
        ZStack {
            state.theme.background.ignoresSafeArea()

            VStack(spacing: 24) {
                // Header
                HStack {
                    Spacer()
                    Button {
                        state.sanitizedDocument = nil
                        dismiss()
                    } label: {
                        ArcaGlyph(.close, size: 18, color: state.theme.textSecondary)
                            .padding(8)
                            .background(state.theme.surfaceSecondary)
                            .clipShape(Circle())
                    }
                }
                .padding(.horizontal, 20)
                .padding(.top, 16)

                // Shield
                VStack(spacing: 16) {
                    ArcaShieldShape()
                        .stroke(ArcaTheme.cleanGreen, lineWidth: 3)
                        .frame(width: 72, height: 86)
                        .overlay(
                            ArcaGlyph(.shieldCheck, size: 36, color: ArcaTheme.cleanGreen)
                        )

                    VStack(spacing: 6) {
                        Text("Стерильная копия создана")
                            .font(.system(size: 22, weight: .bold))
                            .foregroundColor(state.theme.textPrimary)

                        Text("Копия пересобрана заново и повторно проверена рентгеном: макросы, связи и метаданные удалены.")
                            .font(.system(size: 14, weight: .regular))
                            .foregroundColor(state.theme.textSecondary)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 20)
                    }
                }

                // Actions List
                if !sanitized.info.actions.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("ВЫПОЛНЕННЫЕ ОПЕРАЦИИ")
                            .font(.system(size: 11, weight: .bold))
                            .foregroundColor(state.theme.textSecondary)

                        VStack(spacing: 6) {
                            ForEach(sanitized.info.actions) { action in
                                HStack {
                                    ArcaGlyph(.check, size: 14, color: ArcaTheme.cleanGreen)
                                    Text(action.kind)
                                        .font(.system(size: 13, weight: .semibold))
                                        .foregroundColor(state.theme.textPrimary)
                                    Spacer()
                                    Text(action.target)
                                        .font(.system(size: 11, weight: .regular, design: .monospaced))
                                        .foregroundColor(state.theme.textSecondary)
                                        .lineLimit(1)
                                }
                                .padding(10)
                                .background(state.theme.surface)
                                .clipShape(RoundedRectangle(cornerRadius: 8))
                            }
                        }
                    }
                    .padding(.horizontal, 20)
                }

                Spacer()

                // Share Button
                Button {
                    isSharePresented = true
                    ArcaHaptics.selection()
                } label: {
                    HStack(spacing: 8) {
                        ArcaGlyph(.share, size: 18, color: .white)
                        Text("Поделиться очищенным файлом")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundColor(.white)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 16)
                    .background(ArcaTheme.cleanGreen)
                    .clipShape(RoundedRectangle(cornerRadius: 14))
                }
                .padding(.horizontal, 20)
                .padding(.bottom, 24)
            }
        }
        .sheet(isPresented: $isSharePresented) {
            ActivityView(activityItems: [createTemporaryFile(from: sanitized)])
        }
    }

    private func createTemporaryFile(from doc: SanitizedDocument) -> URL {
        let ext = doc.info.format.lowercased()
        let tempURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("sanitized_copy.\(ext)")
        try? doc.bytes.write(to: tempURL)
        return tempURL
    }
}

private struct ActivityView: UIViewControllerRepresentable {
    let activityItems: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: activityItems, applicationActivities: nil)
    }

    func updateUIViewController(_ uiViewController: UIActivityViewController, context: Context) {}
}
