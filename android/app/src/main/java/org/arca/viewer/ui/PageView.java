// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.Typeface;
import android.view.MotionEvent;
import android.view.View;

import org.arca.viewer.MainActivity;
import org.arca.viewer.core.PageDrawing;

/**
 * Draws a {@link PageDrawing} on a white sheet scaled to the width of the view. The drawing is
 * plain data (paths, text runs, placeholders for pictures); nothing from the document runs here.
 */
final class PageView extends View {
    private final PageDrawing page;
    private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint sheet = new Paint();
    private final int placeholder;
    private final int placeholderText;
    /** Obsidian Vault: the lightness of the sheet and of everything on it is turned around. */
    private final boolean invert;
    /** Largest zoom of the page, and tallest view it may make (a bitmap-backed canvas must stay sane). */
    private static final float MAX_SCALE = 8f;
    private static final int MAX_HEIGHT_PX = 32768;

    private float scale = 1f;
    private boolean tooLarge;

    PageView(Context context, PageDrawing page, int placeholder, int placeholderText) {
        super(context);
        this.page = page;
        this.placeholder = placeholder;
        this.placeholderText = placeholderText;
        this.invert = Theme.invertSheet(context);
        sheet.setColor(tone(0xFFFFFFFF));
        setContentDescription(null);
        setClickable(true);
    }

    private int tone(int argb) {
        return invert ? Theme.invertTone(argb) : argb;
    }

    /**
     * A tap on a picture of the page opens it (the sandbox decodes it then); a tap anywhere else is
     * left to the page scroller.
     */
    @Override
    public boolean onTouchEvent(MotionEvent e) {
        if (!tooLarge && e.getActionMasked() == MotionEvent.ACTION_UP && e.getEventTime() - e.getDownTime() < 400
            && getContext() instanceof MainActivity) {
            PageDrawing.Prim hit = pictureAt(e.getX() / scale, e.getY() / scale);
            if (hit != null) {
                performClick();
                ImageViewer.show((MainActivity) getContext(), hit.id, hit.label);
                return true;
            }
        }
        return super.onTouchEvent(e);
    }

    @Override
    public boolean performClick() {
        return super.performClick();
    }

    /** The last openable picture that covers the page point (x, y), or {@code null}. */
    private PageDrawing.Prim pictureAt(float x, float y) {
        PageDrawing.Prim found = null;
        for (PageDrawing.Prim p : page.prims) {
            if (p.kind != PageDrawing.IMAGE || p.id < 0 || p.m == null) {
                continue;
            }
            android.graphics.Matrix inverse = new android.graphics.Matrix();
            if (!matrix(p.m).invert(inverse)) {
                continue;
            }
            float[] pt = {x, y};
            inverse.mapPoints(pt);
            if (pt[0] >= 0f && pt[0] <= 1f && pt[1] >= 0f && pt[1] <= 1f) {
                found = p;
            }
        }
        return found;
    }

    @Override
    protected void onMeasure(int widthSpec, int heightSpec) {
        int w = MeasureSpec.getSize(widthSpec);
        scale = Math.min(w / page.width, MAX_SCALE);
        float wanted = page.height * scale;
        tooLarge = !(wanted <= MAX_HEIGHT_PX);
        if (tooLarge) {
            setMeasuredDimension(w, Math.round(getResources().getDisplayMetrics().density * 120f));
            return;
        }
        setMeasuredDimension(w, Math.max(1, Math.round(wanted)));
    }

    @Override
    protected void onDraw(Canvas c) {
        c.drawRect(0, 0, getWidth(), getHeight(), sheet);
        if (tooLarge) {
            Paint note = new Paint(Paint.ANTI_ALIAS_FLAG);
            note.setColor(placeholderText);
            note.setTextAlign(Paint.Align.CENTER);
            note.setTextSize(getResources().getDisplayMetrics().scaledDensity * 14f);
            c.drawText(getContext().getString(org.arca.viewer.R.string.pages_too_large), getWidth() / 2f, getHeight() / 2f, note);
            return;
        }
        c.save();
        c.scale(scale, scale);
        int depth = 0;
        for (PageDrawing.Prim p : page.prims) {
            switch (p.kind) {
                case PageDrawing.SAVE:
                    c.save();
                    depth++;
                    break;
                case PageDrawing.RESTORE:
                    if (depth > 0) {
                        c.restore();
                        depth--;
                    }
                    break;
                case PageDrawing.CLIP:
                    c.clipRect(p.m[0], p.m[1], p.m[0] + p.m[2], p.m[1] + p.m[3]);
                    break;
                case PageDrawing.PATH:
                    drawPath(c, p);
                    break;
                case PageDrawing.TEXT:
                    drawText(c, p);
                    break;
                case PageDrawing.IMAGE:
                    drawImage(c, p);
                    break;
                default:
                    break;
            }
        }
        while (depth-- > 0) {
            c.restore();
        }
        c.restore();
    }

    private void drawPath(Canvas c, PageDrawing.Prim p) {
        Path path = (Path) p.cache;
        if (path == null) {
            path = new Path();
            float[] o = p.ops;
            for (int i = 0; i < o.length; ) {
                int code = (int) o[i];
                if (code == 0 && i + 2 < o.length) {
                    path.moveTo(o[i + 1], o[i + 2]);
                    i += 3;
                } else if (code == 1 && i + 2 < o.length) {
                    path.lineTo(o[i + 1], o[i + 2]);
                    i += 3;
                } else if (code == 2 && i + 6 < o.length) {
                    path.cubicTo(o[i + 1], o[i + 2], o[i + 3], o[i + 4], o[i + 5], o[i + 6]);
                    i += 7;
                } else if (code == 3) {
                    path.close();
                    i += 1;
                } else {
                    break;
                }
            }
            path.setFillType(p.evenOdd ? Path.FillType.EVEN_ODD : Path.FillType.WINDING);
            p.cache = path;
        }
        if ((p.fill >>> 24) != 0) {
            paint.setStyle(Paint.Style.FILL);
            paint.setColor(tone(p.fill));
            c.drawPath(path, paint);
        }
        if ((p.stroke >>> 24) != 0) {
            paint.setStyle(Paint.Style.STROKE);
            paint.setColor(tone(p.stroke));
            // A hairline (width 0) is drawn one device pixel wide.
            paint.setStrokeWidth(p.width > 0f ? p.width : 1f / scale);
            c.drawPath(path, paint);
        }
    }

    private void drawText(Canvas c, PageDrawing.Prim p) {
        if (p.text.isEmpty() || !(p.size > 0f)) {
            return;
        }
        int style = (p.style & PageDrawing.BOLD) != 0
            ? ((p.style & PageDrawing.ITALIC) != 0 ? Typeface.BOLD_ITALIC : Typeface.BOLD)
            : ((p.style & PageDrawing.ITALIC) != 0 ? Typeface.ITALIC : Typeface.NORMAL);
        String family = (p.style & PageDrawing.MONO) != 0 ? "monospace"
            : (p.style & PageDrawing.SERIF) != 0 ? "serif" : "sans-serif";
        paint.setStyle(Paint.Style.FILL);
        paint.setTypeface(Typeface.create(family, style));
        paint.setTextSize(p.size);
        paint.setColor(tone(p.color));
        paint.setTextScaleX(1f);
        // The document says how wide the run is; fit the substitute font to it.
        float natural = paint.measureText(p.text);
        if (p.advance > 0f && natural > 0f) {
            paint.setTextScaleX(Math.max(0.5f, Math.min(2f, p.advance / natural)));
        }
        c.save();
        c.translate(p.x, p.y);
        c.rotate((float) Math.toDegrees(p.angle));
        c.drawText(p.text, 0, 0, paint);
        c.restore();
        paint.setTextScaleX(1f);
    }

    /** A picture is drawn as a grey box with its size; tapping it decodes it in the sandbox. */
    private void drawImage(Canvas c, PageDrawing.Prim p) {
        float[] m = p.m;
        c.save();
        c.concat(matrix(m));
        paint.setStyle(Paint.Style.FILL);
        paint.setColor(placeholder);
        c.drawRect(0f, 0f, 1f, 1f, paint);
        c.restore();
        if (!p.label.isEmpty()) {
            float cx = m[4] + (m[0] + m[2]) / 2f;
            float cy = m[5] + (m[1] + m[3]) / 2f;
            float size = Math.max(6f, Math.min(14f, Math.abs(m[0]) / 8f));
            paint.setTypeface(Typeface.DEFAULT);
            paint.setTextSize(size);
            paint.setTextScaleX(1f);
            paint.setColor(placeholderText);
            paint.setTextAlign(Paint.Align.CENTER);
            c.drawText(p.label, cx, cy, paint);
            paint.setTextAlign(Paint.Align.LEFT);
        }
    }

    private static android.graphics.Matrix matrix(float[] m) {
        android.graphics.Matrix mat = new android.graphics.Matrix();
        mat.setValues(new float[] {m[0], m[2], m[4], m[1], m[3], m[5], 0f, 0f, 1f});
        return mat;
    }
}
