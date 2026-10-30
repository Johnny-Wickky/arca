// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Context;

import org.arca.viewer.R;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Names;
import org.arca.viewer.core.NoticeDetails;

import java.util.IllegalFormatException;
import java.util.Locale;

/**
 * Localised names for the engine's identifiers. Each identifier maps to a string resource named
 * {@code prefix_snake_case_id}; an identifier from a newer engine that the app has no text for
 * is shown as it is (escaped), never hidden. {@code arca-devtools check-resources} checks
 * that every identifier the engine can produce has a text in every language.
 */
public final class Labels {
    private Labels() {}

    /** The string resource called {@code name}, or {@code fallback}. */
    private static String byName(Context c, String name, String fallback) {
        int res = c.getResources().getIdentifier(name, "string", c.getPackageName());
        return res != 0 ? c.getString(res) : fallback;
    }

    /** The string named {@code prefix + snake(id)}, or {@code fallback}. */
    public static String of(Context c, String prefix, String id, String fallback) {
        return byName(c, prefix + Names.snake(id), fallback);
    }

    private static String of(Context c, String prefix, String id) {
        return of(c, prefix, id, Fmt.safe(id));
    }

    public static String threatTitle(Context c, String kind) {
        return byName(c, Names.threatTitle(kind), Fmt.safe(kind));
    }

    public static String threatText(Context c, String kind) {
        return byName(c, Names.threatText(kind), c.getString(R.string.threat_unknown_text));
    }

    public static String format(Context c, String format) {
        return of(c, "format_", format);
    }

    public static String category(Context c, String cat) {
        return of(c, "cat_", cat);
    }

    public static String severity(Context c, String sev) {
        return of(c, "sev_", sev);
    }

    public static String metaKey(Context c, String key) {
        return of(c, "meta_", key);
    }

    public static String refKind(Context c, String kind) {
        return of(c, "ref_kind_", kind);
    }

    public static String refClass(Context c, String cls) {
        return of(c, "ref_class_", cls);
    }

    public static String embeddedKind(Context c, String kind) {
        return of(c, "emb_kind_", kind);
    }

    public static String macroLang(Context c, String lang) {
        return of(c, "macro_lang_", lang);
    }

    /** The name of what a macro is judged to be: everyday automation, suspicious, harmful. */
    public static String macroIntent(Context c, String intent) {
        return of(c, "macro_intent_", intent, c.getString(R.string.macro_intent_suspicious));
    }

    /** One sentence on what a macro of that intent is, in the reader's language. */
    public static String macroIntentText(Context c, String intent) {
        return byName(c, "macro_intent_" + Names.snake(intent) + "_text",
            c.getString(R.string.macro_intent_suspicious_text));
    }

    /** What a macro does, from the engine's {@code BehaviorKind} name; an unknown kind is shown as it is. */
    public static String behavior(Context c, String kind) {
        return of(c, "behavior_", kind);
    }

    public static String sanitizeAction(Context c, String kind) {
        return of(c, "action_", kind);
    }

    public static String notice(Context c, String kind) {
        return of(c, "notice_", kind, c.getString(R.string.notice_unknown));
    }

    /**
     * The sentence for a notice of the preview: the text of its kind with the count and the detail.
     * A detail the app knows ({@link NoticeDetails}) is shown in the reader's language; one it does
     * not know (an engine newer than the app) is shown as the engine worded it, escaped, so that
     * nothing is hidden.
     */
    public static String noticeText(Context c, String kind, long count, String detail) {
        String template = notice(c, kind);
        String phrase = Fmt.safe(detail);
        NoticeDetails.Lookup l = NoticeDetails.lookup(kind, detail);
        if (l != null) {
            int res = c.getResources().getIdentifier(l.key, "string", c.getPackageName());
            if (res != 0) {
                try {
                    Object[] args = new Object[l.args.length];
                    for (int i = 0; i < args.length; i++) {
                        args[i] = Fmt.clip(Fmt.safe(l.args[i]), 80);
                    }
                    phrase = String.format(Locale.getDefault(), c.getString(res), args);
                } catch (IllegalFormatException e) {
                    phrase = Fmt.safe(detail);
                }
            }
        }
        try {
            return String.format(Locale.getDefault(), template, count, phrase);
        } catch (IllegalFormatException e) {
            return template;
        }
    }

    /** Colour resource for a severity name. */
    public static int severityColor(String sev) {
        switch (sev) {
            case "Critical": return R.color.arca_sev_critical;
            case "High": return R.color.arca_sev_high;
            case "Medium": return R.color.arca_sev_medium;
            case "Low": return R.color.arca_sev_low;
            default: return R.color.arca_sev_info;
        }
    }
}
