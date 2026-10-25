// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.app.AlarmManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.os.Build;

import org.arca.viewer.core.ReminderRules;
import org.arca.viewer.core.StatsView;
import org.arca.viewer.ui.Compat;

import java.util.IllegalFormatException;
import java.util.Random;
import java.util.TimeZone;

/**
 * The optional local reminder to support the creators (Lava.top).
 *
 * <p>Everything that is a decision lives in the Rust core ({@code arca_attribution::reminder}),
 * so that every platform follows the same rules: when the next reminder is due (7 to 9 local days
 * ahead, between 12:00 and 17:59), whether it may be shown now (11:00 to 20:30 local time, at
 * least six days after the previous one, only if the system allows notifications), what to do
 * when the app starts (re-arm, replan after a reboot or a clock change) and which text comes next
 * (a text that quotes a counter is skipped while the counter is zero). This class only owns the
 * Android calls: one inexact alarm, one notification channel, one notification.
 *
 * <p>Zero-Trust, Zero-Telemetry, Zero-Permission:
 * <ul>
 *   <li>no network at all: the text is built offline from the app's own resources;
 *   <li>the only permission is POST_NOTIFICATIONS, requested at runtime on Android 13+ only when
 *       the person turns the reminder on; no exact-alarm permission ({@link AlarmManager#set} is
 *       inexact and the system may delay it, for example in Doze), no boot receiver (the app
 *       re-arms the alarm whenever it starts);
 *   <li>a quiet channel (low importance, no sound, no vibration, secret on the lock screen, with a
 *       public version that carries no number);
 *   <li>an honest opt-out in Settings, which cancels the alarm.
 * </ul>
 *
 * <p>Nothing here opens the donation link: tapping the notification opens the About screen, where
 * the full address is shown first and opened only after another explicit tap.
 */
public final class Reminders {
    public static final String CHANNEL_ID = "arca_reminders";
    public static final int NOTIF_ID = 2026;
    private static final int REQ_ALARM = 42;
    /** A planned time further away than this is corrected for a change of the UTC offset (summer time). */
    private static final long REFINE_FROM_MS = 2L * 24 * 60 * 60 * 1000;

    private static final class Item {
        final int title;
        final int body;

        Item(int title, int body) {
            this.title = title;
            this.body = body;
        }
    }

    /**
     * The texts, in the order of the Rust table ({@code ITEM_USES} in reminder.rs, which says which
     * counters each one quotes). A test in the attribution crate compares the two lists and the
     * placeholders of every string in both languages.
     */
    private static final Item[] ITEMS = new Item[] {
        new Item(R.string.push_student_ramen_fuel_title, R.string.push_student_ramen_fuel_body),
        new Item(R.string.push_student_finals_vs_macros_title, R.string.push_student_finals_vs_macros_body),
        new Item(R.string.push_student_coffee_overclock_title, R.string.push_student_coffee_overclock_body),
        new Item(R.string.push_student_stipend_economy_title, R.string.push_student_stipend_economy_body),
        new Item(R.string.push_student_dorm_server_title, R.string.push_student_dorm_server_body),
        new Item(R.string.push_student_diploma_threat_title, R.string.push_student_diploma_threat_body),
        new Item(R.string.push_student_energy_drink_title, R.string.push_student_energy_drink_body),
        new Item(R.string.push_paranoia_foil_hat_title, R.string.push_paranoia_foil_hat_body),
        new Item(R.string.push_paranoia_tracking_pixel_title, R.string.push_paranoia_tracking_pixel_body),
        new Item(R.string.push_paranoia_wasm_bunker_title, R.string.push_paranoia_wasm_bunker_body),
        new Item(R.string.push_paranoia_ntlm_leak_title, R.string.push_paranoia_ntlm_leak_body),
        new Item(R.string.push_paranoia_zero_telemetry_title, R.string.push_paranoia_zero_telemetry_body),
        new Item(R.string.push_paranoia_polyglot_bomb_title, R.string.push_paranoia_polyglot_bomb_body),
        new Item(R.string.push_paranoia_macro_funeral_title, R.string.push_paranoia_macro_funeral_body),
        new Item(R.string.push_bold_clickbait_ex_title, R.string.push_bold_clickbait_ex_body),
        new Item(R.string.push_bold_boss_pdf_title, R.string.push_bold_boss_pdf_body),
        new Item(R.string.push_bold_shawarma_deal_title, R.string.push_bold_shawarma_deal_body),
        new Item(R.string.push_bold_trojan_horse_title, R.string.push_bold_trojan_horse_body),
        new Item(R.string.push_bold_push_guilt_title, R.string.push_bold_push_guilt_body),
        new Item(R.string.push_bold_delete_app_dare_title, R.string.push_bold_delete_app_dare_body),
        new Item(R.string.push_bold_freeloader_badge_title, R.string.push_bold_freeloader_badge_body),
        new Item(R.string.push_spy_case_evidence_title, R.string.push_spy_case_evidence_body),
        new Item(R.string.push_spy_wiretap_foiled_title, R.string.push_spy_wiretap_foiled_body),
        new Item(R.string.push_spy_self_destruct_title, R.string.push_spy_self_destruct_body),
        new Item(R.string.push_spy_hidden_sheet_title, R.string.push_spy_hidden_sheet_body),
        new Item(R.string.push_spy_invisible_ink_title, R.string.push_spy_invisible_ink_body),
        new Item(R.string.push_spy_safehouse_funds_title, R.string.push_spy_safehouse_funds_body),
        new Item(R.string.push_tech_borrow_checker_title, R.string.push_tech_borrow_checker_body),
        new Item(R.string.push_tech_wasm_isolation_title, R.string.push_tech_wasm_isolation_body),
        new Item(R.string.push_tech_buffer_overflow_title, R.string.push_tech_buffer_overflow_body),
        new Item(R.string.push_tech_no_sockets_title, R.string.push_tech_no_sockets_body),
        new Item(R.string.push_tech_cve_vacuum_title, R.string.push_tech_cve_vacuum_body),
        new Item(R.string.push_tech_pure_math_title, R.string.push_tech_pure_math_body),
        new Item(R.string.push_wholesome_student_dream_title, R.string.push_wholesome_student_dream_body),
        new Item(R.string.push_wholesome_milestone_title, R.string.push_wholesome_milestone_body),
        new Item(R.string.push_wholesome_coffee_chat_title, R.string.push_wholesome_coffee_chat_body),
        new Item(R.string.push_wholesome_threat_shield_title, R.string.push_wholesome_threat_shield_body),
        new Item(R.string.push_wholesome_foss_solidarity_title, R.string.push_wholesome_foss_solidarity_body),
    };

    private static final Random RNG = new Random();

    private Reminders() {}

    private static int rnd() {
        synchronized (RNG) {
            return RNG.nextInt();
        }
    }

    private static int offsetAt(long when) {
        return TimeZone.getDefault().getOffset(when);
    }

    /**
     * Called whenever the app starts: creates the channel and puts the alarm back. Alarms do not
     * survive a reboot and no boot receiver is registered on purpose (it would need one more
     * permission), so the core decides what to do with the stored time: keep it, run a slightly
     * overdue one soon, or plan a new one (none yet, long overdue, or absurdly far away because
     * the clock was set back).
     */
    public static void init(Context context) {
        if (context == null) {
            return;
        }
        ensureChannel(context);
        if (!Native.isLoaded()) {
            return;
        }
        try {
            long now = System.currentTimeMillis();
            int off = offsetAt(now);
            ReminderRules.Decision d = ReminderRules.parse(Native.reminderOnStart(
                Prefs.nextReminder(context), now, off, Prefs.reminders(context) ? 1 : 0, rnd()));
            if (d.arms()) {
                arm(context, corrected(d.at, now, off));
            } else {
                cancel(context);
            }
        } catch (RuntimeException | LinkageError ignored) {
            // no reminder is better than a broken start
        }
    }

    /** Creates the quiet notification channel on Android 8.0+ (API 26+). */
    public static void ensureChannel(Context context) {
        if (Build.VERSION.SDK_INT >= 26) {
            NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
            if (nm == null) {
                return;
            }
            NotificationChannel ch = nm.getNotificationChannel(CHANNEL_ID);
            if (ch == null) {
                CharSequence name = context.getString(R.string.notif_channel_reminders);
                ch = new NotificationChannel(CHANNEL_ID, name, NotificationManager.IMPORTANCE_LOW);
                ch.setDescription(context.getString(R.string.notif_channel_reminders_desc));
                ch.enableVibration(false);
                ch.setSound(null, null);
                ch.enableLights(false);
                ch.setShowBadge(false);
                ch.setLockscreenVisibility(Notification.VISIBILITY_SECRET);
                nm.createNotificationChannel(ch);
            }
        }
    }

    /** Plans the next reminder (the person turned the reminders on) and arms the alarm. */
    public static void schedule(Context context) {
        if (!Native.isLoaded()) {
            return;
        }
        try {
            long now = System.currentTimeMillis();
            int off = offsetAt(now);
            long at = Native.reminderPlan(now, off, rnd());
            if (at > 0) {
                arm(context, corrected(at, now, off));
            }
        } catch (RuntimeException | LinkageError ignored) {
            // see init
        }
    }

    /** Cancels the alarm and forgets the planned time. */
    public static void cancel(Context context) {
        AlarmManager am = (AlarmManager) context.getSystemService(Context.ALARM_SERVICE);
        if (am != null) {
            am.cancel(alarmIntent(context));
        }
        Prefs.setNextReminder(context, 0L);
    }

    /**
     * A planned time corrected for a change of the UTC offset between now and then (the summer
     * time starts or ends in between), so the reminder keeps its wall-clock time.
     */
    private static long corrected(long at, long now, int offsetUsed) {
        if (at - now < REFINE_FROM_MS) {
            return at;
        }
        return Native.reminderRefine(at, offsetUsed, offsetAt(at));
    }

    /**
     * Arms one inexact alarm for {@code at} and remembers it. {@link AlarmManager#set} with
     * {@link AlarmManager#RTC} neither wakes the device nor needs a permission; the system may
     * deliver it late (Doze, standby) and the core judges the delivery time, not the planned one.
     */
    private static void arm(Context context, long at) {
        AlarmManager am = (AlarmManager) context.getSystemService(Context.ALARM_SERVICE);
        if (am == null) {
            return;
        }
        Prefs.setNextReminder(context, at);
        am.set(AlarmManager.RTC, at, alarmIntent(context));
    }

    private static PendingIntent alarmIntent(Context context) {
        Intent intent = new Intent(context, RemindReceiver.class);
        return PendingIntent.getBroadcast(
            context,
            REQ_ALARM,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );
    }

    /**
     * Whether the system lets the app post: the permission (Android 13+), notifications not
     * switched off for the app, and the channel not blocked.
     */
    static boolean canPost(Context context) {
        if (!Compat.hasNotificationPermission(context)) {
            return false;
        }
        NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm == null || !nm.areNotificationsEnabled()) {
            return false;
        }
        if (Build.VERSION.SDK_INT >= 26) {
            NotificationChannel ch = nm.getNotificationChannel(CHANNEL_ID);
            if (ch != null && ch.getImportance() == NotificationManager.IMPORTANCE_NONE) {
                return false;
            }
        }
        return true;
    }

    /**
     * The alarm fired (possibly late). The core decides; the next alarm is armed first, so that
     * nothing that goes wrong while posting can end the chain.
     */
    public static void postNotification(Context context) {
        if (!Native.isLoaded()) {
            return;
        }
        try {
            long now = System.currentTimeMillis();
            int off = offsetAt(now);
            ensureChannel(context);
            ReminderRules.Decision d = ReminderRules.parse(Native.reminderOnFire(
                now, off, Prefs.lastReminder(context), Prefs.reminders(context) ? 1 : 0,
                canPost(context) ? 1 : 0, rnd()));
            if (ReminderRules.DISABLED.equals(d.action)) {
                cancel(context);
                return;
            }
            if (d.arms()) {
                arm(context, corrected(d.at, now, off));
            }
            if (ReminderRules.SHOW.equals(d.action)) {
                post(context, now);
            }
        } catch (RuntimeException | LinkageError ignored) {
            // a failed reminder is not worth a crash
        }
    }

    private static void post(Context context, long now) {
        NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm == null) {
            return;
        }
        // The counters come from the local statistics (zero when the person turned them off).
        StatsView.Totals all = Stats.summary(context).all;
        int packed = Native.reminderPick(Prefs.reminderIndex(context), all.opened, all.threats);
        int index = ReminderRules.index(packed);
        int uses = ReminderRules.uses(packed);

        String title;
        String body;
        try {
            if (index >= ITEMS.length || Native.reminderItemCount() != ITEMS.length) {
                throw new IllegalStateException("text table out of step with the core");
            }
            Item item = ITEMS[index];
            Object[] args = ReminderRules.arguments(uses, all.opened, all.threats);
            title = context.getString(item.title, args);
            body = context.getString(item.body, args);
        } catch (IllegalFormatException | IllegalStateException | android.content.res.Resources.NotFoundException e) {
            // Never a broken text: fall back to the neutral one.
            title = context.getString(R.string.app_name);
            body = context.getString(R.string.notif_public_text);
        }

        // Tapping opens the About screen (where the support links are); no link is opened from here.
        Intent aboutIntent = new Intent(context, MainActivity.class);
        aboutIntent.setAction(MainActivity.ACTION_SHOW_ABOUT);
        aboutIntent.setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        PendingIntent clickPi = PendingIntent.getActivity(
            context,
            0,
            aboutIntent,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );

        // The text above may carry a local counter: it is never shown on a locked screen. The public
        // version has no counters, in case a launcher or wearable shows it anyway.
        Notification publicVersion = new Notification.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_heart)
            .setContentTitle(context.getString(R.string.app_name))
            .setContentText(context.getString(R.string.notif_public_text))
            .build();

        Notification.Builder b = new Notification.Builder(context, CHANNEL_ID)
            .setVisibility(Notification.VISIBILITY_SECRET)
            .setPublicVersion(publicVersion)
            .setSmallIcon(R.drawable.ic_heart)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(new Notification.BigTextStyle().bigText(body))
            .setContentIntent(clickPi)
            .setAutoCancel(true);

        nm.notify(NOTIF_ID, b.build());
        Prefs.setLastReminder(context, now);
        Prefs.setReminderIndex(context, index + 1);
    }
}
