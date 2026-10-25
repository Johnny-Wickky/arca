// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.res.ColorStateList;
import android.view.Gravity;
import android.view.View;
import android.widget.CheckBox;
import android.widget.CompoundButton;
import android.widget.LinearLayout;
import android.widget.ProgressBar;
import android.widget.TextView;

import org.arca.viewer.MainActivity;
import org.arca.viewer.Native;
import org.arca.viewer.R;
import org.arca.viewer.Session;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Capabilities;
import org.arca.viewer.core.Model;

/**
 * Builds a sterile copy of an Office document: the document rebuilt without macros, embedded
 * objects, external references or other active content, then scanned again before it is
 * offered. The original file is never touched; the copy is written only where the person
 * chooses with the system file picker.
 */
public final class SanitizeScreen extends Screen {
    /** Shown at most this many changes before "and N more". */
    private static final int MAX_ACTIONS = 100;

    // Kept while the app lives, so a rotation or a fold does not reset the choices.
    private static int selected = Native.SANITIZE_STRIP_METADATA | Native.SANITIZE_REMOVE_CUSTOM_XML;
    private static boolean actionsOpen;

    public SanitizeScreen(MainActivity act) {
        super(act);
    }

    @Override
    public View build() {
        final Session s = Session.get();
        if (s.phase() != Session.Phase.READY) {
            act.getWindow().getDecorView().post(new Runnable() {
                @Override
                public void run() {
                    act.home();
                }
            });
            return new View(act);
        }
        Session.SanitizeState st = s.sanitize();
        if (st.running) {
            return working();
        }
        if (st.failure != null) {
            return failureView(ui.str(R.string.sanitize_title), st.failure, new Runnable() {
                @Override
                public void run() {
                    Session.get().startSanitize(flagsFor(Session.get()));
                }
            });
        }
        if (st.ready()) {
            return result(s, st);
        }
        return options(s);
    }

    /** The options to send: the size of a copy only means something for a picture. */
    private static int flagsFor(Session s) {
        String format = s.report() == null ? "" : s.report().format;
        return Capabilities.isPicture(format) ? selected : selected & ~Native.SANITIZE_FULL_SIZE;
    }

    // ---- options ----------------------------------------------------------------------------

    private View options(final Session s) {
        LinearLayout col = ui.vbox();
        col.addView(ui.text(R.string.sanitize_intro, Ui.BODY), ui.fillWidth());
        col.addView(ui.space(12));

        // What a copy of this kind of file cannot keep, and which options mean something for it.
        String format = s.report() == null ? "" : s.report().format;
        boolean picture = Capabilities.isPicture(format);

        LinearLayout always = ui.card();
        always.addView(ui.heading(ui.str(R.string.sanitize_always_title), Ui.LABEL));
        always.addView(ui.space(4));
        always.addView(ui.text(picture ? R.string.sanitize_always_text_picture : R.string.sanitize_always_text, Ui.SMALL));
        col.addView(always, ui.fillWidth());
        col.addView(ui.space(12));

        boolean pdf = "Pdf".equals(format);
        boolean odf = format.startsWith("Od");
        boolean legacy = "Doc".equals(format) || "Xls".equals(format) || "Ppt".equals(format) || "Rtf".equals(format);
        if (pdf || odf || legacy || picture) {
            int note = picture ? R.string.sanitize_note_picture : pdf ? R.string.sanitize_note_pdf : legacy ? R.string.sanitize_note_legacy : R.string.sanitize_note_odf;
            col.addView(ui.text(note, Ui.SMALL), ui.fillWidth());
            col.addView(ui.space(12));
        }

        // A picture has nothing to choose: it is always written anew from its pixels.
        LinearLayout opt = ui.card();
        opt.addView(ui.heading(ui.str(R.string.sanitize_options_title), Ui.LABEL));
        opt.addView(ui.space(4));
        opt.addView(option(R.string.opt_metadata, R.string.opt_metadata_desc, Native.SANITIZE_STRIP_METADATA));
        if (!pdf && !odf && !legacy) {
            opt.addView(option(R.string.opt_custom_xml, R.string.opt_custom_xml_desc, Native.SANITIZE_REMOVE_CUSTOM_XML));
        }
        opt.addView(option(R.string.opt_comments, R.string.opt_comments_desc, Native.SANITIZE_REMOVE_COMMENTS));
        if (!pdf) {
            opt.addView(option(R.string.opt_revisions, R.string.opt_revisions_desc, Native.SANITIZE_ACCEPT_REVISIONS));
        }
        if (!picture) {
            col.addView(opt, ui.fillWidth());
            col.addView(ui.space(12));
        } else {
            LinearLayout size = ui.card();
            size.addView(ui.heading(ui.str(R.string.sanitize_size_title), Ui.LABEL));
            size.addView(ui.space(4));
            size.addView(option(R.string.opt_full_size, R.string.opt_full_size_desc, Native.SANITIZE_FULL_SIZE));
            col.addView(size, ui.fillWidth());
            col.addView(ui.space(12));
        }

        col.addView(ui.text(R.string.sanitize_original_untouched, Ui.CAPTION), ui.fillWidth());
        col.addView(ui.space(16));
        col.addView(ui.button(R.string.sanitize_start, Ui.PRIMARY, R.drawable.ic_sanitize, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                s.startSanitize(flagsFor(s));
            }
        }), ui.fillWidth());
        return page(header(ui.str(R.string.sanitize_title), true), scrollColumn(col, 640, Adaptive.padding(act)));
    }

    private View option(int titleRes, int descRes, final int flag) {
        LinearLayout row = ui.vbox();
        row.setPadding(0, ui.dp(4), 0, ui.dp(4));
        CheckBox cb = new CheckBox(act);
        cb.setText(titleRes);
        cb.setTextColor(ui.color(R.color.arca_on_surface));
        cb.setButtonTintList(ColorStateList.valueOf(ui.color(R.color.arca_primary)));
        cb.setMinHeight(ui.dp(Ui.TOUCH));
        cb.setChecked((selected & flag) != 0);
        cb.setOnCheckedChangeListener(new CompoundButton.OnCheckedChangeListener() {
            @Override
            public void onCheckedChanged(CompoundButton b, boolean checked) {
                selected = checked ? selected | flag : selected & ~flag;
            }
        });
        row.addView(cb, ui.fillWidth());
        TextView desc = ui.text(descRes, Ui.CAPTION);
        desc.setPadding(ui.dp(40), 0, 0, ui.dp(4));
        row.addView(desc, ui.fillWidth());
        return row;
    }

    // ---- working ----------------------------------------------------------------------------

    private View working() {
        LinearLayout col = ui.vbox();
        col.setGravity(Gravity.CENTER_HORIZONTAL);
        col.setPadding(ui.dp(24), ui.dp(24), ui.dp(24), ui.dp(24));
        ProgressBar bar = new ProgressBar(act);
        bar.setIndeterminate(true);
        bar.setIndeterminateTintList(ColorStateList.valueOf(ui.color(R.color.arca_primary)));
        col.addView(bar, new LinearLayout.LayoutParams(ui.dp(56), ui.dp(56)));
        col.addView(ui.space(16));
        TextView t = ui.heading(ui.str(R.string.sanitize_building), Ui.TITLE);
        t.setGravity(Gravity.CENTER);
        col.addView(t, ui.fillWidth());
        col.addView(ui.space(6));
        TextView note = ui.text(R.string.sanitize_building_note, Ui.CAPTION);
        note.setGravity(Gravity.CENTER);
        col.addView(note, ui.fillWidth());
        col.addView(ui.space(20));
        col.addView(ui.button(R.string.cancel, Ui.OUTLINE, R.drawable.ic_close, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Session.get().cancel();
            }
        }));
        return page(header(ui.str(R.string.sanitize_title), true), centered(col, 480));
    }

    // ---- result -----------------------------------------------------------------------------

    private View result(final Session s, final Session.SanitizeState st) {
        final Model.SanitizedInfo info = st.info;
        boolean clean = info.verdict == Model.Verdict.CLEAN;
        int fg = clean ? R.color.arca_safe : R.color.arca_unknown;
        int bg = clean ? R.color.arca_safe_container : R.color.arca_unknown_container;

        LinearLayout col = ui.vbox();
        LinearLayout banner = ui.banner(bg);
        LinearLayout head = ui.hbox();
        head.addView(ui.icon(clean ? R.drawable.ic_verdict_clean : R.drawable.ic_verdict_unknown, fg, 32));
        head.addView(ui.hspace(12));
        TextView title = ui.heading(ui.str(clean ? R.string.sanitize_done_clean_title : R.string.sanitize_done_unverified_title), Ui.TITLE);
        title.setTextColor(ui.color(fg));
        head.addView(title, ui.weight(1f));
        banner.addView(head);
        banner.addView(ui.space(8));
        banner.addView(ui.text(clean ? R.string.sanitize_done_clean_text : R.string.sanitize_done_unverified_text, Ui.BODY));
        col.addView(banner, ui.fillWidth());
        col.addView(ui.space(12));

        col.addView(fact(ui.str(R.string.format_label), Labels.format(act, info.format), false));
        col.addView(fact(ui.str(R.string.sanitize_size), Fmt.size(info.size), false));
        if (!info.sha256.isEmpty()) {
            col.addView(fact(ui.str(R.string.sha256_label), Fmt.grouped(info.sha256), true));
        }
        col.addView(ui.space(8));
        col.addView(actionsCard(info), ui.fillWidth());
        col.addView(ui.space(16));

        // Saving.
        if (st.saving) {
            LinearLayout row = ui.hbox();
            row.setGravity(Gravity.CENTER_VERTICAL);
            ProgressBar bar = new ProgressBar(act);
            bar.setIndeterminate(true);
            bar.setIndeterminateTintList(ColorStateList.valueOf(ui.color(R.color.arca_primary)));
            row.addView(bar, new LinearLayout.LayoutParams(ui.dp(28), ui.dp(28)));
            row.addView(ui.hspace(12));
            row.addView(ui.text(R.string.sanitize_saving, Ui.BODY), ui.weight(1f));
            col.addView(row, ui.fillWidth());
        } else {
            if (st.saved) {
                LinearLayout ok = ui.banner(R.color.arca_safe_container);
                ok.addView(ui.text(ui.str(R.string.sanitize_saved, Fmt.size(st.savedBytes)), Ui.BODY));
                col.addView(ok, ui.fillWidth());
                col.addView(ui.space(12));
            } else if (st.saveFailure != null) {
                LinearLayout bad = ui.banner(R.color.arca_danger_container);
                TextView t = ui.heading(ui.str(R.string.sanitize_save_failed), Ui.LABEL);
                t.setTextColor(ui.color(R.color.arca_danger));
                bad.addView(t);
                bad.addView(ui.space(4));
                bad.addView(ui.text(Errors.text(st.saveFailure.status, org.arca.viewer.Session.get().isPicture()), Ui.BODY));
                bad.addView(ui.space(4));
                String tech = Errors.code(st.saveFailure.status)
                    + (st.saveFailure.technical.isEmpty() ? "" : ": " + Fmt.safe(st.saveFailure.technical));
                TextView mono = ui.text(tech, Ui.MONO);
                mono.setTextIsSelectable(true);
                bad.addView(mono);
                col.addView(bad, ui.fillWidth());
                col.addView(ui.space(12));
            }
            col.addView(ui.button(st.saved ? R.string.sanitize_save_again : R.string.sanitize_save, st.saved ? Ui.OUTLINE : Ui.PRIMARY,
                R.drawable.ic_save, new View.OnClickListener() {
                    @Override
                    public void onClick(View v) {
                        String ext = Fmt.extensionOf(info.format);
                        act.pickSaveLocation(Fmt.sterileName(s.name(), ext), Fmt.mimeOf(info.format));
                    }
                }), ui.fillWidth());
        }
        col.addView(ui.space(8));
        col.addView(ui.button(R.string.sanitize_change_options, Ui.TEXT_ONLY, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Session.get().closeSanitize();
                act.refresh();
            }
        }), ui.fillWidth());
        col.addView(ui.space(8));
        col.addView(ui.text(R.string.sanitize_original_untouched, Ui.CAPTION), ui.fillWidth());
        return page(header(ui.str(R.string.sanitize_title), true), scrollColumn(col, 640, Adaptive.padding(act)));
    }

    private View actionsCard(Model.SanitizedInfo info) {
        LinearLayout card = ui.card();
        final LinearLayout body = ui.vbox();
        int n = info.actions.size();
        TextView summary = ui.heading(ui.str(R.string.sanitize_actions_title, n), Ui.LABEL);
        final android.widget.ImageView chevron = ui.icon(actionsOpen ? R.drawable.ic_chevron_up : R.drawable.ic_chevron_down,
            R.color.arca_on_surface_variant, 24);
        LinearLayout head = ui.hbox();
        head.setGravity(Gravity.CENTER_VERTICAL);
        head.setMinimumHeight(ui.dp(Ui.TOUCH));
        head.addView(summary, ui.weight(1f));
        head.addView(chevron);
        card.addView(head, ui.fillWidth());

        if (n == 0) {
            body.addView(ui.text(R.string.sanitize_no_actions, Ui.SMALL));
        }
        int shown = Math.min(n, MAX_ACTIONS);
        for (int i = 0; i < shown; i++) {
            Model.SanitizeAction a = info.actions.get(i);
            String line = "• " + Labels.sanitizeAction(act, a.kind);
            if (!a.target.isEmpty()) {
                line += " — " + Fmt.middle(Fmt.safe(a.target), 72);
            }
            TextView t = ui.text(line, Ui.SMALL);
            t.setPadding(0, ui.dp(2), 0, ui.dp(2));
            body.addView(t, ui.fillWidth());
        }
        if (n > shown) {
            body.addView(ui.text(ui.str(R.string.sanitize_more_actions, n - shown), Ui.CAPTION));
        }
        body.setVisibility(actionsOpen ? View.VISIBLE : View.GONE);
        card.addView(body, ui.fillWidth());
        head.setClickable(true);
        head.setFocusable(true);
        head.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                actionsOpen = !actionsOpen;
                body.setVisibility(actionsOpen ? View.VISIBLE : View.GONE);
                chevron.setImageDrawable(act.getDrawable(actionsOpen ? R.drawable.ic_chevron_up : R.drawable.ic_chevron_down));
            }
        });
        return card;
    }

    @Override
    public void onSessionChanged() {
        act.refresh();
    }

    @Override
    public void onLeave() {
        Session.get().closeSanitize();
    }
}
