// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

/**
 * Which colour palette is in force: the mode (System, Light, Dark) decides whether the light or the
 * dark group of palettes applies, and each group remembers its own choice, so the two steps never
 * disagree. Liquid Glass is not part of this: it is a material laid over whichever palette is
 * chosen and has no say in the choice. Pure Java, tested on a desktop JVM.
 *
 * <p>The global palette number runs over the light group first, then the dark one: 0 to
 * {@link #PER_GROUP}{@code - 1} are the light palettes and {@link #PER_GROUP} to {@link #COUNT}
 * {@code - 1} the dark ones, the order of the rows of {@code ui.Theme}.
 */
public final class Appearance {
    private Appearance() {}

    public static final int MODE_SYSTEM = 0;
    public static final int MODE_LIGHT = 1;
    public static final int MODE_DARK = 2;

    /** Palettes in each of the two groups. */
    public static final int PER_GROUP = 5;
    /** Palettes in all. */
    public static final int COUNT = 2 * PER_GROUP;

    /** Whether the dark group applies for {@code mode} when the system itself is (not) dark. */
    public static boolean dark(int mode, boolean systemDark) {
        switch (mode) {
            case MODE_LIGHT:
                return false;
            case MODE_DARK:
                return true;
            default:
                return systemDark;
        }
    }

    /** A choice within a group, brought into {@code 0 .. PER_GROUP - 1}. */
    public static int choice(int v) {
        return v < 0 ? 0 : (v >= PER_GROUP ? PER_GROUP - 1 : v);
    }

    /** The global palette number for the group in force and the choices remembered for each group. */
    public static int palette(boolean dark, int lightChoice, int darkChoice) {
        return dark ? PER_GROUP + choice(darkChoice) : choice(lightChoice);
    }

    /** Whether the global palette number {@code palette} is in the dark group. */
    public static boolean isDarkPalette(int palette) {
        return palette >= PER_GROUP;
    }

    /**
     * What the single palette setting of 0.5 and earlier (0: the classic resource colours, 1 to 6:
     * Alabaster, Obsidian, Cashmere, Sierra, Emerald, Desert) becomes: {@code {mode, light choice,
     * dark choice}}. A named palette used to force light or dark by itself, so the mode is set to the
     * group it was in; the old Sierra was a light palette, and its successor in that group is Nordic
     * Pearl. Classic leaves the mode as it was.
     */
    public static int[] fromLegacy(int legacyPalette, int mode) {
        switch (legacyPalette) {
            case 1:
                return new int[] {MODE_LIGHT, 0, 0};
            case 2:
                return new int[] {MODE_DARK, 0, 0};
            case 3:
                return new int[] {MODE_LIGHT, 1, 0};
            case 4:
                return new int[] {MODE_LIGHT, 2, 0};
            case 5:
                return new int[] {MODE_DARK, 0, 2};
            case 6:
                return new int[] {MODE_DARK, 0, 3};
            default:
                return new int[] {mode, 0, 0};
        }
    }
}
