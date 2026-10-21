// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Map;
import java.util.concurrent.Executor;

import org.arca.viewer.core.Breakpoints;
import org.arca.viewer.core.Bookmarks;
import org.arca.viewer.core.Capabilities;
import org.arca.viewer.core.DocOutline;
import org.arca.viewer.core.DocSearch;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Json;
import org.arca.viewer.core.Model;
import org.arca.viewer.core.Names;
import org.arca.viewer.core.PageCache;
import org.arca.viewer.core.PageDrawing;
import org.arca.viewer.core.Posture;
import org.arca.viewer.core.Zoom;

/** Unit tests of the pure-Java core (no Android, no JUnit: a plain runner, see run.sh). */
public final class CoreTest {
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

    private static void throwsJson(String text, String what) {
        try {
            Json.parse(text);
            check(false, what + ": accepted <" + text + ">");
        } catch (Json.JsonException e) {
            check(true, what);
        } catch (RuntimeException e) {
            check(false, what + ": wrong exception " + e);
        }
    }

    // ---------------------------------------------------------------------------------------

    static void json() {
        eq(Long.valueOf(42), Json.parse("42"), "integer");
        eq(Long.valueOf(-7), Json.parse(" -7 "), "negative integer with spaces");
        eq(Double.valueOf(1.5), Json.parse("1.5"), "double");
        eq(Double.valueOf(1000.0), Json.parse("1e3"), "exponent");
        eq(Boolean.TRUE, Json.parse("true"), "true");
        eq(Boolean.FALSE, Json.parse("false"), "false");
        eq(null, Json.parse("null"), "null");
        eq("a\"b\\c/d\b\f\n\r\t", Json.parse("\"a\\\"b\\\\c\\/d\\b\\f\\n\\r\\t\""), "escapes");
        eq("é€", Json.parse("\"\\u00e9\\u20AC\""), "unicode escapes");
        eq("\uD83D\uDE00", Json.parse("\"\\uD83D\\uDE00\""), "surrogate pair via escapes");
        eq("\uD83D\uDE00", Json.parse("\"\uD83D\uDE00\""), "raw astral character");
        eq("привет", Json.parse("\"привет\""), "raw cyrillic");
        eq("привет", Json.parse("\"привет\"".getBytes(StandardCharsets.UTF_8)), "utf-8 bytes");
        Object o = Json.parse("{\"a\":[1,2,{\"b\":null}],\"c\":\"d\"}");
        check(o instanceof Map, "object");
        Map<String, Object> m = Json.obj(o);
        eq(Arrays.asList("a", "c"), new ArrayList<String>(m.keySet()), "key order kept");
        eq(3, Json.arr(m.get("a")).size(), "array length");
        eq(Long.valueOf(9223372036854775807L), Json.parse("9223372036854775807"), "long max");
        check(Json.parse("9223372036854775808") instanceof Double, "overflowing integer becomes a double");
        eq(0, ((Map<?, ?>) Json.parse("{}")).size(), "empty object");
        eq(0, ((List<?>) Json.parse("[]")).size(), "empty array");

        throwsJson("", "empty");
        throwsJson("{", "unterminated object");
        throwsJson("[1,]", "trailing comma in array");
        throwsJson("{\"a\":1,}", "trailing comma in object");
        throwsJson("{a:1}", "unquoted key");
        throwsJson("\"abc", "unterminated string");
        throwsJson("\"a\nb\"", "raw newline in string");
        throwsJson("\"\\x\"", "bad escape");
        throwsJson("\"\\u12\"", "short unicode escape");
        throwsJson("\"\\u12G4\"", "bad hex digit");
        throwsJson("tru", "truncated literal");
        throwsJson("1 2", "trailing value");
        throwsJson("-", "lone minus");
        throwsJson("1.", "fraction without digits");
        throwsJson("1e", "exponent without digits");
        throwsJson("[1 2]", "missing comma");
        throwsJson("{\"a\" 1}", "missing colon");
        throwsJson("nul", "truncated null");
        throwsJson("[" + new String(new char[100]).replace("\0", "[") + new String(new char[100]).replace("\0", "]") + "]",
            "nesting beyond the limit");
        // Exactly at the limit is fine.
        String deep = new String(new char[Json.MAX_DEPTH]).replace("\0", "[")
            + new String(new char[Json.MAX_DEPTH]).replace("\0", "]");
        check(Json.parse(deep) instanceof List, "nesting at the limit");
        try {
            Json.parse((byte[]) null);
            check(false, "null bytes");
        } catch (Json.JsonException e) {
            check(true, "null bytes");
        }
        // Accessors are tolerant.
        Map<String, Object> t = Json.obj(Json.parse("{\"s\":5,\"n\":\"x\",\"b\":1}"));
        eq("", Json.str(t, "s"), "string accessor on a number");
        eq(0L, Json.num(t, "n"), "number accessor on a string");
        eq(false, Json.bool(t, "b"), "bool accessor on a number");
        eq("", Json.str(t, "missing"), "missing string");
        eq(0, Json.obj("x").size(), "obj of a string");
        eq(0, Json.arr(null).size(), "arr of null");
        eq(Integer.MAX_VALUE, Json.integer(Json.obj(Json.parse("{\"x\":99999999999}")), "x"), "integer saturates");
        eq(Arrays.asList("a", "b"), Json.strings(Json.obj(Json.parse("{\"l\":[\"a\",3,\"b\",null]}")), "l"),
            "strings skips non-strings");
    }

    static void fmt() {
        eq("plain", Fmt.safe("plain"), "plain text is unchanged");
        eq("tab\tnl\n", Fmt.safe("tab\tnl\n"), "tab and newline are kept");
        eq("a\u2039U+202E\u203Ab", Fmt.safe("a\u202Eb"), "right-to-left override is made visible");
        eq("\u2039U+0000\u203A", Fmt.safe("\u0000"), "NUL is made visible");
        eq("\u2039U+000D\u203A", Fmt.safe("\r"), "carriage return is made visible");
        eq("\u2039U+200B\u203A", Fmt.safe("\u200B"), "zero-width space is made visible");
        eq("\u2039U+FEFF\u203A", Fmt.safe("\uFEFF"), "BOM is made visible");
        eq("\u2039U+2066\u203A", Fmt.safe("\u2066"), "isolate is made visible");
        eq("\u2039U+0085\u203A", Fmt.safe("\u0085"), "C1 control is made visible");
        eq("日本語 \uD83D\uDE00", Fmt.safe("日本語 \uD83D\uDE00"), "ordinary Unicode is unchanged");
        eq("", Fmt.safe(null), "null");
        String huge = new String(new char[Fmt.MAX_DISPLAY + 50]).replace('\0', 'x');
        eq(Fmt.MAX_DISPLAY + 1, Fmt.safe(huge).length(), "very long text is cut with an ellipsis");

        eq("abc", Fmt.clip("abc", 5), "clip short");
        eq("ab\u2026", Fmt.clip("abcdef", 3), "clip");
        String emoji = "ab\uD83D\uDE00cd";
        String c = Fmt.clip(emoji, 4);
        check(c.length() == 3 && c.endsWith("\u2026") && !Character.isHighSurrogate(c.charAt(1)), "clip never splits a pair: " + c);
        eq("abcdefghij", Fmt.middle("abcdefghij", 10), "middle short");
        String mid = Fmt.middle("0123456789abcdefghij", 9);
        eq(9, mid.length(), "middle length");
        check(mid.startsWith("0123") && mid.endsWith("ghij") && mid.contains("\u2026"), "middle shape: " + mid);

        eq("0 B", Fmt.size(0), "size 0");
        eq("1023 B", Fmt.size(1023), "size 1023");
        check(Fmt.size(1024).startsWith("1") && Fmt.size(1024).endsWith("KB"), "size 1 KB: " + Fmt.size(1024));
        check(Fmt.size(5L << 20).endsWith("MB"), "size MB");
        check(Fmt.size(3L << 30).endsWith("GB"), "size GB");
        check(Fmt.size(Long.MAX_VALUE).endsWith("TB"), "size huge");
        eq("?", Fmt.size(-1), "size negative");

        eq("abcdefgh ijklmnop qr", Fmt.grouped("abcdefghijklmnopqr"), "grouped");
        eq("", Fmt.grouped(""), "grouped empty");

        eq(0, Fmt.epochDay(0, 0), "epoch day 0");
        eq(1, Fmt.epochDay(86_400_000L, 0), "epoch day 1");
        eq(1, Fmt.epochDay(86_399_999L, 1), "zone offset moves the day");
        eq(0, Fmt.epochDay(0, -3_600_000), "before 1970 is clamped");
        eq(20_000, Fmt.epochDay(20_000L * 86_400_000L + 5, 0), "arbitrary day");

        eq("report.sterile.docx", Fmt.sterileName("report.docm", "docx"), "sterile name");
        eq("b.sterile.docx", Fmt.sterileName("a/b.docm", "docx"), "directory part is dropped");
        eq("b.sterile.docx", Fmt.sterileName("../../etc/b.docm", "docx"), "directory parts removed");
        eq("document.sterile.docx", Fmt.sterileName("", "docx"), "empty name");
        eq("document.sterile.docx", Fmt.sterileName(null, "docx"), "null name");
        eq("hidden.sterile.docx", Fmt.sterileName(".hidden", "docx"), "leading dots are removed");
        eq("document.sterile.docx", Fmt.sterileName("...", "docx"), "only dots");
        eq("a_b.sterile.xlsx", Fmt.sterileName("a:b.xlsm", "xlsx"), "reserved characters replaced");
        eq("evil_.sterile.docx", Fmt.sterileName("evil\u202E.docx", "docx"), "direction mark replaced");
        check(!Fmt.sterileName("x\u202Ey.docx", "docx").contains("\u202E"), "no direction mark survives");
        check(Fmt.sterileName(new String(new char[500]).replace('\0', 'n') + ".docx", "docx").length() < 100, "length capped");
        eq("n.sterile", Fmt.sterileName("n.docx", ""), "no extension");
        eq("n.sterile.docx", Fmt.sterileName("n.docx", "../d/ocx"), "extension is sanitised");

        eq("docx", Fmt.extensionOf("Docm"), "macro-enabled maps to macro-free extension");
        eq("", Fmt.extensionOf("Executable"), "unknown format has no extension");
        check(Fmt.mimeOf("Docx").endsWith("wordprocessingml.document"), "docx MIME");
        eq("application/octet-stream", Fmt.mimeOf("Pdf"), "generic MIME");
    }

    static void names() {
        eq("external_image", Names.snake("ExternalImage"), "snake");
        eq("pdf_java_script", Names.snake("PdfJavaScript"), "snake with acronym-like word");
        eq("vba_macros", Names.snake("VbaMacros"), "snake vba");
        eq("", Names.snake(null), "snake null");
        eq("a1_b", Names.snake("A1B"), "digits kept");
        eq("threat_external_image_title", Names.threatTitle("ExternalImage"), "title key");
        eq("threat_external_image_text", Names.threatText("ExternalImage"), "text key");
        // A hostile kind id can never produce an invalid resource name.
        eq("threat_evil_title", Names.threatTitle("../Evil"), "path characters are dropped");
    }

    // ---- model --------------------------------------------------------------------------------

    static final String REPORT =
        "{\"format\":\"Docx\",\"family\":\"Text\",\"verdict\":\"ActiveContentNeutralized\",\"rejection\":\"\","
        + "\"sha256\":\"ab12\",\"counters\":{\"trackers\":2,\"active\":1,\"findings\":3},"
        + "\"findings\":[{\"kind\":\"MacroAutoExec\",\"cat\":\"ActiveContent\",\"sev\":\"Critical\",\"loc\":\"word/vbaProject.bin\",\"detail\":\"AutoOpen\",\"blocked\":true},"
        + "{\"kind\":\"ExternalImage\",\"cat\":\"RemoteContent\",\"sev\":\"Medium\",\"loc\":\"r.xml\",\"detail\":\"https://t.example/p.gif\",\"blocked\":true},"
        + "{\"kind\":\"AuthorMetadata\",\"cat\":\"PrivacyLeak\",\"sev\":\"Info\",\"loc\":\"docProps/core.xml\",\"detail\":\"\",\"blocked\":false}],"
        + "\"macros\":[{\"lang\":\"Vba\",\"loc\":\"vba\",\"name\":\"ThisDocument\",\"source\":\"Sub AutoOpen()\\nEnd Sub\",\"truncated\":false,"
        + "\"autoExec\":[\"AutoOpen\"],\"suspicious\":[\"Shell\"],\"iocs\":[\"http://x.test\"]}],"
        + "\"refs\":[{\"target\":\"https://t.example/p.gif\",\"kind\":\"Image\",\"class\":\"Web\",\"loc\":\"r.xml\",\"auto\":true}],"
        + "\"meta\":[{\"key\":\"Author\",\"name\":\"\",\"value\":\"Eve\",\"src\":\"core.xml\"}],"
        + "\"embedded\":[{\"loc\":\"o.bin\",\"kind\":\"Package\",\"class\":\"Package\",\"clsid\":\"\",\"file\":\"a.exe\",\"size\":10,\"sha256\":\"ff\"}],"
        + "\"stats\":{\"parts\":5,\"xmlEvents\":99,\"inflated\":1234,\"nesting\":1}}";

    static void report() {
        Model.Report r = Model.parseReport(REPORT.getBytes(StandardCharsets.UTF_8));
        eq("Docx", r.format, "format");
        eq("Text", r.family, "family");
        eq(Model.Verdict.ACTIVE_NEUTRALIZED, r.verdict, "verdict");
        eq(2L, r.trackers, "trackers");
        eq(1L, r.active, "active");
        eq(3, r.findings.size(), "findings");
        eq("MacroAutoExec", r.findings.get(0).kind, "first finding");
        eq(4, r.findings.get(0).rank(), "critical rank");
        eq(true, r.findings.get(0).blocked, "blocked flag");
        eq(1, r.count("RemoteContent"), "count by category");
        eq(4, r.topRank(), "top rank");
        eq(1, r.macros.size(), "macros");
        eq("Sub AutoOpen()\nEnd Sub", r.macros.get(0).source, "macro source keeps newlines");
        eq(Arrays.asList("AutoOpen"), r.macros.get(0).autoExec, "auto exec");
        eq(true, r.refs.get(0).auto, "auto-load reference");
        eq("Eve", r.meta.get(0).value, "metadata");
        eq(10L, r.embedded.get(0).size, "embedded size");
        eq("a.exe", r.embedded.get(0).file, "embedded file name");
        eq(1234L, r.inflated, "inflated");

        // Tolerant parsing: an empty object still gives a (non-clean) report.
        Model.Report e = Model.parseReport("{}".getBytes(StandardCharsets.UTF_8));
        eq(Model.Verdict.UNVERIFIED, e.verdict, "a missing verdict is never 'clean'");
        eq("Unknown", e.format, "default format");
        // Unknown verdict names from a newer engine are treated as unverified, not clean.
        eq(Model.Verdict.UNVERIFIED, Model.Verdict.of("SomethingNew"), "unknown verdict");
        eq(Model.Verdict.CLEAN, Model.Verdict.of("Clean"), "clean");
        eq(Model.Verdict.REJECTED, Model.Verdict.of("Rejected"), "rejected");
        eq(Model.Verdict.HIDDEN_TRACKERS, Model.Verdict.of("HiddenTrackers"), "trackers");
        // Broken JSON throws.
        try {
            Model.parseReport("{\"format\":".getBytes(StandardCharsets.UTF_8));
            check(false, "truncated report accepted");
        } catch (Json.JsonException ex) {
            check(true, "truncated report rejected");
        }
        eq(0, Model.severityRank("Info"), "rank info");
        eq(1, Model.severityRank("Low"), "rank low");
        eq(0, Model.severityRank("Bogus"), "rank unknown");
    }

    static void preview() {
        String meta = "{\"format\":\"Xlsx\",\"family\":\"Spreadsheet\",\"sections\":[{\"kind\":\"Sheet\",\"name\":\"S1\",\"hidden\":true,\"items\":5}],"
            + "\"notices\":[{\"kind\":\"HiddenShown\",\"count\":2,\"detail\":\"text\"}],\"stats\":{\"chars\":10,\"items\":5,\"truncated\":true}}";
        Model.PreviewMeta p = Model.parsePreviewMeta(meta.getBytes(StandardCharsets.UTF_8));
        eq("Spreadsheet", p.family, "family");
        eq(1, p.sections.size(), "sections");
        eq(true, p.sections.get(0).hidden, "hidden section");
        eq(5L, p.sections.get(0).items, "items");
        eq("HiddenShown", p.notices.get(0).kind, "notice");
        eq(true, p.truncated, "truncated");

        // A document whose parts are all hidden (and not asked for) has no section at all; the
        // preview screen must be able to show that instead of indexing into an empty list.
        String none = "{\"format\":\"Xlsx\",\"family\":\"Spreadsheet\",\"sections\":[],"
            + "\"notices\":[{\"kind\":\"HiddenOmitted\",\"count\":1,\"detail\":\"sheets\"}],\"stats\":{\"chars\":0,\"items\":0,\"truncated\":false}}";
        Model.PreviewMeta empty = Model.parsePreviewMeta(none.getBytes(StandardCharsets.UTF_8));
        eq(0, empty.sections.size(), "no sections");
        eq(1, empty.notices.size(), "the notice survives");
        eq("HiddenOmitted", empty.notices.get(0).kind, "hidden omitted notice");

        String items ="[{\"k\":\"p\",\"s\":\"Heading1\",\"a\":1,\"l\":2,\"m\":\"1.\",\"r\":[[\"Hello \",1],[\"link\",4,\"https://x.test\"]]},"
            + "{\"k\":\"tr\",\"n\":3,\"first\":true,\"last\":false,\"cells\":[{\"c\":2,\"h\":true,\"p\":[{\"r\":[[\"a\",0]]}]},{\"c\":1,\"h\":false,\"p\":[]}]},"
            + "{\"k\":\"gh\",\"cols\":3,\"w\":[8,12,6],\"rows\":120,\"trunc\":false},"
            + "{\"k\":\"gr\",\"i\":7,\"h\":true,\"c\":[[0,0,\"text\",\"https://cell.test/x\"],[2,1,\"1,5\"]]},"
            + "{\"k\":\"i\",\"alt\":\"logo\",\"media\":\"png\",\"bytes\":1234,\"ext\":true},"
            + "{\"k\":\"r\"},{\"k\":\"zzz\"},{\"nokind\":1}]";
        List<Model.Item> list = Model.parseItems(items.getBytes(StandardCharsets.UTF_8));
        eq(6, list.size(), "unknown item kinds are skipped");
        Model.Para para = ((Model.ParaItem) list.get(0)).para;
        eq("Heading1", para.style, "paragraph style");
        eq(1, para.align, "align");
        eq(2, para.level, "level");
        eq("1.", para.marker, "marker");
        eq(2, para.runs.size(), "runs");
        eq("Hello link", para.plain(), "plain text");
        eq(Model.BOLD, para.runs.get(0).style, "bold");
        eq("https://x.test", para.runs.get(1).href, "link target");
        eq("", para.runs.get(0).href, "no link");
        Model.TableRow row = (Model.TableRow) list.get(1);
        eq(3, row.columns, "columns");
        eq(true, row.first, "first row");
        eq(2, row.cells.get(0).span, "span");
        eq(true, row.cells.get(0).header, "header cell");
        eq("a", row.cells.get(0).paras.get(0).plain(), "cell text");
        Model.GridHeader gh = (Model.GridHeader) list.get(2);
        eq(3, gh.cols, "grid columns");
        check(Arrays.equals(new int[] {8, 12, 6}, gh.widths), "grid widths");
        eq(120L, gh.rows, "grid rows");
        Model.GridRow gr = (Model.GridRow) list.get(3);
        eq(7L, gr.index, "grid row index");
        eq(true, gr.hidden, "hidden row");
        eq(2, gr.cells.size(), "grid cells");
        eq(2, gr.cells.get(1).col, "cell column");
        eq(Model.CELL_NUMBER, gr.cells.get(1).kind, "cell kind");
        eq("1,5", gr.cells.get(1).text, "cell text");
        eq("https://cell.test/x", gr.cells.get(0).href, "link of a cell");
        eq("", gr.cells.get(1).href, "a cell without a link has an empty address");
        Model.ImageItem img = (Model.ImageItem) list.get(4);
        eq("logo", img.alt, "image alt");
        eq(true, img.external, "external image");
        check(list.get(5) instanceof Model.RuleItem, "rule");
        eq(0, Model.parseItems("[]".getBytes(StandardCharsets.UTF_8)).size(), "empty page");
    }

    static void other() {
        Model.SanitizedInfo s = Model.parseSanitized(("{\"format\":\"Docx\",\"size\":99,\"actions\":[{\"kind\":\"RemovedPart\",\"target\":\"word/vbaProject.bin\"}],"
            + "\"verification\":{\"verdict\":\"Clean\",\"sha256\":\"aa\",\"findings\":0}}").getBytes(StandardCharsets.UTF_8));
        eq("Docx", s.format, "sanitized format");
        eq(99L, s.size, "sanitized size");
        eq(1, s.actions.size(), "actions");
        eq("RemovedPart", s.actions.get(0).kind, "action kind");
        eq(Model.Verdict.CLEAN, s.verdict, "verification verdict");

        Model.About a = Model.parseAbout(("{\"appName\":\"Arca\",\"fingerprint\":\"f\",\"screen\":{\"lang\":\"ru\",\"cards\":["
            + "{\"role\":\"Upstream\",\"title\":\"T\",\"project\":\"P\",\"people\":[{\"name\":\"N\",\"url\":null},{\"name\":\"M\",\"url\":\"https://m\"}],"
            + "\"supportLabel\":\"S\",\"support\":[{\"label\":\"L\",\"url\":\"https://l\"}],\"homepage\":\"https://h\",\"source\":null,\"modifications\":null}],"
            + "\"legal\":\"legal\",\"modified\":null}}").getBytes(StandardCharsets.UTF_8));
        eq("Arca", a.appName, "app name");
        eq(1, a.screen.cards.size(), "cards");
        eq(true, a.screen.cards.get(0).upstream, "upstream card");
        eq("", a.screen.cards.get(0).people.get(0).url, "null url becomes empty");
        eq("https://m", a.screen.cards.get(0).people.get(1).url, "person url");
        eq("https://h", a.screen.cards.get(0).homepage, "homepage");
        eq("", a.screen.cards.get(0).source, "null source");
        eq("", a.screen.modified, "null modified notice");

        Model.Passport p = Model.parsePassport(("{\"title\":\"t\",\"body\":\"b\",\"cta\":\"c\",\"documents\":3,\"trackers\":4,\"active\":5,\"days\":6,"
            + "\"screen\":{\"lang\":\"en\",\"cards\":[],\"legal\":\"\",\"modified\":null}}").getBytes(StandardCharsets.UTF_8));
        eq(3L, p.documents, "documents");
        eq(6L, p.days, "days");

        Model.ReleaseInfo r = Model.parseReleaseInfo("{\"ready\":false,\"blockers\":[\"a\",\"b\"]}".getBytes(StandardCharsets.UTF_8));
        eq(false, r.ready, "not ready");
        eq(2, r.blockers.size(), "blockers");

        Model.SelfTest t = Model.parseSelfTest(("{\"ok\":true,\"guestSha256\":\"g\",\"steps\":[{\"name\":\"n\",\"ok\":true,\"ms\":5,\"detail\":\"d\"}]}")
            .getBytes(StandardCharsets.UTF_8));
        eq(true, t.ok, "self-test ok");
        eq(1, t.steps.size(), "steps");

        Model.RunInfo ri = Model.parseRunInfo(("{\"status\":2,\"statusName\":\"Timeout\",\"op\":1,\"message\":\"m\","
            + "\"stats\":{\"fuel\":1,\"fuelLimit\":2,\"peakMemory\":3,\"slices\":4,\"wallMs\":5}}").getBytes(StandardCharsets.UTF_8));
        eq(2, ri.status, "status");
        eq(5L, ri.wallMs, "wall");

        Model.EngineInfo ei = Model.parseEngineInfo(("{\"engine\":\"0.1.0\",\"abi\":2,\"guestSha256\":\"g\",\"arch\":\"aarch64\",\"os\":\"android\","
            + "\"appName\":\"Arca\",\"maxInputBytes\":10,\"maxMemoryBytes\":20}").getBytes(StandardCharsets.UTF_8));
        eq(2, ei.abi, "abi");
        eq("aarch64", ei.arch, "arch");
    }

    // ---- page cache ---------------------------------------------------------------------------

    /** Runs tasks when told to, so the test controls the interleaving. */
    static final class Queue implements Executor {
        final List<Runnable> tasks = new ArrayList<>();

        @Override
        public void execute(Runnable r) {
            tasks.add(r);
        }

        void drain() {
            while (!tasks.isEmpty()) {
                tasks.remove(0).run();
            }
        }
    }

    static void pageCache() {
        final List<long[]> requests = new ArrayList<>();
        final Queue worker = new Queue();
        final Queue ui = new Queue();
        final int[] changed = {0};
        final int[] failed = {0};
        final boolean[] failNext = {false};
        PageCache cache = new PageCache(new PageCache.Loader() {
            @Override
            public List<Model.Item> load(int section, long from, int count) throws Exception {
                requests.add(new long[] {section, from, count});
                if (failNext[0]) {
                    throw new Exception("boom");
                }
                List<Model.Item> out = new ArrayList<>();
                for (int i = 0; i < count; i++) {
                    out.add(new Model.RuleItem());
                }
                return out;
            }
        }, worker, ui, new PageCache.Listener() {
            @Override
            public void onChanged() {
                changed[0]++;
            }

            @Override
            public void onFailed(Exception e) {
                failed[0]++;
            }
        });

        final long total = 1_000_000L;
        check(cache.get(0, 5, total) instanceof Model.Pending, "first access is pending");
        check(cache.get(0, 6, total) instanceof Model.Pending, "second access still pending");
        eq(1, worker.tasks.size(), "one load request for the same page");
        worker.drain();
        eq(1, requests.size(), "loader called once");
        eq(0L, requests.get(0)[1], "page starts at item 0");
        eq((long) PageCache.PAGE, requests.get(0)[2], "a full page");
        eq(0, changed[0], "UI not told before it runs the delivery");
        ui.drain();
        eq(1, changed[0], "UI told once");
        check(cache.get(0, 5, total) instanceof Model.RuleItem, "loaded item is returned");

        // Near the end of a page the next one is requested ahead of time.
        cache.get(0, PageCache.PAGE - 2, total);
        eq(1, worker.tasks.size(), "read-ahead requested");
        worker.drain();
        ui.drain();
        eq(2, cache.cachedPages(), "two pages cached");

        // The last, short page.
        long last = total - 1;
        check(cache.get(0, last, total) instanceof Model.Pending, "last page pending");
        worker.drain();
        long[] lastReq = requests.get(requests.size() - 1);
        eq(last / PageCache.PAGE * PageCache.PAGE, lastReq[1], "last page start");
        eq(total - lastReq[1], lastReq[2], "last page is short");
        ui.drain();
        check(cache.get(0, last, total) instanceof Model.RuleItem, "last item loaded");

        // Out-of-range positions never load anything.
        int before = requests.size();
        check(cache.get(0, -1, total) instanceof Model.Pending, "negative index");
        check(cache.get(0, total, total) instanceof Model.Pending, "index == total");
        worker.drain();
        eq(before, requests.size(), "no request for out-of-range positions");

        // A cleared cache ignores results of requests started before the clear.
        cache.get(1, 0, total);
        cache.clear();
        worker.drain();
        int changedBefore = changed[0];
        ui.drain();
        eq(changedBefore, changed[0], "stale result ignored after clear");
        eq(0, cache.cachedPages(), "cache empty after clear");

        // Failures are reported once and not retried in a loop.
        failNext[0] = true;
        cache.get(2, 0, total);
        worker.drain();
        ui.drain();
        eq(1, failed[0], "failure reported");
        cache.get(2, 0, total);
        eq(0, worker.tasks.size(), "a failed page is not re-requested on every draw");

        // Eviction keeps memory bounded.
        failNext[0] = false;
        PageCache small = new PageCache(new PageCache.Loader() {
            @Override
            public List<Model.Item> load(int section, long from, int count) {
                List<Model.Item> out = new ArrayList<>();
                for (int i = 0; i < count; i++) {
                    out.add(new Model.RuleItem());
                }
                return out;
            }
        }, new Executor() {
            @Override
            public void execute(Runnable r) {
                r.run();
            }
        }, new Executor() {
            @Override
            public void execute(Runnable r) {
                r.run();
            }
        }, new PageCache.Listener() {
            @Override
            public void onChanged() {}

            @Override
            public void onFailed(Exception e) {}
        });
        for (long page = 0; page < 200; page++) {
            small.get(0, page * PageCache.PAGE + 30, 1_000_000L);
        }
        check(small.cachedPages() <= 40, "cache is bounded: " + small.cachedPages());
    }

    static void breakpoints() {
        // Window widths of real devices, in dp.
        int[][] widths = {
            {320, Breakpoints.COMPACT},   // small phone
            {344, Breakpoints.COMPACT},   // Galaxy Z Fold cover display
            {360, Breakpoints.COMPACT},   // typical phone, Flip main display
            {412, Breakpoints.COMPACT},   // Pixel / Tecno Phantom V Flip
            {599, Breakpoints.COMPACT},
            {600, Breakpoints.MEDIUM},    // 7-inch tablet
            {673, Breakpoints.MEDIUM},    // Galaxy Z Fold inner display, portrait
            {839, Breakpoints.MEDIUM},
            {840, Breakpoints.EXPANDED},
            {841, Breakpoints.EXPANDED},  // Pixel Fold / Z Fold inner display, landscape
            {1484, Breakpoints.EXPANDED}, // Pixel Tablet
        };
        for (int[] w : widths) {
            eq(w[1], Breakpoints.widthClass(w[0]), "width class of " + w[0] + " dp");
            check(Breakpoints.padding(w[0]) >= 12 && Breakpoints.padding(w[0]) <= 32, "padding of " + w[0]);
            // A cap, not a width: a window narrower than the cap simply uses all of itself.
            eq(w[1] == Breakpoints.COMPACT ? w[0] : 720, Breakpoints.readingWidth(w[0]), "reading width of " + w[0]);
            check(Breakpoints.leftPane(w[0]) >= 320 && Breakpoints.leftPane(w[0]) <= 420, "left pane of " + w[0]);
        }
        eq(12, Breakpoints.padding(320), "padding on a very narrow window");
        eq(12, Breakpoints.padding(359), "padding below 360 dp");
        eq(16, Breakpoints.padding(360), "padding at 360 dp");
        eq(24, Breakpoints.padding(700), "padding, medium");
        eq(32, Breakpoints.padding(1000), "padding, expanded");
        eq(412, Breakpoints.readingWidth(412), "a phone uses its whole width");
        eq(720, Breakpoints.readingWidth(600), "text column is capped");
        eq(720, Breakpoints.readingWidth(1484), "text column is capped on a tablet");
        check(!Breakpoints.twoPane(839), "no two panes at 839 dp");
        check(Breakpoints.twoPane(840), "two panes at 840 dp");
        eq(320, Breakpoints.leftPane(840), "left pane minimum");
        eq(366, Breakpoints.leftPane(1100), "left pane is a third");
        eq(420, Breakpoints.leftPane(2000), "left pane maximum");
        check(Breakpoints.isShort(479), "479 dp is short");
        check(!Breakpoints.isShort(480), "480 dp is not short");
        check(Breakpoints.isShort(300), "a flip phone's half screen is short");
    }

    static void capabilities() {
        String[] previews = {"Docx", "Xlsx", "Pptx", "Odt", "Ods", "Odp", "Doc", "Xls", "Ppt", "Pdf", "Msg",
            "Rtf", "PlainText", "Csv", "Tsv", "Markdown", "Vsdx", "Vsdm", "VisioTemplateOrStencil", "Vsd",
            "AccessJet3", "AccessJet4", "AccessAce"};
        for (String f : previews) {
            check(Capabilities.canPreview(f), "preview of " + f);
        }
        String[] noPreview = {"Msi", "OleOther", "Exe", "", "docx", "Unknown"};
        for (String f : noPreview) {
            check(!Capabilities.canPreview(f), "no preview of " + f);
        }
        String[] sterile = {"Docx", "Xlsx", "Pptx", "Docm", "Potx", "Vsdx", "Odt", "Ods", "Odp", "Pdf",
            "Doc", "Xls", "Ppt", "Rtf"};
        for (String f : sterile) {
            check(Capabilities.canSanitize(f), "sterile copy of " + f);
        }
        String[] noSterile = {"Msg", "Csv", "Vsd", "Exe"};
        for (String f : noSterile) {
            check(!Capabilities.canSanitize(f), "no sterile copy of " + f);
        }
        try {
            Capabilities.previewFormats().add("Exe");
            check(false, "the set of previewable formats is mutable");
        } catch (UnsupportedOperationException e) {
            check(true, "the set of previewable formats is read-only");
        }
        try {
            Capabilities.sanitizeFormats().remove("Docx");
            check(false, "the set of sanitizable formats is mutable");
        } catch (UnsupportedOperationException e) {
            check(true, "the set of sanitizable formats is read-only");
        }
        check(Capabilities.previewFormats().containsAll(Arrays.asList(previews)), "the sets agree");
    }

    static void posture() {
        // Readings: closed, half open, flat; the thresholds have a hysteresis.
        eq(Posture.CLOSED, Posture.next(Posture.FLAT, 5f), "closed");
        eq(Posture.HALF_OPEN, Posture.next(Posture.FLAT, 100f), "half open");
        eq(Posture.FLAT, Posture.next(Posture.FLAT, 175f), "flat");
        eq(Posture.FLAT, Posture.next(Posture.UNKNOWN, 161f), "161 is flat when coming from unknown");
        eq(Posture.HALF_OPEN, Posture.next(Posture.HALF_OPEN, 165f), "stays half open until 170");
        eq(Posture.FLAT, Posture.next(Posture.HALF_OPEN, 170f), "flat from 170");
        eq(Posture.UNKNOWN, Posture.next(Posture.FLAT, Float.NaN), "no number");
        eq(Posture.UNKNOWN, Posture.next(Posture.FLAT, -1f), "negative");
        eq(Posture.UNKNOWN, Posture.next(Posture.FLAT, 400f), "beyond a full turn");

        // A nearly square inner display (Galaxy Z Fold, Pixel Fold) folds along its long side;
        // a tall narrow one (Z Flip) across it; rotation turns the fold with the window.
        check(Posture.foldIsVertical(1812, 2176, 0), "book, upright: the fold is vertical");
        check(!Posture.foldIsVertical(1812, 2176, 1), "book, turned: the fold is horizontal");
        check(!Posture.foldIsVertical(1080, 2640, 0), "flip, upright: the fold is horizontal");
        check(Posture.foldIsVertical(1080, 2640, 3), "flip, turned: the fold is vertical");
        check(!Posture.foldIsVertical(0, 0, 0), "an unknown display folds across");

        // Free area: only while half open and only if the side along the fold is long enough.
        check(Posture.freeArea(Posture.FLAT, 900, 700, true, false) == null, "flat: all of it");
        check(Posture.freeArea(Posture.UNKNOWN, 900, 700, true, false) == null, "unknown: all of it");
        check(Posture.freeArea(Posture.CLOSED, 900, 700, true, false) == null, "closed: all of it");
        int[] left = Posture.freeArea(Posture.HALF_OPEN, 904, 750, true, false);
        check(left != null && left[0] == 0 && left[1] == 0 && left[2] == 440 && left[3] == 750, "left half");
        int[] right = Posture.freeArea(Posture.HALF_OPEN, 904, 750, true, true);
        check(right != null && right[0] == 464 && right[2] == 440, "the right half when reading right to left");
        check(left != null && left[2] <= (904 - Posture.GAP_DP) / 2, "the half stays clear of the gap");
        int[] top = Posture.freeArea(Posture.HALF_OPEN, 412, 900, false, false);
        check(top != null && top[1] == 0 && top[2] == 412 && top[3] == 438, "upper half across a horizontal fold");
        check(Posture.freeArea(Posture.HALF_OPEN, 400, 700, true, false) == null, "too narrow to split");
        check(Posture.freeArea(Posture.HALF_OPEN, 900, 400, false, false) == null, "too short to split");
    }

    static void needsPassword() {
        Model.Report office = new Model.Report();
        office.format = "EncryptedOoxml";
        check(office.needsPassword(), "an encrypted Office package asks for a password");
        Model.Report pdf = new Model.Report();
        pdf.format = "Pdf";
        check(!pdf.needsPassword(), "a plain PDF does not");
        Model.Finding f = new Model.Finding();
        f.kind = "EncryptedDocument";
        pdf.findings.add(f);
        check(pdf.needsPassword(), "an encrypted PDF does");
        Model.Report doc = new Model.Report();
        doc.format = "Doc";
        doc.findings.add(f);
        check(!doc.needsPassword(), "a legacy binary file cannot be opened with a password");
        check(!new Model.Report().needsPassword(), "an empty report does not");
    }

    static void numericLooking() {
        String[] yes = {"1", "-3", "+4.5", "1,234.50", "12%", "(1 234)", "$5.00", "\u20ac12", "1E+21",
            "2024-03-05", "12:30:15", "3/5/24", "1.5E-3"};
        for (String t : yes) {
            check(Fmt.looksNumeric(t), t + " is written like a number");
        }
        String[] no = {"", "abc", "TRUE", "#DIV/0!", "5 \u044f\u043d\u0432\u0430\u0440\u044f", "x1", "1 apple"};
        for (String t : no) {
            check(!Fmt.looksNumeric(t), t + " is not");
        }
        check(!Fmt.looksNumeric(null), "null is not");
        eq(6, Model.CELL_CALCULATED, "the wire code of a calculated cell");
    }

    static void chunks() {
        eq(0, Fmt.chunks(null, 10).size(), "null text");
        eq(0, Fmt.chunks("", 10).size(), "empty text");
        eq(0, Fmt.chunks("abc", 1).size(), "a chunk size below 2 is refused");
        eq(Arrays.asList("a\nb\n", "c\n"), Fmt.chunks("a\nb\nc\n", 4), "split at line breaks");
        eq(Arrays.asList("abc"), Fmt.chunks("abc", 10), "short text is one chunk");
        eq(Arrays.asList("abcd", "efgh", "ij"), Fmt.chunks("abcdefghij", 4), "a long line is cut");
        // Nothing is ever lost, whatever the shape of the text.
        String[] samples = {
            "line one\nline two\n\n\nline five",
            "no newline at all, only a rather long run of characters to be cut into pieces",
            "\n\n\n",
            "emoji \uD83D\uDE00\uD83D\uDE00\uD83D\uDE00\uD83D\uDE00 and more \uD83D\uDE00 text",
            "\uD83D\uDE00",
            "日本語のテキスト\nと、改行を含む長い文章がここにあります。",
        };
        for (String text : samples) {
            for (int max = 2; max <= 40; max++) {
                List<String> parts = Fmt.chunks(text, max);
                StringBuilder joined = new StringBuilder();
                boolean small = true;
                boolean split = false;
                for (String part : parts) {
                    joined.append(part);
                    small &= part.length() <= max;
                    split |= part.length() > 0 && Character.isHighSurrogate(part.charAt(part.length() - 1));
                }
                eq(text, joined.toString(), "chunks of size " + max + " give the text back");
                check(small, "every chunk fits " + max + " for <" + text.replace("\n", "\\n") + ">");
                check(!split, "no surrogate pair is split at size " + max);
            }
        }
    }

    // ---------------------------------------------------------------------------------------

    private static List<Model.Item> items(String json) {
        return Model.parseItems(json.getBytes(StandardCharsets.UTF_8));
    }

    private static String para(String text) {
        return "{\"k\":\"p\",\"r\":[[\"" + text + "\",0]]}";
    }

    static void search() {
        // Folding keeps one character per character, so offsets map back to the original text.
        eq("hello", DocSearch.fold("HeLLo"), "ASCII case");
        eq("cafe", DocSearch.fold("Caf\u00e9"), "accents are dropped");
        eq("\u0435\u0436", DocSearch.fold("\u0401\u0436"), "Cyrillic yo finds ye");
        eq("\u0439", DocSearch.fold("\u0419"), "short i keeps its breve");
        eq("stra\u00dfe".length(), DocSearch.fold("Stra\u00dfe").length(), "length never changes");
        eq("\ud83d\ude00", DocSearch.fold("\ud83d\ude00"), "astral characters pass through");
        eq("", DocSearch.fold(null), "null folds to nothing");

        int[] at = DocSearch.occurrences("Abc abC aBc", DocSearch.fold("abc"));
        check(Arrays.equals(new int[] {0, 4, 8}, at), "three matches, any case: " + Arrays.toString(at));
        check(Arrays.equals(new int[] {0, 2}, DocSearch.occurrences("aaaa", "aa")), "matches do not overlap");
        eq(0, DocSearch.occurrences("abc", "").length, "an empty needle matches nothing");
        eq(0, DocSearch.occurrences("ab", "abc").length, "a needle longer than the text");
        eq(0, DocSearch.occurrences(null, "a").length, "null text");
        check(DocSearch.usable("x"), "one character is a query");
        check(!DocSearch.usable("   "), "blanks are not");
        check(!DocSearch.usable(null), "null is not");

        // What is searched is what is shown.
        List<Model.Item> sample = items("[" + para("Caf\u00e9 au lait") + ","
            + "{\"k\":\"p\",\"m\":\"1.\",\"r\":[[\"item\",0]]},"
            + "{\"k\":\"tr\",\"n\":2,\"first\":true,\"last\":true,\"cells\":[{\"c\":1,\"h\":false,\"p\":[{\"r\":[[\"left\",0]]}]},"
            + "{\"c\":1,\"h\":false,\"p\":[{\"r\":[[\"right\",0]]}]}]},"
            + "{\"k\":\"gr\",\"i\":0,\"h\":false,\"c\":[[0,0,\"alpha\"],[1,0,\"beta\"]]},"
            + "{\"k\":\"i\",\"alt\":\"logo\",\"media\":\"png\",\"bytes\":1,\"ext\":false},{\"k\":\"r\"}]");
        eq("Caf\u00e9 au lait", DocSearch.textOf(sample.get(0)), "paragraph text");
        eq("1. item", DocSearch.textOf(sample.get(1)), "the marker is part of the text");
        eq("left\tright", DocSearch.textOf(sample.get(2)), "table cells are tab separated");
        eq("alpha\tbeta", DocSearch.textOf(sample.get(3)), "grid cells are tab separated");
        eq("logo", DocSearch.textOf(sample.get(4)), "alt text of a picture");
        eq("", DocSearch.textOf(sample.get(5)), "a rule has no text");
        eq("\u2039U+202E\u203a", DocSearch.textOf(items("[" + para("\\u202e") + "]").get(0)),
            "invisible characters are searched as the marker the screen shows");

        // A scan over three sections, one page per task, interleaved with other work.
        final long[] sizes = {150, 0, 70, 10};
        final List<long[]> requests = new ArrayList<>();
        final Queue worker = new Queue();
        final Queue ui = new Queue();
        final int[] told = {0};
        final boolean[] failAt = {false};
        DocSearch.Loader loader = new DocSearch.Loader() {
            @Override
            public List<Model.Item> load(int section, long from, int count) throws Exception {
                requests.add(new long[] {section, from, count});
                if (failAt[0] && section == 2) {
                    throw new Exception("boom");
                }
                StringBuilder sb = new StringBuilder("[");
                for (int i = 0; i < count; i++) {
                    long item = from + i;
                    // Every 50th item says "needle" (twice in section 0's item 100).
                    String text = item % 50 == 0 ? "Some NEEDLE here" + (section == 0 && item == 100 ? " needle" : "") : "hay";
                    sb.append(i == 0 ? "" : ",").append(para(text));
                }
                return items(sb.append("]").toString());
            }
        };
        DocSearch.Listener listener = new DocSearch.Listener() {
            @Override
            public void onChanged() {
                told[0]++;
            }
        };
        DocSearch search = new DocSearch("  needle".trim(), sizes, loader, worker, ui, listener);
        eq("needle", search.needle(), "the query is folded");
        eq(230L, search.total(), "all items to look at");
        search.start();
        eq(1, worker.tasks.size(), "the first page is queued");
        int rounds = 0;
        while (!search.done() && rounds++ < 100) {
            worker.drain();
            ui.drain();
        }
        check(search.done(), "the scan finishes");
        check(!search.failed(), "without a failure");
        eq(230L, search.scanned(), "every item was looked at");
        // Pages: section 0 -> 64, 64, 22; section 2 -> 64, 6; section 3 -> 10 (the empty section is skipped).
        eq(6, requests.size(), "one request per page: " + requests.size());
        eq(0L, requests.get(0)[1], "starts at the beginning");
        eq(64L, requests.get(0)[2], "a full page");
        eq(2L, requests.get(3)[0], "the empty section 1 was skipped");
        // Matches: section 0 items 0, 50, 100 (x2), 150 is outside; section 2 items 0, 50; section 3 item 0.
        eq(7, search.hitCount(), "matches (the second one in the same item counts)");
        eq(0, search.hitSection(0), "first match section");
        eq(0L, search.hitItem(0), "first match item");
        eq(100L, search.hitItem(2), "item with two matches");
        eq(100L, search.hitItem(3), "item with two matches, second");
        eq(2, search.hitSection(4), "later section");
        eq(3, search.hitSection(6), "last section");
        eq(2, search.firstAtOrAfter(0, 51), "first match at or after item 51");
        eq(4, search.firstAtOrAfter(1, 0), "an empty section is passed over");
        eq(-1, search.firstAtOrAfter(3, 1), "none after the last");
        check(search.isHit(2, 0, 100), "isHit is true for its own place");
        check(!search.isHit(2, 0, 99), "and false elsewhere");
        check(!search.isHit(-1, 0, 0) && !search.isHit(99, 0, 0), "out of range is no hit");
        check(told[0] >= 6, "the UI was told after every page");

        // Cancelling stops the scan and the callbacks.
        requests.clear();
        told[0] = 0;
        DocSearch stopped = new DocSearch("needle", sizes, loader, worker, ui, listener);
        stopped.start();
        worker.drain();
        stopped.cancel();
        ui.drain();
        worker.drain();
        ui.drain();
        eq(1, requests.size(), "no page is requested after cancel");
        eq(0, told[0], "and the UI is not told");
        check(!stopped.done(), "a cancelled scan is not 'done'");

        // A page that cannot be read ends the scan but keeps what was found.
        failAt[0] = true;
        DocSearch broken = new DocSearch("needle", sizes, loader, worker, ui, listener);
        broken.start();
        rounds = 0;
        while (!broken.done() && rounds++ < 100) {
            worker.drain();
            ui.drain();
        }
        check(broken.done() && broken.failed(), "failure is reported");
        eq(4, broken.hitCount(), "matches before the failure are kept");

        // Nothing to look for or nothing to look in.
        DocSearch empty = new DocSearch("", sizes, loader, worker, ui, listener);
        empty.start();
        check(empty.done() && empty.hitCount() == 0, "empty query finishes at once");
        DocSearch nothing = new DocSearch("a", new long[] {0, 0}, loader, worker, ui, listener);
        nothing.start();
        check(nothing.done(), "no items finishes at once");
        eq(0, worker.tasks.size(), "and queues nothing");

        // The match list is capped.
        final long many = 6_000;
        DocSearch.Loader every = new DocSearch.Loader() {
            @Override
            public List<Model.Item> load(int section, long from, int count) {
                StringBuilder sb = new StringBuilder("[");
                for (int i = 0; i < count; i++) {
                    sb.append(i == 0 ? "" : ",").append(para("needle"));
                }
                return items(sb.append("]").toString());
            }
        };
        DocSearch capped = new DocSearch("needle", new long[] {many}, every, worker, ui, listener);
        capped.start();
        rounds = 0;
        while (!capped.done() && rounds++ < 200) {
            worker.drain();
            ui.drain();
        }
        check(capped.done() && capped.capped(), "the cap stops the scan");
        eq(DocSearch.MAX_HITS, capped.hitCount(), "and keeps exactly the cap");
        check(capped.scanned() < many, "without reading the whole document");
    }

    // ---------------------------------------------------------------------------------------

    static void outlineBookmarksZoom() {
        // Heading levels.
        eq(0, DocOutline.levelOf("Title"), "title is level 0");
        eq(1, DocOutline.levelOf("Heading1"), "heading 1");
        eq(6, DocOutline.levelOf("Heading6"), "heading 6");
        eq(-1, DocOutline.levelOf("Heading7"), "no heading 7");
        eq(-1, DocOutline.levelOf("Heading"), "bare Heading");
        eq(-1, DocOutline.levelOf("Subtitle"), "subtitle is not a heading");
        eq(-1, DocOutline.levelOf("Body"), "body");
        eq(-1, DocOutline.levelOf(null), "null");

        // A document of two sections whose headings sit among other items.
        final long[] sizes = {70, 3};
        final Queue worker = new Queue();
        final Queue ui = new Queue();
        final int[] told = {0};
        DocOutline outline = new DocOutline(sizes, new DocOutline.Loader() {
            @Override
            public List<Model.Item> load(int section, long from, int count) {
                StringBuilder sb = new StringBuilder("[");
                for (int i = 0; i < count; i++) {
                    long item = from + i;
                    String style = "Body";
                    String text = "text";
                    if (section == 0 && item == 0) { style = "Title"; text = "The title"; }
                    if (section == 0 && item == 10) { style = "Heading1"; text = "  One\\nline  "; }
                    if (section == 0 && item == 66) { style = "Heading2"; text = "Deep"; }
                    if (section == 0 && item == 67) { style = "Heading3"; text = "   "; }
                    if (section == 0 && item == 68) { style = "Subtitle"; text = "not a heading"; }
                    if (section == 1 && item == 2) { style = "Heading1"; text = "Second"; }
                    sb.append(i == 0 ? "" : ",").append("{\"k\":\"p\",\"s\":\"").append(style)
                        .append("\",\"r\":[[\"").append(text).append("\",0]]}");
                }
                return Model.parseItems(sb.append("]").toString().getBytes(StandardCharsets.UTF_8));
            }
        }, worker, ui, new DocOutline.Listener() {
            @Override
            public void onChanged() {
                told[0]++;
            }
        });
        outline.start();
        int rounds = 0;
        while (!outline.done() && rounds++ < 50) {
            worker.drain();
            ui.drain();
        }
        check(outline.done() && !outline.failed(), "the outline scan finishes");
        List<DocOutline.Entry> e = outline.entries();
        eq(4, e.size(), "title, two headings and the second section's one (blank and subtitle skipped)");
        eq("The title", e.get(0).text, "title text");
        eq(0, e.get(0).level, "title level");
        eq(10L, e.get(1).item, "heading place");
        eq("One line", e.get(1).text, "line breaks become spaces, ends are trimmed");
        eq(1, e.get(1).level, "heading level");
        eq("Deep", e.get(2).text, "a heading in the second page");
        eq(2, e.get(2).level, "heading 2");
        eq(1, e.get(3).section, "heading in the second section");
        eq(2L, e.get(3).item, "its item");
        check(!outline.capped(), "not capped");
        check(told[0] >= 2, "told after pages");

        // Bookmarks: kept in document order, one per place, limited.
        Bookmarks b = new Bookmarks();
        check(b.add(1, 5, "later"), "add");
        check(b.add(0, 9, "first\nline"), "add earlier place");
        check(b.add(0, 9, "renewed"), "marking the same place again renews it");
        eq(2, b.size(), "no duplicates");
        eq("renewed", b.list().get(0).label, "sorted by place; label renewed");
        eq(1, b.list().get(1).section, "second bookmark");
        check(b.has(1, 5) && !b.has(1, 6), "has");
        b.remove(0);
        eq(1, b.size(), "removed");
        b.remove(5);
        b.remove(-1);
        eq(1, b.size(), "out-of-range removal is ignored");
        Bookmarks clipped = new Bookmarks();
        clipped.add(0, 0, new String(new char[500]).replace('\0', 'x'));
        eq(Bookmarks.MAX_LABEL, clipped.list().get(0).label.length(), "labels are clipped");
        Bookmarks full = new Bookmarks();
        for (int i = 0; i < Bookmarks.MAX; i++) {
            check(full.add(0, i, "m" + i), "room for " + i);
        }
        check(!full.add(0, 999, "one too many"), "the list is full");
        check(full.add(0, 3, "renew when full"), "a known place can still be renewed when full");
        full.clear();
        eq(0, full.size(), "cleared");

        // Zoom.
        eq(1.0f, Zoom.clamp(1.0f), "100%");
        eq(0.6f, Zoom.clamp(0.1f), "lower bound");
        eq(3.0f, Zoom.clamp(99f), "upper bound");
        eq(1.0f, Zoom.clamp(Float.NaN), "NaN is 100%");
        eq(1.0f, Zoom.clamp(Float.POSITIVE_INFINITY), "infinity is 100%");
        eq(1.15f, Zoom.clamp(1.1337f), "rounded to 5%");
        eq(1.1f, Zoom.step(1.0f, 1), "zoom in");
        eq(0.9f, Zoom.step(1.0f, -1), "zoom out");
        eq(3.0f, Zoom.step(3.0f, 1), "no zoom beyond the top");
        eq(0.6f, Zoom.step(0.6f, -1), "no zoom below the bottom");
        eq(1.5f, Zoom.pinch(1.0f, 1.5f), "pinch out");
        eq(0.6f, Zoom.pinch(1.0f, 0.01f), "pinch in, clamped");
        eq(1.0f, Zoom.pinch(1.0f, Float.NaN), "NaN factor is ignored");
        eq(1.0f, Zoom.pinch(1.0f, -3f), "negative factor is ignored");
        eq(120, Zoom.percent(1.2f), "percent");
        eq(60, Zoom.percent(0f), "percent is clamped");
        float z = 1.0f;
        for (int i = 0; i < 100; i++) {
            z = Zoom.step(z, 1);
        }
        eq(3.0f, z, "100 presses stop at the top");
    }

    static void pageDrawing() {
        String json = "{\"w\":595.5,\"h\":842,\"index\":1,\"pages\":3,\"truncated\":false,\"approx\":true,\"p\":["
            + "[\"P\",4294901760,0,1.5,0,[0,10,10,1,50,10,2,1,2,3,4,5,6,3]],"
            + "[\"T\",72,100,12,0.5,40.5,9,4278190080,\"Привет \\\"мир\\\"\"],"
            + "[\"I\",100,0,0,50,10,20,\"JPEG\"],[\"S\"],[\"C\",0,0,100,100],[\"R\"],[\"?\",1],[]]}";
        PageDrawing d = PageDrawing.parse(json.getBytes(java.nio.charset.StandardCharsets.UTF_8));
        eq(595.5f, d.width, "page width");
        eq(842f, d.height, "page height");
        eq(1, d.index, "page index");
        eq(3, d.pages, "page count");
        check(d.approximate && !d.truncated, "page flags");
        eq(6, d.prims.size(), "unknown and empty primitives are skipped");
        PageDrawing.Prim path = d.prims.get(0);
        eq(PageDrawing.PATH, path.kind, "a path");
        eq(0xFFFF0000, path.fill, "the fill is ARGB");
        eq(14, path.ops.length, "path operands");
        PageDrawing.Prim text = d.prims.get(1);
        eq(PageDrawing.TEXT, text.kind, "a text run");
        eq("Привет \"мир\"", text.text, "the text survives JSON escapes");
        eq(PageDrawing.SERIF | PageDrawing.BOLD, text.style, "style bits");
        eq(1, d.textRuns(), "text runs");
        eq("JPEG", d.prims.get(2).label, "the placeholder label");
        eq(PageDrawing.CLIP, d.prims.get(4).kind, "a clip");
        for (String bad : new String[] {"{}", "{\"w\":0,\"h\":10,\"p\":[]}", "{\"w\":1e9,\"h\":10,\"p\":[]}",
            "[1]"}) {
            try {
                PageDrawing.parse(bad.getBytes(java.nio.charset.StandardCharsets.UTF_8));
                check(false, "rejects " + bad);
            } catch (RuntimeException e) {
                check(true, "rejects " + bad);
            }
        }
        check(Capabilities.canRender("Pdf") && !Capabilities.canRender("Docx") && !Capabilities.canRender(""),
            "only PDF has a page view");
    }

    // ---------------------------------------------------------------------------------------

    public static void main(String[] args) {
        String[] names = {"json", "fmt", "names", "report", "preview", "other", "pageCache", "breakpoints",
            "capabilities", "chunks", "search", "outlineBookmarksZoom", "numericLooking", "needsPassword", "posture", "pageDrawing"};
        for (String name : names) {
            current = name;
            int before = failures;
            try {
                switch (name) {
                    case "json": json(); break;
                    case "fmt": fmt(); break;
                    case "names": names(); break;
                    case "report": report(); break;
                    case "preview": preview(); break;
                    case "other": other(); break;
                    case "breakpoints": breakpoints(); break;
                    case "capabilities": capabilities(); break;
                    case "chunks": chunks(); break;
                    case "search": search(); break;
                    case "outlineBookmarksZoom": outlineBookmarksZoom(); break;
                    case "numericLooking": numericLooking(); break;
                    case "needsPassword": needsPassword(); break;
                    case "posture": posture(); break;
                    case "pageDrawing": pageDrawing(); break;
                    default: pageCache(); break;
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
