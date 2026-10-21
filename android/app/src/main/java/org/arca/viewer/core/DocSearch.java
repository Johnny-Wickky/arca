// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.text.Normalizer;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.Executor;

/**
 * Search in the text of a preview.
 *
 * <p>The preview is only ever held a page at a time, so a search walks the pages itself, in the
 * background and one page per task (the tasks queue behind the page loads of scrolling, so the
 * list stays responsive), and collects the matches as it goes. It can be cancelled at any time.
 *
 * <p>Matching ignores case and diacritics ({@code é} finds {@code e}, {@code Ё} finds {@code е}),
 * and does it one character for one character, so an offset in the folded text is an offset in
 * the original.
 *
 * <p>No Android dependencies; tested on a desktop JVM.
 */
public final class DocSearch {
    /** Matches kept; further ones are counted as "more". */
    public static final int MAX_HITS = 5_000;
    /** Shortest query worth searching for. */
    public static final int MIN_QUERY = 1;
    /** Longest query. */
    public static final int MAX_QUERY = 200;

    /** Fetches items; runs on the worker, may block. */
    public interface Loader {
        /** At most {@code count} items of {@code section} starting at item {@code from}. */
        List<Model.Item> load(int section, long from, int count) throws Exception;
    }

    /** Told (on the UI executor) when matches were added or the search ended. */
    public interface Listener {
        void onChanged();
    }

    private final String needle;
    private final long[] sectionItems;
    private final Loader loader;
    private final Executor worker;
    private final Executor ui;
    private final Listener listener;
    private final long totalItems;

    // Hits, in document order. Touched on the UI executor only.
    private int[] hitSection = new int[64];
    private long[] hitItem = new long[64];
    private int hits;
    private boolean capped;
    private long scanned;
    private boolean done;
    private boolean failed;
    private volatile boolean cancelled;

    /**
     * @param query         what to look for (trimmed by the caller; may be anything)
     * @param sectionItems  the number of items of each section
     */
    public DocSearch(String query, long[] sectionItems, Loader loader, Executor worker, Executor ui,
                     Listener listener) {
        this.needle = fold(clip(query));
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

    private static String clip(String s) {
        if (s == null) {
            return "";
        }
        return s.length() > MAX_QUERY ? s.substring(0, MAX_QUERY) : s;
    }

    /** Whether {@code query} can be searched for at all. */
    public static boolean usable(String query) {
        return query != null && query.trim().length() >= MIN_QUERY;
    }

    /** The folded form of the query, as it is matched. */
    public String needle() {
        return needle;
    }

    // ---- folding and matching ---------------------------------------------------------------

    /** Folded form of every character already folded (0 = not yet; a folded character is never 0). */
    private static final char[] CACHE = new char[0x10000];

    /** Folds one character: lower case, no accent. The result is always one character. */
    static char foldChar(char c) {
        if (c < 0x80) {
            return c >= 'A' && c <= 'Z' ? (char) (c + 32) : c;
        }
        char cached = CACHE[c];
        if (cached != 0) {
            return cached;
        }
        char folded = foldSlow(c);
        CACHE[c] = folded;
        return folded;
    }

    private static char foldSlow(char c) {
        if (c == '\u0451' || c == '\u0401') { // ё Ё
            return '\u0435'; // е
        }
        if (c == '\u0439' || c == '\u0419') { // й Й
            return '\u0439'; // a letter of its own, not "и" with an accent
        }
        if (Character.isSurrogate(c)) {
            return c;
        }
        String d = Normalizer.normalize(String.valueOf(c), Normalizer.Form.NFD);
        char base = d.isEmpty() ? c : d.charAt(0);
        if (Character.getType(base) == Character.NON_SPACING_MARK) {
            base = c;
        }
        return Character.toLowerCase(base);
    }

    /** Folds {@code s} character by character, so offsets in the result are offsets in {@code s}. */
    public static String fold(String s) {
        if (s == null || s.isEmpty()) {
            return "";
        }
        char[] out = new char[s.length()];
        for (int i = 0; i < out.length; i++) {
            out[i] = foldChar(s.charAt(i));
        }
        return new String(out);
    }

    /**
     * Start offsets of the (non-overlapping) matches of the folded {@code needle} in {@code text}.
     * An empty needle matches nothing.
     */
    public static int[] occurrences(String text, String needle) {
        if (needle == null || needle.isEmpty() || text == null || text.length() < needle.length()) {
            return new int[0];
        }
        String hay = fold(text);
        int[] found = new int[4];
        int n = 0;
        int from = 0;
        while (true) {
            int at = hay.indexOf(needle, from);
            if (at < 0) {
                break;
            }
            if (n == found.length) {
                found = Arrays.copyOf(found, n * 2);
            }
            found[n++] = at;
            from = at + needle.length();
        }
        return Arrays.copyOf(found, n);
    }

    /** The text of an item as the screen shows it (also what the matches are counted in). */
    public static String textOf(Model.Item item) {
        if (item instanceof Model.ParaItem) {
            return textOf(((Model.ParaItem) item).para);
        }
        if (item instanceof Model.TableRow) {
            StringBuilder sb = new StringBuilder();
            for (Model.Cell c : ((Model.TableRow) item).cells) {
                if (sb.length() > 0) {
                    sb.append('\t');
                }
                for (Model.Para p : c.paras) {
                    if (sb.length() > 0 && sb.charAt(sb.length() - 1) != '\t') {
                        sb.append('\n');
                    }
                    sb.append(textOf(p));
                }
            }
            return sb.toString();
        }
        if (item instanceof Model.GridRow) {
            StringBuilder sb = new StringBuilder();
            for (Model.GridCell c : ((Model.GridRow) item).cells) {
                if (sb.length() > 0) {
                    sb.append('\t');
                }
                sb.append(Fmt.safe(c.text));
            }
            return sb.toString();
        }
        if (item instanceof Model.ImageItem) {
            return Fmt.safe(((Model.ImageItem) item).alt);
        }
        return "";
    }

    private static String textOf(Model.Para p) {
        StringBuilder sb = new StringBuilder();
        if (!p.marker.isEmpty()) {
            sb.append(Fmt.safe(p.marker)).append(' ');
        }
        for (Model.Run r : p.runs) {
            sb.append(Fmt.safe(r.text));
        }
        return sb.toString();
    }

    // ---- running ----------------------------------------------------------------------------

    /** Starts the background scan. Call once, on the UI thread. */
    public void start() {
        int first = 0;
        while (first < sectionItems.length && sectionItems[first] <= 0) {
            first++;
        }
        if (needle.isEmpty() || first >= sectionItems.length) {
            done = true;
            return;
        }
        step(first, 0);
    }

    /** Stops the scan; no further callbacks are made. */
    public void cancel() {
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
                final List<int[]> found = new ArrayList<>();
                if (items != null) {
                    for (int i = 0; i < items.size(); i++) {
                        int[] at = occurrences(textOf(items.get(i)), needle);
                        if (at.length > 0) {
                            found.add(new int[] {i, at.length});
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
                        for (int[] f : found) {
                            for (int k = 0; k < f[1]; k++) {
                                add(section, from + f[0]);
                            }
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
                        if (broke || nextSection >= sectionItems.length || capped) {
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

    private void add(int section, long item) {
        if (hits >= MAX_HITS) {
            capped = true;
            return;
        }
        if (hits == hitSection.length) {
            hitSection = Arrays.copyOf(hitSection, hits * 2);
            hitItem = Arrays.copyOf(hitItem, hits * 2);
        }
        hitSection[hits] = section;
        hitItem[hits] = item;
        hits++;
    }

    // ---- results (UI thread) ----------------------------------------------------------------

    /** Matches found so far. */
    public int hitCount() {
        return hits;
    }

    /** Section of match {@code i}. */
    public int hitSection(int i) {
        return hitSection[i];
    }

    /** Item (within its section) of match {@code i}. */
    public long hitItem(int i) {
        return hitItem[i];
    }

    /** More matches exist than {@link #MAX_HITS}. */
    public boolean capped() {
        return capped;
    }

    /** The scan has finished (or was stopped by the cap or a failure). */
    public boolean done() {
        return done;
    }

    /** A page could not be read; the matches found before it are still valid. */
    public boolean failed() {
        return failed;
    }

    /** Items looked at so far. */
    public long scanned() {
        return scanned;
    }

    /** Items to look at. */
    public long total() {
        return totalItems;
    }

    /** Index of the first match at or after the given place, or -1 if there is none yet. */
    public int firstAtOrAfter(int section, long item) {
        int lo = 0;
        int hi = hits;
        while (lo < hi) {
            int mid = (lo + hi) >>> 1;
            if (hitSection[mid] < section || (hitSection[mid] == section && hitItem[mid] < item)) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        return lo < hits ? lo : -1;
    }

    /** Whether item {@code item} of {@code section} holds the match {@code i}. */
    public boolean isHit(int i, int section, long item) {
        return i >= 0 && i < hits && hitSection[i] == section && hitItem[i] == item;
    }
}
