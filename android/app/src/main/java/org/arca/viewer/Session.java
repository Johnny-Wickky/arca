// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.content.ContentResolver;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelFileDescriptor;
import android.os.SystemClock;
import android.provider.OpenableColumns;

import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.ImageFacts;
import org.arca.viewer.core.Model;

import java.io.FileNotFoundException;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.ThreadFactory;

/**
 * The document the user is looking at, and everything done with it.
 *
 * <p>The session outlives the activity (rotating, folding or resizing never restarts a scan),
 * owns every native handle, and is the only place that calls the long-running natives. All
 * public methods and all listener callbacks run on the main thread; the blocking natives run on
 * one worker thread, so engine runs are serialised, and page loads for the preview run on a
 * second one so scrolling stays responsive while a run is in progress.
 */
public final class Session {
    /** What the session is doing with the document. */
    public enum Phase { IDLE, READING, SCANNING, READY, FAILED }

    // Failure codes beyond the engine's own statuses (see Native).
    public static final int F_OPEN = 100;
    public static final int F_EMPTY = 101;
    public static final int F_ENGINE = 102;
    public static final int F_PARSE = 103;

    /** Why something failed: an engine status (or an {@code F_*} code) plus technical detail. */
    public static final class Failure {
        public final int status;
        public final String technical;

        Failure(int status, String technical) {
            this.status = status;
            this.technical = technical == null ? "" : technical;
        }
    }

    /** Told on the main thread whenever anything changed. */
    public interface Listener {
        void onSessionChanged();
    }

    /** State of the preview of the current document. */
    public static final class PreviewState {
        public boolean running;
        public boolean reveal;
        /** The person cancelled the run; the screen goes back instead of showing an error. */
        public boolean cancelled;
        long outcome;
        public Model.PreviewMeta meta;
        public Failure failure;

        public boolean ready() {
            return meta != null && outcome > 0;
        }
    }

    /** State of the page being drawn (the page view of a document). */
    public static final class PageState {
        public boolean running;
        /** The page asked for (zero-based). */
        public int index;
        public org.arca.viewer.core.PageDrawing drawing;
        public Failure failure;
    }

    /** State of the sterile copy of the current document. */
    public static final class SanitizeState {
        public boolean running;
        public int flags;
        /** The person cancelled the run; the screen goes back instead of showing an error. */
        public boolean cancelled;
        long outcome;
        public Model.SanitizedInfo info;
        public Failure failure;
        public boolean saving;
        public boolean saved;
        public long savedBytes;
        public Failure saveFailure;

        public boolean ready() {
            return info != null && outcome > 0;
        }
    }

    private static final Session INSTANCE = new Session();

    public static Session get() {
        return INSTANCE;
    }

    private final Handler main = new Handler(Looper.getMainLooper());
    private final List<Listener> listeners = new ArrayList<>();
    private final ExecutorService worker = Executors.newSingleThreadExecutor(named("arca-engine"));
    private final ExecutorService pager = Executors.newSingleThreadExecutor(named("arca-pages"));

    /** Guards {@link #epoch}, {@link #doc} and {@link #job}, which the worker thread also touches. */
    private final Object lock = new Object();
    private volatile int epoch;
    // Bumped whenever the preview / sterile copy is reset, so a run that finishes late is dropped
    // instead of being shown to whoever opens the screen next (main thread only).
    private int previewEpoch;
    private int sanitizeEpoch;
    private Phase phase = Phase.IDLE;
    private String name = "";
    private String mime = "";
    private long size = -1;
    private long startedAt;
    private Uri uri;
    private Failure failure;
    private Model.Report report;
    /** How the document was opened if it was encrypted (see {@link Model.RunInfo#unlock}). */
    private int unlock;
    private boolean wrongPassword;
    private boolean unlocking;
    private long doc;
    private long job;
    private final PreviewState preview = new PreviewState();
    private final SanitizeState sanitize = new SanitizeState();
    private final PageState page = new PageState();

    private Session() {}

    private static ThreadFactory named(final String name) {
        return new ThreadFactory() {
            @Override
            public Thread newThread(Runnable r) {
                Thread t = new Thread(r, name);
                t.setDaemon(true);
                return t;
            }
        };
    }

    /** What to do when a background task dies with anything at all (OutOfMemoryError included). */
    private interface Recovery {
        void recover(Throwable t);
    }

    /**
     * Wraps a background task so that nothing it throws can leave the session stuck in a running
     * state: the recovery posts the failure to the main thread like any other failed run.
     */
    private static Runnable guarded(final Runnable body, final Recovery recovery) {
        return new Runnable() {
            @Override
            public void run() {
                try {
                    body.run();
                } catch (Throwable t) {
                    try {
                        recovery.recover(t);
                    } catch (Throwable ignored) {
                        // nothing more can be done
                    }
                }
            }
        };
    }

    // ---- observation ------------------------------------------------------------------------

    public void addListener(Listener l) {
        if (!listeners.contains(l)) {
            listeners.add(l);
        }
    }

    public void removeListener(Listener l) {
        listeners.remove(l);
    }

    private void changed() {
        for (Listener l : new ArrayList<>(listeners)) {
            l.onSessionChanged();
        }
    }

    private void post(final Runnable r) {
        main.post(r);
    }

    public Phase phase() {
        return phase;
    }

    /** Display name of the document (untrusted: pass through {@link Fmt#safe}). */
    public String name() {
        return name;
    }

    public String mime() {
        return mime;
    }

    /** Size in bytes, or -1 while unknown. */
    public long size() {
        return size;
    }

    /** {@link SystemClock#elapsedRealtime()} at the moment the current document was opened. */
    public long startedAt() {
        return startedAt;
    }

    public Uri uri() {
        return uri;
    }

    public Failure failure() {
        return failure;
    }

    /** 0: the document was not encrypted; 1: opened with the reader's password; 2: needed none. */
    public int unlock() {
        return unlock;
    }

    /** Whether a password is being tried (the X-ray runs again over the decrypted content). */
    public boolean unlocking() {
        return unlocking;
    }

    /** Whether the last password the reader typed was refused. */
    public boolean wrongPassword() {
        return wrongPassword;
    }

    /** Whether the open document is itself a picture (so a failure is a picture's, not a document's). */
    public boolean isPicture() {
        return report != null && Model.isPictureFormat(report.format);
    }

    public Model.Report report() {
        return report;
    }

    public PageState page() {
        return page;
    }

    public PreviewState preview() {
        return preview;
    }

    public SanitizeState sanitize() {
        return sanitize;
    }

    /** Executor for page loads of the preview. */
    public java.util.concurrent.Executor pageExecutor() {
        return pager;
    }

    /** Executor that delivers results on the main thread. */
    public java.util.concurrent.Executor mainExecutor() {
        return new java.util.concurrent.Executor() {
            @Override
            public void execute(Runnable r) {
                main.post(r);
            }
        };
    }

    // ---- opening ----------------------------------------------------------------------------

    /** Starts reading and scanning {@code uri}; whatever was open before is released first. */
    public void open(Context context, final Uri source, String mimeType) {
        final Context app = context.getApplicationContext();
        close();
        final int ep;
        synchronized (lock) {
            ep = ++epoch;
        }
        phase = Phase.READING;
        startedAt = SystemClock.elapsedRealtime();
        uri = source;
        mime = mimeType == null ? "" : mimeType;
        name = "";
        size = -1;
        failure = null;
        report = null;
        unlock = 0;
        wrongPassword = false;
        unlocking = false;
        changed();
        worker.execute(new Runnable() {
            @Override
            public void run() {
                try {
                    load(app, source, ep);
                } catch (Throwable t) {
                    // OutOfMemoryError included: the phase must never stay stuck on READING/SCANNING.
                    fail(ep, F_PARSE, t.toString());
                }
            }
        });
    }

    private boolean current(int ep) {
        return ep == epoch;
    }

    private long docHandle() {
        synchronized (lock) {
            return doc;
        }
    }

    private void fail(final int ep, final int status, final String technical) {
        post(new Runnable() {
            @Override
            public void run() {
                if (!current(ep)) {
                    return;
                }
                unlocking = false;
                if (status == Native.CANCELLED) {
                    phase = Phase.IDLE;
                } else {
                    phase = Phase.FAILED;
                    failure = new Failure(status, technical);
                }
                changed();
            }
        });
    }

    private void load(Context app, Uri source, final int ep) {
        ContentResolver cr = app.getContentResolver();
        final String[] meta = queryMeta(cr, source);
        final long declared = parseLong(meta[1]);
        post(new Runnable() {
            @Override
            public void run() {
                if (current(ep)) {
                    name = meta[0];
                    size = declared;
                    changed();
                }
            }
        });

        if (Engine.await(20_000) != Engine.State.READY) {
            fail(ep, F_ENGINE, Engine.failure());
            return;
        }
        long limit = Engine.info().maxInputBytes > 0 ? Engine.info().maxInputBytes : 256L << 20;
        if (declared > limit) {
            fail(ep, Native.TOO_LARGE, "declared size " + declared + " exceeds " + limit);
            return;
        }

        ParcelFileDescriptor pfd;
        try {
            pfd = cr.openFileDescriptor(source, "r");
        } catch (FileNotFoundException | SecurityException | IllegalArgumentException e) {
            fail(ep, F_OPEN, e.toString());
            return;
        } catch (RuntimeException e) {
            fail(ep, F_OPEN, e.toString());
            return;
        }
        if (pfd == null) {
            fail(ep, F_OPEN, "the provider returned no descriptor");
            return;
        }
        // A pipe or socket (some content:// providers hand those out) has no size and may never
        // deliver an end: the read would block with no timeout. Only regular files are read.
        if (pfd.getStatSize() < 0) {
            try {
                pfd.close();
            } catch (java.io.IOException ignored) {
                // nothing to do
            }
            fail(ep, F_OPEN, "not a regular file");
            return;
        }

        long handle = Native.docNew(limit);
        if (handle <= 0) {
            try {
                pfd.close();
            } catch (java.io.IOException ignored) {
                // nothing to do
            }
            fail(ep, (int) -handle, "docNew");
            return;
        }
        // The native side takes ownership of the descriptor and closes it.
        long read = Native.docReadFd(handle, pfd.detachFd());
        if (read < 0) {
            Native.docFree(handle);
            fail(ep, (int) -read, "docReadFd");
            return;
        }
        if (read == 0) {
            Native.docFree(handle);
            fail(ep, F_EMPTY, "empty file");
            return;
        }
        final long length = read;
        final long jobHandle = Native.jobNew();
        final long docHandle = handle;
        boolean stale;
        synchronized (lock) {
            stale = !current(ep);
            if (!stale) {
                doc = docHandle;
                job = jobHandle;
            }
        }
        if (stale) {
            Native.jobFree(jobHandle);
            Native.docFree(docHandle);
            return;
        }
        post(new Runnable() {
            @Override
            public void run() {
                if (current(ep)) {
                    size = length;
                    phase = Phase.SCANNING;
                    changed();
                }
            }
        });

        long outcome = Native.run(jobHandle, docHandle, Native.OP_SCAN, 0);
        // The job only lives as long as its run: startPreview/startSanitize replace the field with
        // their own job, and the scan's handle would be lost (and leaked) if it were kept for close().
        clearJob(ep, jobHandle);
        Native.jobFree(jobHandle);
        if (outcome <= 0) {
            fail(ep, (int) -outcome, "run");
            return;
        }
        int status = Native.outcomeStatus(outcome);
        if (status != Native.OK) {
            String message = describe(outcome);
            Native.outcomeFree(outcome);
            fail(ep, status, message);
            return;
        }
        final int unlockLevel = unlockOf(outcome);
        final Model.Report parsed;
        try {
            byte[] json = Native.report(outcome);
            if (json == null) {
                fail(ep, F_PARSE, "no report");
                return;
            }
            parsed = Model.parseReport(json);
        } catch (RuntimeException | OutOfMemoryError e) {
            fail(ep, F_PARSE, e.toString());
            return;
        } finally {
            Native.outcomeFree(outcome);
        }
        recordStats(app, parsed);
        post(new Runnable() {
            @Override
            public void run() {
                if (!current(ep)) {
                    return;
                }
                report = parsed;
                unlock = unlockLevel;
                phase = Phase.READY;
                changed();
            }
        });
    }

    /**
     * Opens the encrypted document with the password the reader typed (the engine decrypts it in
     * the sandbox and the X-ray runs again over what is inside). A wrong password leaves the
     * session as it was and sets {@link #wrongPassword()}. The array is wiped.
     */
    public void unlock(final char[] password) {
        final byte[] bytes = Fmt.utf8(password);
        java.util.Arrays.fill(password, '\0');
        if (phase != Phase.READY) {
            java.util.Arrays.fill(bytes, (byte) 0);
            return;
        }
        final int ep = epoch;
        final long d = docHandle();
        unlocking = true;
        wrongPassword = false;
        changed();
        worker.execute(guarded(new Runnable() {
            @Override
            public void run() {
                int st;
                try {
                    st = Native.docSetPassword(d, bytes, 0, bytes.length);
                } finally {
                    java.util.Arrays.fill(bytes, (byte) 0);
                }
                if (st != Native.OK) {
                    fail(ep, st, "docSetPassword");
                    return;
                }
                long j = Native.jobNew();
                setJob(ep, j);
                long outcome = Native.run(j, d, Native.OP_SCAN, 0);
                clearJob(ep, j);
                Native.jobFree(j);
                if (outcome <= 0) {
                    fail(ep, (int) -outcome, "run");
                    return;
                }
                int status = Native.outcomeStatus(outcome);
                if (status == Native.WRONG_PASSWORD) {
                    Native.outcomeFree(outcome);
                    Native.docSetPassword(d, new byte[0], 0, 0);
                    post(new Runnable() {
                        @Override
                        public void run() {
                            if (current(ep)) {
                                wrongPassword = true;
                                unlocking = false;
                                changed();
                            }
                        }
                    });
                    return;
                }
                if (status != Native.OK) {
                    String message = describe(outcome);
                    Native.outcomeFree(outcome);
                    fail(ep, status, message);
                    return;
                }
                final int unlockLevel = unlockOf(outcome);
                final Model.Report parsed;
                try {
                    byte[] json = Native.report(outcome);
                    if (json == null) {
                        fail(ep, F_PARSE, "no report");
                        return;
                    }
                    parsed = Model.parseReport(json);
                } catch (RuntimeException | OutOfMemoryError e) {
                    fail(ep, F_PARSE, e.toString());
                    return;
                } finally {
                    Native.outcomeFree(outcome);
                }
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (!current(ep)) {
                            return;
                        }
                        // What was computed for the locked file is about something else now.
                        final long po = preview.outcome;
                        final long so = sanitize.outcome;
                        resetPreview();
                        resetSanitize();
                        if (po > 0 || so > 0) {
                            worker.execute(new Runnable() {
                                @Override
                                public void run() {
                                    if (po > 0) {
                                        Native.outcomeFree(po);
                                    }
                                    if (so > 0) {
                                        Native.outcomeFree(so);
                                    }
                                }
                            });
                        }
                        report = parsed;
                        unlock = unlockLevel;
                        wrongPassword = false;
                        unlocking = false;
                        changed();
                    }
                });
            }
        }, new Recovery() {
            @Override
            public void recover(Throwable t) {
                fail(ep, F_PARSE, t.toString());
            }
        }));
    }

    private static int unlockOf(long outcome) {
        try {
            byte[] json = Native.outcomeInfo(outcome);
            return json == null ? 0 : Model.parseRunInfo(json).unlock;
        } catch (RuntimeException | OutOfMemoryError e) {
            return 0;
        }
    }

    /**
     * Counts the scan in the local statistics (counters per day, nothing about the document; does
     * nothing when the person turned statistics off). Never lets a failure break the scan.
     */
    private void recordStats(Context app, Model.Report r) {
        try {
            Stats.recordScan(app, r);
        } catch (RuntimeException ignored) {
            // statistics are a courtesy
        }
    }

    private static String describe(long outcome) {
        try {
            byte[] json = Native.outcomeInfo(outcome);
            return json == null ? "" : Model.parseRunInfo(json).message;
        } catch (RuntimeException | OutOfMemoryError e) {
            return "";
        }
    }

    private static String[] queryMeta(ContentResolver cr, Uri uri) {
        String name = "";
        String size = "-1";
        Cursor c = null;
        try {
            c = cr.query(uri, new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE}, null, null, null);
            if (c != null && c.moveToFirst()) {
                int n = c.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                int s = c.getColumnIndex(OpenableColumns.SIZE);
                if (n >= 0 && !c.isNull(n)) {
                    name = c.getString(n);
                }
                if (s >= 0 && !c.isNull(s)) {
                    size = Long.toString(c.getLong(s));
                }
            }
        } catch (RuntimeException ignored) {
            // some providers refuse queries; the name then falls back to the URI
        } finally {
            if (c != null) {
                c.close();
            }
        }
        if (name == null || name.isEmpty()) {
            String last = uri.getLastPathSegment();
            name = last == null ? "" : last;
        }
        return new String[] {name, size};
    }

    private static long parseLong(String s) {
        try {
            return Long.parseLong(s);
        } catch (NumberFormatException e) {
            return -1;
        }
    }

    // ---- cancelling and closing -------------------------------------------------------------

    /** Asks the running operation to stop (the engine stops at its next check). */
    public void cancel() {
        long j;
        synchronized (lock) {
            j = job;
        }
        if (j > 0) {
            Native.jobCancel(j);
        }
    }

    /** Releases the document and every result; the session goes back to {@link Phase#IDLE}. */
    public void close() {
        cancel();
        final long d;
        final long j;
        synchronized (lock) {
            epoch++;
            d = doc;
            j = job;
            doc = 0;
            job = 0;
        }
        final long po = preview.outcome;
        final long so = sanitize.outcome;
        phase = Phase.IDLE;
        name = "";
        size = -1;
        uri = null;
        failure = null;
        report = null;
        resetPreview();
        resetSanitize();
        resetImages();
        if (d != 0 || j != 0 || po != 0 || so != 0) {
            // After whatever the worker is running right now.
            worker.execute(new Runnable() {
                @Override
                public void run() {
                    if (po > 0) {
                        Native.outcomeFree(po);
                    }
                    if (so > 0) {
                        Native.outcomeFree(so);
                    }
                    if (j > 0) {
                        Native.jobFree(j);
                    }
                    if (d > 0) {
                        Native.docFree(d);
                    }
                }
            });
        }
        changed();
    }

    private void resetPreview() {
        previewEpoch++;
        resetPage();
        preview.running = false;
        preview.cancelled = false;
        preview.outcome = 0;
        preview.meta = null;
        preview.failure = null;
    }

    private void resetSanitize() {
        sanitizeEpoch++;
        sanitize.running = false;
        sanitize.cancelled = false;
        sanitize.outcome = 0;
        sanitize.info = null;
        sanitize.failure = null;
        sanitize.saving = false;
        sanitize.saved = false;
        sanitize.savedBytes = 0;
        sanitize.saveFailure = null;
    }

    // ---- preview ----------------------------------------------------------------------------

    /** Runs the preview in the sandbox (again, if hidden content is toggled). */
    public void startPreview(final boolean reveal) {
        if (phase != Phase.READY || preview.running) {
            return;
        }
        final int ep = epoch;
        final long d = docHandle();
        final long oldOutcome = preview.outcome;
        resetPreview();
        final int pep = previewEpoch;
        preview.running = true;
        preview.reveal = reveal;
        changed();
        worker.execute(guarded(new Runnable() {
            @Override
            public void run() {
                if (oldOutcome > 0) {
                    Native.outcomeFree(oldOutcome);
                }
                long j = Native.jobNew();
                setJob(ep, j);
                long outcome = Native.run(j, d, Native.OP_PREVIEW, reveal ? Native.PREVIEW_REVEAL_HIDDEN : 0);
                clearJob(ep, j);
                Native.jobFree(j);
                Failure f = null;
                Model.PreviewMeta meta = null;
                if (outcome <= 0) {
                    f = new Failure((int) -outcome, "run");
                    outcome = 0;
                } else {
                    int status = Native.outcomeStatus(outcome);
                    if (status != Native.OK) {
                        f = new Failure(status, describe(outcome));
                        Native.outcomeFree(outcome);
                        outcome = 0;
                    } else {
                        try {
                            byte[] json = Native.previewMeta(outcome);
                            if (json == null) {
                                throw new IllegalStateException("no preview");
                            }
                            meta = Model.parsePreviewMeta(json);
                        } catch (RuntimeException | OutOfMemoryError e) {
                            f = new Failure(F_PARSE, e.toString());
                            Native.outcomeFree(outcome);
                            outcome = 0;
                        }
                    }
                }
                final long out = outcome;
                final Failure failed = f;
                final Model.PreviewMeta parsed = meta;
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (!current(ep) || pep != previewEpoch) {
                            if (out > 0) {
                                final long stale = out;
                                worker.execute(new Runnable() {
                                    @Override
                                    public void run() {
                                        Native.outcomeFree(stale);
                                    }
                                });
                            }
                            return;
                        }
                        preview.running = false;
                        preview.outcome = out;
                        preview.meta = parsed;
                        preview.cancelled = failed != null && failed.status == Native.CANCELLED;
                        preview.failure = preview.cancelled ? null : failed;
                        changed();
                    }
                });
            }
        }, new Recovery() {
            @Override
            public void recover(final Throwable t) {
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (current(ep) && pep == previewEpoch) {
                            preview.running = false;
                            preview.failure = new Failure(F_PARSE, t.toString());
                            changed();
                        }
                    }
                });
            }
        }));
    }

    private void resetPage() {
        page.running = false;
        page.drawing = null;
        page.failure = null;
    }

    /** Forgets the drawn page (when the page view is left). */
    public void closePage() {
        if (page.running) {
            cancel();
        }
        previewEpoch++;
        resetPage();
    }

    /** Draws page {@code index} (zero-based) of the document in the sandbox. */
    public void renderPage(final int index) {
        if (phase != Phase.READY || page.running) {
            return;
        }
        final int ep = epoch;
        final long d = docHandle();
        resetPage();
        final int pep = ++previewEpoch;
        page.running = true;
        page.index = index;
        changed();
        worker.execute(guarded(new Runnable() {
            @Override
            public void run() {
                long j = Native.jobNew();
                setJob(ep, j);
                long outcome = Native.run(j, d, Native.OP_RENDER, index);
                clearJob(ep, j);
                Native.jobFree(j);
                Failure f = null;
                org.arca.viewer.core.PageDrawing drawing = null;
                if (outcome <= 0) {
                    f = new Failure((int) -outcome, "run");
                } else {
                    int status = Native.outcomeStatus(outcome);
                    if (status != Native.OK) {
                        f = new Failure(status, describe(outcome));
                    } else {
                        try {
                            byte[] json = Native.page(outcome);
                            if (json == null) {
                                throw new IllegalStateException("no page");
                            }
                            drawing = org.arca.viewer.core.PageDrawing.parse(json);
                        } catch (RuntimeException | OutOfMemoryError e) {
                            f = new Failure(F_PARSE, e.toString());
                        }
                    }
                    Native.outcomeFree(outcome);
                }
                final Failure failed = f;
                final org.arca.viewer.core.PageDrawing shown = drawing;
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (!current(ep) || pep != previewEpoch) {
                            return;
                        }
                        page.running = false;
                        page.drawing = shown;
                        boolean cancelled = failed != null && failed.status == Native.CANCELLED;
                        page.failure = cancelled ? null : failed;
                        changed();
                    }
                });
            }
        }, new Recovery() {
            @Override
            public void recover(final Throwable t) {
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (current(ep) && pep == previewEpoch) {
                            page.running = false;
                            page.failure = new Failure(F_PARSE, t.toString());
                            changed();
                        }
                    }
                });
            }
        }));
    }

    /** Loads one page of preview items (called on the page executor, blocking). */
    public List<Model.Item> loadItems(long outcome, int section, long from, int count) {
        try {
            byte[] json = Native.previewItems(outcome, section, from, count);
            if (json == null) {
                throw new IllegalStateException("page not available");
            }
            return Model.parseItems(json);
        } catch (OutOfMemoryError e) {
            // An Error would skip the callers' Exception handlers and leave the page "loading".
            throw new IllegalStateException("out of memory", e);
        }
    }

    /** The native outcome handle of the current preview (for page loading), or 0. */
    public long previewOutcome() {
        return preview.outcome;
    }

    /** Forgets the preview (when its screen is left). */
    public void closePreview() {
        if (preview.running) {
            cancel();
        }
        final long po = preview.outcome;
        resetPreview();
        if (po > 0) {
            worker.execute(new Runnable() {
                @Override
                public void run() {
                    Native.outcomeFree(po);
                }
            });
        }
    }

    // ---- pictures ---------------------------------------------------------------------------

    /** A picture that was loaded, or the reason it was not. */
    public static final class ImageResult {
        /** The picture (reduced and upright), or {@code null} if it could not be loaded. */
        public final android.graphics.Bitmap bitmap;
        public final ImageFacts facts;
        public final Failure failure;

        ImageResult(android.graphics.Bitmap bitmap, ImageFacts facts, Failure failure) {
            this.bitmap = bitmap;
            this.facts = facts;
            this.failure = failure;
        }
    }

    /** Told on the main thread when a requested picture has been loaded, or has failed. */
    public interface ImageCallback {
        void onImage(int id, ImageResult result);
    }

    /** Most pictures kept, and the most bytes of pixels they may hold together (a 4096 x 4096 one is 64 MiB). */
    private static final int IMAGE_CACHE = 3;
    private static final long IMAGE_CACHE_BYTES = 48L << 20;
    /** Largest side of a picture the app accepts from the engine. */
    private static final int MAX_IMAGE_SIDE = 4096;

    private final ExecutorService imager = Executors.newSingleThreadExecutor(named("arca-images"));
    private final java.util.LinkedHashMap<Integer, ImageResult> images =
        new java.util.LinkedHashMap<>(8, 0.75f, true);
    private final java.util.Map<Integer, List<ImageCallback>> imageWaiting = new java.util.HashMap<>();
    private final java.util.Set<Long> imageJobs = new java.util.HashSet<>();

    /** Most pictures that may wait for the sandbox at once (the one being decoded included). */
    static final int MAX_IMAGE_PENDING = 4;

    /** A request for a picture that has not finished. Guarded by {@code lock} where it says so. */
    private static final class ImageTask {
        final int id;
        /** The worker has begun to decode it (it can then only be stopped through its job). */
        boolean started;
        /** Dropped while it was still queued: the worker skips it. */
        boolean cancelled;

        ImageTask(int id) {
            this.id = id;
        }
    }

    /** Requests that have not finished, oldest first (main thread). */
    private final java.util.LinkedHashMap<Integer, ImageTask> imageTasks = new java.util.LinkedHashMap<>();

    /** Bytes of pixels held by the cached pictures (main thread). */
    private long imageBytes() {
        long total = 0;
        for (ImageResult r : images.values()) {
            if (r.bitmap != null) {
                total += r.bitmap.getByteCount();
            }
        }
        return total;
    }

    /** A picture that is already loaded, or {@code null} (main thread). */
    public ImageResult cachedImage(int id) {
        return images.get(id);
    }

    /** Whether picture {@code id} is being loaded right now (main thread). */
    public boolean imageLoading(int id) {
        return imageWaiting.containsKey(id);
    }

    /**
     * Decodes picture {@code id} of the open document in the sandbox and reports back on the main
     * thread. Nothing of the picture is decoded before this is asked for, and the app's own image
     * decoders never see the document's bytes: what comes back is a buffer of pixels that was
     * built inside the sandbox and is checked here before it becomes a {@code Bitmap}.
     */
    public void loadImage(final int id, ImageCallback callback) {
        ImageResult have = images.get(id);
        if (have != null) {
            callback.onImage(id, have);
            return;
        }
        List<ImageCallback> waiting = imageWaiting.get(id);
        if (waiting != null) {
            waiting.add(callback);
            return;
        }
        if (phase != Phase.READY) {
            return;
        }
        waiting = new ArrayList<>();
        waiting.add(callback);
        imageWaiting.put(id, waiting);
        if (imageTasks.containsKey(id)) {
            // Asked for again while it is being decoded (the row that asked first went away): the result is delivered to the new waiter.
            return;
        }
        makeRoomForImage();
        final ImageTask task = new ImageTask(id);
        imageTasks.put(id, task);
        final int ep = epoch;
        final long d = docHandle();
        imager.execute(guarded(new Runnable() {
            @Override
            public void run() {
                synchronized (lock) {
                    if (task.cancelled) {
                        return;
                    }
                    task.started = true;
                }
                long j = Native.jobNew();
                synchronized (lock) {
                    if (ep == epoch) {
                        imageJobs.add(j);
                    }
                }
                long outcome = Native.run(j, d, Native.OP_IMAGE, id);
                synchronized (lock) {
                    imageJobs.remove(j);
                }
                Native.jobFree(j);
                Failure f = null;
                android.graphics.Bitmap bitmap = null;
                ImageFacts facts = null;
                if (outcome <= 0) {
                    f = new Failure((int) -outcome, "run");
                } else {
                    int status = Native.outcomeStatus(outcome);
                    if (status != Native.OK) {
                        f = new Failure(status, describe(outcome));
                    } else {
                        try {
                            byte[] info = Native.imageInfo(outcome);
                            byte[] pixels = Native.imagePixels(outcome);
                            if (info == null || pixels == null) {
                                throw new IllegalStateException("no picture");
                            }
                            facts = ImageFacts.parse(info);
                            bitmap = toBitmap(facts, pixels);
                        } catch (RuntimeException | OutOfMemoryError e) {
                            f = new Failure(F_PARSE, e.toString());
                        }
                    }
                    Native.outcomeFree(outcome);
                }
                final ImageResult result = new ImageResult(bitmap, facts, f);
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (!current(ep)) {
                            return;
                        }
                        if (imageTasks.get(id) == task) {
                            imageTasks.remove(id);
                        }
                        List<ImageCallback> callbacks = imageWaiting.remove(id);
                        boolean cancelled = result.failure != null && result.failure.status == Native.CANCELLED;
                        if (result.bitmap != null) {
                            images.put(id, result);
                            // Least recently used go first; a picture over the byte budget on its own is shown but not kept.
                            while (!images.isEmpty() && (images.size() > IMAGE_CACHE || imageBytes() > IMAGE_CACHE_BYTES)) {
                                java.util.Iterator<Integer> it = images.keySet().iterator();
                                it.next();
                                it.remove();
                            }
                        }
                        if (callbacks != null && !cancelled) {
                            for (ImageCallback cb : callbacks) {
                                cb.onImage(id, result);
                            }
                        }
                    }
                });
            }
        }, new Recovery() {
            @Override
            public void recover(Throwable t) {
                final ImageResult result = new ImageResult(null, null, new Failure(F_PARSE, t.toString()));
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (!current(ep)) {
                            return;
                        }
                        if (imageTasks.get(id) == task) {
                            imageTasks.remove(id);
                        }
                        List<ImageCallback> callbacks = imageWaiting.remove(id);
                        if (callbacks != null) {
                            for (ImageCallback cb : callbacks) {
                                cb.onImage(id, result);
                            }
                        }
                    }
                });
            }
        }));
    }

    /**
     * Builds the bitmap from the pixels the engine sent. The engine is not trusted: the size must
     * be sane and the buffer must have exactly that many pixels.
     */
    private static android.graphics.Bitmap toBitmap(ImageFacts facts, byte[] pixels) {
        int w = facts.width;
        int h = facts.height;
        if (w < 1 || h < 1 || w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE || (long) w * h * 4 != pixels.length) {
            throw new IllegalStateException("the picture does not match its size");
        }
        android.graphics.Bitmap bitmap = android.graphics.Bitmap.createBitmap(w, h, android.graphics.Bitmap.Config.ARGB_8888);
        bitmap.copyPixelsFromBuffer(java.nio.ByteBuffer.wrap(pixels));
        return bitmap;
    }

    /**
     * Makes room for one more request: while {@link #MAX_IMAGE_PENDING} are pending, the oldest one
     * that the worker has not begun is dropped (its waiters are told it was cancelled and may ask
     * again). What the reader is looking at is asked for last, so it is what stays.
     */
    private void makeRoomForImage() {
        while (imageTasks.size() >= MAX_IMAGE_PENDING) {
            ImageTask oldest = null;
            synchronized (lock) {
                for (ImageTask t : imageTasks.values()) {
                    if (!t.started) {
                        oldest = t;
                        break;
                    }
                }
                if (oldest != null) {
                    oldest.cancelled = true;
                }
            }
            if (oldest == null) {
                return;
            }
            imageTasks.remove(oldest.id);
            List<ImageCallback> callbacks = imageWaiting.remove(oldest.id);
            if (callbacks != null) {
                ImageResult r = new ImageResult(null, null, new Failure(Native.CANCELLED, "queue"));
                for (ImageCallback cb : callbacks) {
                    cb.onImage(oldest.id, r);
                }
            }
        }
    }

    /**
     * Withdraws a request made with {@link #loadImage} (the row that asked for the picture went
     * away). A request nobody waits for any more is dropped while it is still queued; one that is
     * being decoded finishes and its picture is kept. Returns whether the callback was waiting.
     */
    public boolean cancelImage(int id, ImageCallback callback) {
        List<ImageCallback> waiting = imageWaiting.get(id);
        if (waiting == null || !waiting.remove(callback)) {
            return false;
        }
        if (waiting.isEmpty()) {
            imageWaiting.remove(id);
            ImageTask t = imageTasks.get(id);
            if (t != null) {
                synchronized (lock) {
                    if (!t.started) {
                        t.cancelled = true;
                        imageTasks.remove(id);
                    }
                }
            }
        }
        return true;
    }

    /** Stops the pictures that are being loaded. */
    public void cancelImages() {
        long[] running;
        synchronized (lock) {
            running = new long[imageJobs.size()];
            int i = 0;
            for (long j : imageJobs) {
                running[i++] = j;
            }
        }
        for (long j : running) {
            Native.jobCancel(j);
        }
    }

    private void resetImages() {
        synchronized (lock) {
            for (ImageTask t : imageTasks.values()) {
                if (!t.started) {
                    t.cancelled = true;
                }
            }
        }
        imageTasks.clear();
        cancelImages();
        images.clear();
        imageWaiting.clear();
    }

    // ---- sanitize ---------------------------------------------------------------------------

    /** Builds the sterile copy with the given {@code Native.SANITIZE_*} options. */
    public void startSanitize(final int flags) {
        if (phase != Phase.READY || sanitize.running) {
            return;
        }
        final int ep = epoch;
        final long d = docHandle();
        final long oldOutcome = sanitize.outcome;
        resetSanitize();
        final int sep = sanitizeEpoch;
        sanitize.running = true;
        sanitize.flags = flags;
        changed();
        worker.execute(guarded(new Runnable() {
            @Override
            public void run() {
                if (oldOutcome > 0) {
                    Native.outcomeFree(oldOutcome);
                }
                long j = Native.jobNew();
                setJob(ep, j);
                long outcome = Native.run(j, d, Native.OP_SANITIZE, flags);
                clearJob(ep, j);
                Native.jobFree(j);
                Failure f = null;
                Model.SanitizedInfo info = null;
                if (outcome <= 0) {
                    f = new Failure((int) -outcome, "run");
                    outcome = 0;
                } else {
                    int status = Native.outcomeStatus(outcome);
                    if (status != Native.OK) {
                        f = new Failure(status, describe(outcome));
                        Native.outcomeFree(outcome);
                        outcome = 0;
                    } else {
                        try {
                            byte[] json = Native.sanitizedInfo(outcome);
                            if (json == null) {
                                throw new IllegalStateException("no description");
                            }
                            info = Model.parseSanitized(json);
                        } catch (RuntimeException | OutOfMemoryError e) {
                            f = new Failure(F_PARSE, e.toString());
                            Native.outcomeFree(outcome);
                            outcome = 0;
                        }
                    }
                }
                final long out = outcome;
                final Failure failed = f;
                final Model.SanitizedInfo parsed = info;
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (!current(ep) || sep != sanitizeEpoch) {
                            if (out > 0) {
                                worker.execute(new Runnable() {
                                    @Override
                                    public void run() {
                                        Native.outcomeFree(out);
                                    }
                                });
                            }
                            return;
                        }
                        sanitize.running = false;
                        sanitize.outcome = out;
                        sanitize.info = parsed;
                        sanitize.cancelled = failed != null && failed.status == Native.CANCELLED;
                        sanitize.failure = sanitize.cancelled ? null : failed;
                        changed();
                    }
                });
            }
        }, new Recovery() {
            @Override
            public void recover(final Throwable t) {
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (current(ep) && sep == sanitizeEpoch) {
                            sanitize.running = false;
                            sanitize.failure = new Failure(F_PARSE, t.toString());
                            changed();
                        }
                    }
                });
            }
        }));
    }

    /** Writes the sterile copy to {@code dest} (a document the user chose with the system picker). */
    public void saveSanitized(Context context, final Uri dest) {
        if (!sanitize.ready() || sanitize.saving) {
            return;
        }
        final Context app = context.getApplicationContext();
        final int ep = epoch;
        final long outcome = sanitize.outcome;
        final int sep = sanitizeEpoch;
        sanitize.saving = true;
        sanitize.saved = false;
        sanitize.saveFailure = null;
        changed();
        worker.execute(guarded(new Runnable() {
            @Override
            public void run() {
                long written = -Native.IO;
                String technical = "";
                try {
                    // "wt": create or truncate; plain "w" may leave old bytes behind on some providers.
                    ParcelFileDescriptor pfd = app.getContentResolver().openFileDescriptor(dest, "wt");
                    if (pfd == null) {
                        technical = "the provider returned no descriptor";
                    } else {
                        written = Native.sanitizedWriteFd(outcome, pfd.detachFd());
                    }
                } catch (FileNotFoundException | SecurityException | IllegalArgumentException e) {
                    technical = e.toString();
                } catch (RuntimeException e) {
                    technical = e.toString();
                }
                final long bytes = written;
                final String why = technical;
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (!current(ep) || sep != sanitizeEpoch) {
                            return;
                        }
                        sanitize.saving = false;
                        if (bytes >= 0) {
                            sanitize.saved = true;
                            sanitize.savedBytes = bytes;
                            Stats.recordSterileCopy(app);
                        } else {
                            sanitize.saveFailure = new Failure((int) -bytes, why);
                        }
                        changed();
                    }
                });
            }
        }, new Recovery() {
            @Override
            public void recover(final Throwable t) {
                post(new Runnable() {
                    @Override
                    public void run() {
                        if (current(ep) && sep == sanitizeEpoch) {
                            sanitize.saving = false;
                            sanitize.saveFailure = new Failure(Native.IO, t.toString());
                            changed();
                        }
                    }
                });
            }
        }));
    }

    /** Forgets the sterile copy (when its screen is left). */
    public void closeSanitize() {
        if (sanitize.running) {
            cancel();
        }
        final long so = sanitize.outcome;
        resetSanitize();
        if (so > 0) {
            worker.execute(new Runnable() {
                @Override
                public void run() {
                    Native.outcomeFree(so);
                }
            });
        }
    }

    // ---- job bookkeeping --------------------------------------------------------------------

    private void setJob(int ep, long j) {
        synchronized (lock) {
            if (ep == epoch) {
                job = j;
            }
        }
    }

    private void clearJob(int ep, long j) {
        synchronized (lock) {
            if (job == j) {
                job = 0;
            }
        }
    }
}
