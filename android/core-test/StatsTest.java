// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

import org.arca.viewer.core.ExtraSupport;
import org.arca.viewer.core.Model;
import org.arca.viewer.core.ReminderRules;
import org.arca.viewer.core.StatsStore;
import org.arca.viewer.core.StatsView;

/**
 * Tests of the Java pieces around the Rust statistics and reminder rules: reading the summary JSON,
 * choosing the week / month / all-time window, formatting, the store with its migration of the
 * older counters, the on / off switch, and the readers of the reminder answers and of the extra
 * support addresses. The Rust side has its own tests; the native glue is exercised by the
 * host-test. A plain runner like CoreTest (see run.sh).
 */
public final class StatsTest {
    private static int checks;
    private static int failures;
    private static String current = "";

    private static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            failures++;
            System.out.println("  FAIL [" + current + "] " + what);
        }
    }

    private static void eq(Object expected, Object actual, String what) {
        check(expected == null ? actual == null : expected.equals(actual),
            what + ": expected <" + expected + "> but was <" + actual + ">");
    }

    private static byte[] utf8(String s) {
        return s.getBytes(StandardCharsets.UTF_8);
    }

    /** The shape the Rust core produces (see arca_engine::stats::Stats::summary_json). */
    private static final String RUST_JSON = "{\"version\":1,\"empty\":false,\"firstDay\":19990,\"days\":11,"
        + "\"week\":{\"opened\":2,\"clean\":1,\"trackers\":1,\"active\":2,\"threats\":3,\"rejected\":0,\"unverified\":0,\"sterile\":0,\"pictures\":0},"
        + "\"month\":{\"opened\":5,\"clean\":1,\"trackers\":4,\"active\":2,\"threats\":6,\"rejected\":1,\"unverified\":1,\"sterile\":2,\"pictures\":3},"
        + "\"all\":{\"opened\":1234567,\"clean\":1,\"trackers\":4,\"active\":2,\"threats\":6,\"rejected\":1,\"unverified\":1,\"sterile\":2,\"pictures\":3}}";

    // ---------------------------------------------------------------------------------------

    private static void windows() {
        StatsView.Summary s = StatsView.parse(utf8(RUST_JSON));
        check(!s.empty, "not empty");
        eq(19990L, s.firstDay, "first day");
        eq(11L, s.days, "days");
        eq(2L, s.window(StatsView.WEEK).opened, "week opened");
        eq(3L, s.window(StatsView.WEEK).threats, "week threats");
        eq(5L, s.window(StatsView.MONTH).opened, "month opened");
        eq(3L, s.window(StatsView.MONTH).pictures, "month pictures");
        eq(1234567L, s.window(StatsView.ALL).opened, "all opened");
        eq(1234567L, s.window(99).opened, "an unknown window is all time");
        eq(1234567L, s.window(-1).opened, "a negative window is all time");
        check(s.window(StatsView.WEEK) != s.window(StatsView.MONTH), "week and month are different objects");
        check(!s.window(StatsView.WEEK).isZero(), "week not zero");
        check(new StatsView.Totals().isZero(), "fresh totals are zero");
        // The same JSON read twice gives the same numbers (no hidden state).
        eq(2L, StatsView.parse(utf8(RUST_JSON)).week.opened, "second parse");
        // The largest value the core can send is read exactly.
        StatsView.Summary big = StatsView.parse(utf8("{\"empty\":false,\"all\":{\"opened\":9223372036854775807}}"));
        eq(Long.MAX_VALUE, big.all.opened, "max long");
    }

    private static void garbage() {
        check(StatsView.parse(null).empty, "null");
        check(StatsView.parse(new byte[0]).empty, "empty bytes");
        check(StatsView.parse(utf8("not json")).empty, "text");
        check(StatsView.parse(utf8("[1,2,3]")).empty, "array");
        check(StatsView.parse(utf8("{}")).empty, "object without fields");
        StatsView.Summary neg = StatsView.parse(utf8(
            "{\"empty\":false,\"firstDay\":-5,\"days\":-9,\"week\":{\"opened\":-3,\"trackers\":\"x\"},\"month\":7}"));
        eq(0L, neg.week.opened, "negative becomes zero");
        eq(0L, neg.week.trackers, "a string is zero");
        eq(0L, neg.month.opened, "a number instead of an object is zero");
        eq(0L, neg.firstDay, "negative first day");
        eq(1L, neg.days, "days is at least one");
        check(StatsView.empty().empty && StatsView.empty().all.isZero(), "empty summary");
        eq(1L, StatsView.empty().days, "empty days");
    }

    private static void formatting() {
        eq("0", StatsView.count(0), "zero");
        eq("7", StatsView.count(7), "one digit");
        eq("999", StatsView.count(999), "three digits");
        eq("1 000", StatsView.count(1000), "thousand");
        eq("12 345", StatsView.count(12345), "five digits");
        eq("123 456", StatsView.count(123456), "six digits");
        eq("1 234 567", StatsView.count(1234567), "seven digits");
        eq("9 223 372 036 854 775 807", StatsView.count(Long.MAX_VALUE), "max");
        eq("0", StatsView.count(-4), "negative shows zero");
    }

    private static void verdictTags() {
        eq(0, StatsView.verdictTag(Model.Verdict.CLEAN), "clean");
        eq(1, StatsView.verdictTag(Model.Verdict.HIDDEN_TRACKERS), "trackers");
        eq(2, StatsView.verdictTag(Model.Verdict.ACTIVE_NEUTRALIZED), "active");
        eq(3, StatsView.verdictTag(Model.Verdict.REJECTED), "rejected");
        eq(4, StatsView.verdictTag(Model.Verdict.UNVERIFIED), "unverified");
        eq(4, StatsView.verdictTag(null), "unknown is never clean");
        eq(4, StatsView.verdictTag(Model.Verdict.of("SomethingNew")), "a verdict from the future");
    }

    // ---------------------------------------------------------------------------------------

    /** In-memory storage that remembers every call. */
    private static final class FakeStorage implements StatsStore.Storage {
        byte[] blob = new byte[0];
        boolean on = true;
        byte[] passport = new byte[0];
        long docs;
        long threats;
        boolean legacy;
        int writes;
        int blobDeletes;
        int legacyDeletes;

        @Override public byte[] readBlob() { return blob; }
        @Override public void writeBlob(byte[] b) { blob = b; writes++; }
        @Override public void deleteBlob() { blob = new byte[0]; blobDeletes++; }
        @Override public boolean enabled() { return on; }
        @Override public void setEnabled(boolean v) { on = v; }
        @Override public boolean hasLegacy() { return legacy; }
        @Override public byte[] legacyPassport() { return passport; }
        @Override public long legacyDocuments() { return docs; }
        @Override public long legacyThreats() { return threats; }
        @Override public void deleteLegacy() {
            legacy = false;
            passport = new byte[0];
            docs = 0;
            threats = 0;
            legacyDeletes++;
        }
    }

    /** A codec whose "blob" is a text log of what it was asked; it never needs the native library. */
    private static final class FakeCodec implements StatsStore.Codec {
        final List<String> calls = new ArrayList<>();
        boolean failFold;
        boolean throwLinkage;
        byte[] oversize;

        private byte[] append(byte[] blob, String what) {
            if (throwLinkage) {
                throw new UnsatisfiedLinkError("no native library");
            }
            if (oversize != null) {
                return oversize;
            }
            calls.add(what);
            return (new String(blob, StandardCharsets.UTF_8) + what + ";").getBytes(StandardCharsets.UTF_8);
        }

        @Override public byte[] recordScan(byte[] blob, int day, int verdict, int trackers, int active) {
            return append(blob, "scan:" + day + ":" + verdict + ":" + trackers + ":" + active);
        }

        @Override public byte[] recordEvent(byte[] blob, int day, int event) {
            return append(blob, "event:" + day + ":" + event);
        }

        @Override public byte[] summary(byte[] blob, int today) {
            if (throwLinkage) {
                throw new UnsatisfiedLinkError("no native library");
            }
            calls.add("summary:" + today);
            return utf8(RUST_JSON);
        }

        @Override public byte[] foldLegacy(byte[] blob, byte[] passport, long documents, long threats, int today) {
            if (failFold) {
                return null;
            }
            return append(blob, "fold:" + passport.length + ":" + documents + ":" + threats + ":" + today);
        }

        @Override public byte[] passport(int lang, byte[] blob, int today) {
            if (throwLinkage) {
                throw new UnsatisfiedLinkError("no native library");
            }
            calls.add("passport:" + lang + ":" + today);
            return utf8("{\"title\":\"x\"}");
        }
    }

    private static String text(byte[] b) {
        return new String(b, StandardCharsets.UTF_8);
    }

    private static void recording() {
        FakeStorage st = new FakeStorage();
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        store.recordScan(20000, 1, 3, 0);
        store.recordEvent(20000, StatsStore.EVENT_STERILE_COPY);
        store.recordEvent(20001, StatsStore.EVENT_PICTURE_OPENED);
        eq("scan:20000:1:3:0;event:20000:1;event:20001:2;", text(st.blob), "calls are chained through the blob");
        eq(3, st.writes, "one write per event");
        // Negative and huge counts are clamped before they reach the core.
        store.recordScan(20002, 2, -5, Long.MAX_VALUE);
        check(text(st.blob).endsWith("scan:20002:2:0:2147483647;"), "clamped counts");
        // The summary is read from the core's JSON and the window picked from it.
        StatsView.Summary s = store.summary(20002);
        eq(5L, s.window(StatsView.MONTH).opened, "summary through the store");
        check(codec.calls.contains("summary:20002"), "summary asked the core");
    }

    private static void switchedOff() {
        FakeStorage st = new FakeStorage();
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        store.recordScan(20000, 0, 0, 0);
        check(st.blob.length > 0, "recorded while on");
        store.setEnabled(false);
        check(!store.enabled(), "off");
        eq(0, st.blob.length, "the blob is deleted when statistics are turned off");
        int calls = codec.calls.size();
        int writes = st.writes;
        store.recordScan(20000, 0, 0, 0);
        store.recordEvent(20000, StatsStore.EVENT_PICTURE_OPENED);
        eq(calls, codec.calls.size(), "nothing is recorded while off (the core is not even asked)");
        eq(writes, st.writes, "nothing is written while off");
        check(store.summary(20000).empty, "summary is empty while off");
        eq(calls, codec.calls.size(), "the summary does not ask the core while off");
        check(store.passportJson(0, 20000) == null, "no passport while off");
        store.setEnabled(true);
        store.recordScan(20000, 0, 0, 0);
        check(st.blob.length > 0, "recording resumes when turned on again");
    }

    private static void migration() {
        FakeStorage st = new FakeStorage();
        st.legacy = true;
        st.passport = new byte[36];
        st.docs = 12;
        st.threats = 5;
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        store.recordScan(20000, 0, 0, 0);
        eq("fold:36:12:5:20000;scan:20000:0:0:0;", text(st.blob), "the older counters are folded first, once");
        check(!st.legacy, "the older counters are deleted after the fold");
        eq(1, st.legacyDeletes, "deleted exactly once");
        store.recordScan(20000, 0, 0, 0);
        eq("fold:36:12:5:20000;scan:20000:0:0:0;scan:20000:0:0:0;", text(st.blob), "no second fold");
    }

    private static void migrationWhenReadingOnly() {
        FakeStorage st = new FakeStorage();
        st.legacy = true;
        st.docs = 3;
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        store.summary(20000);
        check(!st.legacy, "a read also migrates");
        check(text(st.blob).startsWith("fold:0:3:0:20000;"), "folded on read");
    }

    private static void migrationSkippedWhenOff() {
        FakeStorage st = new FakeStorage();
        st.on = false;
        st.legacy = true;
        st.docs = 9;
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        store.recordScan(20000, 0, 0, 0);
        check(codec.calls.isEmpty(), "no fold while off");
        // Turning off (or resetting) deletes the older counters too: privacy first.
        store.setEnabled(false);
        check(!st.legacy, "turning off deletes the older counters");
        st.legacy = true;
        st.docs = 4;
        store.reset();
        check(!st.legacy, "reset deletes the older counters");
    }

    private static void failingCore() {
        // A failed fold keeps the older counters for the next attempt and does not break recording.
        FakeStorage st = new FakeStorage();
        st.legacy = true;
        st.docs = 8;
        FakeCodec codec = new FakeCodec();
        codec.failFold = true;
        StatsStore store = new StatsStore(st, codec);
        store.recordScan(20000, 0, 0, 0);
        check(st.legacy, "the older counters survive a failed fold");
        eq("scan:20000:0:0:0;", text(st.blob), "recording went on");
        codec.failFold = false;
        store.recordScan(20000, 0, 0, 0);
        check(!st.legacy, "the fold succeeded on the next attempt");
        check(text(st.blob).startsWith("scan:20000:0:0:0;fold:"), "folded onto the existing blob");

        // A missing native library never throws into the caller.
        FakeStorage st2 = new FakeStorage();
        FakeCodec dead = new FakeCodec();
        dead.throwLinkage = true;
        StatsStore store2 = new StatsStore(st2, dead);
        store2.recordScan(20000, 0, 0, 0);
        store2.recordEvent(20000, 1);
        check(store2.summary(20000).empty, "summary without the core is empty");
        check(store2.passportJson(1, 20000) == null, "no passport without the core");
        eq(0, st2.blob.length, "nothing written");

        // A blob larger than the core ever produces is not stored.
        FakeStorage st3 = new FakeStorage();
        FakeCodec huge = new FakeCodec();
        huge.oversize = new byte[50 * 1024];
        new StatsStore(st3, huge).recordScan(20000, 0, 0, 0);
        eq(0, st3.writes, "an oversized blob is refused");
        huge.oversize = new byte[0];
        new StatsStore(st3, huge).recordScan(20000, 0, 0, 0);
        eq(0, st3.writes, "an empty blob is refused");
    }

    private static void reset() {
        FakeStorage st = new FakeStorage();
        StatsStore store = new StatsStore(st, new FakeCodec());
        store.recordScan(20000, 0, 0, 0);
        store.reset();
        eq(0, st.blob.length, "reset deletes the blob");
        check(store.enabled(), "reset keeps statistics on");
        store.recordScan(20000, 0, 0, 0);
        eq("scan:20000:0:0:0;", text(st.blob), "counting starts again from nothing");
    }

    private static void passportThroughTheStore() {
        FakeStorage st = new FakeStorage();
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        byte[] p = store.passportJson(1, 20000);
        check(p != null && text(p).contains("title"), "passport comes from the core");
        check(codec.calls.contains("passport:1:20000"), "with the language and the day");
    }

    // ---------------------------------------------------------------------------------------

    private static void reminderAnswers() {
        ReminderRules.Decision d = ReminderRules.parse(utf8("{\"action\":\"show\",\"at\":1772323200000}"));
        eq("show", d.action, "action");
        eq(1772323200000L, d.at, "at");
        check(d.arms(), "show arms the alarm");
        check(ReminderRules.parse(utf8("{\"action\":\"quietHours\",\"at\":5}")).arms(), "quiet hours re-arm");
        check(ReminderRules.parse(utf8("{\"action\":\"tooSoon\",\"at\":5}")).arms(), "too soon re-arms");
        check(ReminderRules.parse(utf8("{\"action\":\"noPermission\",\"at\":5}")).arms(), "no permission keeps the chain alive");
        check(!ReminderRules.parse(utf8("{\"action\":\"disabled\",\"at\":0}")).arms(), "disabled does not arm");
        check(!ReminderRules.parse(utf8("{\"action\":\"off\",\"at\":0}")).arms(), "off does not arm");
        check(ReminderRules.parse(utf8("{\"action\":\"keep\",\"at\":9}")).arms(), "keep arms");
        check(ReminderRules.parse(utf8("{\"action\":\"plan\",\"at\":9}")).arms(), "plan arms");
        check(!ReminderRules.parse(utf8("{\"action\":\"show\",\"at\":0}")).arms(), "no time, no alarm");
        check(!ReminderRules.parse(utf8("{\"action\":\"show\",\"at\":-7}")).arms(), "a negative time is none");
        // Anything unreadable is "do nothing".
        for (String bad : new String[] {"", "x", "[]", "{}", "{\"action\":5}"}) {
            ReminderRules.Decision g = ReminderRules.parse(utf8(bad));
            check(!g.arms() && !ReminderRules.SHOW.equals(g.action), "garbage <" + bad + "> does nothing");
        }
        check(!ReminderRules.parse(null).arms(), "null");
    }

    private static void reminderTexts() {
        // index | uses << 8
        eq(5, ReminderRules.index(5 | (1 << 8)), "index");
        eq(1, ReminderRules.uses(5 | (1 << 8)), "uses documents");
        eq(3, ReminderRules.uses(37 | (3 << 8)), "uses both");
        eq(37, ReminderRules.index(37 | (3 << 8)), "last index");
        eq(0, ReminderRules.uses(12), "uses nothing");
        eq(0, ReminderRules.arguments(ReminderRules.USES_NOTHING, 4, 5).length, "no arguments");
        eq(Arrays.asList((Object) 4L), Arrays.asList(ReminderRules.arguments(ReminderRules.USES_DOCUMENTS, 4, 5)), "documents");
        eq(Arrays.asList((Object) 5L), Arrays.asList(ReminderRules.arguments(ReminderRules.USES_THREATS, 4, 5)), "threats");
        eq(Arrays.asList((Object) 4L, (Object) 5L), Arrays.asList(ReminderRules.arguments(ReminderRules.USES_BOTH, 4, 5)), "both");
        eq(Arrays.asList((Object) 0L), Arrays.asList(ReminderRules.arguments(ReminderRules.USES_DOCUMENTS, -4, 5)), "negative is zero");
        // The text of a reminder formats with these arguments.
        eq("You opened 4 documents, 5 threats", String.format("You opened %1$d documents, %2$d threats",
            ReminderRules.arguments(ReminderRules.USES_BOTH, 4, 5)), "format");
    }

    private static void extraSupport() {
        check(ExtraSupport.parse(null).isEmpty(), "null");
        check(ExtraSupport.parse(utf8("{\"items\":[]}")).isEmpty(), "ships empty");
        check(ExtraSupport.parse(utf8("garbage")).isEmpty(), "garbage");
        check(ExtraSupport.parse(utf8("{}")).isEmpty(), "no items key");
        // Synthetic values of realistic shape, not real addresses.
        String json = "{\"items\":["
            + "{\"kind\":\"crypto\",\"label\":\"Bitcoin\",\"network\":\"BTC\",\"value\":\"bc1qtestaddressforunittestsonlyxyz\",\"memo\":\"\"},"
            + "{\"kind\":\"crypto\",\"label\":\"TON\",\"network\":\"TON\",\"value\":\"EQtestaddressfortestsonly0123456789abcdefg\",\"memo\":\"12345\"},"
            + "{\"kind\":\"boosty\",\"label\":\"Boosty\",\"network\":\"\",\"value\":\"https://boosty.to/pharosteam\",\"memo\":\"\"}]}";
        List<ExtraSupport.Entry> items = ExtraSupport.parse(utf8(json));
        eq(3, items.size(), "three entries");
        check(items.get(0).isWallet() && !items.get(2).isWallet(), "wallet vs page");
        eq("12345", items.get(1).memo, "memo kept");
        eq("https://boosty.to/pharosteam", items.get(2).value, "boosty address");
        // Malformed entries are dropped one by one.
        String bad = "{\"items\":["
            + "{\"kind\":\"crypto\",\"label\":\"A\",\"network\":\"BTC\",\"value\":\"has a space in it 0123456789\"},"
            + "{\"kind\":\"crypto\",\"label\":\"B\",\"network\":\"BTC\",\"value\":\"bc1q\\u0430ddressWithCyrillicLookAlike012\"},"
            + "{\"kind\":\"crypto\",\"label\":\"C\",\"network\":\"\",\"value\":\"bc1qtestaddressforunittestsonlyxyz\"},"
            + "{\"kind\":\"crypto\",\"label\":\"\",\"network\":\"BTC\",\"value\":\"bc1qtestaddressforunittestsonlyxyz\"},"
            + "{\"kind\":\"boosty\",\"label\":\"D\",\"value\":\"http://boosty.to/x\"},"
            + "{\"kind\":\"boosty\",\"label\":\"E\",\"value\":\"https://evil.example/boosty.to/x\"},"
            + "{\"kind\":\"other\",\"label\":\"F\",\"network\":\"X\",\"value\":\"bc1qtestaddressforunittestsonlyxyz\"},"
            + "{\"kind\":\"crypto\",\"label\":\"G\",\"network\":\"BTC\",\"value\":\"bc1qtestaddressforunittestsonlyxyz\"}]}";
        List<ExtraSupport.Entry> kept = ExtraSupport.parse(utf8(bad));
        eq(1, kept.size(), "only the well formed entry survives");
        eq("G", kept.get(0).label, "which one");
        // At most 16 entries are read.
        StringBuilder many = new StringBuilder("{\"items\":[");
        for (int i = 0; i < 40; i++) {
            if (i > 0) {
                many.append(',');
            }
            many.append("{\"kind\":\"boosty\",\"label\":\"L\",\"value\":\"https://boosty.to/p").append(i).append("\"}");
        }
        many.append("]}");
        eq(ExtraSupport.MAX_ENTRIES, ExtraSupport.parse(utf8(many.toString())).size(), "capped");
    }

    // ---------------------------------------------------------------------------------------

    public static void main(String[] args) {
        String[] names = {"windows", "garbage", "formatting", "verdictTags", "recording", "switchedOff", "migration",
            "migrationWhenReadingOnly", "migrationSkippedWhenOff", "failingCore", "reset", "passportThroughTheStore",
            "reminderAnswers", "reminderTexts", "extraSupport"};
        for (String name : names) {
            current = name;
            int before = failures;
            try {
                switch (name) {
                    case "windows": windows(); break;
                    case "garbage": garbage(); break;
                    case "formatting": formatting(); break;
                    case "verdictTags": verdictTags(); break;
                    case "recording": recording(); break;
                    case "switchedOff": switchedOff(); break;
                    case "migration": migration(); break;
                    case "migrationWhenReadingOnly": migrationWhenReadingOnly(); break;
                    case "migrationSkippedWhenOff": migrationSkippedWhenOff(); break;
                    case "failingCore": failingCore(); break;
                    case "reset": reset(); break;
                    case "passportThroughTheStore": passportThroughTheStore(); break;
                    case "reminderAnswers": reminderAnswers(); break;
                    case "reminderTexts": reminderTexts(); break;
                    default: extraSupport(); break;
                }
            } catch (Throwable t) {
                failures++;
                System.out.println("  EXCEPTION [" + name + "] " + t);
                t.printStackTrace(System.out);
            }
            System.out.println((failures == before ? "ok    " : "FAIL  ") + name);
        }
        System.out.println(checks + " checks, " + failures + " failure(s)");
        System.exit(failures == 0 ? 0 : 1);
    }
}
