// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.Map;

/**
 * The local usage statistics as the Rust core reports them (the JSON of {@code Native.statsSummary}):
 * week, month and all-time totals. This class only reads that JSON and formats numbers; what is
 * counted, and how a verdict maps to a counter, is decided in Rust and nowhere else.
 */
public final class StatsView {
    private StatsView() {}

    /** The windows of the segmented toggle, in the order of its buttons. */
    public static final int WEEK = 0;
    public static final int MONTH = 1;
    public static final int ALL = 2;
    public static final int WINDOWS = 3;

    /** The counters of one period. */
    public static final class Totals {
        public long opened;
        public long clean;
        public long trackers;
        public long active;
        public long threats;
        public long rejected;
        public long unverified;
        public long sterile;
        public long pictures;

        /** {@code true} if nothing was counted. */
        public boolean isZero() {
            return opened == 0 && clean == 0 && trackers == 0 && active == 0 && rejected == 0
                && unverified == 0 && sterile == 0 && pictures == 0;
        }
    }

    /** The three windows and the day numbers. */
    public static final class Summary {
        public boolean empty = true;
        public long firstDay;
        public long days = 1;
        public Totals week = new Totals();
        public Totals month = new Totals();
        public Totals all = new Totals();

        /** The totals of window {@code which} ({@link #WEEK}, {@link #MONTH}, anything else all time). */
        public Totals window(int which) {
            switch (which) {
                case WEEK: return week;
                case MONTH: return month;
                default: return all;
            }
        }
    }

    /**
     * The core's wire tag of a verdict (0 clean, 1 hidden trackers, 2 active content neutralised,
     * 3 rejected, 4 unverified). An unknown verdict is 4, never "clean".
     */
    public static int verdictTag(Model.Verdict v) {
        if (v == null) {
            return 4;
        }
        switch (v) {
            case CLEAN: return 0;
            case HIDDEN_TRACKERS: return 1;
            case ACTIVE_NEUTRALIZED: return 2;
            case REJECTED: return 3;
            default: return 4;
        }
    }

    /** Empty statistics: what is shown when nothing was recorded, statistics are off, or the core is missing. */
    public static Summary empty() {
        return new Summary();
    }

    private static Totals totals(Object o) {
        Map<String, Object> m = Json.obj(o);
        Totals t = new Totals();
        t.opened = nonNegative(Json.num(m, "opened"));
        t.clean = nonNegative(Json.num(m, "clean"));
        t.trackers = nonNegative(Json.num(m, "trackers"));
        t.active = nonNegative(Json.num(m, "active"));
        t.threats = nonNegative(Json.num(m, "threats"));
        t.rejected = nonNegative(Json.num(m, "rejected"));
        t.unverified = nonNegative(Json.num(m, "unverified"));
        t.sterile = nonNegative(Json.num(m, "sterile"));
        t.pictures = nonNegative(Json.num(m, "pictures"));
        return t;
    }

    private static long nonNegative(long v) {
        return Math.max(0L, v);
    }

    /** Parses the summary JSON; anything unreadable gives {@link #empty()}. */
    public static Summary parse(byte[] json) {
        if (json == null || json.length == 0) {
            return empty();
        }
        try {
            Map<String, Object> root = Json.obj(Json.parse(json));
            Summary s = new Summary();
            s.empty = Json.bool(root.get("empty"), true);
            s.firstDay = nonNegative(Json.num(root, "firstDay"));
            s.days = Math.max(1L, Json.num(root, "days"));
            s.week = totals(root.get("week"));
            s.month = totals(root.get("month"));
            s.all = totals(root.get("all"));
            return s;
        } catch (RuntimeException e) {
            return empty();
        }
    }

    /**
     * A count for display: groups of three digits separated by a no-break space, the same in
     * every language (the labels around it carry the language). Negative counts show as 0.
     */
    public static String count(long v) {
        String digits = Long.toString(Math.max(0L, v));
        StringBuilder out = new StringBuilder();
        int lead = digits.length() % 3;
        for (int i = 0; i < digits.length(); i++) {
            if (i > 0 && (i - lead) % 3 == 0) {
                out.append(' ');
            }
            out.append(digits.charAt(i));
        }
        return out.toString();
    }
}
