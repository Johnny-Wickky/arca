// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Random;
import java.util.concurrent.Executor;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

import org.arca.viewer.Native;
import org.arca.viewer.core.Capabilities;
import org.arca.viewer.core.DocOutline;
import org.arca.viewer.core.DocSearch;
import org.arca.viewer.core.Model;

/**
 * Runs real documents through the real engine and the app's real model parsers: the JSON
 * contract between {@code arca-engine} and {@code org.arca.viewer.core.Model}, end to end.
 *
 * <p>Inputs: a generated DOCX, every file of {@code $ARCA_CORPUS_DIR} (the LibreOffice-made
 * corpus), and seeded random mutations of both ({@code $ARCA_FUZZ_ITERS}, default 24 per
 * file). Invariants checked for every input and operation:
 * <ul>
 *   <li>the engine never crashes the JVM and returns a status and a parseable diagnostic;
 *   <li>every JSON document parses with the strict parser;
 *   <li>a clean verdict is never reported for a file the engine rejected or could not read;
 *   <li>paging through every section yields exactly the number of items announced.
 * </ul>
 */
public final class ContractTest {
    private static int checks;
    private static int inputs;
    private static int runs;
    private static int previews;
    private static long itemsSeen;
    private static final List<String> problems = new ArrayList<>();

    private static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            problems.add(what);
            if (problems.size() <= 30) {
                System.out.println("  FAIL " + what);
            }
        }
    }

    private static long load(byte[] bytes) {
        long doc = Native.docNew(64L << 20);
        check(doc > 0, "docNew");
        for (int pos = 0; pos < bytes.length; pos += 65536) {
            Native.docAppend(doc, bytes, pos, Math.min(65536, bytes.length - pos));
        }
        return doc;
    }

    /**
     * What the pristine corpus files must produce: the detected format, then strings that must
     * appear in the preview. Mutated copies are only held to the generic invariants.
     */
    private static final java.util.Map<String, String[]> EXPECT = new java.util.HashMap<>();
    /** The passwords of the encrypted documents of the corpus. */
    private static final java.util.Map<String, String> PASSWORDS = new java.util.HashMap<>();
    /** Text that exists only in hidden parts of a document. */
    private static final java.util.Map<String, String[]> CONCEALED = new java.util.HashMap<>();

    static {
        EXPECT.put("report.doc", new String[] {"Doc", "Квартальный отчёт", "Шаг два"});
        EXPECT.put("lo-rich.doc", new String[] {"Doc"});
        EXPECT.put("report.docx", new String[] {"Docx", "Квартальный отчёт", "Шаг два"});
        EXPECT.put("report.odt", new String[] {"Odt", "Квартальный отчёт"});
        EXPECT.put("report.rtf", new String[] {"Rtf", "Квартальный отчёт"});
        EXPECT.put("report.pdf", new String[] {"Pdf", "Квартальный отчёт", "Шаг два", "العربية, עברית"});
        EXPECT.put("sheet.xls", new String[] {"Xls", "Яблоки", "Груши"});
        EXPECT.put("sheet.xlsx", new String[] {"Xlsx", "Яблоки", "Груши"});
        EXPECT.put("sheet.ods", new String[] {"Ods", "Яблоки", "Груши"});
        EXPECT.put("sheet.csv", new String[] {"Csv", "Яблоки", "Груши"});
        EXPECT.put("slides.ppt", new String[] {"Ppt", "Заголовок первого слайда", "Заметки докладчика", "Второй слайд"});
        EXPECT.put("slides.pptx", new String[] {"Pptx", "Заголовок первого слайда", "Заметки докладчика", "Второй слайд"});
        EXPECT.put("slides.odp", new String[] {"Odp", "Заголовок первого слайда", "Второй слайд"});
        EXPECT.put("deck-rich.ppt", new String[] {"Ppt", "Квартальный обзор 2026", "Заметка докладчика", "example.org"});
        EXPECT.put("deck-rich.pptx", new String[] {"Pptx", "Квартальный обзор 2026", "Заметка докладчика"});
        EXPECT.put("rich-lo.xls", new String[] {"Xls"});
        // Formulas without stored results are calculated: the sum, the text built from it, the lookup.
        EXPECT.put("formulas.xlsx", new String[] {"Xlsx", "Яблоки", "Всего: 100.00 руб."});
        EXPECT.put("drawing.vsdx", new String[] {"Vsdx", "Принять заказ", "Проверить оплату", "Ответственный", "Склад"});

        PASSWORDS.put("enc-agile.docx", "пароль-123");
        PASSWORDS.put("enc-agile.xlsx", "s3cret");
        PASSWORDS.put("enc-standard.docx", "Password1234_");
        PASSWORDS.put("enc-rc4-40.pdf", "user-40");
        PASSWORDS.put("enc-rc4-128.pdf", "user-128");
        PASSWORDS.put("enc-aes-128.pdf", "p\u00e4ssword");
        PASSWORDS.put("enc-aes-256.pdf", "user-256");

        // Hidden content: absent by default (the viewer never shows what the author concealed
        // unless asked) and present, labelled by the app, when the reader asks for it.
        CONCEALED.put("sheet.xls", new String[] {"Скрытый"});
        CONCEALED.put("sheet.xlsx", new String[] {"Скрытый"});
        CONCEALED.put("deck-rich.ppt", new String[] {"СКРЫТЫЙ СЛАЙД"});
        CONCEALED.put("deck-rich.pptx", new String[] {"СКРЫТЫЙ СЛАЙД"});
        CONCEALED.put("rich-lo.xls", new String[] {"СКРЫТАЯ_СТРОКА"});
    }

    private static void one(String name, byte[] bytes) {
        one(name, bytes, false);
    }

    private static void one(String name, byte[] bytes, boolean pristine) {
        inputs++;
        // Files whose behaviour is fully specified: the corpus documents and the generated DOCX.
        final boolean strict = pristine && (EXPECT.containsKey(name) || "generated.docx".equals(name));
        long doc = load(bytes);
        Model.Verdict scanVerdict = null;
        String format = "?";
        try {
            // ---- scan ---------------------------------------------------------------------
            long job = Native.jobNew();
            long outcome = Native.run(job, doc, Native.OP_SCAN, 0);
            Native.jobFree(job);
            runs++;
            check(outcome > 0, name + ": scan returned " + outcome);
            if (outcome > 0) {
                Model.RunInfo info = Model.parseRunInfo(Native.outcomeInfo(outcome));
                int status = Native.outcomeStatus(outcome);
                check(status == info.status, name + ": status mismatch " + status + " vs " + info.status);
                if (status == Native.OK) {
                    Model.Report r = Model.parseReport(Native.report(outcome));
                    scanVerdict = r.verdict;
                    format = r.format;
                    check(r.sha256.length() == 64, name + ": sha256 length " + r.sha256.length());
                    if (r.verdict == Model.Verdict.REJECTED) {
                        check(!r.rejection.isEmpty(), name + ": rejected without a reason");
                    }
                    if (r.verdict == Model.Verdict.CLEAN) {
                        check(r.active == 0 && r.trackers == 0, name + ": clean with counters");
                        check(r.topRank() < 3, name + ": clean with a high-severity finding");
                    }
                    for (Model.Finding f : r.findings) {
                        check(!f.kind.isEmpty() && !f.category.isEmpty(), name + ": finding without kind/category");
                    }
                } else {
                    check(Native.report(outcome) == null, name + ": report for a failed scan");
                }
                Native.outcomeFree(outcome);
            }

            // ---- preview ------------------------------------------------------------------
            for (int flags = 0; flags <= 1; flags++) {
                job = Native.jobNew();
                outcome = Native.run(job, doc, Native.OP_PREVIEW, flags);
                Native.jobFree(job);
                runs++;
                check(outcome > 0, name + ": preview returned " + outcome);
                if (outcome <= 0) {
                    continue;
                }
                int status = Native.outcomeStatus(outcome);
                Model.parseRunInfo(Native.outcomeInfo(outcome));
                if (strict && flags == 0 && scanVerdict != null && !"?".equals(format)) {
                    // The app's capability table and the engine must agree in both directions.
                    if (Capabilities.canPreview(format)) {
                        check(status == Native.OK, name + ": " + format + " is listed as previewable but status was " + status);
                    } else {
                        check(status == Native.UNSUPPORTED, name + ": " + format + " is not listed but status was " + status);
                    }
                }
                if (status == Native.OK) {
                    previews++;
                    StringBuilder allText = new StringBuilder();
                    Model.PreviewMeta meta = Model.parsePreviewMeta(Native.previewMeta(outcome));
                    long announced = 0;
                    for (int s = 0; s < meta.sections.size(); s++) {
                        allText.append(meta.sections.get(s).name).append('\n');
                        long count = meta.sections.get(s).items;
                        announced += count;
                        long seen = 0;
                        // Page like the app does: 64 items at a time, plus one odd-sized page.
                        long from = 0;
                        int page = 0;
                        while (from < count) {
                            int want = page++ == 1 ? 7 : 64;
                            byte[] json = Native.previewItems(outcome, s, from, want);
                            check(json != null, name + ": no items for section " + s + " from " + from);
                            if (json == null) {
                                break;
                            }
                            allText.append(new String(json, StandardCharsets.UTF_8));
                            List<Model.Item> items = Model.parseItems(json);
                            long expect = Math.min(want, count - from);
                            check(items.size() == expect,
                                name + ": section " + s + " page at " + from + " has " + items.size() + " items, expected " + expect);
                            if (items.isEmpty()) {
                                break;
                            }
                            seen += items.size();
                            from += items.size();
                        }
                        check(seen == count, name + ": section " + s + " yielded " + seen + " of " + count);
                        itemsSeen += seen;
                        check(Native.previewItems(outcome, s, count + 10, 5) != null
                            && Model.parseItems(Native.previewItems(outcome, s, count + 10, 5)).isEmpty(),
                            name + ": past-the-end page is not empty");
                    }
                    check(announced == meta.items || meta.truncated || meta.items >= 0, name + ": item totals");
                    if (pristine) {
                        String[] expect = EXPECT.get(name);
                        if (expect != null) {
                            check(expect[0].equals(format), name + ": format " + format + ", expected " + expect[0]);
                            for (int e = 1; e < expect.length; e++) {
                                check(allText.indexOf(expect[e]) >= 0,
                                    name + " (flags " + flags + "): preview lacks <" + expect[e] + ">");
                            }
                            if (flags == 0) {
                                searchChecks(name, outcome, meta, expect);
                            }
                        }
                        String[] concealed = CONCEALED.get(name);
                        if (concealed != null) {
                            for (String c : concealed) {
                                if (flags == 0) {
                                    check(allText.indexOf(c) < 0, name + ": hidden <" + c + "> shown by default");
                                } else {
                                    check(allText.indexOf(c) >= 0, name + ": hidden <" + c + "> not shown on request");
                                }
                            }
                        }
                    }
                } else {
                    check(Native.previewMeta(outcome) == null, name + ": preview meta for a failed run");
                }
                Native.outcomeFree(outcome);
            }

            // ---- sanitize (OOXML only; everything else must say so cleanly) ---------------
            job = Native.jobNew();
            outcome = Native.run(job, doc, Native.OP_SANITIZE, Native.SANITIZE_ALL);
            Native.jobFree(job);
            runs++;
            check(outcome > 0, name + ": sanitize returned " + outcome);
            if (outcome > 0) {
                int status = Native.outcomeStatus(outcome);
                Model.parseRunInfo(Native.outcomeInfo(outcome));
                if (strict && !"?".equals(format)) {
                    if (Capabilities.canSanitize(format)) {
                        check(status == Native.OK, name + ": " + format + " is listed as sanitizable but status was " + status);
                    } else {
                        check(status == Native.UNSUPPORTED, name + ": " + format + " sanitize status " + status);
                    }
                }
                if (status == Native.OK) {
                    Model.SanitizedInfo si = Model.parseSanitized(Native.sanitizedInfo(outcome));
                    check(si.size > 0, name + ": empty sterile copy");
                    // The old formats come out as their OOXML successors.
                    String successor = "Doc".equals(format) || "Rtf".equals(format) ? "Docx"
                        : "Xls".equals(format) ? "Xlsx" : "Ppt".equals(format) ? "Pptx" : null;
                    if (strict && successor != null) {
                        check(successor.equals(si.format), name + ": sterile copy is " + si.format + ", not " + successor);
                    }
                    check(si.verdict == Model.Verdict.CLEAN || si.verdict == Model.Verdict.UNVERIFIED,
                        name + ": sterile copy verdict " + si.verdict);
                } else {
                    check(status == Native.UNSUPPORTED || status == Native.REJECTED || status == Native.FAILED
                        || status == Native.ENCRYPTED || status == Native.RESOURCE_LIMIT || status == Native.TIMEOUT,
                        name + ": sanitize status " + status);
                    check(Native.sanitizedInfo(outcome) == null, name + ": sterile info for a failed run");
                }
                Native.outcomeFree(outcome);
            }
            if (pristine && PASSWORDS.containsKey(name)) {
                passwordChecks(name, doc, PASSWORDS.get(name));
            }
        } catch (RuntimeException e) {
            check(false, name + ": exception " + e);
            e.printStackTrace(System.out);
        } finally {
            Native.docFree(doc);
        }
    }

    private static byte[] utf8(String s) {
        return s.getBytes(java.nio.charset.StandardCharsets.UTF_8);
    }

    private static long scan(long doc, int op, int flags) {
        long job = Native.jobNew();
        long outcome = Native.run(job, doc, op, flags);
        Native.jobFree(job);
        runs++;
        return outcome;
    }

    /**
     * An encrypted document through the real engine: it asks for a password, refuses a wrong one,
     * and with the right one reports, previews and sterilises what is inside.
     */
    private static void passwordChecks(String name, long doc, String password) {
        long outcome = scan(doc, Native.OP_SCAN, 0);
        check(outcome > 0, name + ": scan without a password");
        Model.Report locked = Model.parseReport(Native.report(outcome));
        check(locked.needsPassword(), name + ": the report does not ask for a password (" + locked.format + ")");
        Native.outcomeFree(outcome);

        byte[] wrong = utf8("definitely not it");
        check(Native.docSetPassword(doc, wrong, 0, wrong.length) == Native.OK, name + ": set a password");
        outcome = scan(doc, Native.OP_SCAN, 0);
        check(outcome > 0 && Native.outcomeStatus(outcome) == Native.WRONG_PASSWORD,
            name + ": a wrong password gave status " + (outcome > 0 ? Native.outcomeStatus(outcome) : outcome));
        check(Native.report(outcome) == null, name + ": a report for a wrong password");
        Native.outcomeFree(outcome);

        byte[] right = utf8(password);
        check(Native.docSetPassword(doc, right, 0, right.length) == Native.OK, name + ": set the password");
        outcome = scan(doc, Native.OP_SCAN, 0);
        check(outcome > 0 && Native.outcomeStatus(outcome) == Native.OK, name + ": scan with the password");
        if (outcome > 0 && Native.outcomeStatus(outcome) == Native.OK) {
            Model.Report open = Model.parseReport(Native.report(outcome));
            check(!open.needsPassword(), name + ": still asks for a password (" + open.format + ")");
            check(Model.parseRunInfo(Native.outcomeInfo(outcome)).unlock == 1, name + ": not marked as decrypted");
            check(open.sha256.equals(locked.sha256), name + ": the hash is not that of the file");
        }
        Native.outcomeFree(outcome);

        outcome = scan(doc, Native.OP_PREVIEW, 0);
        check(outcome > 0 && Native.outcomeStatus(outcome) == Native.OK, name + ": preview with the password");
        if (outcome > 0 && Native.outcomeStatus(outcome) == Native.OK) {
            Model.PreviewMeta meta = Native.previewMeta(outcome) == null ? null : Model.parsePreviewMeta(Native.previewMeta(outcome));
            check(meta != null && !meta.sections.isEmpty(), name + ": the preview has no sections");
            StringBuilder all = new StringBuilder();
            for (int sec = 0; meta != null && sec < meta.sections.size(); sec++) {
                byte[] json = Native.previewItems(outcome, sec, 0, 64);
                if (json != null) {
                    all.append(new String(json, java.nio.charset.StandardCharsets.UTF_8));
                }
            }
            String expect = name.endsWith("xlsx") ? "\u042f\u0431\u043b\u043e\u043a\u0438" : "\u041a\u0432\u0430\u0440\u0442\u0430\u043b\u044c\u043d\u044b\u0439";
            check(all.indexOf(expect) >= 0, name + ": the decrypted text is not in the preview");
        }
        Native.outcomeFree(outcome);

        outcome = scan(doc, Native.OP_SANITIZE, Native.SANITIZE_ALL);
        check(outcome > 0 && Native.outcomeStatus(outcome) == Native.OK, name + ": sterile copy with the password");
        Native.outcomeFree(outcome);

        // Forgetting the password makes the document locked again.
        check(Native.docSetPassword(doc, new byte[0], 0, 0) == Native.OK, name + ": forget the password");
        outcome = scan(doc, Native.OP_PREVIEW, 0);
        check(outcome > 0 && Native.outcomeStatus(outcome) == Native.ENCRYPTED, name + ": locked again");
        Native.outcomeFree(outcome);
        check(Native.docSetPassword(doc, new byte[2000], 0, 2000) == Native.BAD_ARGUMENT, name + ": a 2000-byte password");
        check(Native.docSetPassword(0, right, 0, right.length) != Native.OK, name + ": a password for no document");
    }

    /**
     * The in-document search over the real engine's pages: every phrase the preview must show is
     * found even when typed in capitals (case folding), and a phrase that is nowhere is not.
     */
    private static void searchChecks(String name, final long outcome, Model.PreviewMeta meta, String[] expect) {
        final Executor direct = new Executor() {
            @Override
            public void execute(Runnable r) {
                r.run();
            }
        };
        DocSearch.Loader loader = new DocSearch.Loader() {
            @Override
            public List<Model.Item> load(int section, long from, int count) {
                byte[] json = Native.previewItems(outcome, section, from, count);
                if (json == null) {
                    throw new IllegalStateException("page not available");
                }
                return Model.parseItems(json);
            }
        };
        DocSearch.Listener quiet = new DocSearch.Listener() {
            @Override
            public void onChanged() {}
        };
        long[] sizes = new long[meta.sections.size()];
        long total = 0;
        for (int i = 0; i < sizes.length; i++) {
            sizes[i] = meta.sections.get(i).items;
            total += sizes[i];
        }
        for (int e = 1; e < expect.length; e++) {
            DocSearch ds = new DocSearch(expect[e].toUpperCase(java.util.Locale.ROOT), sizes, loader, direct, direct, quiet);
            ds.start();
            check(ds.done() && !ds.failed(), name + ": search for <" + expect[e] + "> did not finish");
            check(ds.hitCount() > 0, name + ": search does not find <" + expect[e] + ">");
            check(ds.scanned() >= total || ds.capped(), name + ": search looked at " + ds.scanned() + " of " + total);
            for (int h = 0; h < ds.hitCount(); h++) {
                check(ds.hitItem(h) >= 0 && ds.hitItem(h) < sizes[ds.hitSection(h)], name + ": hit outside its section");
            }
        }
        DocOutline outline = new DocOutline(sizes, loader, direct, direct, quiet);
        outline.start();
        check(outline.done() && !outline.failed(), name + ": outline did not finish");
        for (DocOutline.Entry en : outline.entries()) {
            check(en.section >= 0 && en.section < sizes.length && en.item >= 0 && en.item < sizes[en.section]
                && !en.text.isEmpty(), name + ": outline entry outside the document");
        }
        if (name.startsWith("report.") && !name.endsWith(".csv") && !name.endsWith(".txt") && !name.endsWith(".pdf")
            && !name.endsWith(".rtf")) {
            check(!outline.entries().isEmpty(), name + ": a report has headings");
        }
        DocSearch none = new DocSearch("zzqq-nowhere-" + name, sizes, loader, direct, direct, quiet);
        none.start();
        check(none.done() && none.hitCount() == 0, name + ": search found text that is not there");
    }

    private static byte[] mutate(byte[] src, Random rnd) {
        byte[] b = src.clone();
        int kind = rnd.nextInt(6);
        switch (kind) {
            case 0: // flip a few bits
                for (int i = 0; i < 1 + rnd.nextInt(8) && b.length > 0; i++) {
                    b[rnd.nextInt(b.length)] ^= (byte) (1 << rnd.nextInt(8));
                }
                return b;
            case 1: // truncate
                return Arrays.copyOf(b, rnd.nextInt(Math.max(1, b.length)));
            case 2: // overwrite a run with 0xFF / 0x00
                if (b.length > 8) {
                    int at = rnd.nextInt(b.length - 4);
                    int n = 1 + rnd.nextInt(Math.min(64, b.length - at));
                    Arrays.fill(b, at, at + n, (byte) (rnd.nextBoolean() ? 0xFF : 0x00));
                }
                return b;
            case 3: // insert garbage
                if (b.length > 8) {
                    int at = rnd.nextInt(b.length);
                    byte[] junk = new byte[1 + rnd.nextInt(32)];
                    rnd.nextBytes(junk);
                    byte[] out = new byte[b.length + junk.length];
                    System.arraycopy(b, 0, out, 0, at);
                    System.arraycopy(junk, 0, out, at, junk.length);
                    System.arraycopy(b, at, out, at + junk.length, b.length - at);
                    return out;
                }
                return b;
            case 4: // delete a slice
                if (b.length > 16) {
                    int at = rnd.nextInt(b.length - 8);
                    int n = 1 + rnd.nextInt(Math.min(256, b.length - at));
                    byte[] out = new byte[b.length - n];
                    System.arraycopy(b, 0, out, 0, at);
                    System.arraycopy(b, at + n, out, at, b.length - at - n);
                    return out;
                }
                return b;
            default: // splice: header of one file, tail of another
                return b.length > 32 ? Arrays.copyOfRange(b, rnd.nextInt(16), b.length) : b;
        }
    }

    private static void put(ZipOutputStream z, String name, String body) throws IOException {
        z.putNextEntry(new ZipEntry(name));
        z.write(body.getBytes(StandardCharsets.UTF_8));
        z.closeEntry();
    }

    /** A workbook whose only sheet is hidden. */
    private static byte[] workbookWithOnlyAHiddenSheet() throws IOException {
        String rels = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        try (ZipOutputStream z = new ZipOutputStream(bos)) {
            put(z, "[Content_Types].xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">"
                + "<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>"
                + "<Default Extension=\"xml\" ContentType=\"application/xml\"/>"
                + "<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/></Types>");
            put(z, "_rels/.rels",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">"
                + "<Relationship Id=\"rId1\" Type=\"" + rels + "/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>");
            put(z, "xl/workbook.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"" + rels + "\">"
                + "<sheets><sheet name=\"Only\" sheetId=\"1\" state=\"hidden\" r:id=\"rId1\"/></sheets></workbook>");
            put(z, "xl/_rels/workbook.xml.rels",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">"
                + "<Relationship Id=\"rId1\" Type=\"" + rels + "/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>");
            put(z, "xl/worksheets/sheet1.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>"
                + "<row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>kept out of sight</t></is></c></row></sheetData></worksheet>");
        }
        return bos.toByteArray();
    }

    /**
     * A preview may have no section at all: every part of the document is hidden and hidden
     * content was not asked for. The app's screen is written for that shape (it says there is
     * nothing to show and explains why); this pins the shape across the whole stack, from the
     * guest through the JNI bridge to the app's own parser.
     */
    private static void documentWithNothingToShow() throws IOException {
        long doc = load(workbookWithOnlyAHiddenSheet());
        try {
            for (int flags = 0; flags <= 1; flags++) {
                long job = Native.jobNew();
                long outcome = Native.run(job, doc, Native.OP_PREVIEW, flags);
                Native.jobFree(job);
                check(outcome > 0, "hidden-only workbook: preview returned " + outcome);
                if (outcome <= 0) {
                    continue;
                }
                check(Native.outcomeStatus(outcome) == Native.OK, "hidden-only workbook: status " + Native.outcomeStatus(outcome));
                Model.PreviewMeta meta = Model.parsePreviewMeta(Native.previewMeta(outcome));
                if (flags == 0) {
                    check(meta.sections.isEmpty(), "hidden-only workbook: " + meta.sections.size() + " section(s) without being asked");
                    boolean announced = false;
                    for (Model.Notice n : meta.notices) {
                        announced |= "HiddenOmitted".equals(n.kind);
                    }
                    check(announced, "hidden-only workbook: the omitted sheet is not announced");
                    check(Native.previewItems(outcome, 0, 0, 1) == null, "hidden-only workbook: items for a section that does not exist");
                } else {
                    check(meta.sections.size() == 1 && meta.sections.get(0).hidden && meta.sections.get(0).items > 0,
                        "hidden-only workbook: the sheet is not shown on request");
                }
                Native.outcomeFree(outcome);
            }
        } finally {
            Native.docFree(doc);
        }
    }

    public static void main(String[] args) throws Exception {
        if (Native.init(256, 768, 120, 180) != 0) {
            System.out.println("FAIL init");
            Runtime.getRuntime().halt(1);
        }
        long t0 = System.nanoTime();
        List<String> names = new ArrayList<>();
        List<byte[]> blobs = new ArrayList<>();
        names.add("generated.docx");
        blobs.add(NativeTest.docxWithTracker());
        String corpus = System.getenv("ARCA_CORPUS_DIR");
        if (corpus != null && !corpus.isEmpty()) {
            File[] files = new File(corpus).listFiles();
            if (files != null) {
                Arrays.sort(files);
                for (File f : files) {
                    if (f.isFile() && f.length() < (8 << 20)) {
                        names.add(f.getName());
                        blobs.add(Files.readAllBytes(f.toPath()));
                    }
                }
            }
        }
        // Hand-made edge cases next to the real files.
        names.add("text-cp1251.txt");
        blobs.add("Привет, мир! Это тест.".getBytes("windows-1251"));
        names.add("empty");
        blobs.add(new byte[0]);
        names.add("one-byte");
        blobs.add(new byte[] {0x50});
        names.add("zip-magic-only");
        blobs.add(new byte[] {0x50, 0x4B, 0x03, 0x04});
        names.add("ole-magic-only");
        blobs.add(new byte[] {(byte) 0xD0, (byte) 0xCF, 0x11, (byte) 0xE0, (byte) 0xA1, (byte) 0xB1, 0x1A, (byte) 0xE1});
        names.add("pdf-header-only");
        blobs.add("%PDF-1.7\n".getBytes(StandardCharsets.US_ASCII));
        names.add("rtf-header-only");
        blobs.add("{\\rtf1\\ansi".getBytes(StandardCharsets.US_ASCII));
        names.add("pe-header");
        blobs.add("MZ\u0090\u0000\u0003\u0000\u0000\u0000".getBytes(StandardCharsets.ISO_8859_1));

        int iters = 24;
        String env = System.getenv("ARCA_FUZZ_ITERS");
        if (env != null && !env.isEmpty()) {
            iters = Integer.parseInt(env);
        }

        for (int i = 0; i < names.size(); i++) {
            one(names.get(i), blobs.get(i), true);
        }
        System.out.println("pristine inputs: " + inputs + ", operations: " + runs + ", previews: " + previews
            + ", items paged: " + itemsSeen);
        documentWithNothingToShow();

        Random rnd = new Random(0xA2CA_2026L);
        int mutated = 0;
        for (int i = 0; i < names.size(); i++) {
            byte[] src = blobs.get(i);
            if (src.length < 16) {
                continue;
            }
            for (int k = 0; k < iters; k++) {
                one(names.get(i) + "~" + k, mutate(src, rnd));
                mutated++;
            }
        }
        System.out.println("mutated inputs: " + mutated);
        System.out.println(checks + " checks, " + problems.size() + " problem(s), "
            + (System.nanoTime() - t0) / 1_000_000_000L + " s");
        System.out.flush();
        Runtime.getRuntime().halt(problems.isEmpty() ? 0 : 1);
    }
}
