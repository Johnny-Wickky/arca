// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.content.Context;
import android.content.SharedPreferences;
import android.content.res.Configuration;
import android.util.Base64;

import org.arca.viewer.core.Appearance;
import org.arca.viewer.core.ReminderRules;

import java.util.Locale;

/**
 * The app's few settings, the three numbers of the support card and the opaque counter blob of the Rust core.
 * Everything is stored in the app's private storage (never backed up: {@code allowBackup="false"});
 * nothing here is ever sent anywhere, the app has no network permission. Nothing about a document
 * is stored: no name, no content, no hash; the statistics blob holds counters per day and nothing
 * else.
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
    /** The single palette of 0.5 and earlier: read once by {@link #migrateAppearance}, then deleted. */
    private static final String K_PALETTE = "palette";
    private static final String K_PALETTE_LIGHT = "palette_light";
    private static final String K_PALETTE_DARK = "palette_dark";
    private static final String K_GLASS = "glass";
    private static final String K_LANG = "lang";
    private static final String K_TEXT = "text_scale";
    private static final String K_ZOOM = "zoom_pct";
    private static final String K_REVEAL = "reveal_hidden";
    private static final String K_IMAGES = "auto_images";
    private static final String K_SECURE = "secure_screen";
    /**
     * Keys that earlier versions wrote and this one never reads: counters, a first-use record and
     * a time of day of the last reminder. {@link #purgeLegacy} deletes them at start so that
     * nothing of the old timeline survives an update.
     */
    private static final String[] LEGACY_KEYS = {"passport", "doc_count", "threat_count", "next_reminder", "last_reminder"};
    private static final String K_STATS = "stats";
    private static final String K_STATS_ON = "stats_on";
    private static final String K_REMINDERS = "reminders";
    /** The whole state of the weekly support card: these three numbers and the switch. */
    private static final String K_INSTALL_DAY = "install_epoch_day";
    private static final String K_LAST_REMINDER_DAY = "last_reminder_epoch_day";
    private static final String K_REMINDER_INDEX = "next_message_index";

    private static SharedPreferences sp(Context c) {
        return c.getApplicationContext().getSharedPreferences(FILE, Context.MODE_PRIVATE);
    }

    /**
     * One time: the single palette setting of 0.5 becomes the mode and one palette per group (see
     * {@link Appearance#fromLegacy}), so that what a person had chosen still looks the same.
     */
    private static void migrateAppearance(SharedPreferences p) {
        if (!p.contains(K_PALETTE)) {
            return;
        }
        int[] now = Appearance.fromLegacy(p.getInt(K_PALETTE, 0),
            clamp(p.getInt(K_THEME, THEME_SYSTEM), THEME_SYSTEM, THEME_DARK));
        p.edit().putInt(K_THEME, now[0]).putInt(K_PALETTE_LIGHT, now[1]).putInt(K_PALETTE_DARK, now[2])
            .remove(K_PALETTE).apply();
    }

    /** The mode: {@link #THEME_SYSTEM}, {@link #THEME_LIGHT} or {@link #THEME_DARK}. */
    public static int theme(Context c) {
        SharedPreferences p = sp(c);
        migrateAppearance(p);
        return clamp(p.getInt(K_THEME, THEME_SYSTEM), THEME_SYSTEM, THEME_DARK);
    }

    public static void setTheme(Context c, int v) {
        sp(c).edit().putInt(K_THEME, v).apply();
    }

    /** The palette chosen among the light ones ({@code 0 .. Appearance.PER_GROUP - 1}, see {@code ui.Theme}). */
    public static int paletteLight(Context c) {
        SharedPreferences p = sp(c);
        migrateAppearance(p);
        return Appearance.choice(p.getInt(K_PALETTE_LIGHT, 0));
    }

    /** The palette chosen among the dark ones ({@code 0 .. Appearance.PER_GROUP - 1}). */
    public static int paletteDark(Context c) {
        SharedPreferences p = sp(c);
        migrateAppearance(p);
        return Appearance.choice(p.getInt(K_PALETTE_DARK, 0));
    }

    public static void setPaletteLight(Context c, int choice) {
        sp(c).edit().putInt(K_PALETTE_LIGHT, Appearance.choice(choice)).apply();
    }

    public static void setPaletteDark(Context c, int choice) {
        sp(c).edit().putInt(K_PALETTE_DARK, Appearance.choice(choice)).apply();
    }

    /** Whether the translucent "liquid glass" panels are on (off by default). */
    public static boolean glass(Context c) {
        return sp(c).getBoolean(K_GLASS, false);
    }

    public static void setGlass(Context c, boolean on) {
        sp(c).edit().putBoolean(K_GLASS, on).apply();
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
            // The mode decides the group of palettes and, through the night bits, the system widgets.
            conf.uiMode = theme == THEME_DARK ? Configuration.UI_MODE_NIGHT_YES : Configuration.UI_MODE_NIGHT_NO;
        }
        if (lang != LANG_SYSTEM) {
            Locale l = lang == LANG_RU ? new Locale("ru") : Locale.ENGLISH;
            Locale.setDefault(l);
            conf.setLocales(new android.os.LocaleList(l));
        }
        return base.createConfigurationContext(conf);
    }

    /**
     * Whether the weekly support card is on. It is on by default; only the person's own choice in
     * Settings turns it off (the "About and support" entry stays available either way).
     */
    public static boolean reminders(Context c) {
        SharedPreferences p = sp(c);
        return ReminderRules.enabledOrDefault(p.contains(K_REMINDERS), p.getBoolean(K_REMINDERS, ReminderRules.ENABLED_BY_DEFAULT));
    }

    public static void setReminders(Context c, boolean on) {
        sp(c).edit().putBoolean(K_REMINDERS, on).apply();
    }

    /** The local calendar day (days since 1970-01-01) of the last support card, 0 if none. */
    public static int lastReminderDay(Context c) {
        return sp(c).getInt(K_LAST_REMINDER_DAY, 0);
    }

    public static void setLastReminderDay(Context c, int day) {
        sp(c).edit().putInt(K_LAST_REMINDER_DAY, day).apply();
    }

    public static int reminderIndex(Context c) {
        return sp(c).getInt(K_REMINDER_INDEX, 0);
    }

    public static void setReminderIndex(Context c, int idx) {
        sp(c).edit().putInt(K_REMINDER_INDEX, idx).apply();
    }

    /**
     * The local calendar day the app was first started, remembered on the first call. It feeds the
     * "days with Arca" line of the optional Passport and the quiet first week of the support card;
     * it is a single number, not a log.
     */
    public static int installDay(Context c) {
        SharedPreferences p = sp(c);
        int day = p.getInt(K_INSTALL_DAY, 0);
        if (day <= 0) {
            long now = System.currentTimeMillis();
            day = org.arca.viewer.core.Fmt.epochDay(now, java.util.TimeZone.getDefault().getOffset(now));
            p.edit().putInt(K_INSTALL_DAY, day).apply();
        }
        return day;
    }

    /** Deletes what earlier versions stored (see {@link #LEGACY_KEYS}). Called once per start. */
    public static void purgeLegacy(Context c) {
        SharedPreferences.Editor e = sp(c).edit();
        for (String key : LEGACY_KEYS) {
            e.remove(key);
        }
        e.apply();
    }

    /**
     * The private storage of the optional counter: the blob of the Rust core (base64) and the
     * on / off switch, which is OFF on a fresh install.
     */
    public static org.arca.viewer.core.StatsStore.Storage statsStorage(final Context c) {
        final Context app = c.getApplicationContext();
        return new org.arca.viewer.core.StatsStore.Storage() {
            @Override
            public byte[] readBlob() {
                return decode(sp(app).getString(K_STATS, null));
            }

            @Override
            public void writeBlob(byte[] blob) {
                sp(app).edit().putString(K_STATS, Base64.encodeToString(blob, Base64.NO_WRAP)).apply();
            }

            @Override
            public void deleteBlob() {
                sp(app).edit().remove(K_STATS).apply();
            }

            @Override
            public boolean enabled() {
                return sp(app).getBoolean(K_STATS_ON, false);
            }

            @Override
            public void setEnabled(boolean on) {
                sp(app).edit().putBoolean(K_STATS_ON, on).apply();
            }
        };
    }

    private static byte[] decode(String s) {
        if (s == null) {
            return new byte[0];
        }
        try {
            return Base64.decode(s, Base64.NO_WRAP);
        } catch (IllegalArgumentException e) {
            return new byte[0];
        }
    }

    private static int clamp(int v, int lo, int hi) {
        return Math.max(lo, Math.min(hi, v));
    }
}
