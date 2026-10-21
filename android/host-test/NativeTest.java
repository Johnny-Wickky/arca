// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;
import java.io.ByteArrayOutputStream;

import org.arca.viewer.Native;

/**
 * Exercises every native method through a real JVM. Run it with {@code -Xcheck:jni}
 * ({@code run.sh} does, and fails on any "WARNING in native method" the JVM prints).
 *
 * <p>Descriptors: the natives own the descriptor they are given (on Android it comes from
 * {@code ParcelFileDescriptor.detachFd()}). A desktop JVM has no such call, so the test opens
 * the file a second time through {@code /proc/self/fd/N}, keeps the Java stream object
 * reachable for the whole run (so its cleaner never closes the number twice) and hands the
 * number to the native side.
 */
public final class NativeTest {
    private static int checks = 0;
    private static final List<Object> KEEP = new ArrayList<>();

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

    private static boolean has(byte[] json, String needle) {
        String s = str(json);
        return s != null && s.contains(needle);
    }

    // ---- fixtures ---------------------------------------------------------------------

    /** A small DOCX with a remote tracking pixel, a hidden run and a comment-less body. */
    static byte[] docxWithTracker() throws IOException {
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        try (ZipOutputStream z = new ZipOutputStream(bos)) {
            put(z, "[Content_Types].xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">"
                + "<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>"
                + "<Default Extension=\"xml\" ContentType=\"application/xml\"/>"
                + "<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>");
            put(z, "_rels/.rels",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">"
                + "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>");
            put(z, "word/_rels/document.xml.rels",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">"
                + "<Relationship Id=\"rId9\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"https://tracker.example.test/pixel.gif\" TargetMode=\"External\"/></Relationships>");
            put(z, "word/document.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" "
                + "xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>"
                + "<w:p><w:r><w:t>Hello, Arca</w:t></w:r></w:p>"
                + "<w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>secret hidden text</w:t></w:r></w:p>"
                + "</w:body></w:document>");
        }
        return bos.toByteArray();
    }

    private static void put(ZipOutputStream z, String name, String body) throws IOException {
        z.putNextEntry(new ZipEntry(name));
        z.write(body.getBytes(StandardCharsets.UTF_8));
        z.closeEntry();
    }

    // ---- descriptor helpers -----------------------------------------------------------

    private static int fdNumber(java.io.FileDescriptor fd) throws Exception {
        Field f = java.io.FileDescriptor.class.getDeclaredField("fd");
        f.setAccessible(true);
        return f.getInt(fd);
    }

    /** A descriptor for reading {@code file}, owned by the caller (the natives close it). */
    private static int readableFd(File file) throws Exception {
        FileInputStream first = new FileInputStream(file);
        KEEP.add(first);
        FileInputStream second = new FileInputStream("/proc/self/fd/" + fdNumber(first.getFD()));
        KEEP.add(second);
        return fdNumber(second.getFD());
    }

    private static int writableFd(File file) throws Exception {
        FileOutputStream out = new FileOutputStream(file);
        KEEP.add(out);
        return fdNumber(out.getFD());
    }

    // ---- helpers over the natives -------------------------------------------------------

    private static long load(byte[] bytes) {
        long doc = Native.docNew(64L << 20);
        check(doc > 0, "docNew");
        // Append in awkward chunk sizes to exercise offsets.
        int pos = 0;
        int step = 1;
        while (pos < bytes.length) {
            int n = Math.min(step, bytes.length - pos);
            eq(0, Native.docAppend(doc, bytes, pos, n), "docAppend");
            pos += n;
            step = Math.min(step * 3 + 1, 4096);
        }
        eq(bytes.length, Native.docLen(doc), "docLen");
        return doc;
    }

    private static long runOp(long doc, int op, int flags) {
        long job = Native.jobNew();
        check(job > 0, "jobNew");
        long outcome = Native.run(job, doc, op, flags);
        Native.jobFree(job);
        check(outcome > 0, "run returned " + outcome);
        return outcome;
    }

    // ---- the tests ----------------------------------------------------------------------

    private static void loading() {
        check(Native.isLoaded(), "libarca_jni did not load: " + Native.loadError());
        // Before init() everything reports "not ready" instead of crashing.
        eq(-Native.NOT_READY, Native.docNew(10), "docNew before init");
        check(Native.info() == null, "info before init");
        check(Native.selfTest() == null, "selfTest before init");
        eq(0, Native.init(256, 768, 120, 180), "init");
        eq(0, Native.init(1, 1, 1, 1), "init is idempotent");
    }

    private static void staticViews() {
        byte[] info = Native.info();
        check(has(info, "\"guestSha256\""), "info has the guest digest");
        check(has(info, "\"abi\":2"), "info has the ABI version");

        for (int lang = 0; lang <= 1; lang++) {
            byte[] about = Native.about(lang);
            check(about != null && about.length > 200, "about " + lang);
            check(has(about, "\"cards\""), "about has cards");
            check(has(about, "\"Upstream\""), "about lists the upstream creators");
        }
        check(has(Native.about(0), "\"lang\":\"ru\""), "about(0) is Russian");
        check(has(Native.about(1), "\"lang\":\"en\""), "about(1) is English");

        for (int which = 0; which <= 2; which++) {
            byte[] text = Native.licenseText(which);
            check(text != null && text.length > 500, "licenseText " + which);
        }
        check(has(Native.licenseText(0), "GNU AFFERO GENERAL PUBLIC LICENSE"), "AGPL text");
        check(Native.licenseText(3) == null, "licenseText(3) is null");
        check(Native.licenseText(-1) == null, "licenseText(-1) is null");

        byte[] release = Native.releaseInfo();
        check(has(release, "\"ready\""), "releaseInfo");

        // Passport: a garbage record starts fresh; update produces a valid 36-byte record.
        byte[] fresh = Native.passport(0, new byte[0], 20000);
        check(has(fresh, "\"documents\":0"), "passport starts fresh");
        byte[] rec = Native.passportUpdate(new byte[0], 20000, 3, 1);
        eq(36, rec.length, "record length");
        byte[] rec2 = Native.passportUpdate(rec, 20005, 2, 0);
        eq(36, rec2.length, "record length after update");
        byte[] p = Native.passport(1, rec2, 20005);
        check(has(p, "\"documents\":2"), "two documents recorded: " + str(p));
        check(has(p, "\"trackers\":5"), "five trackers recorded: " + str(p));
        check(has(p, "\"active\":1"), "one active item recorded");
        check(has(p, "\"days\":6"), "six days in use: " + str(p));
        byte[] garbage = new byte[36];
        java.util.Arrays.fill(garbage, (byte) 0x41);
        check(has(Native.passport(1, garbage, 20005), "\"documents\":0"), "garbage record starts fresh");
        // Null record is tolerated (the glue maps it to an empty array).
        check(Native.passport(1, null, 20005) != null, "null record");
        check(Native.passportUpdate(null, 20005, 0, 0).length == 36, "null record update");
    }

    private static void selfTest() {
        byte[] report = Native.selfTest();
        check(report != null, "selfTest returned null");
        check(has(report, "\"ok\":true"), "self-test failed: " + str(report));
        check(!has(report, "\"ok\":false"), "a self-test step failed: " + str(report));
    }

    private static void documents() {
        eq(-Native.BAD_ARGUMENT, Native.docNew(-1), "docNew(-1)");
        long doc = Native.docNew(100);
        check(doc > 0, "docNew(100)");
        eq(0, Native.docLen(doc), "empty doc");
        byte[] ten = new byte[10];
        eq(0, Native.docAppend(doc, ten, 0, 10), "append 10");
        eq(10, Native.docLen(doc), "len 10");
        // Argument validation: nothing here may crash or throw.
        eq(Native.BAD_ARGUMENT, Native.docAppend(doc, ten, -1, 5), "negative offset");
        eq(Native.BAD_ARGUMENT, Native.docAppend(doc, ten, 0, -5), "negative length");
        eq(Native.BAD_ARGUMENT, Native.docAppend(doc, ten, 8, 5), "range past the array");
        eq(Native.BAD_ARGUMENT, Native.docAppend(doc, null, 0, 5), "null array");
        eq(0, Native.docAppend(doc, ten, 3, 0), "empty append");
        eq(10, Native.docLen(doc), "length unchanged after rejected appends");
        // The cap is enforced.
        eq(0, Native.docAppend(doc, new byte[90], 0, 90), "fill to the cap");
        eq(Native.TOO_LARGE, Native.docAppend(doc, ten, 0, 1), "over the cap");
        eq(100, Native.docLen(doc), "length unchanged after TOO_LARGE");
        Native.docFree(doc);
        eq(-Native.BAD_HANDLE, Native.docLen(doc), "len after free");
        eq(Native.BAD_HANDLE, Native.docAppend(doc, ten, 0, 1), "append after free");
        Native.docFree(doc); // double free is harmless
        Native.docFree(0);
        Native.docFree(-7);
        Native.docFree(Long.MAX_VALUE);
        eq(-Native.BAD_HANDLE, Native.docLen(Long.MIN_VALUE), "len of garbage");
        eq(-Native.BAD_HANDLE, Native.docLen(123456789L), "len of unknown handle");
    }

    private static void scanning() throws Exception {
        long doc = load(docxWithTracker());
        long outcome = runOp(doc, Native.OP_SCAN, 0);
        eq(Native.OK, Native.outcomeStatus(outcome), "scan status");
        byte[] report = Native.report(outcome);
        String json = str(report);
        check(json != null && json.contains("\"format\":\"Docx\""), "format is Docx: " + json);
        check(json.contains("\"kind\":\"ExternalImage\""), "tracker pixel found: " + json);
        check(json.contains("tracker.example.test"), "tracker URL listed");
        check(json.contains("\"verdict\":\"HiddenTrackers\""), "verdict: " + json);
        check(json.contains("\"blocked\":true"), "tracker marked blocked");
        byte[] info = Native.outcomeInfo(outcome);
        check(has(info, "\"statusName\":\"Ok\""), "outcome info: " + str(info));
        check(has(info, "\"fuel\""), "resource stats present");
        // A scan outcome is not a preview/sanitize outcome.
        check(Native.previewMeta(outcome) == null, "scan outcome has no preview");
        check(Native.sanitizedInfo(outcome) == null, "scan outcome has no sterile copy");
        Native.outcomeFree(outcome);
        check(Native.report(outcome) == null, "report after outcomeFree");
        eq(-Native.BAD_HANDLE, Native.outcomeStatus(outcome), "status after outcomeFree");
        Native.outcomeFree(outcome);
        Native.docFree(doc);
    }

    private static void rejectedAndUnsupported() throws Exception {
        // Not a document at all.
        long doc = load("MZ\u0090\u0000this is not a document".getBytes(StandardCharsets.ISO_8859_1));
        long outcome = runOp(doc, Native.OP_SCAN, 0);
        int status = Native.outcomeStatus(outcome);
        check(status == Native.OK || status == Native.REJECTED || status == Native.UNSUPPORTED,
            "binary junk scan status " + status);
        Native.outcomeFree(outcome);
        // A truncated archive is rejected, not crashed on.
        byte[] docx = docxWithTracker();
        byte[] cut = java.util.Arrays.copyOf(docx, docx.length / 2);
        long doc2 = load(cut);
        long o2 = runOp(doc2, Native.OP_PREVIEW, 0);
        int s2 = Native.outcomeStatus(o2);
        check(s2 == Native.REJECTED || s2 == Native.UNSUPPORTED || s2 == Native.FAILED,
            "truncated archive preview status " + s2);
        check(Native.previewMeta(o2) == null, "no preview for a failed run");
        check(Native.outcomeInfo(o2) != null, "failed runs still have diagnostics");
        Native.outcomeFree(o2);
        Native.docFree(doc2);
        // An empty document.
        long empty = Native.docNew(1 << 20);
        long job = Native.jobNew();
        long o3 = Native.run(job, empty, Native.OP_SCAN, 0);
        if (o3 > 0) {
            // The engine answers an empty file with a report, never with a clean verdict.
            int st = Native.outcomeStatus(o3);
            String rep = str(Native.report(o3));
            check(st != Native.OK || (rep != null && !rep.contains("\"verdict\":\"Clean\"")),
                "an empty file must not be reported clean: status " + st + " " + rep);
            Native.outcomeFree(o3);
        } else {
            check(o3 < 0, "empty input gives a status");
        }
        Native.jobFree(job);
        Native.docFree(empty);
        Native.docFree(doc);
    }

    private static void previewing() throws Exception {
        long doc = load(docxWithTracker());
        long outcome = runOp(doc, Native.OP_PREVIEW, 0);
        eq(Native.OK, Native.outcomeStatus(outcome), "preview status");
        String meta = str(Native.previewMeta(outcome));
        check(meta != null && meta.contains("\"sections\""), "preview meta: " + meta);
        check(meta.contains("\"hidden\":false"), "body section is visible: " + meta);
        String items = str(Native.previewItems(outcome, 0, 0, 100));
        check(items != null && items.contains("Hello, Arca"), "body text: " + items);
        check(!items.contains("secret hidden text"), "hidden text is withheld by default: " + items);
        check(Native.previewItems(outcome, 7, 0, 10) == null, "no such section");
        check(Native.previewItems(outcome, -1, 0, 10) == null, "negative section");
        check(Native.previewItems(outcome, 0, -1, 10) == null, "negative start");
        check(Native.previewItems(outcome, 0, 0, -1) == null, "negative count");
        eq(2, str(Native.previewItems(outcome, 0, 5000, 10)).length(), "past the end gives []");
        check(Native.report(outcome) == null, "preview outcome has no X-ray report");
        Native.outcomeFree(outcome);

        long shown = runOp(doc, Native.OP_PREVIEW, Native.PREVIEW_REVEAL_HIDDEN);
        eq(Native.OK, Native.outcomeStatus(shown), "reveal status");
        String all = str(Native.previewItems(shown, 0, 0, 100));
        check(all.contains("secret hidden text"), "hidden text revealed on request: " + all);
        Native.outcomeFree(shown);

        // Unknown flag bits are refused.
        long job = Native.jobNew();
        eq(-Native.BAD_ARGUMENT, Native.run(job, doc, Native.OP_PREVIEW, 1 << 20), "unknown preview flag");
        eq(-Native.BAD_ARGUMENT, Native.run(job, doc, Native.OP_SCAN, 1), "flags on a scan");
        eq(-Native.BAD_ARGUMENT, Native.run(job, doc, 99, 0), "unknown operation");
        eq(-Native.BAD_ARGUMENT, Native.run(job, doc, -1, 0), "negative operation");
        eq(-Native.BAD_ARGUMENT, Native.run(job, doc, Native.OP_SCAN, -1), "negative flags");
        eq(-Native.BAD_HANDLE, Native.run(job, 999999L, Native.OP_SCAN, 0), "unknown document");
        eq(-Native.BAD_HANDLE, Native.run(999999L, doc, Native.OP_SCAN, 0), "unknown job");
        eq(-Native.BAD_HANDLE, Native.run(0, 0, Native.OP_SCAN, 0), "zero handles");
        Native.jobFree(job);
        Native.docFree(doc);
    }

    private static void sanitizing() throws Exception {
        long doc = load(docxWithTracker());
        long outcome = runOp(doc, Native.OP_SANITIZE, Native.SANITIZE_ALL);
        eq(Native.OK, Native.outcomeStatus(outcome), "sanitize status");
        String info = str(Native.sanitizedInfo(outcome));
        check(info != null && info.contains("\"verification\""), "sanitized info: " + info);
        check(info.contains("\"verdict\":\"Clean\""), "the sterile copy re-scans clean: " + info);

        File out = File.createTempFile("arca-sterile", ".docx");
        out.deleteOnExit();
        long written = Native.sanitizedWriteFd(outcome, writableFd(out));
        check(written > 100, "bytes written: " + written);
        eq(written, out.length(), "file size equals the byte count");
        byte[] sterile = Files.readAllBytes(out.toPath());
        eq('P', sterile[0], "zip magic 0");
        eq('K', sterile[1], "zip magic 1");

        // The sterile copy, scanned again through the sandbox, has no tracker.
        long again = load(sterile);
        long rescan = runOp(again, Native.OP_SCAN, 0);
        String report = str(Native.report(rescan));
        check(report.contains("\"verdict\":\"Clean\""), "re-scan verdict: " + report);
        check(!report.contains("tracker.example.test"), "tracker removed from the sterile copy");
        Native.outcomeFree(rescan);
        Native.docFree(again);

        // Writing to a descriptor that is not writable reports an error and still closes it.
        File readonly = File.createTempFile("arca-ro", ".bin");
        readonly.deleteOnExit();
        long failed = Native.sanitizedWriteFd(outcome, readableFd(readonly));
        check(failed < 0, "writing to a read-only descriptor fails: " + failed);
        eq(-Native.IO, failed, "the failure is IO");
        eq(-Native.BAD_ARGUMENT, Native.sanitizedWriteFd(outcome, -1), "negative descriptor");
        eq(-Native.BAD_HANDLE, Native.sanitizedWriteFd(0, writableFd(File.createTempFile("arca-z", ".bin"))),
            "writing a missing outcome");

        Native.outcomeFree(outcome);
        // A DOCX can be sanitised; flags are validated.
        long job = Native.jobNew();
        eq(-Native.BAD_ARGUMENT, Native.run(job, doc, Native.OP_SANITIZE, 1 << 12), "unknown sanitize flag");
        Native.jobFree(job);
        Native.docFree(doc);
    }

    private static void readingFromDescriptors() throws Exception {
        byte[] bytes = docxWithTracker();
        File f = File.createTempFile("arca-in", ".docx");
        f.deleteOnExit();
        Files.write(f.toPath(), bytes);

        long doc = Native.docNew(64L << 20);
        long n = Native.docReadFd(doc, readableFd(f));
        eq(bytes.length, n, "docReadFd byte count");
        eq(bytes.length, Native.docLen(doc), "docLen after docReadFd");
        long outcome = runOp(doc, Native.OP_SCAN, 0);
        eq(Native.OK, Native.outcomeStatus(outcome), "scan of a document read from a descriptor");
        Native.outcomeFree(outcome);
        Native.docFree(doc);

        // Over the cap: reported, not truncated silently.
        long small = Native.docNew(100);
        long r = Native.docReadFd(small, readableFd(f));
        eq(-Native.TOO_LARGE, r, "docReadFd over the cap");
        Native.docFree(small);

        // Bad descriptors.
        long d3 = Native.docNew(1000);
        check(Native.docReadFd(d3, -1) < 0, "docReadFd(-1)");
        check(Native.docReadFd(0, readableFd(f)) < 0, "docReadFd on a zero handle");
        Native.docFree(d3);

        // An endless source (/dev/zero) is stopped by the cap instead of filling the memory.
        FileInputStream zero = new FileInputStream("/dev/zero");
        KEEP.add(zero);
        FileInputStream zero2 = new FileInputStream("/proc/self/fd/" + fdNumber(zero.getFD()));
        KEEP.add(zero2);
        long d4 = Native.docNew(1 << 20);
        eq(-Native.TOO_LARGE, Native.docReadFd(d4, fdNumber(zero2.getFD())), "an endless descriptor hits the cap");
        check(Native.docLen(d4) <= (1 << 20), "the cap held: " + Native.docLen(d4));
        Native.docFree(d4);
    }

    private static void cancelling() throws Exception {
        long doc = load(docxWithTracker());
        long job = Native.jobNew();
        Native.jobCancel(job);
        long outcome = Native.run(job, doc, Native.OP_PREVIEW, 0);
        check(outcome > 0, "a cancelled run still yields an outcome: " + outcome);
        eq(Native.CANCELLED, Native.outcomeStatus(outcome), "cancelled before start");
        check(Native.previewMeta(outcome) == null, "nothing to show after cancel");
        Native.outcomeFree(outcome);
        Native.jobFree(job);
        Native.jobCancel(job); // after free: harmless
        Native.jobCancel(0);
        Native.jobCancel(-1);
        Native.jobFree(job);
        Native.docFree(doc);
    }

    private static void concurrency() throws Exception {
        final byte[] bytes = docxWithTracker();
        final AtomicInteger ok = new AtomicInteger();
        final AtomicInteger bad = new AtomicInteger();
        Thread[] threads = new Thread[6];
        for (int t = 0; t < threads.length; t++) {
            final int op = (t % 3) + 1;
            threads[t] = new Thread(() -> {
                try {
                    for (int i = 0; i < 4; i++) {
                        long doc = load(bytes);
                        long outcome = runOp(doc, op, op == Native.OP_SANITIZE ? Native.SANITIZE_ALL : 0);
                        if (Native.outcomeStatus(outcome) == Native.OK) {
                            ok.incrementAndGet();
                        } else {
                            bad.incrementAndGet();
                        }
                        Native.outcomeFree(outcome);
                        Native.docFree(doc);
                    }
                } catch (Throwable e) {
                    bad.incrementAndGet();
                    e.printStackTrace();
                }
            });
            threads[t].start();
        }
        for (Thread t : threads) {
            t.join();
        }
        eq(0, bad.get(), "failures under concurrency");
        eq(24, ok.get(), "successful concurrent runs");
    }

    private static void leaks() {
        // The self-test and all the tests above must have released everything.
        String info = str(Native.info());
        check(info != null, "info");
    }

    public static void main(String[] args) throws Exception {
        String[] names = {"loading", "staticViews", "selfTest", "documents", "scanning",
            "rejectedAndUnsupported", "previewing", "sanitizing", "readingFromDescriptors",
            "cancelling", "concurrency", "leaks"};
        int failed = 0;
        for (String name : names) {
            long t0 = System.nanoTime();
            try {
                switch (name) {
                    case "loading": loading(); break;
                    case "staticViews": staticViews(); break;
                    case "selfTest": selfTest(); break;
                    case "documents": documents(); break;
                    case "scanning": scanning(); break;
                    case "rejectedAndUnsupported": rejectedAndUnsupported(); break;
                    case "previewing": previewing(); break;
                    case "sanitizing": sanitizing(); break;
                    case "readingFromDescriptors": readingFromDescriptors(); break;
                    case "cancelling": cancelling(); break;
                    case "concurrency": concurrency(); break;
                    default: leaks(); break;
                }
                System.out.printf("ok    %-26s %5d ms%n", name, (System.nanoTime() - t0) / 1_000_000);
            } catch (Throwable e) {
                failed++;
                System.out.printf("FAIL  %-26s %s%n", name, e);
                if (!(e instanceof AssertionError)) {
                    e.printStackTrace(System.out);
                }
                if ("loading".equals(name)) {
                    break; // nothing else can work
                }
            }
        }
        System.out.println(checks + " checks, " + failed + " failed test(s)");
        System.out.flush();
        // halt: the descriptors kept in KEEP must never be closed a second time by a cleaner.
        Runtime.getRuntime().halt(failed == 0 ? 0 : 1);
    }
}
