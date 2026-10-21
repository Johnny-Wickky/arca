// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Context;
import android.content.res.Configuration;

import org.arca.viewer.core.Breakpoints;

/**
 * Window size classes, computed from the <em>window</em>, not the display:
 * {@code screenWidthDp} follows split-screen, freeform windows, a folded or unfolded
 * foldable, and a flip phone's half-open posture. The thresholds live in
 * {@link Breakpoints}, which is tested without a device.
 */
public final class Adaptive {
    private Adaptive() {}

    public static final int COMPACT = Breakpoints.COMPACT;
    public static final int MEDIUM = Breakpoints.MEDIUM;
    public static final int EXPANDED = Breakpoints.EXPANDED;

    /** Window width in dp. */
    public static int widthDp(Context c) {
        return c.getResources().getConfiguration().screenWidthDp;
    }

    /** Window height in dp. */
    public static int heightDp(Context c) {
        return c.getResources().getConfiguration().screenHeightDp;
    }

    /** The width class of the window {@code c} lives in. */
    public static int widthClass(Context c) {
        return Breakpoints.widthClass(widthDp(c));
    }

    /** {@code true} for a short, wide window. */
    public static boolean isShort(Context c) {
        return Breakpoints.isShort(heightDp(c));
    }

    /** {@code true} when the window is landscape-shaped. */
    public static boolean isLandscape(Context c) {
        Configuration conf = c.getResources().getConfiguration();
        return conf.screenWidthDp > conf.screenHeightDp;
    }

    /** Side padding for page content, in dp. */
    public static int padding(Context c) {
        return Breakpoints.padding(widthDp(c));
    }

    /** Widest comfortable column, in dp. */
    public static int readingWidth(Context c) {
        return Breakpoints.readingWidth(widthDp(c));
    }

    /** Whether a two-pane layout fits. */
    public static boolean twoPane(Context c) {
        return Breakpoints.twoPane(widthDp(c));
    }

    /** Width of the left pane of a two-pane layout, in dp. */
    public static int leftPane(Context c) {
        return Breakpoints.leftPane(widthDp(c));
    }
}
