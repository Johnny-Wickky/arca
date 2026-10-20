// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

/**
 * Window-size breakpoints (those of the Material window-size classes) and the layout numbers
 * derived from them. Pure functions of the window size in dp, so that the adaptive behaviour on
 * phones, tablets and foldables can be tested without a device.
 */
public final class Breakpoints {
    private Breakpoints() {}

    /** Narrower than 600 dp: phones in portrait, a folded foldable's cover display. */
    public static final int COMPACT = 0;
    /** 600 to 839 dp: small tablets, an unfolded foldable in portrait. */
    public static final int MEDIUM = 1;
    /** 840 dp and wider: tablets, an unfolded foldable in landscape, desktop windows. */
    public static final int EXPANDED = 2;

    /** The width class of a window {@code widthDp} wide. */
    public static int widthClass(int widthDp) {
        if (widthDp < 600) {
            return COMPACT;
        }
        return widthDp < 840 ? MEDIUM : EXPANDED;
    }

    /** {@code true} for a short window (landscape phone, flip phone in tent posture). */
    public static boolean isShort(int heightDp) {
        return heightDp < 480;
    }

    /** Side padding for page content, in dp. */
    public static int padding(int widthDp) {
        switch (widthClass(widthDp)) {
            case COMPACT:
                return widthDp < 360 ? 12 : 16;
            case MEDIUM:
                return 24;
            default:
                return 32;
        }
    }

    /** The widest a column of text or controls may be, in dp (comfortable line length). */
    public static int readingWidth(int widthDp) {
        return widthClass(widthDp) == COMPACT ? widthDp : 720;
    }

    /**
     * Whether a two-pane layout (navigation or summary beside the content) fits: wide enough
     * for two usable panes of at least 320 dp each.
     */
    public static boolean twoPane(int widthDp) {
        return widthDp >= 840;
    }

    /** Width of the left pane of a two-pane layout, in dp: a third of the window, 320 to 420. */
    public static int leftPane(int widthDp) {
        return Math.max(320, Math.min(420, widthDp / 3));
    }
}
