// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

/**
 * The zoom of the preview text: a multiplier on top of the text size chosen in the settings,
 * changed by pinching or by the buttons, always within sensible bounds and in steps of 5 %.
 *
 * <p>No Android dependencies; tested on a desktop JVM.
 */
public final class Zoom {
    private Zoom() {}

    public static final float MIN = 0.6f;
    public static final float MAX = 3.0f;
    public static final float DEFAULT = 1.0f;
    /** The change made by one press of a button. */
    public static final float STEP = 0.1f;

    /** {@code z} limited to the allowed range and rounded to 5 %; anything not finite is 100 %. */
    public static float clamp(float z) {
        if (Float.isNaN(z) || Float.isInfinite(z)) {
            return DEFAULT;
        }
        float rounded = Math.round(z * 20f) / 20f;
        return Math.max(MIN, Math.min(MAX, rounded));
    }

    /** One button press: {@code direction} is +1 to zoom in, -1 to zoom out. */
    public static float step(float z, int direction) {
        return clamp(clamp(z) + (direction > 0 ? STEP : -STEP));
    }

    /** The result of a pinch that scaled by {@code factor}. */
    public static float pinch(float z, float factor) {
        if (Float.isNaN(factor) || Float.isInfinite(factor) || factor <= 0f) {
            return clamp(z);
        }
        return clamp(clamp(z) * factor);
    }

    /** The zoom as a whole percentage. */
    public static int percent(float z) {
        return Math.round(clamp(z) * 100f);
    }
}
