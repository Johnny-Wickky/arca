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
import android.provider.OpenableColumns;

import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Model;

import java.io.FileNotFoundException;
import java.util.ArrayList;
import java.util.List;
import java.util.TimeZone;
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
    private Uri uri;
    private Failure failure;
    private Model.Report report;
    private long doc;
    private long job;
    private final PreviewState preview = new PreviewState();
    private final SanitizeState sanitize = new SanitizeState();

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

    public Uri uri() {
        return uri;
    }

    public Failure failure() {
        return failure;
    }

    public Model.Report report() {
        return report;
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
        uri = source;
        mime = mimeType == null ? "" : mimeType;
        name = "";
        size = -1;
        failure = null;
        report = null;
        changed();
        worker.execute(new Runnable() {
            @Override
            public void run() {
                load(app, source, ep);
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
        final Model.Report parsed;
        try {
            byte[] json = Native.report(outcome);
            if (json == null) {
                fail(ep, F_PARSE, "no report");
                return;
            }
            parsed = Model.parseReport(json);
        } catch (RuntimeException e) {
            fail(ep, F_PARSE, e.toString());
            return;
        } finally {
            Native.outcomeFree(outcome);
        }
        recordPassport(app, parsed);
        post(new Runnable() {
            @Override
            public void run() {
                if (!current(ep)) {
                    return;
                }
                report = parsed;
                phase = Phase.READY;
                changed();
            }
        });
    }

    /** Counts the document in the Patron Passport (local statistics only). */
    private void recordPassport(Context app, Model.Report r) {
        try {
            int day = Fmt.epochDay(System.currentTimeMillis(), TimeZone.getDefault().getOffset(System.currentTimeMillis()));
            byte[] next = Native.passportUpdate(Prefs.passport(app), day,
                (int) Math.min(r.trackers, Integer.MAX_VALUE), (int) Math.min(r.active, Integer.MAX_VALUE));
            if (next != null && next.length == 36) {
                Prefs.setPassport(app, next);
            }
        } catch (RuntimeException ignored) {
            // statistics are a courtesy; never let them break a scan
        }
    }

    private static String describe(long outcome) {
        try {
            byte[] json = Native.outcomeInfo(outcome);
            return json == null ? "" : Model.parseRunInfo(json).message;
        } catch (RuntimeException e) {
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
        worker.execute(new Runnable() {
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
                        } catch (RuntimeException e) {
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
        });
    }

    /** Loads one page of preview items (called on the page executor, blocking). */
    public List<Model.Item> loadItems(long outcome, int section, long from, int count) {
        byte[] json = Native.previewItems(outcome, section, from, count);
        if (json == null) {
            throw new IllegalStateException("page not available");
        }
        return Model.parseItems(json);
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
        worker.execute(new Runnable() {
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
                        } catch (RuntimeException e) {
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
        });
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
        worker.execute(new Runnable() {
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
                        } else {
                            sanitize.saveFailure = new Failure((int) -bytes, why);
                        }
                        changed();
                    }
                });
            }
        });
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
