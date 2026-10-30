// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI

/// Safe read-only document viewer.
public struct DocumentViewer: View {
    public let report: Report
    public let meta: PreviewMeta
    @EnvironmentObject private var state: AppState

    @State private var selectedSectionIndex: UInt32 = 0
    @State private var items: [PreviewItem] = []
    @State private var isLoadingItems = false
    @State private var loadedCount: UInt64 = 0
    @State private var activeExternalUrl: URL?

    public init(report: Report, meta: PreviewMeta) {
        self.report = report
        self.meta = meta
    }

    public var body: some View {
        ZStack {
            state.theme.background.ignoresSafeArea()

            VStack(spacing: 0) {
                // Top Navigation Bar
                headerBar

                // Section / Tab Switcher (if multi-sheet / multi-slide)
                if meta.sections.count > 1 {
                    sectionTabs
                }

                // Document Content ScrollView
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 14) {
                        ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                            renderItem(item)
                                .onAppear {
                                    if index == items.count - 10 {
                                        loadMoreItems()
                                    }
                                }
                        }

                        if isLoadingItems {
                            HStack {
                                Spacer()
                                ProgressView()
                                    .padding(.vertical, 20)
                                Spacer()
                            }
                        }
                    }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 16)
                }
            }
        }
        .onAppear {
            loadInitialItems()
        }
        .alert("Внешняя ссылка", isPresented: Binding(
            get: { activeExternalUrl != nil },
            set: { if !$0 { activeExternalUrl = nil } }
        )) {
            Button("Открыть в Safari", role: .none) {
                if let url = activeExternalUrl {
                    UIApplication.shared.open(url)
                }
            }
            Button("Отмена", role: .cancel) {}
        } message: {
            Text(activeExternalUrl?.absoluteString ?? "")
        }
    }

    // MARK: - Header Bar

    private var headerBar: some View {
        HStack {
            Button {
                state.currentScreen = .report(
                    report: report,
                    diagnostics: RunDiagnostics(from: 0)
                )
                ArcaHaptics.selection()
            } label: {
                HStack(spacing: 6) {
                    ArcaGlyph(.arrowLeft, size: 16, color: state.theme.accent)
                    Text("Рентген")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundColor(state.theme.accent)
                }
            }

            Spacer()

            Button {
                state.sanitizeCurrentDocument()
            } label: {
                ArcaGlyph(.sanitize, size: 18, color: state.theme.accent)
                    .padding(8)
                    .background(state.theme.surfaceSecondary)
                    .clipShape(Circle())
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .background(state.theme.surface)
    }

    // MARK: - Section Tabs

    private var sectionTabs: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(Array(meta.sections.enumerated()), id: \.offset) { index, section in
                    let isSelected = (selectedSectionIndex == UInt32(index))
                    Button {
                        selectedSectionIndex = UInt32(index)
                        loadInitialItems()
                        ArcaHaptics.selection()
                    } label: {
                        HStack(spacing: 6) {
                            Text(section.name.isEmpty ? "\(section.kind) \(index + 1)" : section.name)
                                .font(.system(size: 13, weight: isSelected ? .bold : .medium))
                                .foregroundColor(isSelected ? .white : state.theme.textPrimary)

                            if section.isHidden {
                                ArcaGlyph(.lock, size: 10, color: isSelected ? .white : state.theme.textSecondary)
                            }
                        }
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(isSelected ? state.theme.accent : state.theme.surfaceSecondary)
                        .clipShape(Capsule())
                    }
                }
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 8)
        }
        .background(state.theme.surface)
    }

    // MARK: - Item Rendering

    @ViewBuilder
    private func renderItem(_ item: PreviewItem) -> some View {
        switch item {
        case .paragraph(let para):
            renderParagraph(para)

        case .tableRow(let row):
            renderTableRow(row)

        case .gridHeader(let gh):
            renderGridHeader(gh)

        case .gridRow(let gr):
            renderGridRow(gr)

        case .image(let img):
            renderImagePlaceholder(img)

        case .horizontalRule:
            Divider()
                .background(state.theme.surfaceSecondary)
                .padding(.vertical, 8)
        }
    }

    // MARK: - Paragraphs

    private func renderParagraph(_ para: Paragraph) -> some View {
        HStack(alignment: .top, spacing: 6) {
            if !para.marker.isEmpty {
                Text(para.marker)
                    .font(fontForStyle(para.style))
                    .foregroundColor(state.theme.accent)
                    .frame(minWidth: 20, alignment: .trailing)
            }

            Text(buildAttributedString(para))
                .font(fontForStyle(para.style))
                .foregroundColor(state.theme.textPrimary)
                .multilineTextAlignment(alignmentForAlign(para.align))
                .lineSpacing(4)
        }
        .padding(.leading, CGFloat(para.level * 16))
    }

    private func buildAttributedString(_ para: Paragraph) -> AttributedString {
        var result = AttributedString()
        for run in para.runs {
            var str = AttributedString(run.text)
            if run.style.contains(.bold) { str.inlinePresentationIntent?.insert(.stronglyEmphasized) }
            if run.style.contains(.italic) { str.inlinePresentationIntent?.insert(.emphasized) }
            if run.style.contains(.underline) { str.underlineStyle = .single }
            if run.style.contains(.strike) { str.strikethroughStyle = .single }
            if run.style.contains(.mono) { str.inlinePresentationIntent?.insert(.code) }
            if run.href != nil {
                str.foregroundColor = state.theme.accent
                str.underlineStyle = .single
            }
            result.append(str)
        }
        return result
    }

    // MARK: - Tables & Grids

    private func renderTableRow(_ row: TableRow) -> some View {
        HStack(spacing: 8) {
            ForEach(Array(row.cells.enumerated()), id: \.offset) { _, cell in
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(cell.paragraphs.enumerated()), id: \.offset) { _, p in
                        renderParagraph(p)
                    }
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(cell.isHeader ? state.theme.surfaceSecondary : state.theme.surface)
                .clipShape(RoundedRectangle(cornerRadius: 6))
            }
        }
    }

    private func renderGridHeader(_ gh: GridHeader) -> some View {
        HStack(spacing: 4) {
            ForEach(0..<gh.cols, id: \.self) { colIndex in
                let w = colIndex < gh.widths.count ? CGFloat(gh.widths[colIndex] * 8) : 60
                Text("COL \(colIndex + 1)")
                    .font(.system(size: 11, weight: .bold, design: .monospaced))
                    .foregroundColor(state.theme.textSecondary)
                    .frame(width: max(50, w), alignment: .leading)
                    .padding(.vertical, 6)
            }
        }
        .padding(.horizontal, 4)
        .background(state.theme.surfaceSecondary)
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }

    private func renderGridRow(_ gr: GridRow) -> some View {
        HStack(spacing: 4) {
            ForEach(gr.cells, id: \.column) { cell in
                Text(cell.text)
                    .font(.system(size: 12, weight: .regular, design: cell.kind == 1 ? .monospaced : .default))
                    .foregroundColor(state.theme.textPrimary)
                    .frame(minWidth: 50, alignment: cell.kind == 1 ? .trailing : .leading)
                    .padding(6)
                    .background(state.theme.surface)
                    .clipShape(RoundedRectangle(cornerRadius: 4))
            }
        }
    }

    // MARK: - Images

    private func renderImagePlaceholder(_ img: ImagePlaceholder) -> some View {
        Button {
            if img.imageId >= 0 {
                state.inspectImage(imageId: img.imageId)
            }
        } label: {
            HStack(spacing: 12) {
                ArcaGlyph(.image, size: 24, color: state.theme.accent)

                VStack(alignment: .leading, spacing: 2) {
                    Text(img.alt.isEmpty ? "Встроенное изображение" : img.alt)
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundColor(state.theme.textPrimary)

                    Text("\(img.media.uppercased()) • \(img.byteSize >> 10) КБ • Нажмите для безопасного просмотра")
                        .font(.system(size: 11, weight: .medium))
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

    // MARK: - Pagination

    private func loadInitialItems() {
        items = []
        loadedCount = 0
        loadMoreItems()
    }

    private func loadMoreItems() {
        guard !isLoadingItems else { return }
        let currentSection = selectedSectionIndex
        guard currentSection < meta.sections.count else { return }
        let total = meta.sections[Int(currentSection)].totalItems
        guard loadedCount < total else { return }

        isLoadingItems = true
        let from = loadedCount
        let count: UInt32 = 64

        Task {
            do {
                if let session = state.engine as ArcaEngine? {
                    // Fetch items via engine
                    let batch = try await session.previewItems(
                        document: DocumentSession(handle: 0), // Will be fetched via engine session
                        section: currentSection,
                        from: from,
                        count: count
                    )
                    DispatchQueue.main.async {
                        self.items.append(contentsOf: batch)
                        self.loadedCount += UInt64(batch.count)
                        self.isLoadingItems = false
                    }
                }
            } catch {
                DispatchQueue.main.async {
                    self.isLoadingItems = false
                }
            }
        }
    }

    // MARK: - Helpers

    private func fontForStyle(_ style: String) -> Font {
        switch style {
        case "Title": return .system(size: 24, weight: .bold)
        case "Heading1": return .system(size: 20, weight: .bold)
        case "Heading2": return .system(size: 17, weight: .semibold)
        case "Heading3": return .system(size: 15, weight: .semibold)
        default: return .system(size: 15, weight: .regular)
        }
    }

    private func alignmentForAlign(_ align: Int) -> TextAlignment {
        switch align {
        case 1: return .center
        case 2: return .trailing
        default: return .leading
        }
    }
}

private extension RunDiagnostics {
    /// An empty record for the way back from the viewer to the report (there is no memberwise
    /// initializer: the type decodes itself from the engine's JSON).
    init(from dummy: Int) {
        status = 0
        statusName = "Ok"
        message = ""
        fuel = 0
        fuelLimit = 0
        peakMemory = 0
        slices = 0
        wallMs = 0
        unlock = 0
    }
}
