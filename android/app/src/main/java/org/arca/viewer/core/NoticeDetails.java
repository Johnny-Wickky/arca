// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Finds the localized text for the {@code detail} of a preview notice.
 *
 * <p>The engine words a detail in English ({@code "grid"}, {@code "visio hyperlinks are not
 * read"}). The app keeps one string resource per detail, named
 * {@code notice_d_<group>_<slug>}: the group is the notice kind ({@code hidden} for both
 * hidden-content kinds), the slug is the detail in lower case with every run of other
 * characters turned into {@code _}. A few details carry a value made at run time ({@code
 * "table Orders"}, {@code "3 slide part(s) ..."}); they are matched by a pattern, and the value
 * is passed to the string as its argument {@code %1$s}.
 *
 * <p>This class only builds the names: whether a text exists is the caller's question, and when
 * there is none, it shows the raw (escaped) detail in the generic sentence of the kind. The list
 * of the details the engine emits is {@code android/core-test/notice_details.txt}; a test of
 * the core checks that each of them has a text in every language.
 */
public final class NoticeDetails {
    private NoticeDetails() {}

    /** The resource to look for and the values to put into it. */
    public static final class Lookup {
        /** Resource name of the text. */
        public final String key;
        /** Values made at run time, for {@code %1$s}, {@code %2$s}, ... (usually none or one). */
        public final String[] args;

        Lookup(String key, String... args) {
            this.key = key;
            this.args = args;
        }
    }

    /** A detail with a run-time value: the kind, how it looks, and the template it stands for. */
    private static final class Dynamic {
        final String kind;
        final Pattern pattern;
        final String template;

        Dynamic(String kind, String regex, String template) {
            this.kind = kind;
            this.pattern = Pattern.compile(regex, Pattern.DOTALL);
            this.template = template;
        }
    }

    private static final Dynamic[] DYNAMIC = {
        new Dynamic("BestEffort", "text-encoding=cp(\\d{1,6})", "text-encoding=cp{}"),
        new Dynamic("Skipped", "older revisions of slides: (.{1,40})", "older revisions of slides: {}"),
        new Dynamic("Skipped", "query (.{1,300}): no definition rows", "query {}: no definition rows"),
        new Dynamic("Skipped", "table (.{1,300})", "table {}"),
        new Dynamic("Skipped", "(\\d{1,12}) slide part\\(s\\) that the presentation does not list",
            "{} slide part(s) that the presentation does not list"),
        new Dynamic("Skipped", "(\\d{1,12}) worksheet part\\(s\\) that the workbook does not list",
            "{} worksheet part(s) that the workbook does not list"),
    };

    /** Longest slug: resource names stay short and readable. */
    private static final int MAX_SLUG = 56;

    /**
     * The resource for {@code detail} of a notice of {@code kind}, or {@code null} for a notice
     * without a detail (the sentence of the kind says everything). {@code kind} is the engine's
     * name ({@code "Unsupported"}).
     */
    public static Lookup lookup(String kind, String detail) {
        if (kind == null || detail == null || detail.isEmpty()) {
            return null;
        }
        for (Dynamic d : DYNAMIC) {
            if (d.kind.equals(kind)) {
                Matcher m = d.pattern.matcher(detail);
                if (m.matches()) {
                    return new Lookup(key(kind, d.template), m.group(1));
                }
            }
        }
        return new Lookup(key(kind, detail));
    }

    /** {@code notice_d_<group>_<slug>} for a detail (or a template with {@code {}}). */
    public static String key(String kind, String detail) {
        String group = "HiddenShown".equals(kind) || "HiddenOmitted".equals(kind) ? "hidden" : Names.snake(kind);
        StringBuilder sb = new StringBuilder("notice_d_").append(group).append('_');
        int start = sb.length();
        boolean gap = false;
        String text = detail.replace("{}", " n ");
        for (int i = 0; i < text.length() && sb.length() - start < MAX_SLUG; i++) {
            char c = text.charAt(i);
            if (c >= 'A' && c <= 'Z') {
                c = (char) (c + ('a' - 'A'));
            }
            if ((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')) {
                if (gap && sb.length() > start) {
                    sb.append('_');
                }
                gap = false;
                sb.append(c);
            } else {
                gap = true;
            }
        }
        return sb.toString();
    }
}
