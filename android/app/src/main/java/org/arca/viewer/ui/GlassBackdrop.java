// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.RadialGradient;
import android.graphics.Shader;
import android.view.View;

/**
 * What Liquid Glass panels sit on: the window colour of the palette in force with a few soft
 * washes of its brand and link colours. The panels are translucent, so this is what gives them
 * something to frost; where the system can blur a view (Android 12+) the backdrop is blurred too.
 *
 * <p>The washes never get stronger than {@link Theme#GLASS_WASH} where they overlap in the worst
 * case: that is the figure the contrast test in {@code arca-devtools} checks the text of every
 * palette against. Drawn from colours only; nothing from a document is involved.
 */
public final class GlassBackdrop extends View {
    /** Peak opacity of each wash; two of them together stay inside {@link Theme#GLASS_WASH}. */
    private static final int PRIMARY_PEAK = 0x44;
    private static final int LINK_PEAK = 0x30;

    private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final int bg;
    private final int primary;
    private final int link;

    public GlassBackdrop(Context c, int bg, int primary, int link) {
        super(c);
        this.bg = bg;
        this.primary = primary;
        this.link = link;
        setImportantForAccessibility(IMPORTANT_FOR_ACCESSIBILITY_NO);
        setClickable(false);
        Compat.blurView(this, 32f * c.getResources().getDisplayMetrics().density);
    }

    @Override
    protected void onDraw(Canvas canvas) {
        int w = getWidth();
        int h = getHeight();
        if (w <= 0 || h <= 0) {
            return;
        }
        canvas.drawColor(bg);
        float reach = Math.max(w, h);
        wash(canvas, w * 0.10f, h * 0.08f, reach * 0.70f, primary, PRIMARY_PEAK);
        wash(canvas, w * 0.95f, h * 0.42f, reach * 0.55f, link, LINK_PEAK);
        wash(canvas, w * 0.25f, h * 1.00f, reach * 0.60f, primary, PRIMARY_PEAK / 2);
    }

    private void wash(Canvas canvas, float cx, float cy, float radius, int color, int peak) {
        int solid = Theme.withAlpha(color, peak);
        int clear = Theme.withAlpha(color, 0);
        paint.setShader(new RadialGradient(cx, cy, radius, new int[] {solid, clear}, new float[] {0f, 1f},
            Shader.TileMode.CLAMP));
        canvas.drawRect(0f, 0f, getWidth(), getHeight(), paint);
    }
}
