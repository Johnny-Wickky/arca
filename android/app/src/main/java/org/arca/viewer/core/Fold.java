// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

/**
 * A fold or hinge across the window: where it lies, and whether it hides pixels. Pure data and
 * pure functions, so that the rules can be tested without a device; {@link FoldLayout} turns a
 * fold into the areas that text may use.
 *
 * <p>There are two sources, and the app has no AndroidX (so no Jetpack WindowManager
 * {@code FoldingFeature}):
 * <ul>
 * <li>a <em>hinge cutout</em>: dual-screen devices (Surface Duo class) report the hinge as a
 *     display cutout, a rectangle in window coordinates. It hides pixels (it is
 *     <em>occluding</em>) and always separates the window into two areas;</li>
 * <li>an <em>inferred</em> fold: a flexible screen has no rectangle in the public API, so its
 *     position is guessed (see {@link Posture}) and the platform's hinge-angle sensor says whether
 *     it is bent. It hides nothing, and separates only while the device is half open.</li>
 * </ul>
 * All numbers are dp in window coordinates (origin at the top left of the window).
 */
public final class Fold {
    /** {@code true} when the fold line runs vertically (the window is split into left and right). */
    public final boolean vertical;
    /** The extent of the fold across its line: x for a vertical fold, y for a horizontal one. */
    public final int start;
    /** The end of that extent ({@code start} for a hairline). */
    public final int end;
    /** The fold hides pixels (a hinge cutout); nothing may be placed in {@code start..end}. */
    public final boolean occluding;
    /** The fold splits the window into two areas. An occluding fold always does. */
    public final boolean separating;

    /** A cutout this close to an edge of the window (percent of the window) is a notch, not a fold. */
    static final int EDGE_PERCENT = 15;
    /** A hinge spans at least this much (percent) of the window along its line... */
    static final int SPAN_PERCENT = 90;
    /** ...and is at most this thick (one third of the window). */
    static final int THICK_DIVISOR = 3;

    private Fold(boolean vertical, int start, int end, boolean occluding, boolean separating) {
        this.vertical = vertical;
        this.start = Math.min(start, end);
        this.end = Math.max(start, end);
        this.occluding = occluding;
        // What hides pixels cannot be read across, so it separates whatever the caller said.
        this.separating = separating || occluding;
    }

    /** A fold that hides nothing, at {@code position}. */
    public static Fold hairline(boolean vertical, int position, boolean separating) {
        return new Fold(vertical, position, position, false, separating);
    }

    /** A hinge that hides the band {@code start..end}. */
    public static Fold hinge(boolean vertical, int start, int end) {
        return new Fold(vertical, start, end, true, true);
    }

    /**
     * The fold inferred from the hinge angle and the shape of the display: in the middle of the
     * window, separating only while the device is half open. {@code null} when it is not half
     * open (flat, closed, or no reading), and so there is nothing to keep clear of.
     */
    public static Fold inferred(int posture, int widthDp, int heightDp, boolean vertical) {
        if (posture != Posture.HALF_OPEN || widthDp <= 0 || heightDp <= 0) {
            return null;
        }
        return hairline(vertical, (vertical ? widthDp : heightDp) / 2, true);
    }

    /**
     * Reads a display-cutout rectangle (dp, window coordinates) as a hinge, or {@code null} when it
     * is a camera hole or a notch: a hinge spans (nearly) the whole window along its line, is at
     * most a third of the window thick, and lies clear of the window's edges.
     */
    public static Fold fromRect(int left, int top, int right, int bottom, int widthDp, int heightDp) {
        int w = right - left;
        int h = bottom - top;
        if (w <= 0 || h <= 0 || widthDp <= 0 || heightDp <= 0) {
            return null;
        }
        if (h * 100 >= heightDp * SPAN_PERCENT && w * THICK_DIVISOR <= widthDp
            && left * 100 >= widthDp * EDGE_PERCENT && right * 100 <= widthDp * (100 - EDGE_PERCENT)) {
            return hinge(true, left, right);
        }
        if (w * 100 >= widthDp * SPAN_PERCENT && h * THICK_DIVISOR <= heightDp
            && top * 100 >= heightDp * EDGE_PERCENT && bottom * 100 <= heightDp * (100 - EDGE_PERCENT)) {
            return hinge(false, top, bottom);
        }
        return null;
    }

    /**
     * The hinge among the cutout rectangles the platform reported ({@code {left, top, right,
     * bottom}} in pixels, window coordinates), or {@code null} when none is a hinge. Pixels are
     * converted outward (the fold is never made smaller by rounding). With several candidates the
     * thickest wins.
     */
    public static Fold fromRectsPx(int[][] rects, int widthPx, int heightPx, float density) {
        if (rects == null || density <= 0f || widthPx <= 0 || heightPx <= 0) {
            return null;
        }
        int w = Math.round(widthPx / density);
        int h = Math.round(heightPx / density);
        Fold best = null;
        for (int[] r : rects) {
            if (r == null || r.length != 4) {
                continue;
            }
            Fold f = fromRect((int) Math.floor(r[0] / density), (int) Math.floor(r[1] / density),
                (int) Math.ceil(r[2] / density), (int) Math.ceil(r[3] / density), w, h);
            if (f != null && (best == null || f.end - f.start > best.end - best.start)) {
                best = f;
            }
        }
        return best;
    }

    /** What the platform reported beats what was inferred; {@code null} when neither is a fold. */
    public static Fold pick(Fold reported, Fold inferred) {
        return reported != null ? reported : inferred;
    }
}
