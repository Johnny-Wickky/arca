// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.content.Context;
import android.content.SharedPreferences;
import android.content.res.Configuration;
import android.util.Base64;

import java.util.Locale;

/**
 * The app's few settings and the Patron Passport record. Everything is stored in the app's
 * private storage (never backed up: {@code allowBackup="false"}); nothing here is ever sent
 * anywhere, the app has no network permission.
 */
public final class Prefs {
    private Prefs() {}

    public static final int THEME_SYSTEM = 0;
    public static final int THEME_LIGHT = 1;
    public static final int THEME_DARK = 2;

    public static final int LANG_SYSTEM = 0;
    public static final int LANG_RU = 1;
    public static final int LANG_EN = 2;

    private static final String FILE = "arca";
    private static final String K_THEME = "theme";
    private static final String K_LANG = "lang";
    private static final String K_TEXT = "text_scale";
    private static final String K_ZOOM = "zoom_pct";
    private static final String K_REVEAL = "reveal_hidden";
    private static final String K_IMAGES = "auto_images";
    private static final String K_SECURE = "secure_screen";
    private static final String K_PASSPORT = "passport";
    private static final String K_REMINDERS = "reminders";
    private static final String K_LAST_REMINDER = "last_reminder";
    private static final String K_NEXT_REMINDER = "next_reminder";
    private static final String K_REMINDER_INDEX = "reminder_idx";
    private static final String K_DOC_COUNT = "doc_count";
    private static final String K_THREAT_COUNT = "threat_count";

    private static SharedPreferences sp(Context c) {
        return c.getApplicationContext().getSharedPreferences(FILE, Context.MODE_PRIVATE);
    }

    public static int theme(Context c) {
        return clamp(sp(c).getInt(K_THEME, THEME_SYSTEM), THEME_SYSTEM, THEME_DARK);
    }

    public static void setTheme(Context c, int v) {
        sp(c).edit().putInt(K_THEME, v).apply();
    }

    public static int lang(Context c) {
        return clamp(sp(c).getInt(K_LANG, LANG_SYSTEM), LANG_SYSTEM, LANG_EN);
    }

    public static void setLang(Context c, int v) {
        sp(c).edit().putInt(K_LANG, v).apply();
    }

    /** Preview text size: 0 small, 1 normal, 2 large, 3 extra large. */
    public static int textSize(Context c) {
        return clamp(sp(c).getInt(K_TEXT, 1), 0, 3);
    }

    public static void setTextSize(Context c, int v) {
        sp(c).edit().putInt(K_TEXT, v).apply();
    }

    /** The multiplier for {@link #textSize(Context)}. */
    public static float textScale(Context c) {
        switch (textSize(c)) {
            case 0: return 0.88f;
            case 2: return 1.2f;
            case 3: return 1.45f;
            default: return 1.0f;
        }
    }

    /** The zoom of the preview text on top of {@link #textScale(Context)}: 0.6 to 3.0, 1.0 by default. */
    public static float zoom(Context c) {
        return org.arca.viewer.core.Zoom.clamp(sp(c).getInt(K_ZOOM, 100) / 100f);
    }

    public static void setZoom(Context c, float zoom) {
        sp(c).edit().putInt(K_ZOOM, org.arca.viewer.core.Zoom.percent(zoom)).apply();
    }

    /** Whether pictures inside documents are decoded as soon as they come into view (default no). */
    public static boolean autoImages(Context c) {
        return sp(c).getBoolean(K_IMAGES, false);
    }

    public static void setAutoImages(Context c, boolean v) {
        sp(c).edit().putBoolean(K_IMAGES, v).apply();
    }

    /** Whether hidden content (hidden text, sheets, slides) is shown by default in the preview. */
    public static boolean revealHidden(Context c) {
        return sp(c).getBoolean(K_REVEAL, false);
    }

    public static void setRevealHidden(Context c, boolean v) {
        sp(c).edit().putBoolean(K_REVEAL, v).apply();
    }

    /** Whether screenshots, screen recording and the recents thumbnail are blocked (default yes). */
    public static boolean secureScreen(Context c) {
        return sp(c).getBoolean(K_SECURE, true);
    }

    public static void setSecureScreen(Context c, boolean v) {
        sp(c).edit().putBoolean(K_SECURE, v).apply();
    }

    /** The stored Patron Passport record (36 bytes), or an empty array. */
    public static byte[] passport(Context c) {
        String s = sp(c).getString(K_PASSPORT, null);
        if (s == null) {
            return new byte[0];
        }
        try {
            return Base64.decode(s, Base64.NO_WRAP);
        } catch (IllegalArgumentException e) {
            return new byte[0];
        }
    }

    public static void setPassport(Context c, byte[] record) {
        sp(c).edit().putString(K_PASSPORT, Base64.encodeToString(record, Base64.NO_WRAP)).apply();
    }

    /** Language code passed to the engine: 0 Russian, 1 English. */
    public static int engineLang(Context c) {
        switch (lang(c)) {
            case LANG_RU:
                return 0;
            case LANG_EN:
                return 1;
            default:
                String sys = c.getResources().getConfiguration().getLocales().get(0).getLanguage();
                return "ru".equals(sys) ? 0 : 1;
        }
    }

    /**
     * Applies the theme and language overrides to a base context (called from
     * {@code attachBaseContext}); the resources of the returned context load the night or
     * Russian variants regardless of the system settings.
     */
    public static Context wrap(Context base) {
        int theme = theme(base);
        int lang = lang(base);
        if (theme == THEME_SYSTEM && lang == LANG_SYSTEM) {
            return base;
        }
        // Only the fields that are overridden, as a delta: the platform merges it onto the live
        // configuration. A full copy of today's configuration would also pin the window size,
        // orientation and density of this very moment, and the layout would stop following
        // rotation, folding and split-screen. (An unset field is 0 / "undefined" here.)
        Configuration conf = new Configuration();
        if (theme != THEME_SYSTEM) {
            conf.uiMode = theme == THEME_DARK ? Configuration.UI_MODE_NIGHT_YES : Configuration.UI_MODE_NIGHT_NO;
        }
        if (lang != LANG_SYSTEM) {
            Locale l = lang == LANG_RU ? new Locale("ru") : Locale.ENGLISH;
            Locale.setDefault(l);
            conf.setLocales(new android.os.LocaleList(l));
        }
        return base.createConfigurationContext(conf);
    }

    public static boolean reminders(Context c) {
        return sp(c).getBoolean(K_REMINDERS, true);
    }

    public static void setReminders(Context c, boolean on) {
        sp(c).edit().putBoolean(K_REMINDERS, on).apply();
    }

    public static long lastReminder(Context c) {
        return sp(c).getLong(K_LAST_REMINDER, 0L);
    }

    public static void setLastReminder(Context c, long time) {
        sp(c).edit().putLong(K_LAST_REMINDER, time).apply();
    }

    public static long nextReminder(Context c) {
        return sp(c).getLong(K_NEXT_REMINDER, 0L);
    }

    public static void setNextReminder(Context c, long time) {
        sp(c).edit().putLong(K_NEXT_REMINDER, time).apply();
    }

    public static int reminderIndex(Context c) {
        return sp(c).getInt(K_REMINDER_INDEX, 0);
    }

    public static void setReminderIndex(Context c, int idx) {
        sp(c).edit().putInt(K_REMINDER_INDEX, idx).apply();
    }

    public static int docCount(Context c) {
        return sp(c).getInt(K_DOC_COUNT, 0);
    }

    public static void incrementDocCount(Context c) {
        sp(c).edit().putInt(K_DOC_COUNT, docCount(c) + 1).apply();
    }

    public static int threatCount(Context c) {
        return sp(c).getInt(K_THREAT_COUNT, 0);
    }

    public static void addThreats(Context c, int n) {
        if (n > 0) {
            sp(c).edit().putInt(K_THREAT_COUNT, threatCount(c) + n).apply();
        }
    }

    /** Forgets the local usage counters and the Patron Passport; nothing leaves the device either way. */
    public static void resetUsage(Context c) {
        sp(c).edit().remove(K_DOC_COUNT).remove(K_THREAT_COUNT).remove(K_PASSPORT).apply();
    }

    private static int clamp(int v, int lo, int hi) {
        return Math.max(lo, Math.min(hi, v));
    }
}
