// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

/**
 * Decides which of a document's colours the reading view may use. A document can colour its text
 * and shade it, but the reading view shows everything the document contains: a colour that would
 * make text hard to read on the viewer's own background (white text on a white page, black text
 * on the dark theme) is dropped and the text is shown in the viewer's colour.
 */
public final class Colors {
    private Colors() {}

    /** The least contrast of body text against the background (WCAG's level for large text). */
    public static final double MIN_TEXT_CONTRAST = 3.0;

    private static double channel(int c) {
        double v = c / 255.0;
        return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
    }

    /** Relative luminance of {@code 0xAARRGGBB} (alpha ignored), 0 to 1. */
    public static double luminance(int argb) {
        return 0.2126 * channel((argb >> 16) & 0xFF) + 0.7152 * channel((argb >> 8) & 0xFF)
            + 0.0722 * channel(argb & 0xFF);
    }

    /** The WCAG contrast ratio of two colours, 1 to 21. */
    public static double contrast(int a, int b) {
        double la = luminance(a);
        double lb = luminance(b);
        double hi = Math.max(la, lb);
        double lo = Math.min(la, lb);
        return (hi + 0.05) / (lo + 0.05);
    }

    /** {@code fg} when it is a colour (non-zero) readable on {@code background}, else {@code 0}: use the viewer's own. */
    public static int readable(int fg, int background) {
        if (fg == 0) {
            return 0;
        }
        return contrast(fg | 0xFF000000, background | 0xFF000000) >= MIN_TEXT_CONTRAST ? fg | 0xFF000000 : 0;
    }

    /**
     * {@code bg} when text of colour {@code text} stays readable over it and it does not swallow the
     * page (a highlight as dark as the dark theme's page is not a highlight), else {@code 0}.
     */
    public static int highlight(int bg, int text, int background) {
        if (bg == 0) {
            return 0;
        }
        int shade = bg | 0xFF000000;
        if (contrast(shade, text | 0xFF000000) < MIN_TEXT_CONTRAST) {
            return 0;
        }
        return shade;
    }
}
