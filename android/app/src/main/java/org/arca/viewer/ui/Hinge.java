// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.app.Activity;
import android.content.Context;
import android.content.res.Configuration;
import android.util.DisplayMetrics;
import android.view.Display;
import android.view.Gravity;
import android.view.View;
import android.view.WindowInsets;
import android.widget.FrameLayout;

import org.arca.viewer.core.Fold;
import org.arca.viewer.core.FoldLayout;
import org.arca.viewer.core.Posture;

import java.util.Arrays;

/**
 * Keeps the content clear of the fold or hinge across the window. Where the fold lies comes from
 * two public, non-AndroidX sources, the better one winning:
 * <ol>
 * <li>a hinge reported as a display cutout in the window's insets (dual-screen devices, API 28+);
 *     it hides pixels, so the window is split around it whatever the posture;</li>
 * <li>the platform's hinge-angle sensor (API 30+) plus a guess of where a flexible screen folds
 *     (see {@link Posture}); the window is split only while the device is half open.</li>
 * </ol>
 * The rules that turn a fold into the document area and the controls area are those of
 * {@link FoldLayout}, which is tested without a device. On a device that does not fold, and
 * whenever the window does not lie across the fold, nothing changes.
 */
public final class Hinge {
    private Hinge() {}

    private static int posture = Posture.UNKNOWN;
    private static Compat.HingeSensor sensor;
    /** The cutout rectangles of the last insets (pixels, window coordinates). */
    private static int[][] cutouts = new int[0][];
    private static FoldLayout layout;
    private static boolean fromCutout;
    /** The window size, pixels, that {@link #layout} was computed for. */
    private static int windowW;
    private static int windowH;
    private static float density = 1f;

    /** Starts listening; {@code onChange} runs on the main thread when the free area may have changed. */
    public static void start(Activity a, final Runnable onChange) {
        stop();
        sensor = new Compat.HingeSensor(a, new Compat.HingeSensor.Listener() {
            @Override
            public void onAngle(float degrees) {
                int before = posture;
                posture = Posture.next(posture, degrees);
                if (posture != before) {
                    onChange.run();
                }
            }
        });
        if (!sensor.start()) {
            sensor = null;
            posture = Posture.UNKNOWN;
        }
    }

    /** Stops listening. */
    public static void stop() {
        if (sensor != null) {
            sensor.stop();
            sensor = null;
        }
        posture = Posture.UNKNOWN;
    }

    /** The posture last reported by the hinge sensor. */
    public static int posture() {
        return posture;
    }

    /**
     * Takes the display cutouts from the window's insets. Returns {@code true} when they differ
     * from before, so that the caller lays the screen out again.
     */
    public static boolean onInsets(WindowInsets insets) {
        int[][] now = Compat.cutoutRects(insets);
        boolean changed = !Arrays.deepEquals(cutouts, now);
        cutouts = now;
        return changed;
    }

    /** Works out the fold and the layout around it for the window as it is now. Call before building a screen. */
    public static void update(Activity a) {
        Configuration conf = a.getResources().getConfiguration();
        density = a.getResources().getDisplayMetrics().density;
        Display d = a.getWindowManager().getDefaultDisplay();
        DisplayMetrics real = new DisplayMetrics();
        d.getRealMetrics(real);
        int[] size = Compat.windowSizePx(a);
        int displayW = real.widthPixels;
        int displayH = real.heightPixels;
        if (size != null) {
            windowW = size[0];
            windowH = size[1];
        } else {
            windowW = Math.round(conf.screenWidthDp * density);
            windowH = Math.round(conf.screenHeightDp * density);
        }
        int wDp = Math.round(windowW / density);
        int hDp = Math.round(windowH / density);
        boolean rtl = conf.getLayoutDirection() == View.LAYOUT_DIRECTION_RTL;

        Fold reported = Fold.fromRectsPx(cutouts, windowW, windowH, density);
        Fold inferred = null;
        // Only a window that fills the display lies across a flexible fold (not split-screen or freeform).
        if (posture == Posture.HALF_OPEN && windowW >= 0.95f * displayW && windowH >= 0.85f * displayH) {
            int rotation = d.getRotation(); // Surface.ROTATION_0..3 are 0..3
            boolean odd = (rotation & 1) != 0;
            int naturalW = odd ? displayH : displayW;
            int naturalH = odd ? displayW : displayH;
            inferred = Fold.inferred(posture, wDp, hDp, Posture.foldIsVertical(naturalW, naturalH, rotation));
        }
        Fold fold = Fold.pick(reported, inferred);
        fromCutout = reported != null;
        layout = FoldLayout.compute(wDp, hDp, fold, rtl);
    }

    /** The layout computed by the last {@link #update}, or {@code null} before the first. */
    public static FoldLayout current() {
        return layout;
    }

    /**
     * The part of the window to use for the document, {@code {left, top, width, height}} in dp, or
     * {@code null} for all of it.
     */
    public static int[] freeArea(Context c) {
        return layout != null && layout.split() ? layout.primary : null;
    }

    /** Whether the window is split with room for controls beside or below the document. */
    public static boolean hasDock() {
        return layout != null && layout.hasDock();
    }

    /** Layout parameters that place a screen in the free area of the window. */
    public static FrameLayout.LayoutParams layout(View root) {
        return place(root, layout != null && layout.split() ? layout.primary : null);
    }

    /** Layout parameters that place the controls (the dock) in the area apart from the document. */
    public static FrameLayout.LayoutParams dockLayout(View root) {
        return place(root, layout != null ? layout.secondary : null);
    }

    private static FrameLayout.LayoutParams place(View root, int[] area) {
        FrameLayout.LayoutParams whole = new FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT,
            FrameLayout.LayoutParams.MATCH_PARENT);
        if (area == null) {
            return whole;
        }
        // The area is in window coordinates; the root view is padded by the system bars.
        int padL = root.getPaddingLeft();
        int padT = root.getPaddingTop();
        int boxR = windowW - root.getPaddingRight();
        int boxB = windowH - root.getPaddingBottom();
        int left = Math.max(padL, Math.round(area[0] * density));
        int top = Math.max(padT, Math.round(area[1] * density));
        int right = Math.min(boxR, Math.round((area[0] + area[2]) * density));
        int bottom = Math.min(boxB, Math.round((area[1] + area[3]) * density));
        if (right <= left || bottom <= top || (left == padL && top == padT && right == boxR && bottom == boxB)) {
            return whole;
        }
        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(right - left, bottom - top);
        // Absolute left: the area was worked out in window coordinates, whatever the layout direction.
        lp.gravity = Gravity.TOP | Gravity.LEFT;
        lp.leftMargin = left - padL;
        lp.topMargin = top - padT;
        return lp;
    }

    /** One line for the diagnostics: the posture and where the fold came from. */
    public static String describe() {
        if (layout == null) {
            return "fold: not computed";
        }
        return "fold: " + layout.describe() + (layout.split() ? (fromCutout ? ", hinge cutout" : ", hinge sensor") : "");
    }
}
