// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.widget.HorizontalScrollView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.SeekBar;
import android.widget.TextView;

import org.arca.viewer.MainActivity;
import org.arca.viewer.Prefs;
import org.arca.viewer.R;
import org.arca.viewer.Session;
import org.arca.viewer.core.PageDrawing;
import org.arca.viewer.core.Zoom;

/**
 * One page of the document drawn as the document lays it out. The drawing comes from the
 * sandbox as plain primitives (see {@link PageView}); there is no decoding of pictures or fonts.
 */
public final class PagesScreen extends Screen {
    /** The page shown; kept across rebuilds (rotation, folding). */
    private static int index;
    private static boolean started;
    /** Zoom of the sheet (1 = fits the width), kept across rebuilds; 25 % steps up to 400 %. */
    private static float sheetZoom = 1f;
    private static final float SHEET_ZOOM_STEP = 0.25f;
    private static final float SHEET_ZOOM_MAX = 4f;
    /** The page count of the drawing last shown, for the keyboard (0 until a page is drawn). */
    private static int pageCount;

    public PagesScreen(MainActivity act) {
        super(act);
    }

    /** The page controls, when the window is folded (see {@link #dock()}). */
    private View dockView;

    @Override
    public View dock() {
        return dockView;
    }

    @Override
    public View build() {
        final Session s = Session.get();
        final Session.PageState st = s.page();
        if (!started) {
            started = true;
            index = 0;
            sheetZoom = 1f;
            pageCount = 0;
            s.renderPage(0);
        }
        LinearLayout col = ui.vbox();
        col.setPadding(ui.dp(12), ui.dp(8), ui.dp(12), ui.dp(16));
        final PageDrawing d = st.drawing;
        int pages = d == null ? 0 : d.pages;
        pageCount = pages;

        LinearLayout nav = ui.hbox();
        View prev = ui.iconButton(R.drawable.ic_chevron_up, R.string.pages_prev, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                go(s, index - 1);
            }
        });
        View next = ui.iconButton(R.drawable.ic_chevron_down, R.string.pages_next, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                go(s, index + 1);
            }
        });
        prev.setEnabled(index > 0 && !st.running);
        next.setEnabled(d != null && index + 1 < pages && !st.running);
        TextView pos = ui.text(d == null ? "" : ui.str(R.string.pages_position, index + 1, pages), Ui.SMALL);
        nav.addView(prev);
        nav.addView(pos, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f));
        pos.setGravity(android.view.Gravity.CENTER);
        nav.addView(next);
        if (Hinge.hasDock()) {
            // Folded window (tabletop or book): the page is on one part, the page controls on the other.
            dockView = dockPane(nav);
        } else {
            col.addView(nav, ui.fillWidth());
        }

        if (st.running) {
            col.addView(ui.text(R.string.pages_loading, Ui.BODY));
        } else if (st.failure != null) {
            col.addView(ui.text(Errors.text(st.failure.status, org.arca.viewer.Session.get().isPicture()), Ui.BODY));
            col.addView(ui.text(R.string.pages_failed, Ui.SMALL));
        } else if (d != null) {
            if (d.truncated) {
                col.addView(ui.text(R.string.pages_note_truncated, Ui.SMALL));
            }
            col.addView(ui.text(R.string.pages_note_approx, Ui.CAPTION));
            org.arca.viewer.core.Model.Report rep = org.arca.viewer.Session.get().report();
            if (rep != null && org.arca.viewer.core.Capabilities.isVisio(rep.format)) {
                col.addView(ui.text(R.string.pages_note_visio, Ui.CAPTION));
            }
            col.addView(ui.space(8));
            PageView sheet = new PageView(act, d, 0xFFD5DAE0, 0xFF46515E);
            if (sheetZoom > 1f) {
                // Zoomed: the sheet is wider than the window and scrolls sideways.
                HorizontalScrollView sideways = new HorizontalScrollView(act);
                int baseDp = Math.max(240, Math.min(Adaptive.widthDp(act), 720) - 24);
                sideways.addView(sheet, new FrameLayout.LayoutParams(ui.dp(baseDp * sheetZoom),
                    ViewGroup.LayoutParams.WRAP_CONTENT));
                col.addView(sideways, ui.fillWidth());
            } else {
                col.addView(sheet, ui.fillWidth());
            }
        }
        ScrollView sv = new ScrollView(act);
        sv.addView(col);
        LinearLayout body = ui.vbox();
        body.addView(centered(sv, 720), new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));
        if (d != null && pages > 0) {
            body.addView(bottomBar(s, st, pages), ui.fillWidth());
        }
        return page(header(ui.str(R.string.pages_title), true), body);
    }

    /**
     * The bar at the bottom of the page view: a slider over the pages, the position, the way back
     * to the reading view and the zoom of the sheet. Everything in it also works from a keyboard
     * (PageUp / PageDown, Ctrl + / Ctrl -).
     */
    private View bottomBar(final Session s, final Session.PageState st, final int pages) {
        LinearLayout bar = ui.vbox();
        bar.setBackgroundColor(ui.color(R.color.arca_surface));
        bar.setPadding(ui.dp(12), ui.dp(6), ui.dp(12), ui.dp(8));

        LinearLayout top = ui.hbox();
        final TextView pos = ui.text(ui.str(R.string.pages_position, index + 1, pages), Ui.SMALL);
        SeekBar slider = new SeekBar(act);
        slider.setMax(Math.max(0, pages - 1));
        slider.setProgress(Math.min(index, Math.max(0, pages - 1)));
        slider.setEnabled(pages > 1 && !st.running);
        slider.setContentDescription(ui.str(R.string.pages_slider));
        slider.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override
            public void onProgressChanged(SeekBar b, int progress, boolean fromUser) {
                if (fromUser) {
                    pos.setText(ui.str(R.string.pages_position, progress + 1, pages));
                }
            }

            @Override
            public void onStartTrackingTouch(SeekBar b) {
                // nothing: the page changes when the slider is let go
            }

            @Override
            public void onStopTrackingTouch(SeekBar b) {
                if (b.getProgress() != index) {
                    go(s, b.getProgress());
                }
            }
        });
        top.addView(slider, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        top.addView(ui.hspace(8));
        top.addView(pos);
        bar.addView(top, ui.fillWidth());

        LinearLayout row = ui.hbox();
        row.setGravity(android.view.Gravity.CENTER_VERTICAL);
        row.addView(ui.button(R.string.pages_bar_text, Ui.TONAL, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.back();
            }
        }));
        // Pictures in the text: on, the pictures of the page are decoded in the sandbox as soon as it
        // is drawn; off, each is a grey box to tap. The same switch as in the reading view.
        final boolean pictures = Prefs.autoImages(act);
        row.addView(ui.iconButton(pictures ? R.drawable.ic_eye : R.drawable.ic_eye_off,
            pictures ? R.string.pictures_hide : R.string.pictures_show, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    Prefs.setAutoImages(act, !pictures);
                    act.refresh();
                }
            }));
        row.addView(new View(act), new LinearLayout.LayoutParams(0, 1, 1f));
        row.addView(ui.iconButton(R.drawable.ic_minus, R.string.zoom_out, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                onZoom(-1);
            }
        }));
        TextView zoom = ui.heading(ui.str(R.string.zoom_value, Math.round(sheetZoom * 100f)), Ui.LABEL);
        zoom.setGravity(android.view.Gravity.CENTER);
        row.addView(zoom, new LinearLayout.LayoutParams(ui.dp(64), ViewGroup.LayoutParams.WRAP_CONTENT));
        row.addView(ui.iconButton(R.drawable.ic_plus, R.string.zoom_in, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                onZoom(1);
            }
        }));
        bar.addView(row, ui.fillWidth());
        return bar;
    }

    @Override
    public boolean onZoom(int steps) {
        float next = Math.max(1f, Math.min(SHEET_ZOOM_MAX, sheetZoom + steps * SHEET_ZOOM_STEP));
        if (next != sheetZoom) {
            sheetZoom = next;
            act.refresh();
        }
        return true;
    }

    @Override
    public boolean onPage(int delta) {
        Session s = Session.get();
        int to = index + delta;
        if (s.page().running || to < 0 || to >= pageCount) {
            return true;
        }
        go(s, to);
        return true;
    }

    private void go(Session s, int to) {
        if (to < 0) {
            return;
        }
        index = to;
        s.renderPage(to);
    }

    @Override
    public void onSessionChanged() {
        act.refresh();
    }

    @Override
    public void onLeave() {
        started = false;
        Session.get().closePage();
    }
}
