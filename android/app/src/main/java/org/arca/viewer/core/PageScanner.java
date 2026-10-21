// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.Executor;

/**
 * A background walk over every item of a preview, one page per task.
 *
 * <p>The preview is only ever held a page at a time, so anything that needs the whole document
 * (search, the list of headings) pages through it itself. Each page is one task on the worker
 * queue, so the tasks interleave with the page loads of scrolling and the list stays responsive.
 * A scan can be cancelled at any time; after that no callback is made.
 *
 * <p>Subclasses look at each item on the worker thread ({@link #examine}) and collect what they
 * found on the UI thread ({@link #accept}). No Android dependencies; tested on a desktop JVM.
 */
public abstract class PageScanner {
    /** Fetches items; runs on the worker, may block. */
    public interface Loader {
        /** At most {@code count} items of {@code section} starting at item {@code from}. */
        List<Model.Item> load(int section, long from, int count) throws Exception;
    }

    /** Told (on the UI executor) when something was collected or the scan ended. */
    public interface Listener {
        void onChanged();
    }

    private final long[] sectionItems;
    private final Loader loader;
    private final Executor worker;
    private final Executor ui;
    private final Listener listener;
    private final long totalItems;
    private boolean enabled = true;

    private long scanned;
    private boolean done;
    private boolean failed;
    private volatile boolean cancelled;

    /**
     * @param sectionItems the number of items of each section
     */
    protected PageScanner(long[] sectionItems, Loader loader, Executor worker, Executor ui, Listener listener) {
        this.sectionItems = sectionItems.clone();
        this.loader = loader;
        this.worker = worker;
        this.ui = ui;
        this.listener = listener;
        long sum = 0;
        for (long n : sectionItems) {
            sum += Math.max(0, n);
        }
        this.totalItems = sum;
    }

    /** Looks at one item (worker thread). Returns what to collect, or {@code null} for nothing. */
    protected abstract Object examine(Model.Item item);

    /**
     * Collects what {@link #examine} returned for item {@code item} of {@code section} (UI thread).
     * Return {@code false} to end the scan.
     */
    protected abstract boolean accept(int section, long item, Object found);

    /** Lets a subclass that has nothing to look for finish at once. */
    protected final void disable() {
        enabled = false;
    }

    /** Starts the scan. Call once, on the UI thread. */
    public final void start() {
        int first = 0;
        while (first < sectionItems.length && sectionItems[first] <= 0) {
            first++;
        }
        if (!enabled || first >= sectionItems.length) {
            done = true;
            return;
        }
        step(first, 0);
    }

    /** Stops the scan; no further callbacks are made. */
    public final void cancel() {
        cancelled = true;
    }

    private void step(final int section, final long from) {
        worker.execute(new Runnable() {
            @Override
            public void run() {
                if (cancelled) {
                    return;
                }
                List<Model.Item> items = null;
                boolean error = false;
                long count = Math.min(PageCache.PAGE, sectionItems[section] - from);
                try {
                    items = loader.load(section, from, (int) count);
                } catch (Exception e) {
                    error = true;
                }
                final List<Object[]> found = new ArrayList<>();
                if (items != null) {
                    for (int i = 0; i < items.size(); i++) {
                        Object what = examine(items.get(i));
                        if (what != null) {
                            found.add(new Object[] {Integer.valueOf(i), what});
                        }
                    }
                }
                final long taken = items == null ? count : Math.max(items.size(), 1);
                final boolean broke = error || items == null;
                ui.execute(new Runnable() {
                    @Override
                    public void run() {
                        if (cancelled) {
                            return;
                        }
                        boolean more = true;
                        for (Object[] f : found) {
                            more &= accept(section, from + (Integer) f[0], f[1]);
                        }
                        scanned += taken;
                        if (broke) {
                            failed = true;
                        }
                        // Next page: this section, the next one, or the end.
                        long nextFrom = from + taken;
                        int nextSection = section;
                        while (nextSection < sectionItems.length && nextFrom >= sectionItems[nextSection]) {
                            nextSection++;
                            nextFrom = 0;
                        }
                        if (broke || nextSection >= sectionItems.length || !more) {
                            done = true;
                        }
                        listener.onChanged();
                        if (!done) {
                            step(nextSection, nextFrom);
                        }
                    }
                });
            }
        });
    }

    /** The scan has finished (or was stopped by the subclass or a failure). */
    public final boolean done() {
        return done;
    }

    /** A page could not be read; what was collected before it is still valid. */
    public final boolean failed() {
        return failed;
    }

    /** Items looked at so far. */
    public final long scanned() {
        return scanned;
    }

    /** Items to look at. */
    public final long total() {
        return totalItems;
    }
}
