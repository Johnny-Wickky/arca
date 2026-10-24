// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation

/// Run style bitflags defined by `arca_preview::model::style`.
public struct RunStyle: OptionSet, Sendable, Hashable {
    public let rawValue: Int

    public init(rawValue: Int) {
        self.rawValue = rawValue
    }

    public static let bold = RunStyle(rawValue: 1)
    public static let italic = RunStyle(rawValue: 2)
    public static let underline = RunStyle(rawValue: 4)
    public static let strike = RunStyle(rawValue: 8)
    public static let mono = RunStyle(rawValue: 16)
    public static let `super` = RunStyle(rawValue: 32)
    public static let sub = RunStyle(rawValue: 64)
    public static let hidden = RunStyle(rawValue: 128)
    public static let deleted = RunStyle(rawValue: 256)
    public static let inserted = RunStyle(rawValue: 512)
}

/// A slice of styled text within a paragraph.
public struct TextRun: Sendable, Hashable, Identifiable {
    public var id: String { "\(text):\(style.rawValue):\(href ?? "")" }

    public let text: String
    public let style: RunStyle
    /// Display-only link destination (never automatically navigated).
    public let href: String?

    public init(text: String, style: RunStyle = [], href: String? = nil) {
        self.text = text
        self.style = style
        self.href = href
    }
}

/// A paragraph of text with formatting and alignment.
public struct Paragraph: Sendable, Hashable {
    /// Named style (e.g. "Body", "Heading1", "Heading2", "Title").
    public let style: String
    /// Text alignment: 0=start, 1=center, 2=end, 3=justify.
    public let align: Int
    /// Indentation or list level (0-based).
    public let level: Int
    /// List bullet marker or numbering (e.g. "1.", "•").
    public let marker: String
    /// Formatted runs.
    public let runs: [TextRun]

    public init(style: String = "Body", align: Int = 0, level: Int = 0, marker: String = "", runs: [TextRun] = []) {
        self.style = style
        self.align = align
        self.level = level
        self.marker = marker
        self.runs = runs
    }

    /// Concatenated plain text content.
    public var plainText: String {
        runs.map(\.text).joined()
    }
}

/// Table cell holding one or more paragraphs.
public struct TableCell: Sendable, Hashable {
    /// Column span (defaults to 1).
    public let span: Int
    /// Whether this cell is a table header.
    public let isHeader: Bool
    /// Paragraphs inside the cell.
    public let paragraphs: [Paragraph]

    public init(span: Int = 1, isHeader: Bool = false, paragraphs: [Paragraph] = []) {
        self.span = span
        self.isHeader = isHeader
        self.paragraphs = paragraphs
    }
}

/// One row of a document table.
public struct TableRow: Sendable, Hashable {
    public let columns: Int
    public let isFirst: Bool
    public let isLast: Bool
    public let cells: [TableCell]

    public init(columns: Int = 1, isFirst: Bool = false, isLast: Bool = false, cells: [TableCell] = []) {
        self.columns = columns
        self.isFirst = isFirst
        self.isLast = isLast
        self.cells = cells
    }
}

/// Header descriptor for a spreadsheet grid.
public struct GridHeader: Sendable, Hashable {
    public let cols: Int
    public let widths: [Int]
    public let totalRows: Int64
    public let isTruncated: Bool

    public init(cols: Int, widths: [Int], totalRows: Int64, isTruncated: Bool) {
        self.cols = cols
        self.widths = widths
        self.totalRows = totalRows
        self.isTruncated = isTruncated
    }
}

/// One individual cell inside a spreadsheet grid row.
public struct GridCell: Sendable, Hashable {
    public let column: Int
    /// Cell data kind (0=text, 1=number, 2=date, 3=bool, 4=error, 5=formula, 6=calculated).
    public let kind: Int
    public let text: String
    public let href: String?

    public init(column: Int, kind: Int, text: String, href: String? = nil) {
        self.column = column
        self.kind = kind
        self.text = text
        self.href = href
    }
}

/// One row in a spreadsheet grid.
public struct GridRow: Sendable, Hashable {
    public let index: Int64
    public let isHidden: Bool
    public let cells: [GridCell]

    public init(index: Int64, isHidden: Bool, cells: [GridCell]) {
        self.index = index
        self.isHidden = isHidden
        self.cells = cells
    }
}

/// Safe placeholder for an embedded image (decoded only on demand inside the sandbox).
public struct ImagePlaceholder: Sendable, Hashable {
    public let alt: String
    public let media: String
    public let byteSize: Int64
    public let isExternal: Bool
    /// Engine identifier to fetch RGBA raster via `ARCA_OP_IMAGE`.
    public let imageId: Int

    public init(alt: String, media: String, byteSize: Int64, isExternal: Bool, imageId: Int) {
        self.alt = alt
        self.media = media
        self.byteSize = byteSize
        self.isExternal = isExternal
        self.imageId = imageId
    }
}

/// Polymorphic renderable item returned in paginated chunks.
public enum PreviewItem: Sendable, Hashable {
    case paragraph(Paragraph)
    case tableRow(TableRow)
    case gridHeader(GridHeader)
    case gridRow(GridRow)
    case image(ImagePlaceholder)
    case horizontalRule
}

// MARK: - JSON Decoding for Preview Items

extension PreviewItem {
    static func parse(from jsonObject: [String: Any]) -> PreviewItem? {
        guard let kind = jsonObject["k"] as? String else { return nil }
        switch kind {
        case "p":
            return .paragraph(parseParagraph(jsonObject))
        case "tr":
            let n = max(1, (jsonObject["n"] as? NSNumber)?.intValue ?? 1)
            let first = (jsonObject["first"] as? Bool) ?? false
            let last = (jsonObject["last"] as? Bool) ?? false
            var cells: [TableCell] = []
            if let cellsArr = jsonObject["cells"] as? [[String: Any]] {
                for c in cellsArr {
                    let span = max(1, (c["c"] as? NSNumber)?.intValue ?? 1)
                    let h = (c["h"] as? Bool) ?? false
                    var paras: [Paragraph] = []
                    if let pArr = c["p"] as? [[String: Any]] {
                        paras = pArr.map(parseParagraph)
                    }
                    cells.append(TableCell(span: span, isHeader: h, paragraphs: paras))
                }
            }
            return .tableRow(TableRow(columns: n, isFirst: first, isLast: last, cells: cells))
        case "gh":
            let cols = (jsonObject["cols"] as? NSNumber)?.intValue ?? 0
            let widths = (jsonObject["w"] as? [NSNumber])?.map(\.intValue) ?? []
            let rows = (jsonObject["rows"] as? NSNumber)?.int64Value ?? 0
            let trunc = (jsonObject["trunc"] as? Bool) ?? false
            return .gridHeader(GridHeader(cols: cols, widths: widths, totalRows: rows, isTruncated: trunc))
        case "gr":
            let idx = (jsonObject["i"] as? NSNumber)?.int64Value ?? 0
            let hidden = (jsonObject["h"] as? Bool) ?? false
            var cells: [GridCell] = []
            if let cArr = jsonObject["c"] as? [[Any]] {
                for rawCell in cArr where rawCell.count >= 3 {
                    let col = (rawCell[0] as? NSNumber)?.intValue ?? 0
                    let cKind = (rawCell[1] as? NSNumber)?.intValue ?? 0
                    let txt = (rawCell[2] as? String) ?? ""
                    let href = rawCell.count > 3 ? (rawCell[3] as? String) : nil
                    cells.append(GridCell(column: col, kind: cKind, text: txt, href: href))
                }
            }
            return .gridRow(GridRow(index: idx, isHidden: hidden, cells: cells))
        case "i":
            let alt = (jsonObject["alt"] as? String) ?? ""
            let media = (jsonObject["media"] as? String) ?? ""
            let bytes = (jsonObject["bytes"] as? NSNumber)?.int64Value ?? 0
            let ext = (jsonObject["ext"] as? Bool) ?? false
            let imgId = (jsonObject["id"] as? NSNumber)?.intValue ?? -1
            return .image(ImagePlaceholder(alt: alt, media: media, byteSize: bytes, isExternal: ext, imageId: imgId))
        case "r":
            return .horizontalRule
        default:
            return nil
        }
    }

    private static func parseParagraph(_ m: [String: Any]) -> Paragraph {
        let style = (m["s"] as? String) ?? "Body"
        let align = (m["a"] as? NSNumber)?.intValue ?? 0
        let level = (m["l"] as? NSNumber)?.intValue ?? 0
        let marker = (m["m"] as? String) ?? ""
        var runs: [TextRun] = []
        if let rawRuns = m["r"] as? [[Any]] {
            for raw in rawRuns where !raw.isEmpty {
                let text = (raw[0] as? String) ?? ""
                let styleBits = raw.count > 1 ? ((raw[1] as? NSNumber)?.intValue ?? 0) : 0
                let href = raw.count > 2 ? (raw[2] as? String) : nil
                runs.append(TextRun(text: text, style: RunStyle(rawValue: styleBits), href: href))
            }
        }
        return Paragraph(style: style, align: align, level: level, marker: marker, runs: runs)
    }
}

// MARK: - Preview Metadata

/// Description of one section (sheet, slide, page, or body).
public struct SectionInfo: Codable, Sendable, Equatable, Identifiable {
    public var id: String { "\(kind):\(name)" }

    /// Section kind (e.g. "Body", "Sheet", "Slide", "Page").
    public let kind: String
    /// User-assigned name (e.g. sheet tab title "Q3 Forecast").
    public let name: String
    /// Whether the section was marked hidden in the document.
    public let isHidden: Bool
    /// Total item count in this section.
    public let totalItems: Int64

    enum CodingKeys: String, CodingKey {
        case kind
        case name
        case isHidden = "hidden"
        case totalItems = "items"
    }
}

/// Notice explaining content that is deliberately simplified or omitted.
public struct Notice: Codable, Sendable, Equatable, Identifiable {
    public var id: String { "\(kind):\(detail)" }

    public let kind: String
    public let count: Int64
    public let detail: String
}

/// Top-level metadata for preview navigation.
public struct PreviewMeta: Codable, Sendable, Equatable {
    public let format: String
    public let family: String
    public let sections: [SectionInfo]
    public let notices: [Notice]
    public let totalChars: Int64
    public let totalItems: Int64
    public let isTruncated: Bool

    private enum CodingKeys: String, CodingKey {
        case format, family, sections, notices, stats
    }

    private struct Stats: Codable {
        let chars: Int64
        let items: Int64
        let truncated: Bool
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        format = try c.decodeIfPresent(String.self, forKey: .format) ?? "Unknown"
        family = try c.decodeIfPresent(String.self, forKey: .family) ?? "None"
        sections = try c.decodeIfPresent([SectionInfo].self, forKey: .sections) ?? []
        notices = try c.decodeIfPresent([Notice].self, forKey: .notices) ?? []

        let st = try c.decodeIfPresent(Stats.self, forKey: .stats)
        totalChars = st?.chars ?? 0
        totalItems = st?.items ?? 0
        isTruncated = st?.truncated ?? false
    }
}
