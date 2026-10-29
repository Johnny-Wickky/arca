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
        // Visio: the text, data, links and connections of the shapes (the page view draws the geometry)
        "Vsdx", "Vsdm", "VisioTemplateOrStencil",
        // OpenDocument
        "Odt", "Ods", "Odp",
        // Legacy binary Office, PDF and Outlook messages: text extraction, nothing is rendered
        "Doc", "Xls", "Ppt", "Pdf", "Msg",
        // Binary Visio: the text of the shapes of each page
        "Vsd",
        // Access: the tables, schema, indexes and relationships as grids (the page view draws the relationship diagram); saved queries as rebuilt SQL; forms, reports, macros and modules are only listed
        "AccessJet3", "AccessJet4", "AccessAce",
        // Pictures: decoded in the sandbox, reduced, without their metadata
        "Png", "Jpeg", "Gif", "Bmp",
        // Text
        "Rtf", "PlainText", "Csv", "Tsv", "Markdown");

    /** Visio formats: drawn flat, as the specification describes the shapes (there is no 3-D scene). */
    private static final Set<String> VISIO = set("Vsdx", "Vsdm", "VisioTemplateOrStencil", "Vsd");

    /** Formats that can be drawn page by page: PDF, slides, sheets, Visio drawings and the relationship diagram of Access databases. */
    private static final Set<String> RENDER = set(
        "Pdf",
        "Pptx", "Pptm", "Ppsx", "Ppsm", "Potx", "Potm", "Odp",
        "Xlsx", "Xlsm", "Xltx", "Xltm", "Ods", "Xls",
        "Vsdx", "Vsdm", "VisioTemplateOrStencil", "Vsd",
        "AccessJet3", "AccessJet4", "AccessAce");

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
        "Doc", "Xls", "Ppt", "Rtf",
        // a picture file comes out as a new PNG written from the decoded pixels
        "Png", "Jpeg", "Gif", "Bmp");

    /** Whether {@code format} is a picture file (PNG, JPEG, GIF, BMP). */
    public static boolean isPicture(String format) {
        return "Png".equals(format) || "Jpeg".equals(format) || "Gif".equals(format) || "Bmp".equals(format);
    }

    /** Whether the engine can build a safe preview of {@code format}. */
    public static boolean canPreview(String format) {
        return PREVIEW.contains(format);
    }

    /** Whether the engine can draw {@code format} page by page. */
    public static boolean canRender(String format) {
        return RENDER.contains(format);
    }

    /** Whether {@code format} is a Visio drawing (its page view says that 3-D effects are not drawn). */
    public static boolean isVisio(String format) {
        return VISIO.contains(format);
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
