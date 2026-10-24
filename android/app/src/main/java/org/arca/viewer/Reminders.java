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

import org.arca.viewer.ui.Compat;

import java.util.Calendar;
import java.util.Random;

/**
 * Локальная система периодических ненавязчивых напоминаний о поддержке команды
 * студентов-разработчиков ядра Pharos (Lava.top).
 *
 * <p>Соблюдение принципов Zero-Trust и Zero-Telemetry:
 * <ul>
 *   <li>0 сетевых запросов: уведомления полностью формируются офлайн внутри приложения.</li>
 *   <li>Уважение к покою: отправка не чаще одного раза в 7–10 дней строго в дневное время (11:00–20:30).</li>
 *   <li>Тихий канал: канал уведомлений создается с низким приоритетом без звука и вибрации.</li>
 *   <li>Честный opt-out: пользователь может в любой момент отключить напоминания в Настройках.</li>
 * </ul>
 */
public final class Reminders {
    public static final String CHANNEL_ID = "arca_reminders";
    public static final int NOTIF_ID = 2026;
    private static final int REQ_ALARM = 42;

    private static final int FLAG_NONE = 0;
    private static final int FLAG_DOCS = 1;
    private static final int FLAG_THREATS = 2;
    private static final int FLAG_BOTH = 3;

    private static final class Item {
        final int title;
        final int body;
        final int flags;

        Item(int title, int body, int flags) {
            this.title = title;
            this.body = body;
            this.flags = flags;
        }
    }

    private static final Item[] ITEMS = new Item[] {
        new Item(R.string.push_student_ramen_fuel_title, R.string.push_student_ramen_fuel_body, FLAG_DOCS),
        new Item(R.string.push_student_finals_vs_macros_title, R.string.push_student_finals_vs_macros_body, FLAG_THREATS),
        new Item(R.string.push_student_coffee_overclock_title, R.string.push_student_coffee_overclock_body, FLAG_NONE),
        new Item(R.string.push_student_stipend_economy_title, R.string.push_student_stipend_economy_body, FLAG_DOCS),
        new Item(R.string.push_student_dorm_server_title, R.string.push_student_dorm_server_body, FLAG_NONE),
        new Item(R.string.push_student_diploma_threat_title, R.string.push_student_diploma_threat_body, FLAG_THREATS),
        new Item(R.string.push_student_energy_drink_title, R.string.push_student_energy_drink_body, FLAG_BOTH),
        new Item(R.string.push_paranoia_foil_hat_title, R.string.push_paranoia_foil_hat_body, FLAG_NONE),
        new Item(R.string.push_paranoia_tracking_pixel_title, R.string.push_paranoia_tracking_pixel_body, FLAG_THREATS),
        new Item(R.string.push_paranoia_wasm_bunker_title, R.string.push_paranoia_wasm_bunker_body, FLAG_DOCS),
        new Item(R.string.push_paranoia_ntlm_leak_title, R.string.push_paranoia_ntlm_leak_body, FLAG_NONE),
        new Item(R.string.push_paranoia_zero_telemetry_title, R.string.push_paranoia_zero_telemetry_body, FLAG_NONE),
        new Item(R.string.push_paranoia_polyglot_bomb_title, R.string.push_paranoia_polyglot_bomb_body, FLAG_THREATS),
        new Item(R.string.push_paranoia_macro_funeral_title, R.string.push_paranoia_macro_funeral_body, FLAG_BOTH),
        new Item(R.string.push_bold_clickbait_ex_title, R.string.push_bold_clickbait_ex_body, FLAG_NONE),
        new Item(R.string.push_bold_boss_pdf_title, R.string.push_bold_boss_pdf_body, FLAG_DOCS),
        new Item(R.string.push_bold_shawarma_deal_title, R.string.push_bold_shawarma_deal_body, FLAG_NONE),
        new Item(R.string.push_bold_trojan_horse_title, R.string.push_bold_trojan_horse_body, FLAG_THREATS),
        new Item(R.string.push_bold_push_guilt_title, R.string.push_bold_push_guilt_body, FLAG_DOCS),
        new Item(R.string.push_bold_delete_app_dare_title, R.string.push_bold_delete_app_dare_body, FLAG_NONE),
        new Item(R.string.push_bold_freeloader_badge_title, R.string.push_bold_freeloader_badge_body, FLAG_BOTH),
        new Item(R.string.push_spy_case_evidence_title, R.string.push_spy_case_evidence_body, FLAG_DOCS),
        new Item(R.string.push_spy_wiretap_foiled_title, R.string.push_spy_wiretap_foiled_body, FLAG_DOCS),
        new Item(R.string.push_spy_self_destruct_title, R.string.push_spy_self_destruct_body, FLAG_NONE),
        new Item(R.string.push_spy_hidden_sheet_title, R.string.push_spy_hidden_sheet_body, FLAG_THREATS),
        new Item(R.string.push_spy_invisible_ink_title, R.string.push_spy_invisible_ink_body, FLAG_DOCS),
        new Item(R.string.push_spy_safehouse_funds_title, R.string.push_spy_safehouse_funds_body, FLAG_NONE),
        new Item(R.string.push_tech_borrow_checker_title, R.string.push_tech_borrow_checker_body, FLAG_NONE),
        new Item(R.string.push_tech_wasm_isolation_title, R.string.push_tech_wasm_isolation_body, FLAG_DOCS),
        new Item(R.string.push_tech_buffer_overflow_title, R.string.push_tech_buffer_overflow_body, FLAG_THREATS),
        new Item(R.string.push_tech_no_sockets_title, R.string.push_tech_no_sockets_body, FLAG_NONE),
        new Item(R.string.push_tech_cve_vacuum_title, R.string.push_tech_cve_vacuum_body, FLAG_THREATS),
        new Item(R.string.push_tech_pure_math_title, R.string.push_tech_pure_math_body, FLAG_DOCS),
        new Item(R.string.push_wholesome_student_dream_title, R.string.push_wholesome_student_dream_body, FLAG_NONE),
        new Item(R.string.push_wholesome_milestone_title, R.string.push_wholesome_milestone_body, FLAG_DOCS),
        new Item(R.string.push_wholesome_coffee_chat_title, R.string.push_wholesome_coffee_chat_body, FLAG_NONE),
        new Item(R.string.push_wholesome_threat_shield_title, R.string.push_wholesome_threat_shield_body, FLAG_THREATS),
        new Item(R.string.push_wholesome_foss_solidarity_title, R.string.push_wholesome_foss_solidarity_body, FLAG_BOTH),
    };

    private Reminders() {}

    /**
     * Создает безопасный тихий канал уведомлений и планирует первое напоминание при установке.
     */
    public static void init(Context context) {
        if (context == null) {
            return;
        }
        ensureChannel(context);
        if (Prefs.reminders(context) && Prefs.nextReminder(context) <= 0) {
            schedule(context);
        }
    }

    /**
     * Создает NotificationChannel на Android 8.0+ (API 26+) с минимальной навязчивостью.
     */
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
                nm.createNotificationChannel(ch);
            }
        }
    }

    /**
     * Планирует следующий локальный будильник на 7-9 дней вперед в дневное время (12:00-18:00).
     */
    public static void schedule(Context context) {
        AlarmManager am = (AlarmManager) context.getSystemService(Context.ALARM_SERVICE);
        if (am == null) {
            return;
        }
        PendingIntent pi = getAlarmIntent(context);
        Random rng = new Random();
        int days = 7 + rng.nextInt(3); // 7, 8 или 9 дней
        int hour = 12 + rng.nextInt(6); // с 12:00 до 18:00
        int minute = rng.nextInt(60);

        Calendar cal = Calendar.getInstance();
        cal.add(Calendar.DAY_OF_YEAR, days);
        cal.set(Calendar.HOUR_OF_DAY, hour);
        cal.set(Calendar.MINUTE, minute);
        cal.set(Calendar.SECOND, 0);
        cal.set(Calendar.MILLISECOND, 0);

        long targetMs = cal.getTimeInMillis();
        Prefs.setNextReminder(context, targetMs);

        // Используем AlarmManager.RTC (не пробуждает спящее устройство ночью)
        am.set(AlarmManager.RTC, targetMs, pi);
    }

    /**
     * Отменяет запланированные напоминания.
     */
    public static void cancel(Context context) {
        AlarmManager am = (AlarmManager) context.getSystemService(Context.ALARM_SERVICE);
        if (am != null) {
            am.cancel(getAlarmIntent(context));
        }
        Prefs.setNextReminder(context, 0L);
    }

    private static PendingIntent getAlarmIntent(Context context) {
        Intent intent = new Intent(context, RemindReceiver.class);
        return PendingIntent.getBroadcast(
            context,
            REQ_ALARM,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );
    }

    /**
     * Показывает локальное уведомление при наступлении запланированного времени.
     */
    public static void postNotification(Context context) {
        if (!Prefs.reminders(context)) {
            cancel(context);
            return;
        }
        if (!Compat.hasNotificationPermission(context)) {
            return;
        }

        // Защита от ночных уведомлений: разрешено только с 11:00 до 20:30
        Calendar now = Calendar.getInstance();
        int hour = now.get(Calendar.HOUR_OF_DAY);
        if (hour < 11) {
            // Утро до 11: переносим на сегодня в 12:30
            postponeTo(context, 12, 30, false);
            return;
        }
        if (hour >= 21) {
            // Вечер/ночь: переносим на завтра в 12:30
            postponeTo(context, 12, 30, true);
            return;
        }

        ensureChannel(context);
        NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm == null) {
            return;
        }

        int docs = Math.max(1, Prefs.docCount(context));
        int threats = Math.max(1, Prefs.threatCount(context));

        int index = Math.abs(Prefs.reminderIndex(context)) % ITEMS.length;
        Item item = ITEMS[index];
        Prefs.setReminderIndex(context, index + 1);

        String title;
        String body;
        switch (item.flags) {
            case FLAG_DOCS:
                title = context.getString(item.title, docs);
                body = context.getString(item.body, docs);
                break;
            case FLAG_THREATS:
                title = context.getString(item.title, threats);
                body = context.getString(item.body, threats);
                break;
            case FLAG_BOTH:
                title = context.getString(item.title, docs, threats);
                body = context.getString(item.body, docs, threats);
                break;
            case FLAG_NONE:
            default:
                title = context.getString(item.title);
                body = context.getString(item.body);
                break;
        }

        // Явный Intent с переходом на экран "О программе" (где находится ссылка на Lava.top)
        Intent aboutIntent = new Intent(context, MainActivity.class);
        aboutIntent.setAction(MainActivity.ACTION_SHOW_ABOUT);
        aboutIntent.setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP);

        PendingIntent clickPi = PendingIntent.getActivity(
            context,
            0,
            aboutIntent,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );

        Notification.Builder b = new Notification.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_heart)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(new Notification.BigTextStyle().bigText(body))
            .setContentIntent(clickPi)
            .setAutoCancel(true);

        nm.notify(NOTIF_ID, b.build());
        Prefs.setLastReminder(context, System.currentTimeMillis());

        // Планируем следующий показ через 7-9 дней
        schedule(context);
    }

    private static void postponeTo(Context context, int hour, int minute, boolean tomorrow) {
        AlarmManager am = (AlarmManager) context.getSystemService(Context.ALARM_SERVICE);
        if (am == null) {
            return;
        }
        Calendar cal = Calendar.getInstance();
        if (tomorrow) {
            cal.add(Calendar.DAY_OF_YEAR, 1);
        }
        cal.set(Calendar.HOUR_OF_DAY, hour);
        cal.set(Calendar.MINUTE, minute);
        cal.set(Calendar.SECOND, 0);
        long targetMs = cal.getTimeInMillis();
        Prefs.setNextReminder(context, targetMs);
        am.set(AlarmManager.RTC, targetMs, getAlarmIntent(context));
    }
}
