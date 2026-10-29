// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.content.Context;

import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.ReminderRules;

import java.util.Random;
import java.util.TimeZone;

/**
 * The weekly card that asks, once, whether you would like to support the developers. It is shown
 * inside the app only.
 *
 * <p>Everything that is a decision lives in the Rust core ({@code arca_attribution::reminder}),
 * so that every platform follows the same rules: whether a card may be shown now (11:00 to 20:30
 * local time, at least seven days after the previous one). This class only asks the core when the
 * home screen is built and keeps the answer until the person closes the card.
 *
 * <p>The whole state is three numbers in private storage: the day the app was installed, the day of
 * the last card and the position of the next message. Nothing records what the person opened or
 * when. Zero-Trust, Zero-Telemetry, Zero-Permission: no notification, no alarm, no receiver and no
 * permission, nothing leaves the device. Nothing here opens the donation link: the card leads to
 * the About screen, where the full address is shown first and opened only after another explicit
 * tap.
 */
public final class Reminders {
    /** The card to show on the home screen. */
    public static final class Card {
        public final String title;
        public final String body;
        /** The privacy note printed under every message. */
        public final String privacy;

        Card(String title, String body, String privacy) {
            this.title = title;
            this.body = body;
            this.privacy = privacy;
        }
    }

    /**
     * The thirty messages, in the order of the Rust table ({@code MESSAGE_COUNT} in reminder.rs). A
     * test in the attribution crate compares the count, the order and both languages.
     */
    private static final int[] MESSAGES = new int[] {
        R.string.support_msg_01,
        R.string.support_msg_02,
        R.string.support_msg_03,
        R.string.support_msg_04,
        R.string.support_msg_05,
        R.string.support_msg_06,
        R.string.support_msg_07,
        R.string.support_msg_08,
        R.string.support_msg_09,
        R.string.support_msg_10,
        R.string.support_msg_11,
        R.string.support_msg_12,
        R.string.support_msg_13,
        R.string.support_msg_14,
        R.string.support_msg_15,
        R.string.support_msg_16,
        R.string.support_msg_17,
        R.string.support_msg_18,
        R.string.support_msg_19,
        R.string.support_msg_20,
        R.string.support_msg_21,
        R.string.support_msg_22,
        R.string.support_msg_23,
        R.string.support_msg_24,
        R.string.support_msg_25,
        R.string.support_msg_26,
        R.string.support_msg_27,
        R.string.support_msg_28,
        R.string.support_msg_29,
        R.string.support_msg_30,
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
     * The card due now, or {@code null}. Asked every time the home screen is built; a card that
     * was offered stays until {@link #dismiss} (so a redraw does not change its text).
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
            // The core compares instants; the host keeps only the local day of the last card and
            // hands over that day's local midnight.
            int lastDay = Prefs.lastReminderDay(context);
            long lastMs = lastDay > 0 ? lastDay * 86_400_000L - off : 0L;
            ReminderRules.Decision d = ReminderRules.parse(Native.reminderOnFire(
                now, off, lastMs, Prefs.reminders(context) ? 1 : 0, 1, rnd()));
            if (!ReminderRules.SHOW.equals(d.action)) {
                return null;
            }
            // Not on the very day of the install: the first card comes a week later.
            if (lastDay == 0 && Fmt.epochDay(now, off) - Prefs.installDay(context) < 7) {
                return null;
            }
            shown = build(context);
            return shown;
        } catch (RuntimeException | LinkageError ignored) {
            // a failed card is not worth a crash
            return null;
        }
    }

    /** The person closed the card (or followed it): the next one is due after the core's interval. */
    public static synchronized void dismiss(Context context) {
        Card c = shown;
        shown = null;
        if (c != null && context != null) {
            long now = System.currentTimeMillis();
            Prefs.setLastReminderDay(context, Fmt.epochDay(now, TimeZone.getDefault().getOffset(now)));
            Prefs.setReminderIndex(context, Prefs.reminderIndex(context) + 1);
        }
    }

    private static Card build(Context context) {
        String privacy = context.getString(R.string.support_msg_privacy, new Object[0]);
        String title = context.getString(R.string.support_card_title);
        try {
            int index = Native.reminderPick(Prefs.reminderIndex(context));
            if (index < 0 || index >= MESSAGES.length || Native.reminderItemCount() != MESSAGES.length) {
                throw new IllegalStateException("text table out of step with the core");
            }
            // getString with arguments formats the text, which turns the escaped "%%" into "%".
            return new Card(title, context.getString(MESSAGES[index], new Object[0]), privacy);
        } catch (IllegalStateException | android.content.res.Resources.NotFoundException e) {
            // Never a broken text: fall back to the neutral one.
            return new Card(title, context.getString(R.string.support_card_fallback), privacy);
        }
    }
}
