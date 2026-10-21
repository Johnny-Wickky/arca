// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.Arrays;
import java.util.Collections;
import java.util.HashSet;
import java.util.Set;

/**
 * What the engine can do with each format, so that the app enables a button only where the
 * operation exists (the engine still answers {@code UNSUPPORTED} cleanly if this list and the
 * engine ever disagree). Keep in sync with {@code crates/arca-preview/src/detect.rs}
 * (previews) and {@code crates/arca-xray/src/sanitize/mod.rs} (sterile copies); the contract
 * test in {@code android/host-test} checks both directions against the real engine.
 */
public final class Capabilities {
    private Capabilities() {}

    private static Set<String> set(String... names) {
        return Collections.unmodifiableSet(new HashSet<>(Arrays.asList(names)));
    }

    private static final Set<String> PREVIEW = set(
        // OOXML
        "Docx", "Docm", "Dotx", "Dotm",
        "Xlsx", "Xlsm", "Xltx", "Xltm", "Xlam",
        "Pptx", "Pptm", "Ppsx", "Ppsm", "Potx", "Potm",
        // Visio: the text, data, links and connections of the shapes; no geometry
        "Vsdx", "Vsdm", "VisioTemplateOrStencil",
        // OpenDocument
        "Odt", "Ods", "Odp",
        // Legacy binary Office, PDF and Outlook messages: text extraction, nothing is rendered
        "Doc", "Xls", "Ppt", "Pdf", "Msg",
        // Binary Visio: the text of the shapes of each page
        "Vsd",
        // Access: the tables, as grids; queries, forms, reports, macros and modules are only listed
        "AccessJet3", "AccessJet4", "AccessAce",
        // Text
        "Rtf", "PlainText", "Csv", "Tsv", "Markdown");

    /** Formats that can be drawn page by page: PDF so far (slides, sheets and pictures follow). */
    private static final Set<String> RENDER = set("Pdf");

    private static final Set<String> SANITIZE = set(
        "Docx", "Docm", "Dotx", "Dotm",
        "Xlsx", "Xlsm", "Xltx", "Xltm", "Xlam",
        "Pptx", "Pptm", "Ppsx", "Ppsm", "Potx", "Potm",
        "Vsdx", "Vsdm", "VisioTemplateOrStencil",
        "Odt", "Ods", "Odp", "Odg", "OdfOther",
        // PDF is rebuilt from its pages
        "Pdf",
        // the binary Office formats and RTF come out as .docx / .xlsx / .pptx written from what
        // the viewer reads of them
        "Doc", "Xls", "Ppt", "Rtf");

    /** Whether the engine can build a safe preview of {@code format}. */
    public static boolean canPreview(String format) {
        return PREVIEW.contains(format);
    }

    /** Whether the engine can draw {@code format} page by page. */
    public static boolean canRender(String format) {
        return RENDER.contains(format);
    }

    /** Formats with a page view (for tests and diagnostics). */
    public static Set<String> renderFormats() {
        return RENDER;
    }

    /** Whether the engine can rebuild {@code format} as a sterile copy. */
    public static boolean canSanitize(String format) {
        return SANITIZE.contains(format);
    }

    /** Formats with a preview (for tests and diagnostics). */
    public static Set<String> previewFormats() {
        return PREVIEW;
    }

    /** Formats with a sterile copy (for tests and diagnostics). */
    public static Set<String> sanitizeFormats() {
        return SANITIZE;
    }
}
