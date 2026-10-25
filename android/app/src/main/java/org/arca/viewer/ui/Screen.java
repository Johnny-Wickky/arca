// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import org.arca.viewer.MainActivity;
import org.arca.viewer.R;

import android.text.TextUtils;

/**
 * One screen of the app. A screen builds its whole view (header included) in {@link #build()};
 * the activity calls it again whenever the window changes shape (rotation, folding, resizing in
 * multi-window), so a screen must derive its layout from {@link Adaptive} each time and keep
 * its state in the {@code Session} or in its own fields, never in its views.
 */
public abstract class Screen {
    protected final MainActivity act;
    protected final Ui ui;

    protected Screen(MainActivity act) {
        this.act = act;
        this.ui = new Ui(act);
    }

    /** Builds the full view of the screen. */
    public abstract View build();

    /** Called when the session or the engine changed (main thread). */
    public void onSessionChanged() {}

    /** Called for the back gesture; return {@code true} if the screen consumed it. */
    public boolean onBack() {
        return false;
    }

    /** Called when the screen is removed from the stack. */
    public void onLeave() {}

    // ---- common building blocks -------------------------------------------------------------

    /** The header row: optional back arrow, title and action buttons. */
    protected View header(CharSequence title, boolean back, View... actions) {
        LinearLayout row = ui.hbox();
        row.setMinimumHeight(ui.dp(56));
        row.setPadding(ui.dp(back ? 4 : 16), 0, ui.dp(4), 0);
        if (back) {
            row.addView(ui.iconButton(R.drawable.ic_back, R.string.back, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    act.back();
                }
            }));
            row.addView(ui.hspace(4));
        }
        TextView t = ui.heading(title, Ui.TITLE);
        t.setSingleLine(true);
        t.setEllipsize(TextUtils.TruncateAt.END);
        row.addView(t, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        for (View a : actions) {
            row.addView(a);
        }
        return row;
    }

    /** A page: header on top, body filling the rest. */
    protected View page(View header, View body) {
        LinearLayout page = ui.vbox();
        page.addView(header, ui.fillWidth());
        page.addView(body, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));
        return page;
    }

    /** A scrolling, centred column of at most {@code maxDp} wide, padded by {@code padDp}. */
    protected View scrollColumn(View content, int maxDp, int padDp) {
        content.setPadding(ui.dp(padDp), ui.dp(padDp / 2f), ui.dp(padDp), ui.dp(padDp * 1.5f));
        ScrollView sv = new ScrollView(act);
        sv.setFillViewport(false);
        sv.setClipToPadding(false);
        sv.addView(ui.column(content, maxDp), new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.WRAP_CONTENT));
        return sv;
    }

    /** Centres {@code content} both ways (progress, errors). */
    protected View centered(View content, int maxDp) {
        FrameLayout f = new FrameLayout(act);
        Ui.MaxWidthFrame inner = new Ui.MaxWidthFrame(act, ui.dp(maxDp));
        inner.addView(content, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.CENTER));
        f.addView(inner, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.MATCH_PARENT, Gravity.CENTER_HORIZONTAL));
        return f;
    }

    /**
     * The page shown when an operation failed: what happened, in words, and the technical code.
     * {@code retry} may be {@code null}.
     */
    protected View failureView(CharSequence title, org.arca.viewer.Session.Failure f, final Runnable retry) {
        int status = f == null ? org.arca.viewer.Native.FAILED : f.status;
        boolean soft = status == org.arca.viewer.Native.ENCRYPTED || status == org.arca.viewer.Native.UNSUPPORTED;
        int fg = soft ? R.color.arca_unknown : R.color.arca_danger;
        LinearLayout col = ui.vbox();
        LinearLayout banner = ui.banner(soft ? R.color.arca_unknown_container : R.color.arca_danger_container);
        LinearLayout head = ui.hbox();
        head.addView(ui.icon(status == org.arca.viewer.Native.ENCRYPTED ? R.drawable.ic_lock
            : soft ? R.drawable.ic_verdict_unknown : R.drawable.ic_verdict_danger, fg, 32));
        head.addView(ui.hspace(12));
        TextView t = ui.heading(ui.str(Errors.title(status, org.arca.viewer.Session.get().isPicture())), Ui.TITLE);
        t.setTextColor(ui.color(fg));
        head.addView(t, ui.weight(1f));
        banner.addView(head);
        banner.addView(ui.space(8));
        banner.addView(ui.text(Errors.text(status, org.arca.viewer.Session.get().isPicture()), Ui.BODY));
        col.addView(banner, ui.fillWidth());
        col.addView(ui.space(12));
        String technical = Errors.code(status)
            + (f == null || f.technical.isEmpty() ? "" : ": " + org.arca.viewer.core.Fmt.safe(f.technical));
        col.addView(fact(ui.str(R.string.technical_label), technical, true));
        col.addView(ui.space(16));
        if (retry != null) {
            col.addView(ui.button(R.string.retry, Ui.PRIMARY, 0, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    retry.run();
                }
            }), ui.fillWidth());
            col.addView(ui.space(8));
        }
        col.addView(ui.button(R.string.back, retry == null ? Ui.PRIMARY : Ui.OUTLINE, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.back();
            }
        }), ui.fillWidth());
        return page(header(title, true), scrollColumn(col, 560, 20));
    }

    /** A labelled value row (caption above a value). */
    protected View fact(CharSequence label, CharSequence value, boolean mono) {
        LinearLayout l = ui.vbox();
        l.setPadding(0, ui.dp(6), 0, ui.dp(6));
        l.addView(ui.text(label, Ui.CAPTION));
        TextView v = ui.text(value, mono ? Ui.MONO : Ui.BODY);
        v.setTextIsSelectable(true);
        l.addView(v);
        return l;
    }
}
