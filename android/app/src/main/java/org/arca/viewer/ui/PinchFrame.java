// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Context;
import android.view.Gravity;
import android.view.MotionEvent;
import android.view.ScaleGestureDetector;
import android.view.View;
import android.widget.FrameLayout;
import android.widget.TextView;

import org.arca.viewer.R;
import org.arca.viewer.core.Zoom;

/**
 * Wraps a scrolling list and turns a two-finger pinch into a zoom of its text. One-finger
 * gestures reach the list untouched. While the fingers are down a badge shows the zoom that
 * the gesture would give; the zoom itself is applied once, when the fingers lift.
 */
final class PinchFrame extends FrameLayout {
    /** Told once per pinch, with the factor by which the fingers spread (more than 1) or closed. */
    interface Listener {
        void onPinch(float factor);
    }

    private final ScaleGestureDetector detector;
    private final TextView badge;
    private final float base;
    private float factor = 1f;

    PinchFrame(Context context, float currentZoom, final Listener listener) {
        super(context);
        this.base = currentZoom;
        Ui ui = new Ui(context);
        badge = ui.text("", Ui.LABEL);
        badge.setTextColor(ui.color(R.color.arca_on_primary));
        badge.setBackground(ui.rounded(ui.color(R.color.arca_primary), 0, 16));
        badge.setPadding(ui.dp(14), ui.dp(6), ui.dp(14), ui.dp(6));
        badge.setVisibility(View.GONE);
        LayoutParams lp = new LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT,
            Gravity.TOP | Gravity.CENTER_HORIZONTAL);
        lp.topMargin = ui.dp(12);
        addView(badge, lp);

        detector = new ScaleGestureDetector(context, new ScaleGestureDetector.SimpleOnScaleGestureListener() {
            @Override
            public boolean onScaleBegin(ScaleGestureDetector d) {
                factor = 1f;
                return true;
            }

            @Override
            public boolean onScale(ScaleGestureDetector d) {
                factor *= d.getScaleFactor();
                badge.setText(getContext().getString(R.string.zoom_value, Zoom.percent(Zoom.pinch(base, factor))));
                badge.setVisibility(View.VISIBLE);
                return true;
            }

            @Override
            public void onScaleEnd(ScaleGestureDetector d) {
                badge.setVisibility(View.GONE);
                float f = factor;
                factor = 1f;
                if (Math.abs(f - 1f) > 0.03f) {
                    listener.onPinch(f);
                }
            }
        });
    }

    @Override
    public boolean onInterceptTouchEvent(MotionEvent ev) {
        detector.onTouchEvent(ev);
        // Two fingers belong to the pinch; one finger scrolls the list.
        return detector.isInProgress() || ev.getPointerCount() > 1;
    }

    @Override
    public boolean onTouchEvent(MotionEvent ev) {
        detector.onTouchEvent(ev);
        return true;
    }
}
