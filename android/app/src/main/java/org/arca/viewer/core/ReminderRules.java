// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.Map;

/**
 * Reads the answers of the Rust core's reminder rules ({@code Native.reminderOnFire},
 * {@code reminderOnStart}, {@code reminderPick}). When the reminder is due, whether it may be
 * shown, and which text is shown are decided in Rust, the same on every platform; this class only
 * decodes the answers. The Android side keeps the operating-system calls (the alarm, the
 * notification).
 */
public final class ReminderRules {
    private ReminderRules() {}

    /** Actions of the fire decision. */
    public static final String SHOW = "show";
    public static final String DISABLED = "disabled";
    /** Actions of the start decision. */
    public static final String OFF = "off";
    public static final String KEEP = "keep";
    public static final String PLAN = "plan";

    /** An answer: what to do, and when to arm the next alarm. */
    public static final class Decision {
        /** {@code ""} when the answer could not be read. */
        public String action = "";
        /** Epoch milliseconds, 0 for none. */
        public long at;

        /** Whether an alarm is to be armed for {@link #at}. */
        public boolean arms() {
            return at > 0 && !DISABLED.equals(action) && !OFF.equals(action) && !action.isEmpty();
        }
    }

    /** Parses {@code {"action":"show","at":123}}; anything unreadable gives an empty decision. */
    public static Decision parse(byte[] json) {
        Decision d = new Decision();
        if (json == null || json.length == 0) {
            return d;
        }
        try {
            Map<String, Object> m = Json.obj(Json.parse(json));
            d.action = Json.str(m, "action");
            d.at = Math.max(0L, Json.num(m, "at"));
        } catch (RuntimeException e) {
            return new Decision();
        }
        return d;
    }

    /** Counters a text quotes. */
    public static final int USES_NOTHING = 0;
    public static final int USES_DOCUMENTS = 1;
    public static final int USES_THREATS = 2;
    public static final int USES_BOTH = 3;

    /** The text index of a packed pick ({@code index | uses << 8}). */
    public static int index(int packed) {
        return packed & 0xff;
    }

    /** The counters the text of a packed pick quotes. */
    public static int uses(int packed) {
        return (packed >> 8) & 3;
    }

    /** The format arguments for a text that {@code uses} the counters. */
    public static Object[] arguments(int uses, long documents, long threats) {
        switch (uses) {
            case USES_DOCUMENTS: return new Object[] {Math.max(0L, documents)};
            case USES_THREATS: return new Object[] {Math.max(0L, threats)};
            case USES_BOTH: return new Object[] {Math.max(0L, documents), Math.max(0L, threats)};
            default: return new Object[0];
        }
    }
}
