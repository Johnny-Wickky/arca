// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.ColorFilter;
import android.graphics.LinearGradient;
import android.graphics.Paint;
import android.graphics.PixelFormat;
import android.graphics.RectF;
import android.graphics.Shader;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.LayerDrawable;

import org.arca.viewer.Prefs;

import java.util.HashMap;
import java.util.Map;

/**
 * The colour palettes of the app. {@link #CLASSIC} is the pair of resource files
 * ({@code values/colors.xml}, {@code values-night/colors.xml}); the six named palettes replace the
 * surface, text and brand colours of whichever of the two matches their lightness and leave the
 * verdict and severity colours to it (they are tuned for light and for dark surfaces already).
 *
 * <p>Colours are looked up by the name of the resource, so every screen keeps asking for
 * colour resource through {@link Ui#color(int)} and nothing else changes.
 */
public final class Theme {
    private Theme() {}

    /** The resource colours, light or dark by the system or the Light / Dark setting. */
    public static final int CLASSIC = 0;
    /** Light paper. */
    public static final int ALABASTER = 1;
    /** True black for OLED screens; the sheet of a page is inverted. */
    public static final int OBSIDIAN = 2;
    /** Warm cream and coffee, for long reading. */
    public static final int CASHMERE = 3;
    /** Cold titanium and slate. */
    public static final int SIERRA = 4;
    /** Dark emerald, a security panel. */
    public static final int EMERALD = 5;
    /** Warm graphite and bronze. */
    public static final int DESERT = 6;
    /** Number of palettes, {@link #CLASSIC} included. */
    public static final int COUNT = 7;

    /** The resource names a palette overrides, in the order of the rows of {@link #PALETTES}. */
    private static final String[] KEYS = {
        "arca_bg", "arca_surface", "arca_surface_variant", "arca_on_surface", "arca_on_surface_variant",
        "arca_outline", "arca_primary", "arca_on_primary", "arca_primary_container",
        "arca_on_primary_container", "arca_code_bg", "arca_hidden_text", "arca_link",
        "arca_search_hit", "arca_search_current", "arca_scrim",
    };

    private static final int[][] PALETTES = {
        null,
        // Alabaster Studio: paper white
        {0xFFFAF9F6, 0xFFFFFFFF, 0xFFEFEDE6, 0xFF1C1B18, 0xFF5A5750, 0xFFDAD6CC, 0xFF0B6B63, 0xFFFFFFFF,
            0xFFD6EFE9, 0xFF00201D, 0xFFF1EFE9, 0xFF6E6A62, 0xFF1A5A9E, 0xFFFFE9A8, 0xFFFFB347, 0x66000000},
        // Obsidian Vault: true black
        {0xFF000000, 0xFF0A0A0B, 0xFF161719, 0xFFE9EAEC, 0xFF9EA2A8, 0xFF26282C, 0xFF4FD1C5, 0xFF00201D,
            0xFF0C3B36, 0xFFCFF5F0, 0xFF111214, 0xFF7F858C, 0xFF8EC1FF, 0xFF5A4A12, 0xFF8A5C0C, 0xB3000000},
        // Cashmere & Cappuccino: cream and coffee
        {0xFFF4EBDD, 0xFFFBF5EA, 0xFFE9DAC6, 0xFF33261B, 0xFF685240, 0xFFD2BEA4, 0xFF7B4A28, 0xFFFFFFFF,
            0xFFEFD7BD, 0xFF2C1507, 0xFFEDE0CD, 0xFF7C6A58, 0xFF1F5C94, 0xFFF6DC93, 0xFFF2A94A, 0x66000000},
        // Sierra Blue & Slate: cold titanium
        {0xFFE4EAF1, 0xFFF3F6FA, 0xFFD3DCE7, 0xFF16202B, 0xFF44525F, 0xFFB8C4D1, 0xFF2B5F8F, 0xFFFFFFFF,
            0xFFCFE0F2, 0xFF0A2238, 0xFFE1E8F0, 0xFF66737F, 0xFF1A5A9E, 0xFFFFE9A8, 0xFFFFB347, 0x66000000},
        // Emerald SecOps: dark emerald
        {0xFF05130E, 0xFF0A1F17, 0xFF123027, 0xFFD9F4E7, 0xFF8DB8A2, 0xFF1E4A3B, 0xFF34E0A1, 0xFF00281A,
            0xFF0F4A36, 0xFFC8F7E1, 0xFF0E2A20, 0xFF78A08C, 0xFF7FD0FF, 0xFF4F4A10, 0xFF8A5C0C, 0xB3000000},
        // Desert Titanium: warm graphite and bronze
        {0xFF1A1713, 0xFF24201B, 0xFF322C25, 0xFFEEE5D8, 0xFFB5A894, 0xFF483F34, 0xFFDBA466, 0xFF2B1C08,
            0xFF4A3418, 0xFFF5DDBB, 0xFF2A251F, 0xFF9B8F7D, 0xFF9CC8F0, 0xFF5A4A12, 0xFF8A5C0C, 0xB3000000},
    };

    private static final boolean[] DARK = {false, false, true, false, false, true, true};

    /** Resource id to palette column, filled on first use ({@code -1}: not overridden). */
    private static final Map<Integer, Integer> COLUMN = new HashMap<>();

    /** Whether palette {@code p} is dark, so that the system widgets follow; {@code null} for {@link #CLASSIC}. */
    public static Boolean dark(int p) {
        return p > CLASSIC && p < COUNT ? Boolean.valueOf(DARK[p]) : null;
    }

    private static int column(Context c, int res) {
        Integer known = COLUMN.get(res);
        if (known != null) {
            return known;
        }
        int found = -1;
        try {
            String name = c.getResources().getResourceEntryName(res);
            for (int i = 0; i < KEYS.length; i++) {
                if (KEYS[i].equals(name)) {
                    found = i;
                    break;
                }
            }
        } catch (RuntimeException e) {
            found = -1;
        }
        COLUMN.put(res, found);
        return found;
    }

    /** The colour for resource {@code res} under the chosen palette. */
    public static int color(Context c, int res) {
        int palette = Prefs.palette(c);
        if (palette > CLASSIC && palette < COUNT) {
            int col = column(c, res);
            if (col >= 0) {
                return PALETTES[palette][col];
            }
        }
        return c.getColor(res);
    }

    /** Whether the translucent "liquid glass" surfaces are on. */
    public static boolean glass(Context c) {
        return Prefs.glass(c);
    }

    /** Whether the sheet of a drawn page is turned dark with its colours kept in hue (Obsidian Vault). */
    public static boolean invertSheet(Context c) {
        return Prefs.palette(c) == OBSIDIAN;
    }

    /**
     * Turns the lightness of {@code argb} around and keeps hue and saturation: white becomes black,
     * a light yellow a dark yellow, a mid red stays red. Alpha is kept.
     */
    public static int invertTone(int argb) {
        float r = ((argb >> 16) & 0xFF) / 255f;
        float g = ((argb >> 8) & 0xFF) / 255f;
        float b = (argb & 0xFF) / 255f;
        float max = Math.max(r, Math.max(g, b));
        float min = Math.min(r, Math.min(g, b));
        float l = (max + min) / 2f;
        float h = 0f;
        float s = 0f;
        float d = max - min;
        if (d > 1e-6f) {
            s = d / (1f - Math.abs(2f * l - 1f));
            if (max == r) {
                h = ((g - b) / d) % 6f;
            } else if (max == g) {
                h = (b - r) / d + 2f;
            } else {
                h = (r - g) / d + 4f;
            }
            h *= 60f;
            if (h < 0f) {
                h += 360f;
            }
        }
        float l2 = 1f - l;
        float cc = (1f - Math.abs(2f * l2 - 1f)) * Math.min(1f, s);
        float x = cc * (1f - Math.abs((h / 60f) % 2f - 1f));
        float m = l2 - cc / 2f;
        float r1;
        float g1;
        float b1;
        if (h < 60f) {
            r1 = cc; g1 = x; b1 = 0f;
        } else if (h < 120f) {
            r1 = x; g1 = cc; b1 = 0f;
        } else if (h < 180f) {
            r1 = 0f; g1 = cc; b1 = x;
        } else if (h < 240f) {
            r1 = 0f; g1 = x; b1 = cc;
        } else if (h < 300f) {
            r1 = x; g1 = 0f; b1 = cc;
        } else {
            r1 = cc; g1 = 0f; b1 = x;
        }
        int ri = Math.round(Math.max(0f, Math.min(1f, r1 + m)) * 255f);
        int gi = Math.round(Math.max(0f, Math.min(1f, g1 + m)) * 255f);
        int bi = Math.round(Math.max(0f, Math.min(1f, b1 + m)) * 255f);
        return (argb & 0xFF000000) | (ri << 16) | (gi << 8) | bi;
    }

    /** {@code argb} with its alpha replaced. */
    public static int withAlpha(int argb, int alpha) {
        return (argb & 0x00FFFFFF) | ((alpha & 0xFF) << 24);
    }

    /** Opacity of the body of a glass panel, light and dark; the contrast test checks text on exactly these. */
    public static final int GLASS_BODY_LIGHT = 0xA8;
    public static final int GLASS_BODY_DARK = 0xCC;
    /** The sheen at the top-left of a glass panel. */
    public static final int GLASS_SHEEN_LIGHT = 0x99FFFFFF;
    public static final int GLASS_SHEEN_DARK = 0x14FFFFFF;

    /**
     * The fill of a "glass" panel: the surface colour made translucent, with a light sheen from the
     * top-left corner and a half-dp specular edge (a bright gradient that fades along the rim, like
     * light on the edge of glass). Where the system can blur what is behind a window (Android 12+,
     * see {@link Compat#blurBehind}) dialogs and floating panels get a real blur; the panels of the
     * screen itself show the window's soft gradient through their translucency. The body is opaque
     * enough that text stays readable on every version.
     */
    public static Drawable glassPanel(Context c, int surface, boolean dark, float radiusPx, int strokePx) {
        int sheen = dark ? GLASS_SHEEN_DARK : GLASS_SHEEN_LIGHT;
        int body = withAlpha(surface, dark ? GLASS_BODY_DARK : GLASS_BODY_LIGHT);
        GradientDrawable fill = new GradientDrawable(GradientDrawable.Orientation.TL_BR,
            new int[] {blend(body, sheen), body});
        fill.setCornerRadius(radiusPx);
        float edge = Math.max(1f, c.getResources().getDisplayMetrics().density * 0.5f);
        return new LayerDrawable(new Drawable[] {fill, new SpecularEdge(radiusPx, edge, dark)});
    }

    /** A rim of light: a thin gradient outline, bright at the top-left, faint in the middle, glinting at the bottom-right. */
    private static final class SpecularEdge extends Drawable {
        private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final RectF rect = new RectF();
        private final float radius;
        private final float width;
        private final int bright;
        private final int faint;

        SpecularEdge(float radius, float width, boolean dark) {
            this.radius = radius;
            this.width = width;
            this.bright = dark ? 0x99FFFFFF : 0xFFFFFFFF;
            this.faint = dark ? 0x1AFFFFFF : 0x40FFFFFF;
            paint.setStyle(Paint.Style.STROKE);
            paint.setStrokeWidth(width);
        }

        @Override
        protected void onBoundsChange(android.graphics.Rect b) {
            float half = width / 2f;
            rect.set(b.left + half, b.top + half, b.right - half, b.bottom - half);
            paint.setShader(new LinearGradient(b.left, b.top, b.right, b.bottom,
                new int[] {bright, faint, faint, bright & 0x80FFFFFF | 0x40000000}, new float[] {0f, 0.35f, 0.7f, 1f},
                Shader.TileMode.CLAMP));
        }

        @Override
        public void draw(Canvas canvas) {
            canvas.drawRoundRect(rect, radius, radius, paint);
        }

        @Override
        public void setAlpha(int alpha) {
            paint.setAlpha(alpha);
        }

        @Override
        public void setColorFilter(ColorFilter filter) {
            paint.setColorFilter(filter);
        }

        @Override
        public int getOpacity() {
            return PixelFormat.TRANSLUCENT;
        }
    }

    /** The window background under glass panels: a soft diagonal wash of the brand colour. */
    public static GradientDrawable glassBackdrop(int bg, int tint) {
        return new GradientDrawable(GradientDrawable.Orientation.TL_BR,
            new int[] {blend(bg, withAlpha(tint, 0x66)), bg, blend(bg, withAlpha(tint, 0x33))});
    }

    private static int blend(int base, int over) {
        float a = ((over >>> 24) & 0xFF) / 255f;
        int br = Color.red(base);
        int bg = Color.green(base);
        int bb = Color.blue(base);
        int ba = Color.alpha(base);
        int r = Math.round(Color.red(over) * a + br * (1f - a));
        int g = Math.round(Color.green(over) * a + bg * (1f - a));
        int b = Math.round(Color.blue(over) * a + bb * (1f - a));
        return Color.argb(ba, r, g, b);
    }
}
