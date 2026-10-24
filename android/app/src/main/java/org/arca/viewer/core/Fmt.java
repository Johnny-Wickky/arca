// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.Locale;

/**
 * Formatting helpers. Everything that comes from a document (names, URLs, metadata, macro
 * source) is attacker-controlled; {@link #safe(String)} is applied at the display boundary so
 * that it cannot reorder or hide text with invisible characters.
 *
 * <p>No Android dependencies; tested on a desktop JVM.
 */
public final class Fmt {
    private Fmt() {}

    /** Longest string the UI lays out in one piece. */
    public static final int MAX_DISPLAY = 20_000;

    /**
     * Makes untrusted text safe to display: control characters (other than tab and newline) and
     * characters that reorder or hide text are replaced by a visible {@code U+XXXX} marker, so
     * that {@code evil‮txt.exe} cannot masquerade as something else.
     */
    public static String safe(String s) {
        if (s == null || s.isEmpty()) {
            return "";
        }
        int n = Math.min(s.length(), MAX_DISPLAY);
        StringBuilder sb = null;
        for (int i = 0; i < n; i++) {
            char c = s.charAt(i);
            if (isSuspicious(c)) {
                if (sb == null) {
                    sb = new StringBuilder(n + 16);
                    sb.append(s, 0, i);
                }
                sb.append("‹U+").append(String.format(Locale.ROOT, "%04X", (int) c)).append('›');
            } else if (sb != null) {
                sb.append(c);
            }
        }
        String out = sb == null ? s.substring(0, n) : sb.toString();
        return n < s.length() ? out + "…" : out;
    }

    /**
     * The UTF-8 bytes of {@code chars} without going through an immutable String, so that the
     * caller can wipe both arrays. At most 1024 bytes are returned (the protocol's limit).
     */
    public static byte[] utf8(char[] chars) {
        java.nio.ByteBuffer bb = java.nio.charset.StandardCharsets.UTF_8.encode(java.nio.CharBuffer.wrap(chars));
        byte[] out = new byte[Math.min(bb.remaining(), 1024)];
        bb.get(out);
        if (bb.hasArray()) {
            java.util.Arrays.fill(bb.array(), (byte) 0);
        }
        return out;
    }

    /**
     * Whether {@code s} is written like a number: digits with signs, separators, a currency sign, a
     * percent sign, a date or an exponent, and nothing else. Such cells keep to the right edge.
     */
    public static boolean looksNumeric(String s) {
        if (s == null || s.isEmpty()) {
            return false;
        }
        boolean digit = false;
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c >= '0' && c <= '9') {
                digit = true;
            } else if (" \u00a0.,'+-()%/:$\u20ac\u00a3\u00a5\u20bdeE".indexOf(c) < 0) {
                return false;
            }
        }
        return digit;
    }

    private static boolean isSuspicious(char c) {
        if (c == '\t' || c == '\n') {
            return false;
        }
        if (c < 0x20 || (c >= 0x7F && c <= 0x9F)) {
            return true; // C0 / C1 controls (a carriage return can overwrite a line start)
        }
        return c == 0x061C // Arabic letter mark
            || c == 0x180E // Mongolian vowel separator
            || (c >= 0x200B && c <= 0x200F) // zero-width and directional marks
            || (c >= 0x202A && c <= 0x202E) // embeddings and overrides
            || (c >= 0x2060 && c <= 0x2064) // word joiner and invisible operators
            || (c >= 0x2066 && c <= 0x206F) // isolates and deprecated format characters
            || c == 0xFEFF // BOM / zero-width no-break space
            || (c >= 0xFFF9 && c <= 0xFFFB); // interlinear annotation
    }

    /** {@code s} cut to {@code max} characters with an ellipsis (never splits a surrogate pair). */
    public static String clip(String s, int max) {
        if (s == null) {
            return "";
        }
        if (s.length() <= max || max < 2) {
            return s;
        }
        int end = max - 1;
        if (Character.isHighSurrogate(s.charAt(end - 1))) {
            end--;
        }
        return s.substring(0, end) + "…";
    }

    /** {@code s} shortened in the middle (for hashes and long URLs). */
    public static String middle(String s, int max) {
        if (s == null) {
            return "";
        }
        if (s.length() <= max || max < 8) {
            return s;
        }
        int head = (max - 1) / 2;
        int tail = max - 1 - head;
        int a = head;
        if (a > 0 && Character.isHighSurrogate(s.charAt(a - 1))) {
            a--;
        }
        int b = s.length() - tail;
        if (b < s.length() && Character.isLowSurrogate(s.charAt(b))) {
            b++;
        }
        return s.substring(0, a) + "…" + s.substring(b);
    }

    /** A byte count as "12.3 KB" (binary multiples, as file managers show them). */
    public static String size(long bytes) {
        if (bytes < 0) {
            return "?";
        }
        if (bytes < 1024) {
            return bytes + " B";
        }
        String[] units = {"KB", "MB", "GB", "TB"};
        double v = bytes;
        int u = -1;
        do {
            v /= 1024.0;
            u++;
        } while (v >= 1024.0 && u < units.length - 1);
        return String.format(Locale.getDefault(), v < 10 ? "%.2f %s" : v < 100 ? "%.1f %s" : "%.0f %s", v, units[u]);
    }

    /** A hash in groups of eight characters, for reading aloud and comparing. */
    public static String grouped(String hex) {
        if (hex == null || hex.isEmpty()) {
            return "";
        }
        StringBuilder sb = new StringBuilder(hex.length() + hex.length() / 8);
        for (int i = 0; i < hex.length(); i++) {
            if (i > 0 && i % 8 == 0) {
                sb.append(' ');
            }
            sb.append(hex.charAt(i));
        }
        return sb.toString();
    }

    /**
     * Splits long text at line breaks into rows of at most {@code max} characters (long rows are
     * slow to lay out). Nothing is dropped: concatenating the rows gives the text back, and a
     * single line longer than {@code max} is cut into several rows.
     */
    public static java.util.List<String> chunks(String text, int max) {
        java.util.List<String> out = new java.util.ArrayList<>();
        if (text == null || text.isEmpty() || max < 2) {
            return out;
        }
        StringBuilder cur = new StringBuilder();
        int start = 0;
        int len = text.length();
        while (start < len) {
            int end = text.indexOf('\n', start);
            end = end < 0 ? len : end + 1;
            if (end - start > max) {
                end = start + max;
                if (Character.isHighSurrogate(text.charAt(end - 1))) {
                    end--; // never split a surrogate pair
                }
            }
            if (cur.length() > 0 && cur.length() + (end - start) > max) {
                out.add(cur.toString());
                cur.setLength(0);
            }
            cur.append(text, start, end);
            start = end;
        }
        if (cur.length() > 0) {
            out.add(cur.toString());
        }
        return out;
    }

    /** Days since 1970-01-01 in the zone with the given offset from UTC (for the Patron Passport). */
    public static int epochDay(long nowMillis, int zoneOffsetMillis) {
        long local = nowMillis + zoneOffsetMillis;
        long day = Math.floorDiv(local, 86_400_000L);
        return (int) Math.max(0L, Math.min(day, Integer.MAX_VALUE));
    }

    /**
     * A file name for the sterile copy: the source name made safe, with {@code .sterile} before
     * the extension. Path separators, controls, direction marks and reserved characters are
     * replaced, the length is capped, and an empty result becomes {@code document}.
     */
    public static String sterileName(String source, String extension) {
        String base = source == null ? "" : source;
        int slash = Math.max(base.lastIndexOf('/'), base.lastIndexOf('\\'));
        if (slash >= 0) {
            base = base.substring(slash + 1);
        }
        int dot = base.lastIndexOf('.');
        if (dot > 0) {
            base = base.substring(0, dot);
        }
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < base.length() && sb.length() < 80; i++) {
            char c = base.charAt(i);
            if (isSuspicious(c) || "/\\:*?\"<>|".indexOf(c) >= 0) {
                sb.append('_');
            } else {
                sb.append(c);
            }
        }
        String name = sb.toString().trim();
        while (name.startsWith(".")) {
            name = name.substring(1);
        }
        if (name.isEmpty()) {
            name = "document";
        }
        String ext = extension == null ? "" : extension.replaceAll("[^A-Za-z0-9]", "");
        return name + ".sterile" + (ext.isEmpty() ? "" : "." + ext);
    }

    /** The file extension for an engine format name, or {@code ""} if there is none to suggest. */
    public static String extensionOf(String format) {
        switch (format) {
            case "Docx": case "Docm": return "docx";
            case "Dotx": case "Dotm": return "dotx";
            case "Xlsx": case "Xlsm": return "xlsx";
            case "Xltx": case "Xltm": return "xltx";
            case "Xlsb": return "xlsb";
            case "Xlam": return "xlsx";
            case "Pptx": case "Pptm": return "pptx";
            case "Ppsx": case "Ppsm": return "ppsx";
            case "Potx": case "Potm": return "potx";
            case "Vsdx": case "Vsdm": return "vsdx";
            case "Xps": return "xps";
            case "Odt": return "odt";
            case "Ods": return "ods";
            case "Odp": return "odp";
            case "Odg": return "odg";
            case "Doc": return "doc";
            case "Xls": return "xls";
            case "Ppt": return "ppt";
            case "Rtf": return "rtf";
            case "Pdf": return "pdf";
            case "PlainText": return "txt";
            case "Csv": return "csv";
            case "Tsv": return "tsv";
            case "Markdown": return "md";
            case "Png": case "Jpeg": case "Gif": case "Bmp": return "png";
            default: return "";
        }
    }

    /** The MIME type to offer when saving a sterile copy of {@code format}. */
    public static String mimeOf(String format) {
        switch (format) {
            case "Docx": case "Docm": return "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
            case "Dotx": case "Dotm": return "application/vnd.openxmlformats-officedocument.wordprocessingml.template";
            case "Xlsx": case "Xlsm": case "Xlam": return "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
            case "Xltx": case "Xltm": return "application/vnd.openxmlformats-officedocument.spreadsheetml.template";
            case "Pptx": case "Pptm": return "application/vnd.openxmlformats-officedocument.presentationml.presentation";
            case "Ppsx": case "Ppsm": return "application/vnd.openxmlformats-officedocument.presentationml.slideshow";
            case "Potx": case "Potm": return "application/vnd.openxmlformats-officedocument.presentationml.template";
            case "Vsdx": case "Vsdm": return "application/vnd.ms-visio.drawing";
            case "Png": case "Jpeg": case "Gif": case "Bmp": return "image/png";
            default: return "application/octet-stream";
        }
    }
}
