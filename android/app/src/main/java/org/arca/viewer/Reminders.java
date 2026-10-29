// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.content.Context;

import org.arca.viewer.core.ReminderRules;
import org.arca.viewer.core.StatsView;

import java.util.IllegalFormatException;
import java.util.Random;
import java.util.TimeZone;

/**
 * The optional reminder to support the creators, shown inside the app only.
 *
 * <p>Everything that is a decision lives in the Rust core ({@code arca_attribution::reminder}),
 * so that every platform follows the same rules: whether a reminder may be shown now (11:00 to
 * 20:30 local time, at least six days after the previous one) and which text comes next (a text
 * that quotes a counter is skipped while the counter is zero). This class only asks the core when
 * the home screen is built and keeps the answer until the person closes the card.
 *
 * <p>Zero-Trust, Zero-Telemetry, Zero-Permission: no notification, no alarm, no receiver and no
 * permission. The reminder is a card on the home screen, built offline from the app's own
 * resources, with an honest opt-out in Settings. Nothing here opens the donation link: the card
 * leads to the About screen, where the full address is shown first and opened only after another
 * explicit tap.
 */
public final class Reminders {
    /** The reminder to show on the home screen. */
    public static final class Card {
        public final String title;
        public final String body;
        final int index;

        Card(String title, String body, int index) {
            this.title = title;
            this.body = body;
            this.index = index;
        }
    }

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
    private static Card shown;

    private Reminders() {}

    private static int rnd() {
        synchronized (RNG) {
            return RNG.nextInt();
        }
    }

    /**
     * The reminder due now, or {@code null}. Asked every time the home screen is built; a card
     * that was offered stays until {@link #dismiss} (so a redraw does not change its text).
     */
    public static synchronized Card due(Context context) {
        if (context == null || !Native.isLoaded()) {
            return null;
        }
        if (shown != null) {
            return shown;
        }
        try {
            long now = System.currentTimeMillis();
            int off = TimeZone.getDefault().getOffset(now);
            ReminderRules.Decision d = ReminderRules.parse(Native.reminderOnFire(
                now, off, Prefs.lastReminder(context), Prefs.reminders(context) ? 1 : 0, 1, rnd()));
            if (!ReminderRules.SHOW.equals(d.action)) {
                return null;
            }
            shown = build(context);
            return shown;
        } catch (RuntimeException | LinkageError ignored) {
            // a failed reminder is not worth a crash
            return null;
        }
    }

    /** The person closed the card (or followed it): the next one is due after the core's interval. */
    public static synchronized void dismiss(Context context) {
        Card c = shown;
        shown = null;
        if (c != null && context != null) {
            Prefs.setLastReminder(context, System.currentTimeMillis());
            Prefs.setReminderIndex(context, c.index + 1);
        }
    }

    private static Card build(Context context) {
        // The counters come from the local statistics (zero when the person turned them off).
        StatsView.Totals all = Stats.summary(context).all;
        int packed = Native.reminderPick(Prefs.reminderIndex(context), all.opened, all.threats);
        int index = ReminderRules.index(packed);
        int uses = ReminderRules.uses(packed);
        try {
            if (index >= ITEMS.length || Native.reminderItemCount() != ITEMS.length) {
                throw new IllegalStateException("text table out of step with the core");
            }
            Item item = ITEMS[index];
            Object[] args = ReminderRules.arguments(uses, all.opened, all.threats);
            return new Card(context.getString(item.title, args), context.getString(item.body, args), index);
        } catch (IllegalFormatException | IllegalStateException | android.content.res.Resources.NotFoundException e) {
            // Never a broken text: fall back to the neutral one.
            return new Card(context.getString(R.string.app_name), context.getString(R.string.support_card_fallback), index);
        }
    }
}
