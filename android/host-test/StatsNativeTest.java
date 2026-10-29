// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import java.nio.charset.StandardCharsets;

import org.arca.viewer.Native;
import org.arca.viewer.core.ReminderRules;
import org.arca.viewer.core.StatsStore;
import org.arca.viewer.core.StatsView;

/**
 * The native statistics and reminder functions through the real library (run with -Xcheck:jni by
 * run.sh): the contract between the Rust core and the Java readers, the hostile-input behaviour of
 * the blob, and the switch and purge rules of the real {@link StatsStore}.
 */
public final class StatsNativeTest {
    private static int checks;

    private static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            throw new AssertionError(what);
        }
    }

    private static void eq(long expected, long actual, String what) {
        check(expected == actual, what + ": expected " + expected + " but was " + actual);
    }

    private static String str(byte[] b) {
        return b == null ? null : new String(b, StandardCharsets.UTF_8);
    }

    private static final long T0 = 1_772_323_200_000L; // 2026-03-01 00:00:00 UTC
    private static final long DAY = 86_400_000L;
    private static final long HOUR = 3_600_000L;

    private static void blob() {
        byte[] b = new byte[0];
        b = Native.statsRecordScan(b, 0, 0, 0); // clean
        b = Native.statsRecordScan(b, 1, 3, 0); // hidden trackers
        b = Native.statsRecordScan(b, 2, 1, 4); // active content
        b = Native.statsRecordScan(b, 3, 0, 0); // rejected
        b = Native.statsRecordScan(b, 4, 0, 0); // unverified
        b = Native.statsRecordEvent(b, 1);
        b = Native.statsRecordEvent(b, 2);
        check(b.length > 0 && b.length < 40, "a compact blob of flat totals: " + b.length);
        StatsView.Summary s = StatsView.parse(Native.statsSummary(b));
        check(!s.empty, "not empty");
        StatsView.Totals t = s.all;
        eq(4, t.opened, "opened (every scan but the rejected one)");
        eq(1, t.clean, "clean");
        eq(4, t.trackers, "trackers");
        eq(4, t.active, "active");
        eq(8, t.threats, "threats");
        eq(1, t.rejected, "rejected");
        eq(1, t.unverified, "unverified");
        eq(1, t.sterile, "sterile");
        eq(1, t.pictures, "pictures");
        String json = str(Native.statsSummary(b));
        check(!json.contains("week") && !json.contains("month") && !json.contains("firstDay") && !json.contains("days"),
            "the summary holds no dates: " + json);
        // Hostile blobs and arguments are empty totals, never a crash.
        byte[] junk = new byte[300];
        java.util.Arrays.fill(junk, (byte) 0x41);
        check(StatsView.parse(Native.statsSummary(junk)).empty, "junk blob is empty");
        check(StatsView.parse(Native.statsSummary(null)).empty, "null blob is empty");
        byte[] flipped = b.clone();
        flipped[flipped.length / 2] ^= 1;
        check(StatsView.parse(Native.statsSummary(flipped)).empty, "a damaged blob is empty");
        check(Native.statsRecordScan(null, -9, -9, Integer.MIN_VALUE).length > 0, "negative arguments");
        check(Native.statsRecordEvent(junk, 99).length > 0, "unknown event");
        // The passport comes from the same totals and the install day the host passes in.
        String p = str(Native.statsPassport(1, b, 20000, 20002));
        check(p.contains("\"documents\":4") && p.contains("\"trackers\":4"), "passport from the totals: " + p);
        check(p.contains("\"days\":3"), "days from the install day: " + p);
    }

    private static final class Storage implements StatsStore.Storage {
        byte[] blob = new byte[0];
        boolean on;

        public byte[] readBlob() { return blob; }
        public void writeBlob(byte[] b) { blob = b; }
        public void deleteBlob() { blob = new byte[0]; }
        public boolean enabled() { return on; }
        public void setEnabled(boolean v) { on = v; }
    }

    private static final class Codec implements StatsStore.Codec {
        public byte[] recordScan(byte[] blob, int verdict, int trackers, int active) {
            return Native.statsRecordScan(blob, verdict, trackers, active);
        }

        public byte[] recordEvent(byte[] blob, int event) {
            return Native.statsRecordEvent(blob, event);
        }

        public byte[] summary(byte[] blob) {
            return Native.statsSummary(blob);
        }

        public byte[] passport(int lang, byte[] blob, int firstUseDay, int today) {
            return Native.statsPassport(lang, blob, firstUseDay, today);
        }
    }

    private static void store() {
        Storage st = new Storage();
        StatsStore store = new StatsStore(st, new Codec());
        store.recordScan(0, 0, 0);
        eq(0, st.blob.length, "nothing recorded while the counter is off (the default)");
        store.setEnabled(true);
        store.recordScan(0, 0, 0);
        store.recordScan(1, 2, 0);
        StatsView.Summary s = store.summary();
        eq(2, s.all.opened, "two documents");
        eq(2, s.all.trackers, "their trackers");
        // A blob left by an older version while the counter is off is deleted at start.
        st.on = false;
        store.purgeIfOff();
        eq(0, st.blob.length, "purged");
        // Switching off deletes the blob; nothing is recorded afterwards.
        store.setEnabled(true);
        store.recordScan(0, 0, 0);
        store.setEnabled(false);
        eq(0, st.blob.length, "blob deleted");
        store.recordScan(0, 0, 0);
        eq(0, st.blob.length, "nothing recorded while off");
    }

    private static ReminderRules.Decision fire(long now, int off, long last, boolean enabled, boolean can) {
        return ReminderRules.parse(Native.reminderOnFire(now, off, last, enabled ? 1 : 0, can ? 1 : 0, 5));
    }

    private static void reminders() {
        int utc3 = (int) (3 * HOUR);
        long planned = Native.reminderPlan(T0, utc3, 7);
        check(planned > T0 + 6 * DAY && planned < T0 + 10 * DAY, "planned 7 to 9 days ahead");
        long minute = ((planned + 3 * HOUR) % DAY) / 60_000;
        check(minute >= 720 && minute < 1080, "between 12:00 and 17:59 local: " + minute);
        eq(planned, Native.reminderRefine(planned, utc3, utc3), "no change without an offset change");
        eq(planned - HOUR, Native.reminderRefine(planned, utc3, utc3 + (int) HOUR), "kept wall-clock time");

        // Fire decisions.
        long noon = T0 + 13 * HOUR;
        check(ReminderRules.SHOW.equals(fire(noon, 0, 0, true, true).action), "shown at 13:00");
        check("quietHours".equals(fire(T0 + 3 * HOUR, 0, 0, true, true).action), "not at 03:00");
        check("quietHours".equals(fire(T0 + 20 * HOUR + 45 * 60_000, 0, 0, true, true).action), "not at 20:45");
        check("noPermission".equals(fire(noon, 0, 0, true, false).action), "no permission");
        check(fire(noon, 0, 0, true, false).arms(), "the chain survives a missing permission");
        check(!fire(noon, 0, 0, false, true).arms(), "disabled arms nothing");
        check("tooSoon".equals(fire(noon, 0, noon - DAY, true, true).action), "too soon");
        check(ReminderRules.SHOW.equals(fire(noon, 0, noon + 400 * DAY, true, true).action), "last shown in the future is ignored");
        check(fire(noon, 0, 0, true, true).at > noon + 6 * DAY, "next is more than six days away");
        check("tooSoon".equals(fire(noon, 0, noon - 6 * DAY, true, true).action), "six days is still too soon: the gap is seven");

        // Start decisions.
        ReminderRules.Decision none = ReminderRules.parse(Native.reminderOnStart(0, noon, 0, 1, 3));
        check(ReminderRules.PLAN.equals(none.action) && none.at > noon, "no stored time: plan");
        ReminderRules.Decision keep = ReminderRules.parse(Native.reminderOnStart(noon + DAY, noon, 0, 1, 3));
        check(ReminderRules.KEEP.equals(keep.action) && keep.at == noon + DAY, "plausible time: keep");
        ReminderRules.Decision far = ReminderRules.parse(Native.reminderOnStart(noon + 400 * DAY, noon, 0, 1, 3));
        check(ReminderRules.PLAN.equals(far.action), "clock set back: replan");
        ReminderRules.Decision late = ReminderRules.parse(Native.reminderOnStart(noon - 3 * DAY, noon, 0, 1, 3));
        check(ReminderRules.PLAN.equals(late.action) && late.at > noon, "long overdue: replan, never in the past");
        check(!ReminderRules.parse(Native.reminderOnStart(noon + DAY, noon, 0, 0, 3)).arms(), "off");

        // Texts.
        int count = Native.reminderItemCount();
        eq(30, count, "text count");
        for (int seq = 0; seq < 100; seq++) {
            eq(seq % 30, Native.reminderPick(seq), "the texts go round in order");
        }
        check(Native.reminderPick(-1) >= 0 && Native.reminderPick(-1) < count, "negative position");
        // Extreme arguments never fail.
        check(Native.reminderPlan(Long.MIN_VALUE, Integer.MIN_VALUE, Integer.MIN_VALUE) >= 0, "min");
        check(Native.reminderPlan(Long.MAX_VALUE, Integer.MAX_VALUE, Integer.MAX_VALUE) >= 0, "max");
        check(Native.reminderOnFire(Long.MAX_VALUE, Integer.MAX_VALUE, Long.MAX_VALUE, 1, 1, -1) != null, "fire max");
    }

    private static void extraSupport() {
        // Shipped empty until the owner provides the addresses.
        String json = str(Native.extraSupport());
        check(json != null && json.startsWith("{\"items\":["), "extra support JSON: " + json);
        check(org.arca.viewer.core.ExtraSupport.parse(Native.extraSupport()).size() <= 16, "parses");
    }

    public static void main(String[] args) {
        if (!Native.isLoaded()) {
            throw new AssertionError("native library not loaded: " + Native.loadError());
        }
        blob();
        store();
        reminders();
        extraSupport();
        System.out.println("StatsNativeTest: " + checks + " checks passed");
    }
}
