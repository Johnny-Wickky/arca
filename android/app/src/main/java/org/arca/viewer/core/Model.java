// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Map;

/**
 * Typed views of the engine's JSON (the contract is documented in
 * {@code crates/arca-engine/src/render.rs}). Every parser is tolerant: a missing or mistyped
 * field becomes an empty value instead of an exception, because a viewer that refuses to show
 * a result over a cosmetic field is worse than one that shows it slightly degraded. Malformed
 * JSON as a whole still throws {@link Json.JsonException}.
 *
 * <p>No Android dependencies; tested on a desktop JVM.
 */
public final class Model {
    private Model() {}

    // ---- run-style bits (arca_preview::model::style) ----------------------------------------
    public static final int BOLD = 1;
    public static final int ITALIC = 2;
    public static final int UNDERLINE = 4;
    public static final int STRIKE = 8;
    public static final int MONO = 16;
    public static final int SUPER = 32;
    public static final int SUB = 64;
    public static final int HIDDEN = 128;
    public static final int DELETED = 256;
    public static final int INSERTED = 512;
    /** Text the viewer composed from the document's data (a rebuilt SQL query, a diagram link): drawn apart from the document's own text. */
    public static final int SYNTHETIC = 1024;
    /** All bits the engine defines; others (from a newer engine) are dropped. */
    public static final int STYLE_MASK = (1 << 11) - 1;

    /** Whether the engine's format name is a picture the app decodes in the sandbox. */
    public static boolean isPictureFormat(String format) {
        return "Png".equals(format) || "Jpeg".equals(format) || "Gif".equals(format) || "Bmp".equals(format);
    }

    // ---- cell kinds -------------------------------------------------------------------------
    public static final int CELL_TEXT = 0;
    public static final int CELL_NUMBER = 1;
    public static final int CELL_DATE = 2;
    public static final int CELL_BOOL = 3;
    public static final int CELL_ERROR = 4;
    public static final int CELL_FORMULA = 5;
    /** The result of a formula that had no stored result, calculated by the viewer. */
    public static final int CELL_CALCULATED = 6;

    // ---- verdicts ---------------------------------------------------------------------------
    /** What the X-ray concluded (names are those of the engine's {@code Verdict}). */
    public enum Verdict {
        CLEAN("Clean"),
        HIDDEN_TRACKERS("HiddenTrackers"),
        ACTIVE_NEUTRALIZED("ActiveContentNeutralized"),
        REJECTED("Rejected"),
        UNVERIFIED("Unverified");

        final String wire;

        Verdict(String wire) {
            this.wire = wire;
        }

        /** The verdict named {@code wire}; anything unknown is treated as {@link #UNVERIFIED}. */
        public static Verdict of(String wire) {
            for (Verdict v : values()) {
                if (v.wire.equals(wire)) {
                    return v;
                }
            }
            return UNVERIFIED; // never "clean" by accident
        }
    }

    /** Severity ranks (0 = Info … 4 = Critical). */
    public static int severityRank(String sev) {
        switch (sev) {
            case "Critical": return 4;
            case "High": return 3;
            case "Medium": return 2;
            case "Low": return 1;
            default: return 0;
        }
    }

    // ---- X-ray report -----------------------------------------------------------------------

    /** One finding. */
    public static final class Finding {
        public String kind = "";
        public String category = "";
        public String severity = "Info";
        public String location = "";
        public String detail = "";
        public boolean blocked;

        public int rank() {
            return severityRank(severity);
        }
    }

    /** A macro module (source is shown, never run). */
    public static final class MacroModule {
        public String language = "";
        public String location = "";
        public String name = "";
        public String source = "";
        public boolean truncated;
        public List<String> autoExec = new ArrayList<>();
        public List<String> suspicious = new ArrayList<>();
        public List<String> iocs = new ArrayList<>();
        /** What the code does, judged from its text: {@code Benign}, {@code Suspicious} or {@code Malicious}. */
        public String intent = "Suspicious";
        /** Names of its procedures, as written. */
        public List<String> procs = new ArrayList<>();
        /** What it does, the most serious first. */
        public List<Behavior> behaviors = new ArrayList<>();

        /** The worst intent first: Malicious, Suspicious, Benign (an unknown one counts as Suspicious). */
        public int intentRank() {
            return Model.intentRank(intent);
        }
    }

    /** One thing a macro does, with the procedure it is in and the line that shows it. */
    public static final class Behavior {
        /** The engine's {@code BehaviorKind} name, e.g. {@code RunsProgram}. */
        public String kind = "";
        /** The procedure ({@code ""}: outside any procedure). */
        public String proc = "";
        /** The line of source that shows it, or a short description. */
        public String detail = "";
    }

    /** 2 for Malicious, 1 for Suspicious (and anything the app does not know), 0 for Benign. */
    public static int intentRank(String intent) {
        if ("Malicious".equals(intent)) {
            return 2;
        }
        return "Benign".equals(intent) ? 0 : 1;
    }

    /** A blocked reference to something outside the file. */
    public static final class Ref {
        public String target = "";
        public String kind = "";
        public String cls = "";
        public String location = "";
        public boolean auto;
    }

    /** One metadata field. */
    public static final class Meta {
        public String key = "";
        public String name = "";
        public String value = "";
        public String source = "";
    }

    /** An embedded object. */
    public static final class Embedded {
        public String location = "";
        public String kind = "";
        public String className = "";
        public String clsid = "";
        public String file = "";
        public long size;
        public String sha256 = "";
    }

    /** The X-ray of one document. */
    public static final class Report {
        public String format = "Unknown";
        public String family = "None";
        public Verdict verdict = Verdict.UNVERIFIED;
        public String rejection = "";
        public String sha256 = "";
        public long trackers;
        public long active;
        public long findingCount;
        public List<Finding> findings = new ArrayList<>();
        public List<MacroModule> macros = new ArrayList<>();
        public List<Ref> refs = new ArrayList<>();
        public List<Meta> meta = new ArrayList<>();
        public List<Embedded> embedded = new ArrayList<>();
        public long parts;
        public long xmlEvents;
        public long inflated;
        public long nesting;

        /**
         * Whether a password the reader knows would open the document: an encrypted Office package
         * or a PDF with a password (the formats the engine can decrypt).
         */
        public boolean needsPassword() {
            if ("EncryptedOoxml".equals(format)) {
                return true;
            }
            if (!"Pdf".equals(format)) {
                return false;
            }
            for (Finding f : findings) {
                if ("EncryptedDocument".equals(f.kind)) {
                    return true;
                }
            }
            return false;
        }

        /** Number of findings of {@code category}. */
        public int count(String category) {
            int n = 0;
            for (Finding f : findings) {
                if (f.category.equals(category)) {
                    n++;
                }
            }
            return n;
        }

        /** The highest severity rank among the findings (0 if none). */
        public int topRank() {
            int top = 0;
            for (Finding f : findings) {
                top = Math.max(top, f.rank());
            }
            return top;
        }
    }

    /** Parses the engine's X-ray report. */
    public static Report parseReport(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        Report r = new Report();
        r.format = nonEmpty(Json.str(root, "format"), "Unknown");
        r.family = nonEmpty(Json.str(root, "family"), "None");
        r.verdict = Verdict.of(Json.str(root, "verdict"));
        r.rejection = Json.str(root, "rejection");
        r.sha256 = Json.str(root, "sha256");
        Map<String, Object> c = Json.obj(root.get("counters"));
        r.trackers = Json.num(c, "trackers");
        r.active = Json.num(c, "active");
        r.findingCount = Json.num(c, "findings");
        for (Object o : Json.arr(root.get("findings"))) {
            Map<String, Object> m = Json.obj(o);
            Finding f = new Finding();
            f.kind = Json.str(m, "kind");
            f.category = Json.str(m, "cat");
            f.severity = nonEmpty(Json.str(m, "sev"), "Info");
            f.location = Json.str(m, "loc");
            f.detail = Json.str(m, "detail");
            f.blocked = Json.bool(m, "blocked");
            r.findings.add(f);
        }
        for (Object o : Json.arr(root.get("macros"))) {
            Map<String, Object> m = Json.obj(o);
            MacroModule x = new MacroModule();
            x.language = Json.str(m, "lang");
            x.location = Json.str(m, "loc");
            x.name = Json.str(m, "name");
            x.source = Json.str(m, "source");
            x.truncated = Json.bool(m, "truncated");
            x.autoExec = Json.strings(m, "autoExec");
            x.suspicious = Json.strings(m, "suspicious");
            x.iocs = Json.strings(m, "iocs");
            x.intent = nonEmpty(Json.str(m, "intent"), "Suspicious");
            x.procs = Json.strings(m, "procs");
            for (Object bo : Json.arr(m.get("behaviors"))) {
                Map<String, Object> bm = Json.obj(bo);
                Behavior b = new Behavior();
                b.kind = Json.str(bm, "kind");
                b.proc = Json.str(bm, "proc");
                b.detail = Json.str(bm, "detail");
                x.behaviors.add(b);
            }
            r.macros.add(x);
        }
        for (Object o : Json.arr(root.get("refs"))) {
            Map<String, Object> m = Json.obj(o);
            Ref x = new Ref();
            x.target = Json.str(m, "target");
            x.kind = Json.str(m, "kind");
            x.cls = Json.str(m, "class");
            x.location = Json.str(m, "loc");
            x.auto = Json.bool(m, "auto");
            r.refs.add(x);
        }
        for (Object o : Json.arr(root.get("meta"))) {
            Map<String, Object> m = Json.obj(o);
            Meta x = new Meta();
            x.key = Json.str(m, "key");
            x.name = Json.str(m, "name");
            x.value = Json.str(m, "value");
            x.source = Json.str(m, "src");
            r.meta.add(x);
        }
        for (Object o : Json.arr(root.get("embedded"))) {
            Map<String, Object> m = Json.obj(o);
            Embedded x = new Embedded();
            x.location = Json.str(m, "loc");
            x.kind = Json.str(m, "kind");
            x.className = Json.str(m, "class");
            x.clsid = Json.str(m, "clsid");
            x.file = Json.str(m, "file");
            x.size = Json.num(m, "size");
            x.sha256 = Json.str(m, "sha256");
            r.embedded.add(x);
        }
        Map<String, Object> st = Json.obj(root.get("stats"));
        r.parts = Json.num(st, "parts");
        r.xmlEvents = Json.num(st, "xmlEvents");
        r.inflated = Json.num(st, "inflated");
        r.nesting = Json.num(st, "nesting");
        return r;
    }

    // ---- preview ----------------------------------------------------------------------------

    /** A sheet, slide, page or the body. */
    public static final class SectionInfo {
        public String kind = "Body";
        public String name = "";
        public boolean hidden;
        public long items;
    }

    /** A remark about what the preview does not show. */
    public static final class Notice {
        public String kind = "";
        public long count;
        public String detail = "";
    }

    /** Sections and notices of a preview. */
    public static final class PreviewMeta {
        public String format = "Unknown";
        public String family = "None";
        public List<SectionInfo> sections = new ArrayList<>();
        public List<Notice> notices = new ArrayList<>();
        public long chars;
        public long items;
        public boolean truncated;
    }

    /** Parses the preview metadata. */
    public static PreviewMeta parsePreviewMeta(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        PreviewMeta p = new PreviewMeta();
        p.format = nonEmpty(Json.str(root, "format"), "Unknown");
        p.family = nonEmpty(Json.str(root, "family"), "None");
        for (Object o : Json.arr(root.get("sections"))) {
            Map<String, Object> m = Json.obj(o);
            SectionInfo s = new SectionInfo();
            s.kind = nonEmpty(Json.str(m, "kind"), "Body");
            s.name = Json.str(m, "name");
            s.hidden = Json.bool(m, "hidden");
            s.items = Json.num(m, "items");
            p.sections.add(s);
        }
        for (Object o : Json.arr(root.get("notices"))) {
            Map<String, Object> m = Json.obj(o);
            Notice n = new Notice();
            n.kind = Json.str(m, "kind");
            n.count = Json.num(m, "count");
            n.detail = Json.str(m, "detail");
            p.notices.add(n);
        }
        Map<String, Object> st = Json.obj(root.get("stats"));
        p.chars = Json.num(st, "chars");
        p.items = Json.num(st, "items");
        p.truncated = Json.bool(st, "truncated");
        return p;
    }

    /** One run of text. {@code href} is display-only; the app never follows it. */
    public static final class Run {
        public final String text;
        public final int style;
        public final String href;
        /** Text colour as 0xFFRRGGBB, 0 for the viewer's own. */
        public final int fg;
        /** Highlight as 0xFFRRGGBB, 0 for none. */
        public final int bg;
        /** Size relative to the paragraph's text in percent, 0 for the same. */
        public final int scale;

        Run(String text, int style, String href) {
            this(text, style, href, 0, 0, 0);
        }

        Run(String text, int style, String href, int fg, int bg, int scale) {
            this.text = text;
            this.style = style;
            this.href = href;
            this.fg = fg;
            this.bg = bg;
            this.scale = scale;
        }
    }

    /** A paragraph (also used inside table cells). */
    public static final class Para {
        public String style = "Body";
        /** 0 start, 1 centre, 2 end, 3 justify. */
        public int align;
        public int level;
        public String marker = "";
        /** Left indent in points, and the extra indent of the first line (negative: hanging). */
        public int indentLeft;
        public int indentFirst;
        /** Line spacing in percent of single, 0 for single. */
        public int lineSpacing;
        /** Colour of the rule down the left edge (or of the frame), 0xFFRRGGBB, 0 for none. */
        public int border;
        public boolean boxed;
        public List<Run> runs = new ArrayList<>();

        /** Whether any run was composed by the viewer ({@link Model#SYNTHETIC}). */
        public boolean hasSynthetic() {
            for (Run r : runs) {
                if ((r.style & SYNTHETIC) != 0) {
                    return true;
                }
            }
            return false;
        }

        /** The paragraph's plain text. */
        public String plain() {
            if (runs.size() == 1) {
                return runs.get(0).text;
            }
            StringBuilder sb = new StringBuilder();
            for (Run r : runs) {
                sb.append(r.text);
            }
            return sb.toString();
        }
    }

    /** A table cell. */
    public static final class Cell {
        public int span = 1;
        public boolean header;
        public List<Para> paras = new ArrayList<>();
    }

    /** One displayable item of a section. */
    public abstract static class Item {
        /** Item kind: {@code p}, {@code tr}, {@code gh}, {@code gr}, {@code i}, {@code r}. */
        public abstract String kind();
    }

    /** A paragraph item. */
    public static final class ParaItem extends Item {
        public final Para para;

        ParaItem(Para para) {
            this.para = para;
        }

        @Override
        public String kind() {
            return "p";
        }
    }

    /** One row of a table. */
    public static final class TableRow extends Item {
        public int columns = 1;
        public boolean first;
        public boolean last;
        public List<Cell> cells = new ArrayList<>();

        @Override
        public String kind() {
            return "tr";
        }
    }

    /** The header of a spreadsheet grid: column widths in characters. */
    public static final class GridHeader extends Item {
        public int cols;
        public int[] widths = new int[0];
        public long rows;
        public boolean truncated;

        @Override
        public String kind() {
            return "gh";
        }
    }

    /** One cell of a grid row. */
    public static final class GridCell {
        public final int col;
        public final int kind;
        public final String text;
        /** Where the cell links to (shown to the reader, never followed); empty for none. */
        public final String href;

        GridCell(int col, int kind, String text, String href) {
            this.col = col;
            this.kind = kind;
            this.text = text;
            this.href = href;
        }
    }

    /** One row of a spreadsheet grid. */
    public static final class GridRow extends Item {
        public long index;
        public boolean hidden;
        public List<GridCell> cells = new ArrayList<>();

        @Override
        public String kind() {
            return "gr";
        }
    }

    /** A picture placeholder (pictures are never decoded). */
    public static final class ImageItem extends Item {
        public String alt = "";
        public String media = "";
        public long bytes;
        public boolean external;
        /** What to ask the engine for to see the picture, or -1. */
        public int id = -1;
        /** Size in the document in points, 0 where unknown. */
        public int width;
        public int height;

        @Override
        public String kind() {
            return "i";
        }
    }

    /** A horizontal rule. */
    public static final class RuleItem extends Item {
        @Override
        public String kind() {
            return "r";
        }
    }

    /** A placeholder for an item that has not been loaded yet. */
    public static final class Pending extends Item {
        @Override
        public String kind() {
            return "?";
        }
    }

    private static Para para(Map<String, Object> m) {
        Para p = new Para();
        p.style = nonEmpty(Json.str(m, "s"), "Body");
        p.align = Json.integer(m, "a");
        p.level = Json.integer(m, "l");
        p.marker = Json.str(m, "m");
        p.indentLeft = (int) Math.max(0L, Math.min(2000L, Json.num(m, "il")));
        p.indentFirst = (int) Math.max(-2000L, Math.min(2000L, Json.num(m, "if")));
        p.lineSpacing = (int) Math.max(0L, Math.min(500L, Json.num(m, "ls")));
        p.border = (int) (Json.num(m, "bd") & 0xFFFFFFFFL);
        p.boxed = p.border != 0 && Json.num(m, "bx") != 0;
        for (Object o : Json.arr(m.get("r"))) {
            List<Object> run = Json.arr(o);
            if (run.isEmpty()) {
                continue;
            }
            String text = Json.str(run.get(0), "");
            int style = run.size() > 1 ? (int) (Json.num(run.get(1), 0L) & STYLE_MASK) : 0;
            String href = run.size() > 2 ? Json.str(run.get(2), "") : "";
            int fg = run.size() > 3 ? (int) (Json.num(run.get(3), 0L) & 0xFFFFFFFFL) : 0;
            int bg = run.size() > 4 ? (int) (Json.num(run.get(4), 0L) & 0xFFFFFFFFL) : 0;
            int scale = run.size() > 5 ? (int) Math.max(0L, Math.min(400L, Json.num(run.get(5), 0L))) : 0;
            p.runs.add(new Run(text, style, href, fg, bg, scale));
        }
        return p;
    }

    /** Parses a page of items. */
    public static List<Item> parseItems(byte[] json) {
        List<Item> out = new ArrayList<>();
        for (Object o : Json.arr(Json.parse(json))) {
            Map<String, Object> m = Json.obj(o);
            switch (Json.str(m, "k")) {
                case "p":
                    out.add(new ParaItem(para(m)));
                    break;
                case "tr": {
                    TableRow t = new TableRow();
                    t.columns = Math.max(1, Json.integer(m, "n"));
                    t.first = Json.bool(m, "first");
                    t.last = Json.bool(m, "last");
                    for (Object co : Json.arr(m.get("cells"))) {
                        Map<String, Object> cm = Json.obj(co);
                        Cell c = new Cell();
                        c.span = Math.max(1, Json.integer(cm, "c"));
                        c.header = Json.bool(cm, "h");
                        for (Object po : Json.arr(cm.get("p"))) {
                            c.paras.add(para(Json.obj(po)));
                        }
                        t.cells.add(c);
                    }
                    out.add(t);
                    break;
                }
                case "gh": {
                    GridHeader g = new GridHeader();
                    g.cols = Json.integer(m, "cols");
                    List<Object> w = Json.arr(m.get("w"));
                    g.widths = new int[w.size()];
                    for (int i = 0; i < g.widths.length; i++) {
                        g.widths[i] = (int) Json.num(w.get(i), 4L);
                    }
                    g.rows = Json.num(m, "rows");
                    g.truncated = Json.bool(m, "trunc");
                    out.add(g);
                    break;
                }
                case "gr": {
                    GridRow g = new GridRow();
                    g.index = Json.num(m, "i");
                    g.hidden = Json.bool(m, "h");
                    for (Object co : Json.arr(m.get("c"))) {
                        List<Object> c = Json.arr(co);
                        if (c.size() >= 3) {
                            g.cells.add(new GridCell((int) Json.num(c.get(0), 0L), (int) Json.num(c.get(1), 0L),
                                Json.str(c.get(2), ""), c.size() >= 4 ? Json.str(c.get(3), "") : ""));
                        }
                    }
                    out.add(g);
                    break;
                }
                case "i": {
                    ImageItem i = new ImageItem();
                    i.alt = Json.str(m, "alt");
                    i.media = Json.str(m, "media");
                    i.bytes = Json.num(m, "bytes");
                    i.external = Json.bool(m, "ext");
                    i.id = m.containsKey("id") ? Json.integer(m, "id") : -1;
                    i.width = (int) Math.max(0L, Math.min(5000L, Json.num(m, "w")));
                    i.height = (int) Math.max(0L, Math.min(5000L, Json.num(m, "h")));
                    out.add(i);
                    break;
                }
                case "r":
                    out.add(new RuleItem());
                    break;
                default:
                    break; // an unknown kind from a newer engine is skipped, not fatal
            }
        }
        return out;
    }

    // ---- sanitize ---------------------------------------------------------------------------

    /** One change made while sterilising. */
    public static final class SanitizeAction {
        public String kind = "";
        public String target = "";
    }

    /** What the sterile copy is and how it was verified. */
    public static final class SanitizedInfo {
        public String format = "Unknown";
        public long size;
        public List<SanitizeAction> actions = new ArrayList<>();
        public Verdict verdict = Verdict.UNVERIFIED;
        public String sha256 = "";
        public long findings;
    }

    /** Parses the description of a sterile copy. */
    public static SanitizedInfo parseSanitized(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        SanitizedInfo s = new SanitizedInfo();
        s.format = nonEmpty(Json.str(root, "format"), "Unknown");
        s.size = Json.num(root, "size");
        for (Object o : Json.arr(root.get("actions"))) {
            Map<String, Object> m = Json.obj(o);
            SanitizeAction a = new SanitizeAction();
            a.kind = Json.str(m, "kind");
            a.target = Json.str(m, "target");
            s.actions.add(a);
        }
        Map<String, Object> v = Json.obj(root.get("verification"));
        s.verdict = Verdict.of(Json.str(v, "verdict"));
        s.sha256 = Json.str(v, "sha256");
        s.findings = Json.num(v, "findings");
        return s;
    }

    // ---- diagnostics of a run ---------------------------------------------------------------

    /** Diagnostics of a finished run. */
    public static final class RunInfo {
        public int status;
        public String statusName = "";
        public String message = "";
        public long fuel;
        public long fuelLimit;
        public long peakMemory;
        public long slices;
        public long wallMs;
        /** 0: the document was not encrypted; 1: the password opened it; 2: it needed no password. */
        public int unlock;
    }

    /** Parses {@code outcomeInfo}. */
    public static RunInfo parseRunInfo(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        RunInfo r = new RunInfo();
        r.status = Json.integer(root, "status");
        r.statusName = Json.str(root, "statusName");
        r.message = Json.str(root, "message");
        r.unlock = Json.integer(root, "unlock");
        Map<String, Object> st = Json.obj(root.get("stats"));
        r.fuel = Json.num(st, "fuel");
        r.fuelLimit = Json.num(st, "fuelLimit");
        r.peakMemory = Json.num(st, "peakMemory");
        r.slices = Json.num(st, "slices");
        r.wallMs = Json.num(st, "wallMs");
        return r;
    }

    // ---- about, passport --------------------------------------------------------------------

    /** A person or organisation on a creator card. */
    public static final class Person {
        public String name = "";
        /** Empty if the person has no page. */
        public String url = "";
    }

    /** A link label with its URL. */
    public static final class Link {
        public String label = "";
        public String url = "";
    }

    /** A creator card of the "About & Support" screen. */
    public static final class Card {
        public boolean upstream;
        public String title = "";
        public String project = "";
        public List<Person> people = new ArrayList<>();
        public String supportLabel = "";
        public List<Link> support = new ArrayList<>();
        public String homepage = "";
        public String source = "";
        public String modifications = "";
    }

    /** The patronage screen: creator cards, legal notice and the modified-version notice. */
    public static final class Screen {
        public String lang = "en";
        public List<Card> cards = new ArrayList<>();
        public String legal = "";
        public String modified = "";
    }

    /** The "About & Support" content. */
    public static final class About {
        public String appName = "";
        public String fingerprint = "";
        public Screen screen = new Screen();
    }

    private static Screen screen(Map<String, Object> m) {
        Screen s = new Screen();
        s.lang = nonEmpty(Json.str(m, "lang"), "en");
        s.legal = Json.str(m, "legal");
        s.modified = Json.str(m, "modified");
        for (Object o : Json.arr(m.get("cards"))) {
            Map<String, Object> cm = Json.obj(o);
            Card c = new Card();
            c.upstream = "Upstream".equals(Json.str(cm, "role"));
            c.title = Json.str(cm, "title");
            c.project = Json.str(cm, "project");
            for (Object po : Json.arr(cm.get("people"))) {
                Map<String, Object> pm = Json.obj(po);
                Person p = new Person();
                p.name = Json.str(pm, "name");
                p.url = Json.str(pm, "url");
                c.people.add(p);
            }
            c.supportLabel = Json.str(cm, "supportLabel");
            for (Object lo : Json.arr(cm.get("support"))) {
                Map<String, Object> lm = Json.obj(lo);
                Link l = new Link();
                l.label = Json.str(lm, "label");
                l.url = Json.str(lm, "url");
                c.support.add(l);
            }
            c.homepage = Json.str(cm, "homepage");
            c.source = Json.str(cm, "source");
            c.modifications = Json.str(cm, "modifications");
            s.cards.add(c);
        }
        return s;
    }

    /** Parses the "About & Support" JSON. */
    public static About parseAbout(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        About a = new About();
        a.appName = Json.str(root, "appName");
        a.fingerprint = Json.str(root, "fingerprint");
        a.screen = screen(Json.obj(root.get("screen")));
        return a;
    }

    /** The Patron Passport. */
    public static final class Passport {
        public String title = "";
        public String body = "";
        public String cta = "";
        public long documents;
        public long trackers;
        public long active;
        public long days;
        public Screen screen = new Screen();
    }

    /** Parses the Patron Passport JSON. */
    public static Passport parsePassport(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        Passport p = new Passport();
        p.title = Json.str(root, "title");
        p.body = Json.str(root, "body");
        p.cta = Json.str(root, "cta");
        p.documents = Json.num(root, "documents");
        p.trackers = Json.num(root, "trackers");
        p.active = Json.num(root, "active");
        p.days = Json.num(root, "days");
        p.screen = screen(Json.obj(root.get("screen")));
        return p;
    }

    /** Release readiness of the build (placeholder attribution blocks a release). */
    public static final class ReleaseInfo {
        public boolean ready;
        public List<String> blockers = new ArrayList<>();
    }

    /** Parses {@code releaseInfo}. */
    public static ReleaseInfo parseReleaseInfo(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        ReleaseInfo r = new ReleaseInfo();
        r.ready = Json.bool(root, "ready");
        r.blockers = Json.strings(root, "blockers");
        return r;
    }

    /** One step of the self-test. */
    public static final class SelfTestStep {
        public String name = "";
        public boolean ok;
        public long ms;
        public String detail = "";
    }

    /** The self-test report. */
    public static final class SelfTest {
        public boolean ok;
        public String guestSha256 = "";
        public List<SelfTestStep> steps = new ArrayList<>();
    }

    /** Parses the self-test JSON. */
    public static SelfTest parseSelfTest(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        SelfTest t = new SelfTest();
        t.ok = Json.bool(root, "ok");
        t.guestSha256 = Json.str(root, "guestSha256");
        for (Object o : Json.arr(root.get("steps"))) {
            Map<String, Object> m = Json.obj(o);
            SelfTestStep s = new SelfTestStep();
            s.name = Json.str(m, "name");
            s.ok = Json.bool(m, "ok");
            s.ms = Json.num(m, "ms");
            s.detail = Json.str(m, "detail");
            t.steps.add(s);
        }
        return t;
    }

    /** Facts about the engine. */
    public static final class EngineInfo {
        public String engine = "";
        public int abi;
        public String guestSha256 = "";
        public String arch = "";
        public String os = "";
        public String appName = "";
        public long maxInputBytes;
        public long maxMemoryBytes;
    }

    /** Parses {@code info}. */
    public static EngineInfo parseEngineInfo(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        EngineInfo e = new EngineInfo();
        e.engine = Json.str(root, "engine");
        e.abi = Json.integer(root, "abi");
        e.guestSha256 = Json.str(root, "guestSha256");
        e.arch = Json.str(root, "arch");
        e.os = Json.str(root, "os");
        e.appName = Json.str(root, "appName");
        e.maxInputBytes = Json.num(root, "maxInputBytes");
        e.maxMemoryBytes = Json.num(root, "maxMemoryBytes");
        return e;
    }

    // ---- helpers ----------------------------------------------------------------------------

    private static String nonEmpty(String s, String def) {
        return s == null || s.isEmpty() ? def : s;
    }

    /** An unmodifiable list, never null. */
    public static <T> List<T> safe(List<T> list) {
        return list == null ? Collections.<T>emptyList() : Collections.unmodifiableList(list);
    }
}
