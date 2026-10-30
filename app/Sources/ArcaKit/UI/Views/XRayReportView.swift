// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI

/// The central "Рентген безопасности" analysis screen.
public struct XRayReportView: View {
    public let report: Report
    public let diagnostics: RunDiagnostics
    @EnvironmentObject private var state: AppState

    public init(report: Report, diagnostics: RunDiagnostics) {
        self.report = report
        self.diagnostics = diagnostics
    }

    public var body: some View {
        ZStack {
            state.theme.background.ignoresSafeArea()

            VStack(spacing: 0) {
                // Navigation Bar
                HStack {
                    Button {
                        state.closeDocument()
                    } label: {
                        ArcaGlyph(.close, size: 20, color: state.theme.textSecondary)
                            .padding(10)
                            .background(state.theme.surfaceSecondary)
                            .clipShape(Circle())
                    }

                    Spacer()

                    Text(report.format.uppercased())
                        .font(.system(size: 13, weight: .bold, design: .monospaced))
                        .foregroundColor(state.theme.textSecondary)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(state.theme.surfaceSecondary)
                        .clipShape(Capsule())
                }
                .padding(.horizontal, 20)
                .padding(.top, 12)
                .padding(.bottom, 8)

                ScrollView {
                    VStack(spacing: 24) {
                        // Verdict Shield Banner
                        verdictBanner

                        // Primary Action Buttons
                        actionButtons

                        // Stats Summary Row
                        countersGrid

                        // Detailed Findings
                        if !report.findings.isEmpty {
                            findingsSection
                        }

                        // Macros Section (if present)
                        if !report.macros.isEmpty {
                            macrosSection
                        }

                        // Metadata Section
                        if !report.meta.isEmpty {
                            metadataSection
                        }

                        // Execution Telemetry
                        diagnosticsFooter
                    }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 16)
                }
            }
        }
    }

    // MARK: - Verdict Banner

    private var verdictBanner: some View {
        let color = verdictColor(report.verdict)
        return VStack(spacing: 16) {
            ArcaShieldShape()
                .fill(color.opacity(0.15))
                .overlay(
                    ArcaShieldShape()
                        .stroke(color, lineWidth: 3)
                )
                .frame(width: 80, height: 96)
                .overlay(
                    ArcaGlyph(verdictGlyph(report.verdict), size: 40, color: color)
                )

            VStack(spacing: 6) {
                Text(report.verdict.titleRu)
                    .font(.system(size: 24, weight: .bold))
                    .foregroundColor(state.theme.textPrimary)

                Text(verdictSubtitle(report.verdict))
                    .font(.system(size: 14, weight: .medium))
                    .foregroundColor(state.theme.textSecondary)
                    .multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 20)
        .background(state.theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 20))
    }

    // MARK: - Action Buttons

    private var actionButtons: some View {
        VStack(spacing: 12) {
            // An encrypted file is reported, not refused (status OK, `needsPassword`): the way in is
            // here, as in the Android result screen. Some files (a PDF that protects only against
            // editing) open without a password too, so the report stays and the lock is a choice.
            if report.needsPassword {
                Button {
                    state.isPasswordPromptVisible = true
                    ArcaHaptics.selection()
                } label: {
                    HStack(spacing: 8) {
                        ArcaGlyph(.lock, size: 18, color: .white)
                        Text("Ввести пароль")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundColor(.white)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 16)
                    .background(state.theme.accent)
                    .clipShape(RoundedRectangle(cornerRadius: 14))
                }
            }

            Button {
                state.proceedToViewer(report: report)
            } label: {
                HStack(spacing: 8) {
                    ArcaGlyph(.document, size: 18, color: .white)
                    Text("Безопасный просмотр")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundColor(.white)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 16)
                .background(state.theme.accent)
                .clipShape(RoundedRectangle(cornerRadius: 14))
            }

            Button {
                state.sanitizeCurrentDocument()
            } label: {
                HStack(spacing: 8) {
                    ArcaGlyph(.sanitize, size: 18, color: state.theme.textPrimary)
                    Text("Стерильная копия в один тап")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundColor(state.theme.textPrimary)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 14)
                .background(state.theme.surface)
                .clipShape(RoundedRectangle(cornerRadius: 14))
            }
        }
    }

    // MARK: - Counters Grid

    private var countersGrid: some View {
        HStack(spacing: 12) {
            CounterCard(
                title: "Трекеры",
                count: report.trackers,
                color: report.trackers > 0 ? ArcaTheme.trackerAmber : state.theme.textSecondary
            )
            CounterCard(
                title: "Активное",
                count: report.active,
                color: report.active > 0 ? ArcaTheme.activeRed : state.theme.textSecondary
            )
            CounterCard(
                title: "Находки",
                count: report.findingCount,
                color: report.findingCount > 0 ? state.theme.accent : state.theme.textSecondary
            )
        }
    }

    // MARK: - Findings Breakdown

    private var findingsSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("ОБНАРУЖЕННЫЕ УГРОЗЫ")
                .font(.system(size: 12, weight: .bold))
                .foregroundColor(state.theme.textSecondary)

            VStack(spacing: 8) {
                ForEach(report.findings) { finding in
                    HStack(spacing: 12) {
                        Circle()
                            .fill(severityColor(finding.severity))
                            .frame(width: 8, height: 8)

                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Text(finding.kind)
                                    .font(.system(size: 14, weight: .semibold))
                                    .foregroundColor(state.theme.textPrimary)
                                Spacer()
                                if finding.blocked {
                                    Text("Заблокировано")
                                        .font(.system(size: 11, weight: .bold))
                                        .foregroundColor(ArcaTheme.cleanGreen)
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 2)
                                        .background(ArcaTheme.cleanGreen.opacity(0.15))
                                        .clipShape(Capsule())
                                }
                            }

                            Text(finding.detail)
                                .font(.system(size: 12, weight: .regular, design: .monospaced))
                                .foregroundColor(state.theme.textSecondary)
                                .lineLimit(2)
                        }
                    }
                    .padding(14)
                    .background(state.theme.surface)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                }
            }
        }
    }

    // MARK: - Macros Section

    private var macrosSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("МАКРОСЫ (VBA / XLM)")
                .font(.system(size: 12, weight: .bold))
                .foregroundColor(state.theme.textSecondary)

            VStack(spacing: 8) {
                ForEach(report.macros) { macro in
                    Button {
                        state.inspectedMacro = macro
                    } label: {
                        HStack(spacing: 12) {
                            ArcaGlyph(.macro, size: 20, color: ArcaTheme.activeRed)

                            VStack(alignment: .leading, spacing: 2) {
                                Text(macro.name)
                                    .font(.system(size: 14, weight: .semibold))
                                    .foregroundColor(state.theme.textPrimary)

                                Text("\(macro.language) • Посмотреть исходный код")
                                    .font(.system(size: 12, weight: .medium))
                                    .foregroundColor(state.theme.accent)
                            }

                            Spacer()

                            ArcaGlyph(.chevronRight, size: 14, color: state.theme.textSecondary)
                        }
                        .padding(14)
                        .background(state.theme.surface)
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                    }
                }
            }
        }
    }

    // MARK: - Metadata Section

    private var metadataSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("КРИМИНАЛИСТИЧЕСКИЕ МЕТАДАННЫЕ")
                .font(.system(size: 12, weight: .bold))
                .foregroundColor(state.theme.textSecondary)

            VStack(spacing: 1) {
                ForEach(report.meta) { meta in
                    HStack {
                        Text(meta.name.isEmpty ? meta.key : meta.name)
                            .font(.system(size: 13, weight: .medium))
                            .foregroundColor(state.theme.textSecondary)
                        Spacer()
                        Text(meta.value)
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundColor(state.theme.textPrimary)
                            .lineLimit(1)
                    }
                    .padding(12)
                    .background(state.theme.surface)
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 12))
        }
    }

    // MARK: - Diagnostics Footer

    private var diagnosticsFooter: some View {
        VStack(spacing: 4) {
            Text("SHA-256: \(report.sha256.prefix(16))…\(report.sha256.suffix(8))")
                .font(.system(size: 11, weight: .regular, design: .monospaced))
                .foregroundColor(state.theme.textSecondary)

            Text("Песочница: \(diagnostics.wallMs) мс • Память: \(diagnostics.peakMemory >> 20) МиБ")
                .font(.system(size: 11, weight: .medium))
                .foregroundColor(state.theme.textSecondary)
        }
        .padding(.top, 12)
    }

    // MARK: - Helpers

    private func verdictColor(_ v: Verdict) -> Color {
        switch v {
        case .clean: return ArcaTheme.cleanGreen
        case .hiddenTrackers: return ArcaTheme.trackerAmber
        case .activeNeutralized: return ArcaTheme.activeRed
        case .rejected: return ArcaTheme.rejectedCrimson
        case .unverified: return ArcaTheme.unverifiedSlate
        }
    }

    private func verdictGlyph(_ v: Verdict) -> ArcaGlyphKind {
        switch v {
        case .clean: return .shieldCheck
        case .hiddenTrackers, .unverified: return .shieldAlert
        case .activeNeutralized, .rejected: return .shieldCross
        }
    }

    private func verdictSubtitle(_ v: Verdict) -> String {
        switch v {
        case .clean: return "Проверено целиком, угроз не обнаружено"
        case .hiddenTrackers: return "Обнаружены и нейтрализованы внешние трекеры"
        case .activeNeutralized: return "Обезврежен активный исполняемый код"
        case .rejected: return "Файл опасен и не может быть безопасно открыт"
        case .unverified: return "Часть файла не удалось верифицировать"
        }
    }

    private func severityColor(_ sev: String) -> Color {
        switch sev {
        case "Critical", "High": return ArcaTheme.activeRed
        case "Medium": return ArcaTheme.trackerAmber
        case "Low": return ArcaTheme.cleanGreen
        default: return state.theme.textSecondary
        }
    }
}

private struct CounterCard: View {
    let title: String
    let count: Int64
    let color: Color
    @EnvironmentObject private var state: AppState

    var body: some View {
        VStack(spacing: 6) {
            Text("\(count)")
                .font(.system(size: 22, weight: .bold, design: .rounded))
                .foregroundColor(color)
            Text(title)
                .font(.system(size: 11, weight: .medium))
                .foregroundColor(state.theme.textSecondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 14)
        .background(state.theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 14))
    }
}
