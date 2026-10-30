// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import ArcaCore

/// The constants of `arca.h` in the types the functions use.
///
/// Swift imports a C `enum` that has no fixed underlying type as a struct whose `rawValue` is
/// unsigned, while every `arca_*` function returns its status as `int32_t`; and it imports a
/// `#define` as `Int32`. Converting here, in one place, keeps the rest of the code free of
/// `Int32`/`UInt32` mismatches and does not depend on how the importer chose the type.
enum ArcaStatusCode {
    static let ok = Int32(ARCA_STATUS_OK.rawValue)
    static let badArgument = Int32(ARCA_STATUS_BAD_ARGUMENT.rawValue)
}

/// Flags of `arca_run` (`ARCA_FLAG_*`).
///
/// Each value is built from the single-literal macros: a macro that ORs several others is not
/// always imported into Swift, so `ARCA_FLAG_SANITIZE_ALL` itself is not used.
public enum ArcaFlag {
    /// Sanitize: accept tracked revisions.
    public static let acceptRevisions = Int32(ARCA_FLAG_ACCEPT_REVISIONS)
    /// Sanitize: remove comments.
    public static let removeComments = Int32(ARCA_FLAG_REMOVE_COMMENTS)
    /// Sanitize: strip document properties and personal metadata.
    public static let stripMetadata = Int32(ARCA_FLAG_STRIP_METADATA)
    /// Sanitize: remove custom XML parts.
    public static let removeCustomXML = Int32(ARCA_FLAG_REMOVE_CUSTOM_XML)
    /// Sanitize: everything above (`ARCA_FLAG_SANITIZE_ALL`).
    public static let sanitizeAll = acceptRevisions | removeComments | stripMetadata | removeCustomXML
    /// Preview: also show hidden content (`ARCA_FLAG_PREVIEW_ALL`).
    public static let previewAll = Int32(ARCA_FLAG_REVEAL_HIDDEN)
    /// Render: the low 24 bits of the flags carry the page number.
    public static let pageMask = UInt32(ARCA_FLAG_PAGE_MASK)
}
