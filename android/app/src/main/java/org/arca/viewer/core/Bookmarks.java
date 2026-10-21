// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Places the reader marked in the document that is open. They live in memory only and are
 * dropped with the document: nothing about a document is ever written to the device.
 *
 * <p>No Android dependencies; tested on a desktop JVM.
 */
public final class Bookmarks {
    /** Most bookmarks kept. */
    public static final int MAX = 30;
    /** Longest label. */
    public static final int MAX_LABEL = 80;

    /** One marked place. */
    public static final class Mark {
        public final int section;
        public final long item;
        public final String label;

        Mark(int section, long item, String label) {
            this.section = section;
            this.item = item;
            this.label = label;
        }
    }

    private final List<Mark> marks = new ArrayList<>();

    /**
     * Marks a place. Marking a place that is already marked renews its label.
     *
     * @return {@code false} if there is no room for a new one
     */
    public boolean add(int section, long item, String label) {
        String text = Fmt.clip(Fmt.safe(label).replace('\n', ' ').trim(), MAX_LABEL);
        for (int i = 0; i < marks.size(); i++) {
            Mark m = marks.get(i);
            if (m.section == section && m.item == item) {
                marks.set(i, new Mark(section, item, text));
                return true;
            }
        }
        if (marks.size() >= MAX) {
            return false;
        }
        marks.add(new Mark(section, item, text));
        sort();
        return true;
    }

    private void sort() {
        Collections.sort(marks, new java.util.Comparator<Mark>() {
            @Override
            public int compare(Mark a, Mark b) {
                if (a.section != b.section) {
                    return a.section < b.section ? -1 : 1;
                }
                return a.item < b.item ? -1 : a.item > b.item ? 1 : 0;
            }
        });
    }

    /** Whether the place is marked. */
    public boolean has(int section, long item) {
        for (Mark m : marks) {
            if (m.section == section && m.item == item) {
                return true;
            }
        }
        return false;
    }

    /** Removes the bookmark at position {@code index} of {@link #list()}. */
    public void remove(int index) {
        if (index >= 0 && index < marks.size()) {
            marks.remove(index);
        }
    }

    /** The bookmarks in document order. */
    public List<Mark> list() {
        return Collections.unmodifiableList(marks);
    }

    public int size() {
        return marks.size();
    }

    public void clear() {
        marks.clear();
    }
}
