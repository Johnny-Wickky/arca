// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

import org.arca.viewer.core.ExtraSupport;
import org.arca.viewer.core.Model;
import org.arca.viewer.core.ReminderRules;
import org.arca.viewer.core.StatsStore;
import org.arca.viewer.core.StatsView;

/**
 * Tests of the Java pieces around the Rust counter and reminder rules: reading the summary JSON
 * (flat totals, no dates), formatting, the store with its on / off switch (off by default, a blob
 * found while off is deleted), and the readers of the reminder answers and of the extra support
 * addresses. The Rust side has its own tests; the native glue is exercised by the
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
    private static final String RUST_JSON = "{\"version\":2,\"empty\":false,"
        + "\"all\":{\"opened\":1234567,\"clean\":1,\"trackers\":4,\"active\":2,\"threats\":6,\"rejected\":1,\"unverified\":1,\"sterile\":2,\"pictures\":3}}";

    // ---------------------------------------------------------------------------------------

    private static void totals() {
        StatsView.Summary s = StatsView.parse(utf8(RUST_JSON));
        check(!s.empty, "not empty");
        eq(1234567L, s.all.opened, "opened");
        eq(6L, s.all.threats, "threats");
        eq(3L, s.all.pictures, "pictures");
        check(!s.all.isZero(), "not zero");
        check(StatsView.empty().empty && StatsView.empty().all.isZero(), "empty summary");
    }

    private static void garbage() {
        check(StatsView.parse(null).empty, "null");
        check(StatsView.parse(new byte[0]).empty, "empty bytes");
        check(StatsView.parse(utf8("not json")).empty, "text");
        check(StatsView.parse(utf8("[1,2,3]")).empty, "array");
        check(StatsView.parse(utf8("{}")).empty, "object without fields");
        StatsView.Summary neg = StatsView.parse(utf8(
            "{\"empty\":false,\"all\":{\"opened\":-3,\"trackers\":\"x\"}}"));
        eq(0L, neg.all.opened, "negative becomes zero");
        eq(0L, neg.all.trackers, "a string is zero");
        eq(0L, StatsView.parse(utf8("{\"empty\":false,\"all\":7}")).all.opened, "a number instead of an object is zero");
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
        boolean on;
        int writes;
        int blobDeletes;

        @Override public byte[] readBlob() { return blob; }
        @Override public void writeBlob(byte[] b) { blob = b; writes++; }
        @Override public void deleteBlob() { blob = new byte[0]; blobDeletes++; }
        @Override public boolean enabled() { return on; }
        @Override public void setEnabled(boolean v) { on = v; }
    }

    /** A codec that records its calls; it appends one byte per event so the blob grows visibly. */
    private static final class FakeCodec implements StatsStore.Codec {
        final List<String> calls = new ArrayList<>();
        boolean throwRuntime;
        boolean throwLinkage;

        private void gate() {
            if (throwLinkage) {
                throw new UnsatisfiedLinkError("no native library");
            }
            if (throwRuntime) {
                throw new IllegalStateException("boom");
            }
        }

        private static byte[] grow(byte[] blob) {
            byte[] next = new byte[blob.length + 1];
            System.arraycopy(blob, 0, next, 0, blob.length);
            next[blob.length] = 1;
            return next;
        }

        @Override public byte[] recordScan(byte[] blob, int verdict, int trackers, int active) {
            gate();
            calls.add("scan:" + verdict + ":" + trackers + ":" + active);
            return grow(blob);
        }

        @Override public byte[] recordEvent(byte[] blob, int event) {
            gate();
            calls.add("event:" + event);
            return grow(blob);
        }

        @Override public byte[] summary(byte[] blob) {
            gate();
            calls.add("summary");
            return utf8(RUST_JSON);
        }

        @Override public byte[] passport(int lang, byte[] blob, int firstUseDay, int today) {
            gate();
            calls.add("passport:" + lang + ":" + firstUseDay + ":" + today);
            return utf8("{\"title\":\"x\"}");
        }
    }

    private static void offByDefault() {
        FakeStorage st = new FakeStorage();
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        check(!store.enabled(), "a fresh store is off");
        store.recordScan(0, 1, 2);
        store.recordEvent(StatsStore.EVENT_PICTURE_OPENED);
        check(codec.calls.isEmpty(), "nothing reaches the core while off");
        eq(0, st.writes, "nothing is written while off");
        check(store.summary().empty, "summary is empty while off");
        check(store.passportJson(1, 20000, 20001) == null, "no passport while off");
    }

    private static void recording() {
        FakeStorage st = new FakeStorage();
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        store.setEnabled(true);
        store.recordScan(2, 3, 4);
        store.recordEvent(StatsStore.EVENT_STERILE_COPY);
        eq(2, st.writes, "both events stored");
        eq(2, st.blob.length, "the blob grew");
        eq("scan:2:3:4", codec.calls.get(0), "scan arguments carry no day");
        eq("event:1", codec.calls.get(1), "event arguments carry no day");
        store.recordScan(0, Long.MAX_VALUE, -5);
        eq("scan:0:2147483647:0", codec.calls.get(2), "counts are clamped");
        check(codec.calls.contains("scan:0:2147483647:0") && !store.summary().empty, "summary after recording");
    }

    private static void switchingOffDeletes() {
        FakeStorage st = new FakeStorage();
        StatsStore store = new StatsStore(st, new FakeCodec());
        store.setEnabled(true);
        store.recordScan(0, 0, 0);
        check(st.blob.length > 0, "blob stored");
        store.setEnabled(false);
        eq(0, st.blob.length, "off deletes the blob at once");
        store.recordScan(0, 0, 0);
        eq(0, st.blob.length, "and counts nothing afterwards");
    }

    private static void purgeAtStart() {
        FakeStorage st = new FakeStorage();
        st.blob = new byte[] {1, 2, 3}; // left by an older version that counted by default
        StatsStore store = new StatsStore(st, new FakeCodec());
        store.purgeIfOff();
        eq(0, st.blob.length, "a blob found while the counter is off is deleted");
        st.on = true;
        st.blob = new byte[] {1, 2, 3};
        store.purgeIfOff();
        eq(3, st.blob.length, "a blob of a counter that is on is kept");
    }

    private static void failingCore() {
        FakeStorage st = new FakeStorage();
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        store.setEnabled(true);
        codec.throwRuntime = true;
        store.recordScan(0, 1, 1);
        store.recordEvent(1);
        check(store.summary().empty, "a failing core gives empty totals");
        check(store.passportJson(0, 1, 1) == null, "and no passport");
        codec.throwRuntime = false;
        codec.throwLinkage = true;
        store.recordScan(0, 1, 1);
        check(store.summary().empty, "a missing library gives empty totals");
        eq(0, st.writes, "nothing written on failure");
    }

    private static void reset() {
        FakeStorage st = new FakeStorage();
        StatsStore store = new StatsStore(st, new FakeCodec());
        store.setEnabled(true);
        store.recordScan(0, 0, 0);
        store.reset();
        eq(0, st.blob.length, "reset deletes the blob");
        check(st.on, "the switch stays as it was");
    }

    private static void passportThroughTheStore() {
        FakeStorage st = new FakeStorage();
        FakeCodec codec = new FakeCodec();
        StatsStore store = new StatsStore(st, codec);
        store.setEnabled(true);
        check(store.passportJson(1, 20000, 20003) != null, "passport when on");
        check(codec.calls.contains("passport:1:20000:20003"), "with the language, the install day and today");
    }

    private static void reminderAnswers() {
        check(ReminderRules.ENABLED_BY_DEFAULT, "the support card is on by default");
        check(ReminderRules.enabledOrDefault(false, false), "no stored choice: on");
        check(ReminderRules.enabledOrDefault(true, true), "stored on");
        check(!ReminderRules.enabledOrDefault(true, false), "stored off is kept");
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
        String[] names = {"totals", "garbage", "formatting", "verdictTags", "offByDefault", "recording", "switchingOffDeletes", "purgeAtStart", "failingCore", "reset", "passportThroughTheStore", "reminderAnswers", "extraSupport"};
        for (String name : names) {
            current = name;
            int before = failures;
            try {
                switch (name) {
                    case "totals": totals(); break;
                    case "garbage": garbage(); break;
                    case "formatting": formatting(); break;
                    case "verdictTags": verdictTags(); break;
                    case "offByDefault": offByDefault(); break;
                    case "recording": recording(); break;
                    case "switchingOffDeletes": switchingOffDeletes(); break;
                    case "purgeAtStart": purgeAtStart(); break;
                    case "failingCore": failingCore(); break;
                    case "reset": reset(); break;
                    case "passportThroughTheStore": passportThroughTheStore(); break;
                    case "reminderAnswers": reminderAnswers(); break;
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
