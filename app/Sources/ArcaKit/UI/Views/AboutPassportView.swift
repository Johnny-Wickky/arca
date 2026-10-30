// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI

/// Patron Passport & creator attribution view complying with AGPLv3 section 7(b).
public struct AboutPassportView: View {
    @EnvironmentObject private var state: AppState
    @State private var about: AboutInfo?
    @State private var passport: PassportInfo?
    @State private var isLicenseSheetPresented = false
    @State private var licenseText: String = ""
    @State private var weeklySupportReminders = SupportReminderSetting.isEnabled()

    public init() {}

    public var body: some View {
        ZStack {
            state.theme.background.ignoresSafeArea()

            VStack(spacing: 0) {
                // Header
                HStack {
                    Button {
                        state.currentScreen = .idle
                        ArcaHaptics.selection()
                    } label: {
                        HStack(spacing: 6) {
                            ArcaGlyph(.arrowLeft, size: 16, color: state.theme.accent)
                            Text("Назад")
                                .font(.system(size: 15, weight: .semibold))
                                .foregroundColor(state.theme.accent)
                        }
                    }

                    Spacer()

                    Text("Паспорт и Создатели")
                        .font(.system(size: 16, weight: .bold))
                        .foregroundColor(state.theme.textPrimary)

                    Spacer()

                    // Balance layout
                    Color.clear.frame(width: 60, height: 20)
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 14)
                .background(state.theme.surface)

                ScrollView {
                    VStack(spacing: 24) {
                        // Guardian Passport Card
                        if let p = passport {
                            passportCard(p)
                        }

                        // Creator Attribution Cards
                        if let a = about {
                            attributionSection(a)
                        }

                        // Theme Switcher Section
                        themeSelectorSection

                        supportReminderSection

                        // Zero Telemetry & Legal
                        legalSection
                    }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 16)
                }
            }
        }
        .onAppear {
            loadAttributionData()
        }
        .sheet(isPresented: $isLicenseSheetPresented) {
            licenseView
        }
    }

    // MARK: - Passport Card

    private func passportCard(_ p: PassportInfo) -> some View {
        VStack(spacing: 16) {
            HStack {
                ArcaGlyph(.passport, size: 24, color: state.theme.accent)
                Text(p.title)
                    .font(.system(size: 18, weight: .bold))
                    .foregroundColor(state.theme.textPrimary)
                Spacer()
                Text("\(p.daysInUse) дн.")
                    .font(.system(size: 13, weight: .semibold, design: .monospaced))
                    .foregroundColor(state.theme.textSecondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(state.theme.surfaceSecondary)
                    .clipShape(Capsule())
            }

            Text(p.body)
                .font(.system(size: 14, weight: .regular))
                .foregroundColor(state.theme.textSecondary)
                .lineSpacing(4)
                .frame(maxWidth: .infinity, alignment: .leading)

            HStack(spacing: 12) {
                StatBadge(title: "Документов", value: "\(p.documentsOpened)", color: state.theme.textPrimary)
                StatBadge(title: "Трекеров", value: "\(p.trackersBlocked)", color: ArcaTheme.trackerAmber)
                StatBadge(title: "Обезврежено", value: "\(p.activeContentNeutralized)", color: ArcaTheme.activeRed)
            }
        }
        .padding(20)
        .background(state.theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 18))
    }

    // MARK: - Creator Attribution Cards (AGPLv3 §7)

    private func attributionSection(_ a: AboutInfo) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("СОЗДАТЕЛИ И АТРИБУЦИЯ")
                .font(.system(size: 11, weight: .bold))
                .foregroundColor(state.theme.textSecondary)

            ForEach(a.screen.cards) { card in
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(card.title)
                                .font(.system(size: 16, weight: .bold))
                                .foregroundColor(state.theme.textPrimary)

                            Text(card.project)
                                .font(.system(size: 12, weight: .medium))
                                .foregroundColor(state.theme.textSecondary)
                        }

                        Spacer()

                        if card.isUpstream {
                            Text("Upstream")
                                .font(.system(size: 11, weight: .bold, design: .monospaced))
                                .foregroundColor(ArcaTheme.cleanGreen)
                                .padding(.horizontal, 8)
                                .padding(.vertical, 4)
                                .background(ArcaTheme.cleanGreen.opacity(0.15))
                                .clipShape(Capsule())
                        }
                    }

                    // People
                    HStack(spacing: 8) {
                        ForEach(card.people) { person in
                            Text(person.name)
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundColor(state.theme.textPrimary)
                                .padding(.horizontal, 10)
                                .padding(.vertical, 4)
                                .background(state.theme.surfaceSecondary)
                                .clipShape(Capsule())
                        }
                    }

                    // Support Links (Strictly User-Initiated)
                    if !card.support.isEmpty {
                        Divider().background(state.theme.surfaceSecondary)

                        HStack(spacing: 8) {
                            ForEach(card.support) { link in
                                Button {
                                    if let url = URL(string: link.url) {
                                        UIApplication.shared.open(url)
                                    }
                                } label: {
                                    HStack(spacing: 6) {
                                        ArcaGlyph(.externalLink, size: 14, color: state.theme.accent)
                                        Text("\(card.supportLabel): \(link.label)")
                                            .font(.system(size: 13, weight: .semibold))
                                            .foregroundColor(state.theme.accent)
                                    }
                                }
                            }
                        }
                    }
                }
                .padding(18)
                .background(state.theme.surface)
                .clipShape(RoundedRectangle(cornerRadius: 16))
            }
        }
    }

    // MARK: - Theme Selector

    private var themeSelectorSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("ДИЗАЙН-СИСТЕМА И ТЕМЫ")
                .font(.system(size: 11, weight: .bold))
                .foregroundColor(state.theme.textSecondary)

            VStack(spacing: 8) {
                ForEach(ArcaTheme.allCases) { th in
                    Button {
                        state.theme = th
                        ArcaHaptics.selection()
                    } label: {
                        HStack {
                            Circle()
                                .fill(th.background)
                                .frame(width: 20, height: 20)
                                .overlay(Circle().stroke(state.theme.surfaceSecondary, lineWidth: 2))

                            Text(th.rawValue)
                                .font(.system(size: 14, weight: state.theme == th ? .bold : .medium))
                                .foregroundColor(state.theme.textPrimary)

                            Spacer()

                            if state.theme == th {
                                ArcaGlyph(.check, size: 16, color: state.theme.accent)
                            }
                        }
                        .padding(14)
                        .background(state.theme.surface)
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                    }
                }
            }
        }
    }

    // MARK: - Support reminders

    private var supportReminderSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Toggle(isOn: $weeklySupportReminders) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Еженедельные напоминания о поддержке / Weekly support reminders")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundColor(state.theme.textPrimary)
                    Text("Показывать ненавязчивую карточку раз в 7 дней на главном экране / Show a non-intrusive card once every 7 days on the home screen")
                        .font(.system(size: 12))
                        .foregroundColor(state.theme.textSecondary)
                }
            }
            .onChange(of: weeklySupportReminders) { on in
                SupportReminderSetting.set(on)
            }
            .padding(14)
            .background(state.theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: 12))
        }
    }

    // MARK: - Legal Section

    private var legalSection: some View {
        VStack(spacing: 12) {
            Button {
                // The engine is an actor: asking it for the text is asynchronous.
                Task {
                    if let lic = try? await state.engine.licenseText(which: 0) {
                        licenseText = lic
                        isLicenseSheetPresented = true
                    }
                }
            } label: {
                Text("Лицензия GNU AGPLv3 и дополнительные условия")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(state.theme.accent)
                    .underline()
            }

            Text("Arca категорически не имеет доступа к сети, аналитики, рекламы и самозагрузчиков.")
                .font(.system(size: 11, weight: .regular))
                .foregroundColor(state.theme.textSecondary)
                .multilineTextAlignment(.center)
        }
        .padding(.vertical, 16)
    }

    // MARK: - License Modal

    private var licenseView: some View {
        NavigationView {
            ZStack {
                state.theme.background.ignoresSafeArea()

                ScrollView {
                    Text(licenseText)
                        .font(.system(size: 11, weight: .regular, design: .monospaced))
                        .foregroundColor(state.theme.textPrimary)
                        .padding(20)
                }
            }
            .navigationTitle("Лицензия")
            .navigationBarTitleDisplayMode(.inline)
        }
    }

    private func loadAttributionData() {
        Task {
            if let aboutData = try? await state.engine.about(isRussian: true) {
                self.about = aboutData
            }
            let record = await state.patronStore.loadRecord()
            if let passportData = try? await state.engine.passport(record: record, isRussian: true) {
                self.passport = passportData
            }
        }
    }
}

private struct StatBadge: View {
    let title: String
    let value: String
    let color: Color
    @EnvironmentObject private var state: AppState

    var body: some View {
        VStack(spacing: 4) {
            Text(value)
                .font(.system(size: 18, weight: .bold, design: .rounded))
                .foregroundColor(color)
            Text(title)
                .font(.system(size: 11, weight: .medium))
                .foregroundColor(state.theme.textSecondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 10)
        .background(state.theme.surfaceSecondary)
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }
}
