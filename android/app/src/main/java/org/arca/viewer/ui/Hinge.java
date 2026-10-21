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
import android.widget.FrameLayout;

import org.arca.viewer.core.Posture;

/**
 * Keeps the content clear of the fold of a half-open foldable. The hinge angle comes from the
 * platform sensor (Android 11+, through {@link Compat}); the rest is the rules of {@link Posture}.
 * On a device without that sensor, and whenever the device is flat or closed, nothing changes.
 */
public final class Hinge {
    private Hinge() {}

    private static int posture = Posture.UNKNOWN;
    private static Compat.HingeSensor sensor;

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

    /** The posture last reported. */
    public static int posture() {
        return posture;
    }

    /**
     * The part of the window to use, {@code {left, top, width, height}} in dp, or {@code null}
     * for all of it.
     */
    public static int[] freeArea(Context c) {
        if (posture != Posture.HALF_OPEN || !(c instanceof Activity)) {
            return null;
        }
        Activity a = (Activity) c;
        Configuration conf = c.getResources().getConfiguration();
        Display d = a.getWindowManager().getDefaultDisplay();
        DisplayMetrics real = new DisplayMetrics();
        d.getRealMetrics(real);
        float density = real.density;
        // Only a window that fills the display lies across the fold (not split-screen or freeform).
        if (conf.screenWidthDp * density < 0.95f * real.widthPixels
            || conf.screenHeightDp * density < 0.85f * real.heightPixels) {
            return null;
        }
        int rotation = d.getRotation(); // Surface.ROTATION_0..3 are 0..3
        boolean odd = (rotation & 1) != 0;
        int naturalW = odd ? real.heightPixels : real.widthPixels;
        int naturalH = odd ? real.widthPixels : real.heightPixels;
        boolean rtl = conf.getLayoutDirection() == View.LAYOUT_DIRECTION_RTL;
        return Posture.freeArea(posture, conf.screenWidthDp, conf.screenHeightDp,
            Posture.foldIsVertical(naturalW, naturalH, rotation), rtl);
    }

    /** Layout parameters that place a screen in the free area of the window. */
    public static FrameLayout.LayoutParams layout(Context c) {
        int[] area = freeArea(c);
        if (area == null) {
            return new FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.MATCH_PARENT);
        }
        float density = c.getResources().getDisplayMetrics().density;
        Configuration conf = c.getResources().getConfiguration();
        boolean whole = area[2] == conf.screenWidthDp;
        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(
            whole ? FrameLayout.LayoutParams.MATCH_PARENT : Math.round(area[2] * density),
            area[3] == conf.screenHeightDp ? FrameLayout.LayoutParams.MATCH_PARENT : Math.round(area[3] * density));
        boolean right = area[0] > 0;
        lp.gravity = Gravity.TOP | (right ? Gravity.END : Gravity.START);
        return lp;
    }
}
