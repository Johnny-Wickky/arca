// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.ColorFilter;
import android.graphics.LinearGradient;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.PixelFormat;
import android.graphics.RectF;
import android.graphics.Shader;
import android.graphics.drawable.Drawable;
import android.view.View;

import org.arca.viewer.Prefs;
import org.arca.viewer.core.Appearance;

import java.util.HashMap;
import java.util.Map;

/**
 * The colour palettes of the app: five light and five dark, in the order of
 * {@link Appearance}. The mode (System, Light, Dark) picks the group, the choice made inside the
 * group picks the palette; a palette replaces the surface, text and brand colours of whichever of
 * {@code values/colors.xml} and {@code values-night/colors.xml} matches its lightness and leaves
 * the verdict and severity colours to it (they are tuned for light and for dark surfaces already).
 *
 * <p>Colours are looked up by the name of the resource, so every screen keeps asking for a colour
 * resource through {@link Ui#color(int)} and nothing else changes.
 *
 * <p>Liquid Glass is a material, not a palette: it is drawn over whichever palette is in force (see
 * {@link #glassPanel} and {@link GlassBackdrop}) and is never consulted when a colour is chosen.
 */
public final class Theme {
    private Theme() {}

    /** Number of palettes: five light ones, then five dark ones. */
    public static final int COUNT = Appearance.COUNT;
    /** True black for OLED screens; the sheet of a page is inverted. The first of the dark ones. */
    public static final int OBSIDIAN = Appearance.PER_GROUP;

    /** The resource names a palette overrides, in the order of the rows of {@link #PALETTES}. */
    private static final String[] KEYS = {
        "arca_bg", "arca_surface", "arca_surface_variant", "arca_on_surface", "arca_on_surface_variant",
        "arca_outline", "arca_primary", "arca_on_primary", "arca_primary_container",
        "arca_on_primary_container", "arca_code_bg", "arca_hidden_text", "arca_link",
        "arca_search_hit", "arca_search_current", "arca_scrim",
    };

    private static final int[][] PALETTES = {
        // Alabaster Studio: paper white
        {0xFFFAF9F6, 0xFFFFFFFF, 0xFFEFEDE6, 0xFF1C1B18, 0xFF5A5750, 0xFFDAD6CC, 0xFF0B6B63, 0xFFFFFFFF,
            0xFFD6EFE9, 0xFF00201D, 0xFFF1EFE9, 0xFF6E6A62, 0xFF1A5A9E, 0xFFFFE9A8, 0xFFFFB347, 0x66000000},
        // Cashmere & Cappuccino: cream and coffee
        {0xFFF4EBDD, 0xFFFBF5EA, 0xFFE9DAC6, 0xFF33261B, 0xFF685240, 0xFFD2BEA4, 0xFF7B4A28, 0xFFFFFFFF,
            0xFFEFD7BD, 0xFF2C1507, 0xFFEDE0CD, 0xFF7C6A58, 0xFF1F5C94, 0xFFF6DC93, 0xFFF2A94A, 0x66000000},
        // Nordic Pearl: cool pearl blue
        {0xFFE9EFF6, 0xFFF8FAFD, 0xFFDAE4EF, 0xFF101820, 0xFF3F4D5C, 0xFFBFCCDA, 0xFF0A5FB8, 0xFFFFFFFF,
            0xFFD3E5FA, 0xFF001C3A, 0xFFE2E9F1, 0xFF5F6E7E, 0xFF0A58B0, 0xFFFFE9A8, 0xFFFFB347, 0x66000000},
        // Sage & Linen: pale sage and linen
        {0xFFEDEFE4, 0xFFF9FAF4, 0xFFDDE3D0, 0xFF1B2018, 0xFF4A5443, 0xFFC4CCB5, 0xFF3A6636, 0xFFFFFFFF,
            0xFFD8E7CD, 0xFF0C1F08, 0xFFE5E9D9, 0xFF697260, 0xFF1D5E8C, 0xFFFFE9A8, 0xFFFFB347, 0x66000000},
        // Rose Quartz Daylight: airy powder quartz
        {0xFFF8EDEE, 0xFFFFF8F8, 0xFFF0DCDE, 0xFF2A1B1E, 0xFF6A4B50, 0xFFE2C8CB, 0xFFA23A5A, 0xFFFFFFFF,
            0xFFF9D8E0, 0xFF3D0A1C, 0xFFF4E4E6, 0xFF7E6468, 0xFF1F5C96, 0xFFFFE9A8, 0xFFFFB347, 0x66000000},
        // Obsidian Vault: true black
        {0xFF000000, 0xFF0A0A0B, 0xFF161719, 0xFFE9EAEC, 0xFF9EA2A8, 0xFF26282C, 0xFF4FD1C5, 0xFF00201D,
            0xFF0C3B36, 0xFFCFF5F0, 0xFF111214, 0xFF7F858C, 0xFF8EC1FF, 0xFF5A4A12, 0xFF8A5C0C, 0xB3000000},
        // Sierra Blue & Slate: deep blue slate
        {0xFF0F1722, 0xFF16212F, 0xFF213043, 0xFFE3EAF3, 0xFF9DB0C6, 0xFF2D3E54, 0xFF7FB4F0, 0xFF06213D,
            0xFF1D3F66, 0xFFD2E6FB, 0xFF131D2A, 0xFF7F93AA, 0xFF8FC1FF, 0xFF5A4A12, 0xFF8A5C0C, 0xB3000000},
        // Emerald SecOps: dark emerald
        {0xFF05130E, 0xFF0A1F17, 0xFF123027, 0xFFD9F4E7, 0xFF8DB8A2, 0xFF1E4A3B, 0xFF34E0A1, 0xFF00281A,
            0xFF0F4A36, 0xFFC8F7E1, 0xFF0E2A20, 0xFF78A08C, 0xFF7FD0FF, 0xFF4F4A10, 0xFF8A5C0C, 0xB3000000},
        // Desert Titanium: warm graphite and bronze
        {0xFF1A1713, 0xFF24201B, 0xFF322C25, 0xFFEEE5D8, 0xFFBFB29E, 0xFF483F34, 0xFFDBA466, 0xFF2B1C08,
            0xFF4A3418, 0xFFF5DDBB, 0xFF2A251F, 0xFF9B8F7D, 0xFF9CC8F0, 0xFF5A4A12, 0xFF8A5C0C, 0xB3000000},
        // Midnight Amethyst: deep space, violet graphite
        {0xFF120F1C, 0xFF1A1626, 0xFF272037, 0xFFEAE6F4, 0xFFA9A0C0, 0xFF362E4A, 0xFFB79CF2, 0xFF20103F,
            0xFF3B2A66, 0xFFE7DDFF, 0xFF171321, 0xFF8A82A0, 0xFF9EC5FF, 0xFF5A4A12, 0xFF8A5C0C, 0xB3000000},
    };

    /** Whether each palette is dark, in the order of {@link #PALETTES}. */
    private static final boolean[] DARK = {false, false, false, false, false, true, true, true, true, true};

    /** Resource id to palette column, filled on first use ({@code -1}: not overridden). */
    private static final Map<Integer, Integer> COLUMN = new HashMap<>();

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

    /** Whether the window is in night mode: the mode chosen in Settings, or the system's. */
    public static boolean dark(Context c) {
        return (c.getResources().getConfiguration().uiMode & android.content.res.Configuration.UI_MODE_NIGHT_MASK)
            == android.content.res.Configuration.UI_MODE_NIGHT_YES;
    }

    /** The number of the palette in force: the group the mode picks, and the choice made within it. */
    public static int active(Context c) {
        return Appearance.palette(dark(c), Prefs.paletteLight(c), Prefs.paletteDark(c));
    }

    /** The colour for resource {@code res} under the palette in force. */
    public static int color(Context c, int res) {
        int col = column(c, res);
        if (col >= 0) {
            return PALETTES[active(c)][col];
        }
        return c.getColor(res);
    }

    /** Whether the Liquid Glass material is on. It changes how panels are drawn, never which colours are used. */
    public static boolean glass(Context c) {
        return Prefs.glass(c);
    }

    /** Whether the sheet of a drawn page is turned dark with its colours kept in hue (Obsidian Vault). */
    public static boolean invertSheet(Context c) {
        return active(c) == OBSIDIAN;
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

    // ---- Liquid Glass ---------------------------------------------------------------------------

    /*
     * The material is built from a few translucent layers over the palette in force: a body (the
     * surface colour, mostly opaque so that text stays readable on every backdrop), a tint of the
     * brand colour, a sheen from the top-left, a gloss along the top edge, an inner highlight, a
     * specular rim and a soft shadow. The test in arca-devtools (contrast.rs) reads these constants
     * from this file and checks the text colours of all ten palettes on the worst of the layers
     * over the strongest backdrop the screen can show ({@link #GLASS_WASH}).
     */

    /** Opacity of the body of a glass panel, light and dark. */
    public static final int GLASS_BODY_LIGHT = 0xD0;
    public static final int GLASS_BODY_DARK = 0xE4;
    /** The sheen at the top-left of a glass panel. */
    public static final int GLASS_SHEEN_LIGHT = 0x99FFFFFF;
    public static final int GLASS_SHEEN_DARK = 0x14FFFFFF;
    /** The gloss over the top part of a panel. */
    public static final int GLASS_GLOSS_LIGHT = 0x30FFFFFF;
    public static final int GLASS_GLOSS_DARK = 0x0CFFFFFF;
    /** Opacity of the brand colour laid over the lower right of a panel. */
    public static final int GLASS_TINT = 0x14;
    /** The most of the brand colour or the link colour the backdrop wash shows behind a panel. */
    public static final int GLASS_WASH = 0x68;

    /**
     * The glass material for a panel of {@code surface} colour and {@code brand} tint with rounded
     * corners of {@code radiusPx}. {@code rim} draws the light edge all around (bars that meet the
     * screen edge leave it off).
     */
    public static Drawable glassPanel(Context c, int surface, int brand, boolean dark, float radiusPx, boolean rim) {
        return new GlassPanel(c.getResources().getDisplayMetrics().density, surface, brand, dark, radiusPx, rim);
    }

    /**
     * Makes {@code v} a glass panel: the material as its background, a soft deep shadow under it
     * (an elevation tinted with the palette's darkest colour where the system allows) and an outline
     * that says the panel is opaque enough to hide the shadow beneath it, so the shadow shows only
     * outside the panel.
     */
    public static void applyGlass(final View v, int surface, int brand, boolean dark, final float radiusPx, boolean rim) {
        Context c = v.getContext();
        v.setBackground(glassPanel(c, surface, brand, dark, radiusPx, rim));
        float density = c.getResources().getDisplayMetrics().density;
        v.setElevation(dark ? 3f * density : 5f * density);
        v.setOutlineProvider(new android.view.ViewOutlineProvider() {
            @Override
            public void getOutline(View view, android.graphics.Outline outline) {
                outline.setRoundRect(0, 0, view.getWidth(), view.getHeight(), radiusPx);
                outline.setAlpha(1f);
            }
        });
        Compat.tintShadow(v, dark ? 0xFF000000 : withAlpha(darken(surface), 0xFF));
    }

    /** A colour with its lightness scaled down: the shadow colour of a light panel. */
    private static int darken(int argb) {
        return Color.rgb(Math.round(Color.red(argb) * 0.35f), Math.round(Color.green(argb) * 0.35f),
            Math.round(Color.blue(argb) * 0.4f));
    }

    /** The glass material, drawn in one piece. */
    private static final class GlassPanel extends Drawable {
        private final Paint fill = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Paint gloss = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Paint inner = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Paint edge = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Path glossShape = new Path();
        private final RectF box = new RectF();
        private final RectF innerBox = new RectF();
        private final RectF rimBox = new RectF();
        private final float radius;
        private final float line;
        private final boolean rim;
        private final boolean dark;
        private final int litBody;
        private final int body;
        private final int tinted;

        GlassPanel(float density, int surface, int brand, boolean dark, float radius, boolean rim) {
            this.radius = radius;
            this.dark = dark;
            this.rim = rim;
            this.line = Math.max(1f, density * 0.5f);
            int sheen = dark ? GLASS_SHEEN_DARK : GLASS_SHEEN_LIGHT;
            this.body = withAlpha(surface, dark ? GLASS_BODY_DARK : GLASS_BODY_LIGHT);
            this.litBody = blend(body, sheen);
            this.tinted = blend(body, withAlpha(brand, GLASS_TINT));
            inner.setStyle(Paint.Style.STROKE);
            inner.setStrokeWidth(line * 2f);
            edge.setStyle(Paint.Style.STROKE);
            edge.setStrokeWidth(line);
        }

        @Override
        protected void onBoundsChange(android.graphics.Rect b) {
            box.set(b);
            float half = line / 2f;
            rimBox.set(b.left + half, b.top + half, b.right - half, b.bottom - half);
            float in = line * 2f;
            innerBox.set(b.left + in, b.top + in, b.right - in, b.bottom - in);
            fill.setShader(new LinearGradient(b.left, b.top, b.right, b.bottom,
                new int[] {litBody, body, tinted}, new float[] {0f, 0.5f, 1f}, Shader.TileMode.CLAMP));
            // The gloss: the upper 42 % of the panel, brightest at the top, gone by the bottom of the band.
            float bandBottom = b.top + b.height() * 0.42f;
            int glossColor = dark ? GLASS_GLOSS_DARK : GLASS_GLOSS_LIGHT;
            gloss.setShader(new LinearGradient(0f, b.top, 0f, bandBottom,
                glossColor, glossColor & 0x00FFFFFF, Shader.TileMode.CLAMP));
            glossShape.reset();
            float r = Math.max(0f, radius - in);
            glossShape.addRoundRect(innerBox.left, innerBox.top, innerBox.right, bandBottom,
                new float[] {r, r, r, r, 0f, 0f, 0f, 0f}, Path.Direction.CW);
            // The inner highlight: a thin line just inside the edge, bright at the top-left and gone at the bottom-right.
            int top = dark ? 0x40FFFFFF : 0xCCFFFFFF;
            inner.setShader(new LinearGradient(b.left, b.top, b.right, b.bottom,
                new int[] {top, top & 0x00FFFFFF, top & 0x00FFFFFF}, new float[] {0f, 0.45f, 1f}, Shader.TileMode.CLAMP));
            // The specular rim: bright at the top-left, faint in the middle, a glint at the bottom-right.
            int bright = dark ? 0x99FFFFFF : 0xFFFFFFFF;
            int faint = dark ? 0x1AFFFFFF : 0x40FFFFFF;
            edge.setShader(new LinearGradient(b.left, b.top, b.right, b.bottom,
                new int[] {bright, faint, faint, (bright & 0x80FFFFFF) | 0x40000000}, new float[] {0f, 0.35f, 0.7f, 1f},
                Shader.TileMode.CLAMP));
        }

        @Override
        public void draw(Canvas canvas) {
            if (box.isEmpty()) {
                return;
            }
            canvas.drawRoundRect(box, radius, radius, fill);
            canvas.drawPath(glossShape, gloss);
            canvas.drawRoundRect(innerBox, Math.max(0f, radius - line * 2f), Math.max(0f, radius - line * 2f), inner);
            if (rim) {
                canvas.drawRoundRect(rimBox, radius, radius, edge);
            }
        }

        @Override
        public void setAlpha(int alpha) {
            fill.setAlpha(alpha);
            gloss.setAlpha(alpha);
            inner.setAlpha(alpha);
            edge.setAlpha(alpha);
        }

        @Override
        public void setColorFilter(ColorFilter filter) {
            fill.setColorFilter(filter);
            gloss.setColorFilter(filter);
            inner.setColorFilter(filter);
            edge.setColorFilter(filter);
        }

        @Override
        public int getOpacity() {
            return PixelFormat.TRANSLUCENT;
        }

        @Override
        public void getOutline(android.graphics.Outline outline) {
            outline.setRoundRect(getBounds(), radius);
            outline.setAlpha(1f);
        }
    }

    /** {@code over} laid on {@code base} (the alpha of {@code base} is kept). */
    static int blend(int base, int over) {
        float a = ((over >>> 24) & 0xFF) / 255f;
        int r = Math.round(Color.red(over) * a + Color.red(base) * (1f - a));
        int g = Math.round(Color.green(over) * a + Color.green(base) * (1f - a));
        int b = Math.round(Color.blue(over) * a + Color.blue(base) * (1f - a));
        return Color.argb(Color.alpha(base), r, g, b);
    }
}
