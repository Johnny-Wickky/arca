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
        if (n < s.length() && Character.isHighSurrogate(s.charAt(n - 1))) {
            n--; // never cut a pair in half
        }
        StringBuilder sb = null;
        // By code point: a character outside the BMP (a tag, a variation selector) is one unit.
        for (int i = 0; i < n; ) {
            int cp = s.codePointAt(i);
            int len = Character.charCount(cp);
            if (isSuspicious(cp)) {
                if (sb == null) {
                    sb = new StringBuilder(n + 16);
                    sb.append(s, 0, i);
                }
                sb.append("‹U+").append(String.format(Locale.ROOT, "%04X", cp)).append('›');
            } else if (sb != null) {
                sb.appendCodePoint(cp);
            }
            i += len;
        }
        String out = sb == null ? s.substring(0, n) : sb.toString();
        return n < s.length() ? out + "…" : out;
    }

    /**
     * The UTF-8 bytes of {@code chars} without going through an immutable String, so that the
     * caller can wipe both arrays. At most 1024 bytes are returned (the protocol's limit).
     */
    public static byte[] utf8(char[] chars) {
        // Encoded by hand into a buffer of exactly the needed size: the library encoder allocates
        // an oversized array of its own that the caller could never wipe. A lone surrogate
        // becomes '?', as with the library's replacement.
        int need = 0;
        for (int i = 0; i < chars.length; i++) {
            char c = chars[i];
            if (c < 0x80) {
                need += 1;
            } else if (c < 0x800) {
                need += 2;
            } else if (Character.isHighSurrogate(c) && i + 1 < chars.length && Character.isLowSurrogate(chars[i + 1])) {
                need += 4;
                i++;
            } else if (Character.isSurrogate(c)) {
                need += 1;
            } else {
                need += 3;
            }
        }
        byte[] all = new byte[need];
        int o = 0;
        for (int i = 0; i < chars.length; i++) {
            char c = chars[i];
            if (c < 0x80) {
                all[o++] = (byte) c;
            } else if (c < 0x800) {
                all[o++] = (byte) (0xC0 | (c >> 6));
                all[o++] = (byte) (0x80 | (c & 0x3F));
            } else if (Character.isHighSurrogate(c) && i + 1 < chars.length && Character.isLowSurrogate(chars[i + 1])) {
                int cp = Character.toCodePoint(c, chars[++i]);
                all[o++] = (byte) (0xF0 | (cp >> 18));
                all[o++] = (byte) (0x80 | ((cp >> 12) & 0x3F));
                all[o++] = (byte) (0x80 | ((cp >> 6) & 0x3F));
                all[o++] = (byte) (0x80 | (cp & 0x3F));
            } else if (Character.isSurrogate(c)) {
                all[o++] = (byte) '?';
            } else {
                all[o++] = (byte) (0xE0 | (c >> 12));
                all[o++] = (byte) (0x80 | ((c >> 6) & 0x3F));
                all[o++] = (byte) (0x80 | (c & 0x3F));
            }
        }
        if (all.length <= 1024) {
            return all;
        }
        byte[] out = java.util.Arrays.copyOf(all, 1024);
        java.util.Arrays.fill(all, (byte) 0);
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

    /**
     * Whether a code point must not be shown as it is: it is invisible, reorders or joins text, or
     * has no business in a name or an address. Whole Unicode categories are covered (controls,
     * format characters, private use, unassigned, lone surrogates, line and paragraph separators),
     * plus the blank-looking letters and selectors that are not in those categories.
     */
    static boolean isSuspicious(int cp) {
        if (cp == '\t' || cp == '\n') {
            return false;
        }
        switch (Character.getType(cp)) {
            case Character.CONTROL: // C0 / C1 (a carriage return can overwrite a line start)
            case Character.FORMAT: // zero-width, directional, tags, invisible operators, BOM, annotations
            case Character.PRIVATE_USE:
            case Character.UNASSIGNED:
            case Character.SURROGATE: // a lone one
            case Character.LINE_SEPARATOR: // U+2028
            case Character.PARAGRAPH_SEPARATOR: // U+2029
                return true;
            default:
                break;
        }
        return cp == 0x061C // Arabic letter mark
            || cp == 0x115F || cp == 0x1160 // Hangul Choseong / Jungseong fillers
            || cp == 0x3164 || cp == 0xFFA0 // Hangul filler, halfwidth Hangul filler
            || cp == 0x2800 // Braille blank
            || (cp >= 0x180B && cp <= 0x180F) // Mongolian free variation selectors
            || (cp >= 0xFE00 && cp <= 0xFE0F) // variation selectors
            || (cp >= 0xE0000 && cp <= 0xE007F) // Unicode tags
            || (cp >= 0xE0100 && cp <= 0xE01EF); // variation selectors supplement
    }

    /**
     * The host of {@code url} in its ASCII (punycode) form when the host has non-ASCII characters
     * (a look-alike of another site, say), {@code ""} when it has some but cannot be converted, and
     * {@code null} when the host is plain ASCII or there is none. Nothing is resolved or fetched.
     */
    public static String asciiHost(String url) {
        if (url == null) {
            return null;
        }
        int scheme = url.indexOf("://");
        int start = scheme < 0 ? 0 : scheme + 3;
        int end = url.length();
        for (int i = start; i < url.length(); i++) {
            char c = url.charAt(i);
            if (c == '/' || c == '?' || c == '#' || c == '\\') {
                end = i;
                break;
            }
        }
        String host = url.substring(start, end);
        int at = host.lastIndexOf('@');
        if (at >= 0) {
            host = host.substring(at + 1);
        }
        if (!host.startsWith("[")) {
            int colon = host.lastIndexOf(':');
            if (colon >= 0) {
                host = host.substring(0, colon);
            }
        }
        boolean ascii = true;
        for (int i = 0; i < host.length(); i++) {
            if (host.charAt(i) > 0x7F) {
                ascii = false;
                break;
            }
        }
        if (ascii) {
            return null;
        }
        try {
            return java.net.IDN.toASCII(host, java.net.IDN.ALLOW_UNASSIGNED);
        } catch (IllegalArgumentException e) {
            return "";
        }
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
        for (int i = 0; i < base.length() && sb.length() < 80; ) {
            int cp = base.codePointAt(i);
            i += Character.charCount(cp);
            if (isSuspicious(cp) || (cp < 0x80 && "/\\:*?\"<>|".indexOf(cp) >= 0)) {
                sb.append('_');
            } else {
                sb.appendCodePoint(cp);
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
