// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.app.Dialog;
import android.graphics.drawable.ColorDrawable;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.view.WindowManager;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import org.arca.viewer.MainActivity;
import org.arca.viewer.Prefs;
import org.arca.viewer.R;
import org.arca.viewer.Session;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.ImageFacts;

import java.util.ArrayList;
import java.util.List;

/**
 * A picture of the document on the whole screen, with pinch zoom, and what is known about it.
 *
 * <p>The pixels are the ones the sandbox built ({@link Session#loadImage}); this dialog only
 * paints them. Like the main window it keeps the screen out of screenshots unless the reader
 * turned that protection off.
 */
final class ImageViewer {
    private ImageViewer() {}

    /** Opens picture {@code id}, loading it first if it is not at hand. */
    static void show(final MainActivity act, final int id, final String title) {
        final Ui ui = new Ui(act);
        final Dialog dialog = new Dialog(act);
        dialog.requestWindowFeature(Window.FEATURE_NO_TITLE);
        Window w = dialog.getWindow();
        if (w != null) {
            w.setBackgroundDrawable(new ColorDrawable(0xFF121212));
            w.setLayout(WindowManager.LayoutParams.MATCH_PARENT, WindowManager.LayoutParams.MATCH_PARENT);
            if (Prefs.secureScreen(act)) {
                w.addFlags(WindowManager.LayoutParams.FLAG_SECURE);
            }
        }
        final FrameLayout root = new FrameLayout(act);
        dialog.setContentView(root);
        dialog.setCanceledOnTouchOutside(false);
        final Session s = Session.get();
        final Session.ImageCallback cb = new Session.ImageCallback() {
            @Override
            public void onImage(int loaded, Session.ImageResult r) {
                if (dialog.isShowing()) {
                    fill(act, ui, root, dialog, title, r);
                }
            }
        };
        Session.ImageResult have = s.cachedImage(id);
        if (have != null) {
            fill(act, ui, root, dialog, title, have);
        } else {
            fill(act, ui, root, dialog, title, null);
            s.loadImage(id, cb);
        }
        dialog.show();
    }

    private static void fill(final MainActivity act, final Ui ui, FrameLayout root, final Dialog dialog,
                             String title, Session.ImageResult r) {
        root.removeAllViews();
        LinearLayout col = ui.vbox();
        LinearLayout bar = ui.hbox();
        bar.setPadding(ui.dp(4), ui.dp(4), ui.dp(12), ui.dp(4));
        bar.addView(ui.iconButton(R.drawable.ic_back, R.string.back, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                dialog.dismiss();
            }
        }));
        TextView head = ui.text(title == null || title.isEmpty() ? ui.str(R.string.image_title) : Fmt.clip(Fmt.safe(title), 80), Ui.TITLE);
        head.setTextColor(0xFFFFFFFF);
        head.setSingleLine(true);
        bar.addView(head, ui.weight(1f));
        col.addView(bar, ui.fillWidth());
        if (r == null) {
            TextView t = ui.text(R.string.image_loading, Ui.BODY);
            t.setTextColor(0xFFE0E0E0);
            t.setGravity(Gravity.CENTER);
            t.setPadding(ui.dp(24), ui.dp(48), ui.dp(24), ui.dp(24));
            col.addView(t, ui.fillWidth());
        } else if (r.bitmap == null) {
            int status = r.failure == null ? 0 : r.failure.status;
            TextView t = ui.text(status == org.arca.viewer.Native.UNSUPPORTED ? ui.str(R.string.image_unsupported) : ui.str(Errors.text(status)), Ui.BODY);
            t.setTextColor(0xFFE0E0E0);
            t.setPadding(ui.dp(24), ui.dp(32), ui.dp(24), ui.dp(8));
            col.addView(t, ui.fillWidth());
            TextView f = ui.text(R.string.image_failed, Ui.SMALL);
            f.setTextColor(0xFFB0B0B0);
            f.setPadding(ui.dp(24), 0, ui.dp(24), ui.dp(24));
            col.addView(f, ui.fillWidth());
        } else {
            ZoomImageView zoom = new ZoomImageView(act, r.bitmap);
            zoom.setContentDescription(ui.str(R.string.image_description, title == null ? "" : Fmt.clip(Fmt.safe(title), 80)));
            col.addView(zoom, ui.weight(1f));
            TextView hint = ui.text(R.string.image_zoom_hint, Ui.CAPTION);
            hint.setTextColor(0xFFB0B0B0);
            hint.setGravity(Gravity.CENTER);
            hint.setPadding(ui.dp(12), ui.dp(4), ui.dp(12), ui.dp(2));
            col.addView(hint, ui.fillWidth());
            col.addView(infoPanel(ui, r.facts), ui.fillWidth());
        }
        root.addView(col, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
    }

    /** The lines about what was done to the picture and what the file carried, in a scroller of limited height. */
    private static View infoPanel(Ui ui, ImageFacts f) {
        LinearLayout lines = ui.vbox();
        lines.setPadding(ui.dp(16), ui.dp(4), ui.dp(16), ui.dp(12));
        for (String n : processing(ui, f)) {
            TextView t = ui.text(n, Ui.SMALL);
            t.setTextColor(0xFFE0E0E0);
            lines.addView(t, ui.fillWidth());
        }
        List<String> hidden = hiddenParts(ui, f);
        if (!hidden.isEmpty()) {
            lines.addView(ui.space(6));
            TextView title = ui.text(R.string.image_info_hidden_title, Ui.LABEL);
            title.setTextColor(0xFFFFD27A);
            lines.addView(title, ui.fillWidth());
            for (String n : hidden) {
                TextView t = ui.text("• " + n, Ui.SMALL);
                t.setTextColor(0xFFE0E0E0);
                lines.addView(t, ui.fillWidth());
            }
        }
        ScrollView sv = new ScrollView(ui.context) {
            @Override
            protected void onMeasure(int widthSpec, int heightSpec) {
                int cap = (int) (getResources().getDisplayMetrics().heightPixels * 0.33f);
                super.onMeasure(widthSpec, MeasureSpec.makeMeasureSpec(cap, MeasureSpec.AT_MOST));
            }
        };
        sv.addView(lines);
        return sv;
    }

    /** What was done to the picture to show it. */
    static List<String> processing(Ui ui, ImageFacts f) {
        List<String> out = new ArrayList<>();
        out.add(ui.str(R.string.image_info_original, f.srcWidth, f.srcHeight));
        if (f.has(ImageFacts.DOWNSCALED)) {
            out.add(ui.str(R.string.image_info_reduced, f.width, f.height));
        }
        if (f.has(ImageFacts.ORIENTED)) {
            out.add(ui.str(R.string.image_info_upright));
        }
        if (f.has(ImageFacts.ANIMATED) && f.frames > 1) {
            out.add(ui.str(R.string.image_info_animation, f.frames));
        }
        if (f.has(ImageFacts.CMYK)) {
            out.add(ui.str(R.string.image_info_cmyk));
        }
        if (f.has(ImageFacts.DAMAGED)) {
            out.add(ui.str(R.string.image_info_damaged));
        }
        return out;
    }

    /** What the file carried that is neither shown nor kept. */
    static List<String> hiddenParts(Ui ui, ImageFacts f) {
        List<String> out = new ArrayList<>();
        ImageFacts.Exif e = f.exif;
        if (e != null) {
            if (e.gps) {
                out.add(ui.str(R.string.image_info_gps, e.position.isEmpty() ? "?" : Fmt.clip(Fmt.safe(e.position), 40)));
            }
            String camera = e.camera();
            if (!camera.isEmpty()) {
                out.add(ui.str(R.string.image_info_camera, Fmt.clip(Fmt.safe(camera), 120)));
            }
            if (e.serial) {
                out.add(ui.str(R.string.image_info_serial));
            }
            if (e.thumbnail) {
                out.add(ui.str(R.string.image_info_thumbnail));
            }
        }
        if (f.metadataBytes > 0) {
            out.add(ui.str(R.string.image_info_metadata, Fmt.size(f.metadataBytes)));
        }
        if (f.trailing > 0) {
            out.add(ui.str(R.string.image_info_trailing, Fmt.size(f.trailing)));
        }
        return out;
    }
}
