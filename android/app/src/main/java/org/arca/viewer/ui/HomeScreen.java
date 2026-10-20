// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.ClipData;
import android.content.Context;
import android.graphics.drawable.Drawable;
import android.net.Uri;
import android.view.DragEvent;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.TextView;

import org.arca.viewer.Engine;
import org.arca.viewer.MainActivity;
import org.arca.viewer.Native;
import org.arca.viewer.Prefs;
import org.arca.viewer.R;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Model;

import java.util.TimeZone;

/** The start screen: open a file, see that the sandbox is up, reach the other screens. */
public final class HomeScreen extends Screen {
    public HomeScreen(MainActivity act) {
        super(act);
    }

    @Override
    public View build() {
        boolean wide = Adaptive.twoPane(act);
        int pad = Adaptive.padding(act);

        View settingsButton = ui.iconButton(R.drawable.ic_settings, R.string.settings, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.push(MainActivity.Route.SETTINGS);
            }
        });
        View infoButton = ui.iconButton(R.drawable.ic_info, R.string.about_title, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.push(MainActivity.Route.ABOUT);
            }
        });

        View hero = hero();
        View open = openCard();
        View status = statusCard();
        View passport = passportCard();
        View links = linksRow();

        View body;
        if (wide) {
            LinearLayout cols = ui.hbox();
            cols.setGravity(Gravity.TOP);
            LinearLayout left = ui.vbox();
            left.addView(hero);
            left.addView(ui.space(16));
            left.addView(open);
            LinearLayout right = ui.vbox();
            right.addView(status);
            right.addView(ui.space(16));
            right.addView(passport);
            right.addView(ui.space(16));
            right.addView(links);
            cols.addView(left, ui.weight(1f));
            cols.addView(ui.hspace(24));
            cols.addView(right, ui.weight(1f));
            body = scrollColumn(cols, 1100, pad);
        } else {
            LinearLayout col = ui.vbox();
            col.addView(hero);
            col.addView(ui.space(16));
            col.addView(open);
            col.addView(ui.space(16));
            col.addView(status);
            col.addView(ui.space(16));
            col.addView(passport);
            col.addView(ui.space(16));
            col.addView(links);
            body = scrollColumn(col, 640, pad);
        }

        View page = page(header(ui.str(R.string.app_name), false, infoButton, settingsButton), body);
        installDrop(page);
        return page;
    }

    private View hero() {
        LinearLayout row = ui.hbox();
        FrameLayout logo = new FrameLayout(act);
        logo.setBackground(ui.rounded(ui.color(R.color.ic_launcher_background), 0, 20));
        ImageView mark = new ImageView(act);
        Drawable fg = act.getDrawable(R.drawable.ic_launcher_foreground);
        mark.setImageDrawable(fg);
        mark.setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_NO);
        logo.addView(mark, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        row.addView(logo, new LinearLayout.LayoutParams(ui.dp(72), ui.dp(72)));
        row.addView(ui.hspace(16));
        LinearLayout names = ui.vbox();
        names.addView(ui.heading(ui.str(R.string.app_name), Ui.HEADLINE));
        names.addView(ui.text(R.string.home_tagline, Ui.SMALL));
        row.addView(names, ui.weight(1f));
        return row;
    }

    private View openCard() {
        LinearLayout card = ui.card();
        card.addView(ui.text(R.string.home_blurb, Ui.BODY));
        card.addView(ui.space(16));
        View open = ui.button(R.string.home_open, Ui.PRIMARY, R.drawable.ic_open, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.pickFile();
            }
        });
        card.addView(open, ui.fillWidth());
        if (Adaptive.widthClass(act) != Adaptive.COMPACT) {
            card.addView(ui.space(8));
            TextView hint = ui.text(R.string.home_drop_hint, Ui.CAPTION);
            hint.setGravity(Gravity.CENTER);
            card.addView(hint, ui.fillWidth());
        }
        return card;
    }

    private View statusCard() {
        LinearLayout card = ui.card();
        Engine.State state = Engine.state();
        int icon;
        int tint;
        int title;
        switch (state) {
            case READY:
                icon = R.drawable.ic_verdict_clean;
                tint = R.color.arca_safe;
                title = R.string.home_engine_ready;
                break;
            case FAILED:
                icon = R.drawable.ic_verdict_danger;
                tint = R.color.arca_danger;
                title = R.string.home_engine_failed;
                break;
            default:
                icon = R.drawable.ic_verdict_unknown;
                tint = R.color.arca_unknown;
                title = R.string.home_engine_starting;
                break;
        }
        LinearLayout row = ui.hbox();
        row.addView(ui.icon(icon, tint, 28));
        row.addView(ui.hspace(12));
        row.addView(ui.heading(ui.str(title), Ui.TITLE), ui.weight(1f));
        card.addView(row);
        card.addView(ui.space(8));
        if (state == Engine.State.READY) {
            Model.EngineInfo info = Engine.info();
            card.addView(ui.text(ui.str(R.string.home_engine_detail, info.engine, Fmt.middle(info.guestSha256, 17)), Ui.CAPTION));
        } else if (state == Engine.State.FAILED) {
            card.addView(ui.text(Fmt.safe(Engine.failure()), Ui.MONO));
        }
        card.addView(ui.space(4));
        card.addView(ui.text(R.string.home_no_network, Ui.CAPTION));
        return card;
    }

    private View passportCard() {
        LinearLayout card = ui.card();
        Model.Passport p = null;
        if (Engine.state() == Engine.State.READY) {
            try {
                long now = System.currentTimeMillis();
                int day = Fmt.epochDay(now, TimeZone.getDefault().getOffset(now));
                byte[] json = Native.passport(Prefs.engineLang(act), Prefs.passport(act), day);
                if (json != null) {
                    p = Model.parsePassport(json);
                }
            } catch (RuntimeException ignored) {
                // the card is a courtesy; without it the screen still works
            }
        }
        if (p == null) {
            card.addView(ui.heading(ui.str(R.string.passport_fallback_title), Ui.TITLE));
            card.addView(ui.space(6));
            card.addView(ui.text(R.string.passport_fallback_body, Ui.SMALL));
            return card;
        }
        card.addView(ui.heading(p.title, Ui.TITLE));
        card.addView(ui.space(6));
        card.addView(ui.text(p.body, Ui.SMALL));
        card.addView(ui.space(12));
        LinearLayout stats = ui.hbox();
        stats.addView(stat(p.documents, R.string.stat_documents), ui.weight(1f));
        stats.addView(stat(p.trackers, R.string.stat_trackers), ui.weight(1f));
        stats.addView(stat(p.active, R.string.stat_active), ui.weight(1f));
        card.addView(stats, ui.fillWidth());
        card.addView(ui.space(12));
        card.addView(ui.button(p.cta, Ui.TONAL, R.drawable.ic_heart, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.push(MainActivity.Route.ABOUT);
            }
        }), ui.fillWidth());
        return card;
    }

    private View stat(long value, int labelRes) {
        LinearLayout l = ui.vbox();
        l.setGravity(Gravity.CENTER_HORIZONTAL);
        l.addView(ui.text(Long.toString(value), Ui.HEADLINE));
        TextView label = ui.text(labelRes, Ui.CAPTION);
        label.setGravity(Gravity.CENTER);
        l.addView(label);
        return l;
    }

    private View linksRow() {
        LinearLayout col = ui.vbox();
        col.addView(ui.button(R.string.licenses_title, Ui.OUTLINE, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.push(MainActivity.Route.LICENSES);
            }
        }), ui.fillWidth());
        col.addView(ui.space(8));
        col.addView(ui.button(R.string.diagnostics_title, Ui.OUTLINE, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.push(MainActivity.Route.DIAGNOSTICS);
            }
        }), ui.fillWidth());
        return col;
    }

    /** Accepts files dropped onto the screen (tablets, desktop modes, Chromebooks). */
    private void installDrop(View target) {
        target.setOnDragListener(new View.OnDragListener() {
            @Override
            public boolean onDrag(View v, DragEvent event) {
                switch (event.getAction()) {
                    case DragEvent.ACTION_DRAG_STARTED:
                        return event.getClipDescription() != null;
                    case DragEvent.ACTION_DROP:
                        ClipData clip = event.getClipData();
                        if (clip == null || clip.getItemCount() == 0) {
                            return false;
                        }
                        Uri uri = clip.getItemAt(0).getUri();
                        if (uri == null) {
                            return false;
                        }
                        act.requestDragAndDropPermissions(event);
                        String type = clip.getDescription() != null && clip.getDescription().getMimeTypeCount() > 0
                            ? clip.getDescription().getMimeType(0) : null;
                        act.openDocument(uri, type);
                        return true;
                    default:
                        return true;
                }
            }
        });
    }

    @Override
    public void onSessionChanged() {
        // The engine finished starting: refresh the status card.
        act.refresh();
    }
}
