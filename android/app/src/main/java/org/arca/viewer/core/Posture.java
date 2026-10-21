// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

/**
 * The posture of a foldable and the part of the window that stays clear of the fold. Pure
 * functions, so that the rules can be tested without a device.
 *
 * <p>The platform's public API (the app has no AndroidX, so no Jetpack WindowManager) tells the
 * hinge <em>angle</em> (a sensor, Android 11+) but not where the fold lies. The position is
 * inferred: a foldable folds in the middle of its inner display, along the long side when the
 * display is nearly square (a book, like the Galaxy Z Fold or the Pixel Fold) and across it when
 * the display is tall and narrow (a flip phone). Rotation turns the fold line with the window.
 * This is a heuristic, written down here so that it can be argued with; it only applies while the
 * device reports a half-open hinge.
 */
public final class Posture {
    private Posture() {}

    /** No hinge sensor, or no reading. */
    public static final int UNKNOWN = 0;
    /** Unfolded (or nearly). */
    public static final int FLAT = 1;
    /** Half open: a book on a lap, a laptop, a phone propped on its fold. */
    public static final int HALF_OPEN = 2;
    /** Closed (the cover display is in use). */
    public static final int CLOSED = 3;

    /** Below this angle the device is closed. */
    static final float CLOSED_BELOW = 15f;
    /** An open device counts as half open below this angle... */
    static final float HALF_OPEN_BELOW = 160f;
    /** ...and as flat again from this one (hysteresis, so that a hand resting on it does not flicker). */
    static final float FLAT_FROM = 170f;

    /** The width of the band around the fold that is left empty, in dp. */
    public static final int GAP_DP = 24;
    /** Below this size (dp) along the fold a split would leave too little to read. */
    static final int MIN_SPLIT_DP = 480;
    /** A display at least this close to square folds like a book. */
    static final float BOOK_RATIO = 0.7f;

    /** The posture after a hinge reading of {@code angle} degrees, given the previous posture. */
    public static int next(int previous, float angle) {
        if (Float.isNaN(angle) || angle < 0f || angle > 360f) {
            return UNKNOWN;
        }
        if (angle < CLOSED_BELOW) {
            return CLOSED;
        }
        if (previous == HALF_OPEN) {
            return angle >= FLAT_FROM ? FLAT : HALF_OPEN;
        }
        return angle < HALF_OPEN_BELOW ? HALF_OPEN : FLAT;
    }

    /**
     * Whether the fold runs vertically in window coordinates (left and right halves).
     *
     * @param naturalWidth  width of the display in its natural orientation
     * @param naturalHeight height of the display in its natural orientation
     * @param rotation      quarter turns of the window from the natural orientation (0 to 3)
     */
    public static boolean foldIsVertical(int naturalWidth, int naturalHeight, int rotation) {
        int lo = Math.min(naturalWidth, naturalHeight);
        int hi = Math.max(naturalWidth, naturalHeight);
        boolean bookLike = hi > 0 && (float) lo / hi >= BOOK_RATIO;
        boolean upright = (rotation & 1) == 0;
        return upright == bookLike;
    }

    /**
     * The part of the window that stays clear of the fold while the device is half open:
     * {@code {left, top, width, height}} in dp, or {@code null} when the whole window can be used
     * (not half open, or too small to split). With a vertical fold it is one half, on the reading
     * side ({@code rtl} chooses the right one); with a horizontal fold, the upper half.
     */
    public static int[] freeArea(int posture, int widthDp, int heightDp, boolean foldVertical, boolean rtl) {
        if (posture != HALF_OPEN) {
            return null;
        }
        if (foldVertical) {
            if (widthDp < MIN_SPLIT_DP) {
                return null;
            }
            int w = (widthDp - GAP_DP) / 2;
            return new int[] {rtl ? widthDp - w : 0, 0, w, heightDp};
        }
        if (heightDp < MIN_SPLIT_DP) {
            return null;
        }
        return new int[] {0, 0, widthDp, (heightDp - GAP_DP) / 2};
    }
}
