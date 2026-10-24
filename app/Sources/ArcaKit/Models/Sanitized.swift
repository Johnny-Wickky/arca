// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation

/// Specific sanitization action executed during sterilization.
public struct SanitizeAction: Codable, Sendable, Equatable, Identifiable {
    public var id: String { "\(kind):\(target)" }

    /// Action kind (e.g. "StripMetadata", "RemoveMacros", "CleanExternalRels").
    public let kind: String
    /// Targeted container element or field.
    public let target: String
}

/// Verification results after sterilizing the document.
///
/// In Arca, a sterile copy is verified by running the complete X-ray detector over it
/// a second time before returning it to the user.
public struct SanitizedVerification: Codable, Sendable, Equatable {
    public let verdict: Verdict
    public let sha256: String
    public let findings: Int64
}

/// Metadata and verification report of a sterile copy.
public struct SanitizedInfo: Codable, Sendable, Equatable {
    public let format: String
    public let size: Int64
    public let actions: [SanitizeAction]
    public let verification: SanitizedVerification

    enum CodingKeys: String, CodingKey {
        case format
        case size
        case actions
        case verification
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        format = try c.decodeIfPresent(String.self, forKey: .format) ?? "Unknown"
        size = try c.decodeIfPresent(Int64.self, forKey: .size) ?? 0
        actions = try c.decodeIfPresent([SanitizeAction].self, forKey: .actions) ?? []
        verification = try c.decode(SanitizedVerification.self, forKey: .verification)
    }
}

/// Complete sterile document result holding metadata and verified in-memory bytes.
public struct SanitizedDocument: Sendable {
    /// Verification and sanitization details.
    public let info: SanitizedInfo
    /// Raw byte payload of the clean document.
    public let bytes: Data

    public init(info: SanitizedInfo, bytes: Data) {
        self.info = info
        self.bytes = bytes
    }

    /// Writes the sterile document to a filesystem URL atomically.
    public func write(to url: URL) throws {
        try bytes.write(to: url, options: .atomic)
    }
}
