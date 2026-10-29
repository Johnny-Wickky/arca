// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.Map;

/**
 * Reads the answers of the Rust core's reminder rules ({@code Native.reminderOnFire},
 * {@code reminderOnStart}). When the reminder is due and whether it may be shown are decided in
 * Rust, the same on every platform; this class only decodes the answers. Which text comes next is
 * {@code Native.reminderPick}.
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
}
