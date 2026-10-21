// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.Executor;

/**
 * The headings of a document (its contents), collected in the background by paging through the
 * preview: titles and heading paragraphs of every section, with the place of each.
 *
 * <p>No Android dependencies; tested on a desktop JVM.
 */
public final class DocOutline extends PageScanner {
    /** Headings kept; further ones are not listed. */
    public static final int MAX_ENTRIES = 2_000;
    /** Longest heading text kept. */
    public static final int MAX_TEXT = 120;

    /** One heading. */
    public static final class Entry {
        public final int section;
        public final long item;
        /** 0 for a title, 1 to 6 for headings. */
        public final int level;
        public final String text;

        Entry(int section, long item, int level, String text) {
            this.section = section;
            this.item = item;
            this.level = level;
            this.text = text;
        }
    }

    private final List<Entry> entries = new ArrayList<>();
    private boolean capped;

    public DocOutline(long[] sectionItems, Loader loader, Executor worker, Executor ui, Listener listener) {
        super(sectionItems, loader, worker, ui, listener);
    }

    /** The heading level of a paragraph style: 0 title, 1 to 6 headings, -1 anything else. */
    public static int levelOf(String style) {
        if ("Title".equals(style)) {
            return 0;
        }
        if (style != null && style.length() == 8 && style.startsWith("Heading")) {
            int n = style.charAt(7) - '0';
            return n >= 1 && n <= 6 ? n : -1;
        }
        return -1;
    }

    @Override
    protected Object examine(Model.Item item) {
        if (!(item instanceof Model.ParaItem)) {
            return null;
        }
        Model.Para p = ((Model.ParaItem) item).para;
        int level = levelOf(p.style);
        if (level < 0) {
            return null;
        }
        String text = Fmt.clip(Fmt.safe(p.plain()).replace('\n', ' ').trim(), MAX_TEXT);
        return text.isEmpty() ? null : new Object[] {Integer.valueOf(level), text};
    }

    @Override
    protected boolean accept(int section, long item, Object found) {
        if (entries.size() >= MAX_ENTRIES) {
            capped = true;
            return false;
        }
        Object[] f = (Object[]) found;
        entries.add(new Entry(section, item, (Integer) f[0], (String) f[1]));
        return true;
    }

    /** The headings found so far, in document order. */
    public List<Entry> entries() {
        return Collections.unmodifiableList(entries);
    }

    /** There were more headings than {@link #MAX_ENTRIES}. */
    public boolean capped() {
        return capped;
    }
}
