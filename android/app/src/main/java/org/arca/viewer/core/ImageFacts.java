// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.core;

import java.util.Map;

/**
 * What the engine says about a picture besides its pixels: the size of the original, what was
 * done to it to show it (reduced, turned upright, first frame only) and what the file carried
 * that is not shown (location, camera, thumbnail, hidden metadata, bytes after its end).
 *
 * <p>Everything textual comes out of the document and is untrusted: the caller shows it through
 * {@link Fmt#safe}. This class has no Android dependencies; it is tested on a desktop JVM.
 */
public final class ImageFacts {
    // Bits of `flags`, mirroring arca_image::flag.
    public static final int HAS_ALPHA = 1;
    public static final int ANIMATED = 1 << 1;
    public static final int INTERLACED = 1 << 2;
    public static final int ICC_PROFILE = 1 << 3;
    public static final int XMP = 1 << 4;
    public static final int COMMENT = 1 << 5;
    public static final int TEXT_CHUNKS = 1 << 6;
    public static final int DAMAGED = 1 << 7;
    public static final int DOWNSCALED = 1 << 8;
    public static final int ORIENTED = 1 << 9;
    public static final int GRAYSCALE = 1 << 10;
    public static final int CMYK = 1 << 11;
    public static final int PALETTE = 1 << 12;

    /** The EXIF block of a photograph, reduced to facts. */
    public static final class Exif {
        public String make = "";
        public String model = "";
        public String software = "";
        public String datetime = "";
        public String artist = "";
        public String copyright = "";
        public String owner = "";
        public int orientation = 1;
        public boolean serial;
        public boolean makerNote;
        public boolean thumbnail;
        public boolean gps;
        /** "latitude, longitude" in degrees, or empty. */
        public String position = "";

        /** The facts about who or what made the picture, as one line (empty if there are none). */
        public String camera() {
            StringBuilder sb = new StringBuilder();
            append(sb, (make + " " + model).trim());
            append(sb, owner);
            append(sb, artist);
            return sb.toString();
        }

        private static void append(StringBuilder sb, String s) {
            if (!s.isEmpty()) {
                if (sb.length() > 0) {
                    sb.append(" · ");
                }
                sb.append(s);
            }
        }

        /** Whether the block says anything that a person may not want to share. */
        public boolean isPrivate() {
            return gps || serial || thumbnail || makerNote || !artist.isEmpty() || !owner.isEmpty()
                || !copyright.isEmpty();
        }
    }

    /** {@code "Png"}, {@code "Jpeg"}, {@code "Gif"} or {@code "Bmp"}. */
    public String format = "";
    /** Size of the picture that is shown. */
    public int width;
    public int height;
    /** Size of the original. */
    public int srcWidth;
    public int srcHeight;
    public int bits;
    public int flags;
    public int frames = 1;
    public long trailing;
    public long metadataBytes;
    /** The EXIF block, or {@code null}. */
    public Exif exif;

    public boolean has(int bit) {
        return (flags & bit) != 0;
    }

    /** Whether the file carries anything that is not shown and that the reader should know of. */
    public boolean hasHiddenParts() {
        return trailing > 0 || metadataBytes > 0 || (exif != null && exif.isPrivate());
    }

    /** Parses the JSON of {@code Native.imageInfo}. */
    public static ImageFacts parse(byte[] json) {
        Map<String, Object> root = Json.obj(Json.parse(json));
        ImageFacts f = new ImageFacts();
        f.format = Json.str(root, "format");
        f.width = Json.integer(root, "width");
        f.height = Json.integer(root, "height");
        f.srcWidth = Json.integer(root, "srcWidth");
        f.srcHeight = Json.integer(root, "srcHeight");
        f.bits = Json.integer(root, "bits");
        f.flags = Json.integer(root, "flags");
        f.frames = Math.max(1, Json.integer(root, "frames"));
        f.trailing = Json.num(root, "trailing");
        f.metadataBytes = Json.num(root, "metadataBytes");
        Object e = root.get("exif");
        if (e instanceof Map) {
            Map<String, Object> m = Json.obj(e);
            Exif x = new Exif();
            x.make = Json.str(m, "make");
            x.model = Json.str(m, "model");
            x.software = Json.str(m, "software");
            x.datetime = Json.str(m, "datetime");
            x.artist = Json.str(m, "artist");
            x.copyright = Json.str(m, "copyright");
            x.owner = Json.str(m, "owner");
            x.orientation = Math.max(1, Math.min(8, Json.integer(m, "orientation")));
            x.serial = Json.bool(m, "serial");
            x.makerNote = Json.bool(m, "makerNote");
            x.thumbnail = Json.bool(m, "thumbnail");
            x.gps = Json.bool(m, "gps");
            x.position = Json.str(m, "position");
            f.exif = x;
        }
        return f;
    }
}
