// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI

/// Safe read-only viewer for inspected macro source code.
public struct MacroViewer: View {
    public let macro: MacroModule
    @EnvironmentObject private var state: AppState
    @Environment(\.dismiss) private var dismiss

    public init(macro: MacroModule) {
        self.macro = macro
    }

    public var body: some View {
        ZStack {
            state.theme.background.ignoresSafeArea()

            VStack(spacing: 0) {
                // Header
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(macro.name)
                            .font(.system(size: 17, weight: .bold))
                            .foregroundColor(state.theme.textPrimary)

                        Text("Изолированный исходный код • \(macro.language)")
                            .font(.system(size: 12, weight: .medium))
                            .foregroundColor(state.theme.textSecondary)
                    }

                    Spacer()

                    Button {
                        dismiss()
                    } label: {
                        ArcaGlyph(.close, size: 18, color: state.theme.textSecondary)
                            .padding(8)
                            .background(state.theme.surfaceSecondary)
                            .clipShape(Circle())
                    }
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 16)
                .background(state.theme.surface)

                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        // What the code is and does, in words
                        intentSection

                        // Indicators / Badges
                        if !macro.autoExec.isEmpty || !macro.suspicious.isEmpty {
                            badgesSection
                        }

                        // Source code display
                        VStack(alignment: .leading, spacing: 0) {
                            HStack {
                                Text("SOURCE CODE")
                                    .font(.system(size: 11, weight: .bold, design: .monospaced))
                                    .foregroundColor(state.theme.textSecondary)
                                Spacer()
                                Button {
                                    UIPasteboard.general.string = macro.source
                                    ArcaHaptics.selection()
                                } label: {
                                    Text("Копировать")
                                        .font(.system(size: 12, weight: .semibold))
                                        .foregroundColor(state.theme.accent)
                                }
                            }
                            .padding(.bottom, 8)

                            Text(macro.source)
                                .font(.system(size: 12, weight: .regular, design: .monospaced))
                                .foregroundColor(state.theme.textPrimary)
                                .padding(14)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(state.theme.surface)
                                .clipShape(RoundedRectangle(cornerRadius: 12))
                                .textSelection(.enabled)
                        }
                    }
                    .padding(20)
                }
            }
        }
    }

    private var intentColor: Color {
        switch macro.intent {
        case "Benign": return ArcaTheme.unverifiedSlate
        case "Malicious": return ArcaTheme.activeRed
        default: return ArcaTheme.trackerAmber
        }
    }

    /// What the code is (everyday automation, suspicious, harmful), what it does and where, and why
    /// looking at it here is safe. A reading of the text, not a guarantee, and it says so.
    private var intentSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            TagChip(text: MacroBehaviorText.title(intent: macro.intent), color: intentColor)

            Text(MacroBehaviorText.summary(intent: macro.intent))
                .font(.system(size: 13))
                .foregroundColor(state.theme.textPrimary)

            if !macro.behaviors.isEmpty {
                Text("ЧТО ОН ДЕЛАЕТ")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundColor(state.theme.textSecondary)

                ForEach(macro.behaviors) { behavior in
                    VStack(alignment: .leading, spacing: 3) {
                        Text("• " + MacroBehaviorText.text(kind: behavior.kind))
                            .font(.system(size: 13, weight: .medium))
                            .foregroundColor(state.theme.textPrimary)
                        Text(MacroBehaviorText.place(module: macro.name, procedure: behavior.proc))
                            .font(.system(size: 11))
                            .foregroundColor(state.theme.textSecondary)
                        if !behavior.detail.isEmpty {
                            Text(behavior.detail)
                                .font(.system(size: 11, design: .monospaced))
                                .foregroundColor(state.theme.textPrimary)
                                .textSelection(.enabled)
                        }
                    }
                }
            }

            Text(MacroBehaviorText.safeHere)
                .font(.system(size: 12))
                .foregroundColor(state.theme.textPrimary)
            Text(MacroBehaviorText.readingNote)
                .font(.system(size: 11))
                .foregroundColor(state.theme.textSecondary)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(state.theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }

    private var badgesSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !macro.autoExec.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("ТОЧКИ АВТОЗАПУСКА")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundColor(ArcaTheme.activeRed)

                    FlowLayout(spacing: 6) {
                        ForEach(macro.autoExec, id: \.self) { item in
                            TagChip(text: item, color: ArcaTheme.activeRed)
                        }
                    }
                }
            }

            if !macro.suspicious.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("ПОДОЗРИТЕЛЬНЫЕ ВЫЗОВЫ")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundColor(ArcaTheme.trackerAmber)

                    FlowLayout(spacing: 6) {
                        ForEach(macro.suspicious, id: \.self) { item in
                            TagChip(text: item, color: ArcaTheme.trackerAmber)
                        }
                    }
                }
            }
        }
    }
}

private struct TagChip: View {
    let text: String
    let color: Color

    var body: some View {
        Text(text)
            .font(.system(size: 12, weight: .semibold, design: .monospaced))
            .foregroundColor(color)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(color.opacity(0.12))
            .clipShape(RoundedRectangle(cornerRadius: 6))
    }
}

private struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? 0
        var height: CGFloat = 0
        var currentX: CGFloat = 0
        var currentY: CGFloat = 0
        var maxHeight: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if currentX + size.width > width && currentX > 0 {
                currentX = 0
                currentY += maxHeight + spacing
                maxHeight = 0
            }
            maxHeight = max(maxHeight, size.height)
            currentX += size.width + spacing
        }
        height = currentY + maxHeight
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var currentX = bounds.minX
        var currentY = bounds.minY
        var maxHeight: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if currentX + size.width > bounds.maxX && currentX > bounds.minX {
                currentX = bounds.minX
                currentY += maxHeight + spacing
                maxHeight = 0
            }
            subview.place(at: CGPoint(x: currentX, y: currentY), proposal: .unspecified)
            maxHeight = max(maxHeight, size.height)
            currentX += size.width + spacing
        }
    }
}
