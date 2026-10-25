// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.ArrayList;
import java.util.List;

/**
 * The areas of a window that text may use, given the {@link Fold} across it. Pure, so that it
 * can be tested without a device.
 *
 * <p>Two postures, named for what the user holds:
 * <ul>
 * <li>{@link #BOOK}: a vertical fold. The document goes on the reading-side page (left, or right
 *     in a right-to-left locale); the other page carries the controls;</li>
 * <li>{@link #TABLETOP}: a horizontal fold. The document goes on the upper half, the controls on
 *     the lower half.</li>
 * </ul>
 * Whatever the fold, no area reaches within half of {@link Posture#GAP_DP} of it, so no line of
 * text is ever laid under a hinge or across a bend. A fold that does not separate (the device is
 * flat), or that the window does not cross, leaves the window whole ({@link #NONE}).
 *
 * <p>Areas are {@code {left, top, width, height}} in dp, window coordinates, like the fold.
 */
public final class FoldLayout {
    /** The window is used whole. */
    public static final int NONE = 0;
    /** A vertical fold: two pages side by side. */
    public static final int BOOK = 1;
    /** A horizontal fold: upper half and lower half. */
    public static final int TABLETOP = 2;

    /** An area smaller than this (dp, along the split) is not worth using for a document or controls. */
    public static final int MIN_PANE_DP = 200;

    /** Which posture the window is in. */
    public final int kind;
    /** Where the document goes. Never {@code null}. */
    public final int[] primary;
    /** Where the controls go, or {@code null} when there is no room for them (or no fold). */
    public final int[] secondary;

    private FoldLayout(int kind, int[] primary, int[] secondary) {
        this.kind = kind;
        this.primary = primary;
        this.secondary = secondary;
    }

    /** The window used whole. */
    public static FoldLayout whole(int widthDp, int heightDp) {
        return new FoldLayout(NONE, new int[] {0, 0, Math.max(0, widthDp), Math.max(0, heightDp)}, null);
    }

    /**
     * Lays the window out around {@code fold}.
     *
     * @param widthDp  width of the window
     * @param heightDp height of the window
     * @param fold     the fold, or {@code null} for none
     * @param rtl      right-to-left locale: the right page is the reading side of a book
     */
    public static FoldLayout compute(int widthDp, int heightDp, Fold fold, boolean rtl) {
        if (fold == null || !fold.separating || widthDp <= 0 || heightDp <= 0) {
            return whole(widthDp, heightDp);
        }
        int extent = fold.vertical ? widthDp : heightDp;
        if (fold.end <= 0 || fold.start >= extent) {
            return whole(widthDp, heightDp); // the window lies on one side of the fold
        }
        if (!fold.occluding && extent < Posture.MIN_SPLIT_DP) {
            return whole(widthDp, heightDp); // a bend in a window too small to split
        }
        int half = Posture.GAP_DP / 2;
        int firstSize = Math.max(0, fold.start - half);
        int secondFrom = Math.max(0, fold.end + half);
        int secondSize = Math.max(0, extent - secondFrom);
        if (firstSize == 0 && secondSize == 0) {
            return whole(widthDp, heightDp); // the hinge covers the window; nothing better exists
        }
        // The reading side first: left page (right in rtl) of a book, the upper half of a tabletop.
        int readFrom = 0;
        int readSize = firstSize;
        int otherFrom = secondFrom;
        int otherSize = secondSize;
        if (fold.vertical && rtl) {
            readFrom = secondFrom;
            readSize = secondSize;
            otherFrom = 0;
            otherSize = firstSize;
        }
        if (readSize < MIN_PANE_DP && otherSize > readSize) {
            int f = readFrom;
            int s = readSize;
            readFrom = otherFrom;
            readSize = otherSize;
            otherFrom = f;
            otherSize = s;
        }
        int[] primary = area(fold.vertical, readFrom, readSize, widthDp, heightDp);
        int[] secondary = otherSize >= MIN_PANE_DP ? area(fold.vertical, otherFrom, otherSize, widthDp, heightDp) : null;
        return new FoldLayout(fold.vertical ? BOOK : TABLETOP, primary, secondary);
    }

    private static int[] area(boolean vertical, int from, int size, int widthDp, int heightDp) {
        return vertical ? new int[] {from, 0, size, heightDp} : new int[] {0, from, widthDp, size};
    }

    /** The areas that may hold text, in reading order: the document's, then the controls' if any. */
    public List<int[]> columns() {
        List<int[]> out = new ArrayList<>(2);
        out.add(primary);
        if (secondary != null) {
            out.add(secondary);
        }
        return out;
    }

    /** {@code true} when the window is split. */
    public boolean split() {
        return kind != NONE;
    }

    /** {@code true} when there is room for controls apart from the document. */
    public boolean hasDock() {
        return secondary != null;
    }

    /** A short name of the posture, for diagnostics. */
    public String describe() {
        switch (kind) {
            case BOOK: return secondary != null ? "book" : "book (one page)";
            case TABLETOP: return secondary != null ? "tabletop" : "tabletop (upper half)";
            default: return "flat";
        }
    }
}
