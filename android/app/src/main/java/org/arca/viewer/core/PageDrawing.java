// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * The drawing of one page as the engine describes it (see {@code render::page_json} in
 * {@code arca-engine}): paths, text runs, picture placeholders and clips, in points with the origin
 * at the top left. No Android dependencies; the canvas code is in {@code ui/PageView}.
 */
public final class PageDrawing {
    /** Primitive kinds. */
    public static final int PATH = 0;
    public static final int TEXT = 1;
    public static final int IMAGE = 2;
    public static final int SAVE = 3;
    public static final int RESTORE = 4;
    public static final int CLIP = 5;

    /** Text style bits. */
    public static final int BOLD = 1;
    public static final int ITALIC = 2;
    public static final int MONO = 4;
    public static final int SERIF = 8;

    /** Most primitives accepted on one page. */
    public static final int MAX_PRIMS = 200_000;

    /** One thing to draw; the fields that a kind does not use stay at their defaults. */
    public static final class Prim {
        public int kind;
        /** Path: {@code 0 x y}, {@code 1 x y}, {@code 2 x1 y1 x2 y2 x y}, {@code 3}. */
        public float[] ops;
        /** Colours as ARGB. */
        public int fill;
        public int stroke;
        public float width;
        public boolean evenOdd;
        /** Text: baseline start, size, direction (radians), width along the baseline. */
        public float x;
        public float y;
        public float size;
        public float angle;
        public float advance;
        public int style;
        public int color;
        public String text = "";
        /** Image: matrix {@code a b c d e f} of the unit square; clip: {@code x y w h}. */
        public float[] m;
        public String label = "";
        /** Image: what to ask the engine for to see it, or -1. */
        public int id = -1;
        /** Cached platform object (an {@code android.graphics.Path}); the view's business. */
        public Object cache;
    }

    public float width;
    public float height;
    public int index;
    public int pages;
    public boolean truncated;
    public boolean approximate;
    public final List<Prim> prims = new ArrayList<>();

    private static float f(Object o) {
        if (o instanceof Number) {
            float v = ((Number) o).floatValue();
            return Float.isNaN(v) || Float.isInfinite(v) ? 0f : v;
        }
        return 0f;
    }

    private static int argb(Object o) {
        return o instanceof Number ? (int) (((Number) o).longValue() & 0xFFFFFFFFL) : 0;
    }

    /** Parses the JSON of a page. Throws {@link Json.JsonException} or {@link IllegalArgumentException}. */
    public static PageDrawing parse(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        PageDrawing d = new PageDrawing();
        d.width = f(root.get("w"));
        d.height = f(root.get("h"));
        d.index = Json.integer(root, "index");
        d.pages = Json.integer(root, "pages");
        d.truncated = Json.bool(root, "truncated");
        d.approximate = Json.bool(root, "approx");
        if (!(d.width > 0f) || !(d.height > 0f) || d.width > 100000f || d.height > 100000f) {
            throw new IllegalArgumentException("bad page size");
        }
        List<Object> list = Json.arr(root.get("p"));
        if (list.size() > MAX_PRIMS) {
            throw new IllegalArgumentException("too many primitives");
        }
        for (Object o : list) {
            List<Object> a = Json.arr(o);
            if (a.isEmpty()) {
                continue;
            }
            Prim p = new Prim();
            String tag = Json.str(a.get(0), "");
            switch (tag) {
                case "P": {
                    if (a.size() < 6) {
                        continue;
                    }
                    p.kind = PATH;
                    p.fill = argb(a.get(1));
                    p.stroke = argb(a.get(2));
                    p.width = f(a.get(3));
                    p.evenOdd = f(a.get(4)) != 0f;
                    List<Object> ops = Json.arr(a.get(5));
                    p.ops = new float[ops.size()];
                    for (int i = 0; i < p.ops.length; i++) {
                        p.ops[i] = f(ops.get(i));
                    }
                    break;
                }
                case "T": {
                    if (a.size() < 9) {
                        continue;
                    }
                    p.kind = TEXT;
                    p.x = f(a.get(1));
                    p.y = f(a.get(2));
                    p.size = f(a.get(3));
                    p.angle = f(a.get(4));
                    p.advance = f(a.get(5));
                    p.style = (int) f(a.get(6));
                    p.color = argb(a.get(7));
                    p.text = Json.str(a.get(8), "");
                    break;
                }
                case "I": {
                    if (a.size() < 8) {
                        continue;
                    }
                    p.kind = IMAGE;
                    p.m = new float[6];
                    for (int i = 0; i < 6; i++) {
                        p.m[i] = f(a.get(1 + i));
                    }
                    p.label = Json.str(a.get(7), "");
                    if (a.size() > 8) {
                        long id = (long) f(a.get(8));
                        p.id = id >= 0 && id < Integer.MAX_VALUE ? (int) id : -1;
                    }
                    break;
                }
                case "S":
                    p.kind = SAVE;
                    break;
                case "R":
                    p.kind = RESTORE;
                    break;
                case "C": {
                    if (a.size() < 5) {
                        continue;
                    }
                    p.kind = CLIP;
                    p.m = new float[] {f(a.get(1)), f(a.get(2)), f(a.get(3)), f(a.get(4))};
                    break;
                }
                default:
                    continue;
            }
            d.prims.add(p);
        }
        return d;
    }

    /** How much text the page holds (for tests and diagnostics). */
    public int textRuns() {
        int n = 0;
        for (Prim p : prims) {
            if (p.kind == TEXT) {
                n++;
            }
        }
        return n;
    }
}
