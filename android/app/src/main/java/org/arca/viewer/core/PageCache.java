// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.Executor;

/**
 * Pages of preview items, loaded on demand and kept in a small LRU cache, so that a
 * spreadsheet with a million rows costs memory only for the rows around the scroll position.
 *
 * <p>All methods except the {@link Loader} callback run on the UI thread; loading runs on
 * {@code worker}, results are delivered through {@code ui}. No Android dependencies.
 */
public final class PageCache {
    /** Items per page. */
    public static final int PAGE = 64;
    /** Pages kept. */
    private static final int MAX_PAGES = 40;

    /** Fetches items; runs on the worker, may block. */
    public interface Loader {
        /** At most {@code count} items of {@code section} starting at item {@code from}. */
        List<Model.Item> load(int section, long from, int count) throws Exception;
    }

    /** Told when a page arrived (or failed). Called on the UI executor. */
    public interface Listener {
        void onChanged();

        void onFailed(Exception e);
    }

    private static final class Key {
        final int section;
        final long page;

        Key(int section, long page) {
            this.section = section;
            this.page = page;
        }

        @Override
        public boolean equals(Object o) {
            return o instanceof Key && ((Key) o).section == section && ((Key) o).page == page;
        }

        @Override
        public int hashCode() {
            return 31 * section + Long.hashCode(page);
        }
    }

    private static final Model.Item PENDING = new Model.Pending();

    private final Loader loader;
    private final Executor worker;
    private final Executor ui;
    private final Listener listener;
    private final Map<Key, List<Model.Item>> pages = new LinkedHashMap<Key, List<Model.Item>>(16, 0.75f, true) {
        private static final long serialVersionUID = 1L;

        @Override
        protected boolean removeEldestEntry(Map.Entry<Key, List<Model.Item>> eldest) {
            return size() > MAX_PAGES;
        }
    };
    private final Set<Key> loading = new HashSet<>();
    private final Set<Key> failed = new HashSet<>();
    private int generation;

    public PageCache(Loader loader, Executor worker, Executor ui, Listener listener) {
        this.loader = loader;
        this.worker = worker;
        this.ui = ui;
        this.listener = listener;
    }

    /**
     * The item at {@code index} of {@code section}, or a {@link Model.Pending} placeholder
     * while its page is loading. The neighbouring page in the direction of travel is
     * requested ahead of time.
     */
    public Model.Item get(int section, long index, long total) {
        if (index < 0 || index >= total) {
            return PENDING;
        }
        long page = index / PAGE;
        Key key = new Key(section, page);
        List<Model.Item> items = pages.get(key);
        if (items == null) {
            request(key, total);
            return PENDING;
        }
        int offset = (int) (index - page * PAGE);
        // Read ahead one page either way when the position is near an edge.
        if (offset > PAGE - 12) {
            prefetch(new Key(section, page + 1), total);
        } else if (offset < 12 && page > 0) {
            prefetch(new Key(section, page - 1), total);
        }
        return offset < items.size() ? items.get(offset) : PENDING;
    }

    private void prefetch(Key key, long total) {
        if (key.page * PAGE < total && !pages.containsKey(key)) {
            request(key, total);
        }
    }

    private void request(final Key key, long total) {
        if (loading.contains(key) || failed.contains(key)) {
            return;
        }
        loading.add(key);
        final int gen = generation;
        final long from = key.page * PAGE;
        final int count = (int) Math.min(PAGE, total - from);
        worker.execute(new Runnable() {
            @Override
            public void run() {
                List<Model.Item> result = null;
                Exception error = null;
                try {
                    result = loader.load(key.section, from, count);
                } catch (Exception e) {
                    error = e;
                }
                final List<Model.Item> done = result;
                final Exception err = error;
                ui.execute(new Runnable() {
                    @Override
                    public void run() {
                        if (gen != generation) {
                            return; // the document was replaced meanwhile
                        }
                        loading.remove(key);
                        if (err != null || done == null) {
                            failed.add(key);
                            listener.onFailed(err != null ? err : new IllegalStateException("no page"));
                            return;
                        }
                        pages.put(key, done);
                        listener.onChanged();
                    }
                });
            }
        });
    }

    /** Forgets everything (a new document or section was chosen). */
    public void clear() {
        generation++;
        pages.clear();
        loading.clear();
        failed.clear();
    }

    /** Number of pages currently held (for tests). */
    public int cachedPages() {
        return pages.size();
    }
}
