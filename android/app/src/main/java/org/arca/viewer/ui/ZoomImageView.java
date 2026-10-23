// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.BitmapShader;
import android.graphics.Canvas;
import android.graphics.Matrix;
import android.graphics.Paint;
import android.graphics.RectF;
import android.graphics.Shader;
import android.view.GestureDetector;
import android.view.MotionEvent;
import android.view.ScaleGestureDetector;
import android.view.View;

/**
 * Draws one bitmap and lets the reader pinch, drag and double-tap it. The bitmap is a buffer of
 * pixels that the sandbox built; this view only paints it (no decoding of any kind).
 */
final class ZoomImageView extends View {
    private static final float MAX_ZOOM = 8f;

    private final Bitmap bitmap;
    private final Matrix matrix = new Matrix();
    private final Paint paint = new Paint(Paint.FILTER_BITMAP_FLAG | Paint.ANTI_ALIAS_FLAG);
    private final Paint checker = new Paint();
    private final RectF bounds = new RectF();
    private final ScaleGestureDetector scaler;
    private final GestureDetector gestures;
    private float fit = 1f;
    private boolean laidOut;

    ZoomImageView(Context context, Bitmap bitmap) {
        super(context);
        this.bitmap = bitmap;
        // A light checkerboard shows through transparent pixels.
        Bitmap tile = Bitmap.createBitmap(2, 2, Bitmap.Config.ARGB_8888);
        tile.setPixel(0, 0, 0xFFFFFFFF);
        tile.setPixel(1, 1, 0xFFFFFFFF);
        tile.setPixel(1, 0, 0xFFD9D9D9);
        tile.setPixel(0, 1, 0xFFD9D9D9);
        BitmapShader shader = new BitmapShader(tile, Shader.TileMode.REPEAT, Shader.TileMode.REPEAT);
        Matrix scale = new Matrix();
        scale.setScale(8f * getResources().getDisplayMetrics().density, 8f * getResources().getDisplayMetrics().density);
        shader.setLocalMatrix(scale);
        checker.setShader(shader);
        scaler = new ScaleGestureDetector(context, new ScaleGestureDetector.SimpleOnScaleGestureListener() {
            @Override
            public boolean onScale(ScaleGestureDetector d) {
                zoomBy(d.getScaleFactor(), d.getFocusX(), d.getFocusY());
                return true;
            }
        });
        gestures = new GestureDetector(context, new GestureDetector.SimpleOnGestureListener() {
            @Override
            public boolean onDown(MotionEvent e) {
                return true;
            }

            @Override
            public boolean onScroll(MotionEvent e1, MotionEvent e2, float dx, float dy) {
                matrix.postTranslate(-dx, -dy);
                clamp();
                invalidate();
                return true;
            }

            @Override
            public boolean onDoubleTap(MotionEvent e) {
                if (scale() > fit * 1.05f) {
                    fit();
                } else {
                    zoomBy(Math.min(MAX_ZOOM * fit, Math.max(2.5f * fit, 1f)) / scale(), e.getX(), e.getY());
                }
                return true;
            }
        });
        setFocusable(true);
    }

    private float scale() {
        float[] v = new float[9];
        matrix.getValues(v);
        return v[Matrix.MSCALE_X];
    }

    private void fit() {
        if (getWidth() == 0 || getHeight() == 0) {
            return;
        }
        fit = Math.min((float) getWidth() / bitmap.getWidth(), (float) getHeight() / bitmap.getHeight());
        matrix.reset();
        matrix.postScale(fit, fit);
        clamp();
        invalidate();
    }

    private void zoomBy(float factor, float fx, float fy) {
        float target = Math.max(fit * 0.9f, Math.min(MAX_ZOOM * Math.max(fit, 1f), scale() * factor));
        float f = target / scale();
        matrix.postScale(f, f, fx, fy);
        clamp();
        invalidate();
    }

    /** Keeps the picture on the screen: centred when smaller than the view, never past an edge otherwise. */
    private void clamp() {
        bounds.set(0, 0, bitmap.getWidth(), bitmap.getHeight());
        matrix.mapRect(bounds);
        float dx = 0;
        float dy = 0;
        if (bounds.width() <= getWidth()) {
            dx = (getWidth() - bounds.width()) / 2f - bounds.left;
        } else if (bounds.left > 0) {
            dx = -bounds.left;
        } else if (bounds.right < getWidth()) {
            dx = getWidth() - bounds.right;
        }
        if (bounds.height() <= getHeight()) {
            dy = (getHeight() - bounds.height()) / 2f - bounds.top;
        } else if (bounds.top > 0) {
            dy = -bounds.top;
        } else if (bounds.bottom < getHeight()) {
            dy = getHeight() - bounds.bottom;
        }
        matrix.postTranslate(dx, dy);
    }

    @Override
    protected void onSizeChanged(int w, int h, int oldw, int oldh) {
        super.onSizeChanged(w, h, oldw, oldh);
        if (!laidOut) {
            laidOut = true;
            fit();
        } else {
            // Rotation or folding: fit again, the reader's position is not meaningful any more.
            fit();
        }
    }

    @Override
    protected void onDraw(Canvas canvas) {
        bounds.set(0, 0, bitmap.getWidth(), bitmap.getHeight());
        matrix.mapRect(bounds);
        canvas.drawRect(bounds, checker);
        canvas.drawBitmap(bitmap, matrix, paint);
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        scaler.onTouchEvent(event);
        if (!scaler.isInProgress()) {
            gestures.onTouchEvent(event);
        }
        if (event.getActionMasked() == MotionEvent.ACTION_UP) {
            performClick();
        }
        return true;
    }

    @Override
    public boolean performClick() {
        return super.performClick();
    }
}
