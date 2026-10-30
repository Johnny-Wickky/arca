// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation

/// Security verdict reached by the WebAssembly X-Ray parser.
public enum Verdict: String, Codable, Sendable, Equatable, Hashable {
    /// Document is entirely verified clean: no active content or hidden trackers.
    case clean = "Clean"
    /// External links, remote templates, or tracking beacons were detected and neutralized.
    case hiddenTrackers = "HiddenTrackers"
    /// Active payload (VBA macro, XLM 4.0, DDE, Equation Editor exploit) was detected.
    case activeNeutralized = "ActiveContentNeutralized"
    /// Document is hostile or structured as an attack (bomb, zip slip, masked executable).
    case rejected = "Rejected"
    /// Some parts of the document could not be fully parsed or verified (e.g. encrypted).
    case unverified = "Unverified"

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let wire = try container.decode(String.self)
        switch wire {
        case "Clean": self = .clean
        case "HiddenTrackers": self = .hiddenTrackers
        case "ActiveContentNeutralized": self = .activeNeutralized
        case "Rejected": self = .rejected
        default:
            // Security invariant: never fall back to clean by accident
            self = .unverified
        }
    }

    /// User-facing title in Russian.
    public var titleRu: String {
        switch self {
        case .clean: return "Чистый"
        case .hiddenTrackers: return "Скрытые трекеры"
        case .activeNeutralized: return "Активное содержимое"
        case .rejected: return "Отклонён"
        case .unverified: return "Не проверено"
        }
    }

    /// User-facing title in English.
    public var titleEn: String {
        switch self {
        case .clean: return "Clean"
        case .hiddenTrackers: return "Hidden Trackers"
        case .activeNeutralized: return "Active Content Neutralized"
        case .rejected: return "Rejected"
        case .unverified: return "Unverified"
        }
    }
}

/// An individual security finding detected during document analysis.
public struct Finding: Codable, Sendable, Equatable, Identifiable {
    public var id: String { "\(location):\(kind):\(detail)" }

    /// Finding kind identifier (e.g. "ExternalImage", "VbaMacro", "UncPath").
    public let kind: String
    /// Broad category (e.g. "RemoteContent", "ActiveCode", "PrivacyLeak").
    public let category: String
    /// Severity level ("Critical", "High", "Medium", "Low", "Info").
    public let severity: String
    /// Container internal location (e.g. "word/_rels/document.xml.rels").
    public let location: String
    /// Human-readable explanation or sanitized destination URL.
    public let detail: String
    /// Whether the element was actively blocked from execution/display.
    public let blocked: Bool

    enum CodingKeys: String, CodingKey {
        case kind
        case category = "cat"
        case severity = "sev"
        case location = "loc"
        case detail
        case blocked
    }

    /// Severity rank from 0 (Info) to 4 (Critical).
    public var severityRank: Int {
        switch severity {
        case "Critical": return 4
        case "High": return 3
        case "Medium": return 2
        case "Low": return 1
        default: return 0
        }
    }
}

/// Extracted macro module source code.
///
/// Displayed read-only for inspection; never executed.
public struct MacroModule: Codable, Sendable, Equatable, Identifiable {
    public var id: String { "\(location):\(name)" }

    /// Macro language (e.g. "Vba", "Xlm").
    public let language: String
    /// Location inside the container.
    public let location: String
    /// Module name.
    public let name: String
    /// De-obfuscated source code text.
    public let source: String
    /// True if the source exceeded size budget and was safely truncated.
    public let truncated: Bool
    /// Auto-execution entry points identified (e.g. `AutoOpen`, `Workbook_Open`).
    public let autoExec: [String]
    /// Suspicious API calls identified (e.g. `Shell`, `CreateObject`).
    public let suspicious: [String]
    /// Indicators of Compromise found in the macro code (IPs, domains).
    public let iocs: [String]
    /// What the code does, judged from its text: "Benign", "Suspicious" or "Malicious".
    /// A report from an engine that does not say is read as "Suspicious", never as benign.
    public let intent: String
    /// Names of the procedures of the module, as written.
    public let procs: [String]
    /// What the code does, the most serious first.
    public let behaviors: [MacroBehavior]

    enum CodingKeys: String, CodingKey {
        case language = "lang"
        case location = "loc"
        case name
        case source
        case truncated
        case autoExec
        case suspicious
        case iocs
        case intent
        case procs
        case behaviors
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        language = try c.decode(String.self, forKey: .language)
        location = try c.decode(String.self, forKey: .location)
        name = try c.decode(String.self, forKey: .name)
        source = try c.decode(String.self, forKey: .source)
        truncated = try c.decode(Bool.self, forKey: .truncated)
        autoExec = try c.decode([String].self, forKey: .autoExec)
        suspicious = try c.decode([String].self, forKey: .suspicious)
        iocs = try c.decode([String].self, forKey: .iocs)
        intent = try c.decodeIfPresent(String.self, forKey: .intent) ?? "Suspicious"
        procs = try c.decodeIfPresent([String].self, forKey: .procs) ?? []
        behaviors = try c.decodeIfPresent([MacroBehavior].self, forKey: .behaviors) ?? []
    }
}

/// One thing a macro does: what, in which procedure, and the line of source that shows it.
public struct MacroBehavior: Codable, Sendable, Equatable, Identifiable {
    public var id: String { "\(kind):\(proc):\(detail)" }

    /// The engine's behaviour name, e.g. "RunsProgram".
    public let kind: String
    /// The procedure it is in ("" outside any procedure).
    public let proc: String
    /// The line of source that shows it, or a short description.
    public let detail: String
}

/// Blocked external reference or hyperlink.
public struct ExternalRef: Codable, Sendable, Equatable, Identifiable {
    public var id: String { "\(location):\(target)" }

    /// Target destination URL or path.
    public let target: String
    /// Kind of reference (e.g. "Image", "Hyperlink", "OleObject").
    public let kind: String
    /// Classification (e.g. "Web", "Unc").
    public let classification: String
    /// Location inside container.
    public let location: String
    /// Whether the reference would have been triggered automatically.
    public let isAuto: Bool

    enum CodingKeys: String, CodingKey {
        case target
        case kind
        case classification = "class"
        case location = "loc"
        case isAuto = "auto"
    }
}

/// Extracted forensic document metadata.
public struct DocMetadata: Codable, Sendable, Equatable, Identifiable {
    public var id: String { "\(key):\(name):\(source)" }

    /// Standardized metadata key (e.g. "Author", "Company", "Printer").
    public let key: String
    /// Descriptive field name.
    public let name: String
    /// Field value.
    public let value: String
    /// Source part inside container.
    public let source: String

    enum CodingKeys: String, CodingKey {
        case key
        case name
        case value
        case source = "src"
    }
}

/// Embedded object or package inside the document.
public struct EmbeddedObject: Codable, Sendable, Equatable, Identifiable {
    public var id: String { "\(location):\(sha256)" }

    /// Part location inside container.
    public let location: String
    /// Object kind (e.g. "Package", "Ole10Native").
    public let kind: String
    /// Class name.
    public let className: String
    /// CLSID identifier.
    public let clsid: String
    /// Embedded filename if declared.
    public let file: String
    /// Raw byte size.
    public let size: Int64
    /// SHA-256 of the embedded object.
    public let sha256: String

    enum CodingKeys: String, CodingKey {
        case location = "loc"
        case kind
        case className = "class"
        case clsid
        case file
        case size
        case sha256
    }
}

/// Threat X-Ray report produced by Arca core.
public struct Report: Codable, Sendable, Equatable {
    /// Detected document format (e.g. "Docx", "Pdf", "Xlsx").
    public let format: String
    /// High-level format family (e.g. "Text", "Spreadsheet", "Presentation").
    public let family: String
    /// Overall security verdict.
    public let verdict: Verdict
    /// Reason string if document was rejected, empty otherwise.
    public let rejection: String
    /// SHA-256 of the document.
    public let sha256: String

    /// Number of blocked trackers.
    public let trackers: Int64
    /// Number of active content payloads neutralized.
    public let active: Int64
    /// Total findings count.
    public let findingCount: Int64

    /// Detailed security findings.
    public let findings: [Finding]
    /// Extracted macro modules.
    public let macros: [MacroModule]
    /// Blocked external references.
    public let refs: [ExternalRef]
    /// Forensic metadata entries.
    public let meta: [DocMetadata]
    /// Embedded objects.
    public let embedded: [EmbeddedObject]

    /// Container parts parsed.
    public let parts: Int64
    /// XML events processed.
    public let xmlEvents: Int64
    /// Total decompressed byte size.
    public let inflated: Int64
    /// Deepest container nesting level.
    public let nesting: Int64

    private enum CodingKeys: String, CodingKey {
        case format, family, verdict, rejection, sha256
        case counters, findings, macros, refs, meta, embedded, stats
    }

    private struct Counters: Codable {
        let trackers: Int64
        let active: Int64
        let findings: Int64
    }

    private struct Stats: Codable {
        let parts: Int64
        let xmlEvents: Int64
        let inflated: Int64
        let nesting: Int64
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        format = try c.decodeIfPresent(String.self, forKey: .format) ?? "Unknown"
        family = try c.decodeIfPresent(String.self, forKey: .family) ?? "None"
        verdict = try c.decodeIfPresent(Verdict.self, forKey: .verdict) ?? .unverified
        rejection = try c.decodeIfPresent(String.self, forKey: .rejection) ?? ""
        sha256 = try c.decodeIfPresent(String.self, forKey: .sha256) ?? ""

        let counters = try c.decodeIfPresent(Counters.self, forKey: .counters)
        trackers = counters?.trackers ?? 0
        active = counters?.active ?? 0
        findingCount = counters?.findings ?? 0

        findings = try c.decodeIfPresent([Finding].self, forKey: .findings) ?? []
        macros = try c.decodeIfPresent([MacroModule].self, forKey: .macros) ?? []
        refs = try c.decodeIfPresent([ExternalRef].self, forKey: .refs) ?? []
        meta = try c.decodeIfPresent([DocMetadata].self, forKey: .meta) ?? []
        embedded = try c.decodeIfPresent([EmbeddedObject].self, forKey: .embedded) ?? []

        let stats = try c.decodeIfPresent(Stats.self, forKey: .stats)
        parts = stats?.parts ?? 0
        xmlEvents = stats?.xmlEvents ?? 0
        inflated = stats?.inflated ?? 0
        nesting = stats?.nesting ?? 0
    }

    /// Whether this document is encrypted and can be unlocked with a user password.
    public var needsPassword: Bool {
        if format == "EncryptedOoxml" { return true }
        if format == "Pdf" {
            return findings.contains { $0.kind == "EncryptedDocument" }
        }
        return false
    }

    /// Highest severity rank among all findings (0 to 4).
    public var topSeverityRank: Int {
        findings.reduce(0) { max($0, $1.severityRank) }
    }
}
