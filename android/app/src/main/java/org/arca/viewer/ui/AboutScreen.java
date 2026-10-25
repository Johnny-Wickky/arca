// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.view.View;
import android.widget.LinearLayout;
import android.widget.TextView;

import org.arca.viewer.Engine;
import org.arca.viewer.MainActivity;
import org.arca.viewer.Native;
import org.arca.viewer.Prefs;
import org.arca.viewer.R;
import org.arca.viewer.core.ExtraSupport;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Model;

import java.util.List;

/**
 * "About &amp; Support": who made the viewer (the original authors and, if this is a modified
 * version, who modified it), where to support them, the legal notice. All of it comes from the
 * attribution module of the engine, which also refuses to build a release without it.
 */
public final class AboutScreen extends Screen {
    public AboutScreen(MainActivity act) {
        super(act);
    }

    @Override
    public View build() {
        LinearLayout col = ui.vbox();
        col.addView(ui.heading(ui.str(R.string.app_name), Ui.HEADLINE));
        col.addView(ui.text(ui.str(R.string.about_version, versionName()), Ui.CAPTION));
        col.addView(ui.space(12));

        Model.About about = null;
        if (Engine.state() == Engine.State.READY) {
            try {
                byte[] json = Native.about(Prefs.engineLang(act));
                if (json != null) {
                    about = Model.parseAbout(json);
                }
            } catch (RuntimeException ignored) {
                about = null;
            }
        }
        if (about == null) {
            LinearLayout bad = ui.banner(R.color.arca_unknown_container);
            bad.addView(ui.text(R.string.about_unavailable, Ui.BODY));
            col.addView(bad, ui.fillWidth());
        } else {
            addAbout(col, about);
        }

        addExtraSupport(col);

        col.addView(ui.space(16));
        col.addView(ui.button(R.string.licenses_title, Ui.OUTLINE, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.push(MainActivity.Route.LICENSES);
            }
        }), ui.fillWidth());
        return page(header(ui.str(R.string.about_title), true), scrollColumn(col, 640, Adaptive.padding(act)));
    }

    private void addAbout(LinearLayout col, Model.About about) {
        Model.ReleaseInfo release = Engine.release();
        if (!release.ready) {
            LinearLayout warn = ui.banner(R.color.arca_warn_container);
            TextView t = ui.heading(ui.str(R.string.about_not_release_title), Ui.LABEL);
            t.setTextColor(ui.color(R.color.arca_warn));
            warn.addView(t);
            warn.addView(ui.space(4));
            warn.addView(ui.text(R.string.about_not_release_text, Ui.SMALL));
            for (String b : release.blockers) {
                warn.addView(ui.text("• " + Fmt.clip(Fmt.safe(b), 200), Ui.CAPTION));
            }
            col.addView(warn, ui.fillWidth());
            col.addView(ui.space(12));
        }

        for (Model.Card card : about.screen.cards) {
            col.addView(card(card), ui.fillWidth());
            col.addView(ui.space(12));
        }

        if (!about.screen.modified.isEmpty()) {
            LinearLayout mod = ui.card();
            mod.addView(ui.text(Fmt.safe(about.screen.modified), Ui.SMALL));
            col.addView(mod, ui.fillWidth());
            col.addView(ui.space(12));
        }
        if (!about.screen.legal.isEmpty()) {
            TextView legal = ui.text(Fmt.safe(about.screen.legal), Ui.CAPTION);
            legal.setTextIsSelectable(true);
            col.addView(legal, ui.fillWidth());
        }
        if (!about.fingerprint.isEmpty()) {
            col.addView(ui.space(8));
            col.addView(fact(ui.str(R.string.about_fingerprint), Fmt.grouped(about.fingerprint), true));
        }
    }

    /**
     * "Other ways to support": crypto wallets and Boosty, from the list the owner configures in the
     * attribution crate. The list ships empty, and then nothing is drawn. A wallet entry opens a
     * dialog that shows the full address first and copies it only on an explicit tap (flagged
     * sensitive, cleared again after a minute where the system allows); a Boosty entry goes
     * through the same "show the whole address, open on a tap" dialog as every other link.
     */
    private void addExtraSupport(LinearLayout col) {
        List<ExtraSupport.Entry> items;
        try {
            items = ExtraSupport.parse(Native.extraSupport());
        } catch (RuntimeException | LinkageError e) {
            return;
        }
        if (items.isEmpty()) {
            return;
        }
        col.addView(ui.space(12));
        LinearLayout card = ui.card();
        TextView title = ui.heading(ui.str(R.string.about_extra_title), Ui.LABEL);
        title.setTextColor(ui.color(R.color.arca_primary));
        card.addView(title);
        card.addView(ui.space(6));
        card.addView(ui.text(R.string.about_extra_hint, Ui.SMALL));
        card.addView(ui.space(8));
        for (final ExtraSupport.Entry e : items) {
            if (e.isWallet()) {
                card.addView(ui.button(Fmt.safe(e.label + " (" + e.network + ")"), Ui.TONAL, R.drawable.ic_link,
                    new View.OnClickListener() {
                        @Override
                        public void onClick(View v) {
                            showWallet(e);
                        }
                    }));
            } else {
                card.addView(linkButton(Fmt.safe(e.label), Ui.TONAL, e.value));
            }
            card.addView(ui.space(6));
        }
        card.addView(ui.text(R.string.about_extra_cleared, Ui.CAPTION));
        col.addView(card, ui.fillWidth());
    }

    private void showWallet(ExtraSupport.Entry e) {
        StringBuilder title = new StringBuilder(Fmt.safe(e.label));
        title.append('\n').append(ui.str(R.string.about_extra_network, Fmt.safe(e.network)));
        if (!e.memo.isEmpty()) {
            title.append('\n').append(ui.str(R.string.about_extra_memo, Fmt.safe(e.memo)));
        }
        ui.showText(act, title.toString(), e.value, "address");
    }

    private View card(Model.Card c) {
        LinearLayout card = ui.card();
        TextView title = ui.heading(Fmt.safe(c.title), Ui.LABEL);
        title.setTextColor(ui.color(R.color.arca_primary));
        card.addView(title);
        if (!c.project.isEmpty()) {
            card.addView(ui.heading(Fmt.safe(c.project), Ui.TITLE));
        }
        card.addView(ui.space(6));
        for (Model.Person p : c.people) {
            if (p.url.isEmpty()) {
                card.addView(ui.text(Fmt.safe(p.name), Ui.BODY));
            } else {
                card.addView(linkButton(Fmt.safe(p.name), Ui.TEXT_ONLY, p.url));
            }
        }
        if (!c.modifications.isEmpty()) {
            card.addView(ui.space(6));
            card.addView(ui.text(Fmt.safe(c.modifications), Ui.SMALL));
        }
        if (!c.homepage.isEmpty() || !c.source.isEmpty()) {
            card.addView(ui.space(8));
            if (!c.homepage.isEmpty()) {
                card.addView(linkButton(ui.str(R.string.about_homepage), Ui.OUTLINE, c.homepage));
            }
            if (!c.source.isEmpty()) {
                card.addView(ui.space(6));
                card.addView(linkButton(ui.str(R.string.about_source), Ui.OUTLINE, c.source));
            }
        }
        if (!c.support.isEmpty()) {
            card.addView(ui.space(12));
            if (!c.supportLabel.isEmpty()) {
                card.addView(ui.text(Fmt.safe(c.supportLabel), Ui.LABEL));
                card.addView(ui.space(4));
            }
            for (Model.Link l : c.support) {
                card.addView(linkButton(Fmt.safe(l.label.isEmpty() ? l.url : l.label), Ui.TONAL, l.url));
                card.addView(ui.space(6));
            }
        }
        return card;
    }

    private View linkButton(final String label, int kind, final String url) {
        return ui.button(label, kind, R.drawable.ic_link, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                ui.showLink(act, label, url);
            }
        });
    }

    @SuppressWarnings("deprecation")
    private String versionName() {
        try {
            PackageInfo pi = act.getPackageManager().getPackageInfo(act.getPackageName(), 0);
            return pi.versionName == null ? "?" : pi.versionName;
        } catch (PackageManager.NameNotFoundException | RuntimeException e) {
            return "?";
        }
    }

    @Override
    public void onSessionChanged() {
        act.refresh();
    }
}
