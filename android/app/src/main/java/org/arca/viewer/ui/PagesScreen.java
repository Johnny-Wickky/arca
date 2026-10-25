// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.view.View;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import org.arca.viewer.MainActivity;
import org.arca.viewer.R;
import org.arca.viewer.Session;
import org.arca.viewer.core.PageDrawing;

/**
 * One page of the document drawn as the document lays it out. The drawing comes from the
 * sandbox as plain primitives (see {@link PageView}); there is no decoding of pictures or fonts.
 */
public final class PagesScreen extends Screen {
    /** The page shown; kept across rebuilds (rotation, folding). */
    private static int index;
    private static boolean started;

    public PagesScreen(MainActivity act) {
        super(act);
    }

    @Override
    public View build() {
        final Session s = Session.get();
        final Session.PageState st = s.page();
        if (!started) {
            started = true;
            index = 0;
            s.renderPage(0);
        }
        LinearLayout col = ui.vbox();
        col.setPadding(ui.dp(12), ui.dp(8), ui.dp(12), ui.dp(16));
        final PageDrawing d = st.drawing;
        int pages = d == null ? 0 : d.pages;

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
        col.addView(nav, ui.fillWidth());

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
            col.addView(ui.space(8));
            col.addView(new PageView(act, d, 0xFFD5DAE0, 0xFF46515E), ui.fillWidth());
        }
        ScrollView sv = new ScrollView(act);
        sv.addView(col);
        return page(header(ui.str(R.string.pages_title), true), centered(sv, 720));
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
