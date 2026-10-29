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
import org.arca.viewer.R;
import org.arca.viewer.Stats;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Model;
import org.arca.viewer.core.StatsView;

/** The start screen: open a file, see that the sandbox is up, reach the other screens. */
public final class HomeScreen extends Screen {
    /** The period shown on the statistics card; kept while the screen is rebuilt (rotation, folding). */
    private static int statsWindow = StatsView.WEEK;

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
        View reminder = reminderCard();
        View status = statusCard();
        View statistics = statsCard();
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
            left.addView(ui.space(16));
            if (reminder != null) {
                left.addView(reminder);
                left.addView(ui.space(16));
            }
            left.addView(statistics);
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
            if (reminder != null) {
                col.addView(reminder);
                col.addView(ui.space(16));
            }
            col.addView(statistics);
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

    /**
     * The support reminder, inside the app only (no notification, no permission). The core decides
     * when one is due; the card leads to the About screen and goes away for the next interval.
     */
    private View reminderCard() {
        final org.arca.viewer.Reminders.Card due = org.arca.viewer.Reminders.due(act);
        if (due == null) {
            return null;
        }
        LinearLayout card = ui.card();
        card.addView(ui.heading(due.title, Ui.TITLE));
        card.addView(ui.space(4));
        card.addView(ui.text(due.body, Ui.SMALL));
        card.addView(ui.space(12));
        LinearLayout row = ui.hbox();
        row.addView(ui.button(R.string.support_card_open, Ui.TONAL, R.drawable.ic_heart, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                org.arca.viewer.Reminders.dismiss(act);
                act.push(MainActivity.Route.ABOUT);
            }
        }), ui.weight(1f));
        row.addView(ui.hspace(8));
        row.addView(ui.button(R.string.support_card_later, Ui.TONAL, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                org.arca.viewer.Reminders.dismiss(act);
                act.refresh();
            }
        }), ui.weight(1f));
        card.addView(row, ui.fillWidth());
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
                // Built by the core from the all-time statistics; null when statistics are off.
                byte[] json = Stats.passportJson(act);
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
            card.addView(ui.space(12));
            card.addView(ui.button(R.string.about_title, Ui.TONAL, R.drawable.ic_heart, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    act.push(MainActivity.Route.ABOUT);
                }
            }), ui.fillWidth());
            return card;
        }
        card.addView(ui.heading(p.title, Ui.TITLE));
        card.addView(ui.space(6));
        card.addView(ui.text(p.body, Ui.SMALL));
        card.addView(ui.space(12));
        card.addView(ui.button(p.cta, Ui.TONAL, R.drawable.ic_heart, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.push(MainActivity.Route.ABOUT);
            }
        }), ui.fillWidth());
        return card;
    }

    /**
     * What Arca did on this device: a card with a Week / Month / All time toggle. The numbers are
     * counted by the Rust core and kept only on the device; the text says so.
     */
    private View statsCard() {
        LinearLayout card = ui.card();
        card.addView(ui.heading(ui.str(R.string.stats_title), Ui.TITLE));
        card.addView(ui.space(8));
        if (!Stats.enabled(act)) {
            card.addView(ui.text(R.string.stats_off_note, Ui.SMALL));
            return card;
        }
        final StatsView.Summary summary = Stats.summary(act);
        card.addView(periodToggle());
        card.addView(ui.space(12));
        StatsView.Totals t = summary.window(statsWindow);
        if (summary.empty || t.isZero()) {
            card.addView(ui.text(R.string.stats_empty, Ui.SMALL));
        } else {
            addTiles(card, t);
        }
        card.addView(ui.space(12));
        card.addView(ui.text(R.string.stats_local_note, Ui.CAPTION));
        return card;
    }

    /** The segmented toggle: three equal buttons that wrap instead of being cut off on narrow screens. */
    private View periodToggle() {
        int[] labels = {R.string.stats_period_week, R.string.stats_period_month, R.string.stats_period_all};
        LinearLayout row = ui.hbox();
        row.setContentDescription(ui.str(R.string.stats_period_label));
        for (int i = 0; i < labels.length; i++) {
            final int which = i;
            TextView chip = ui.chip(ui.str(labels[i]), i == statsWindow, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    if (statsWindow != which) {
                        statsWindow = which;
                        act.refresh();
                    }
                }
            });
            chip.setSingleLine(false);
            chip.setMaxLines(2);
            chip.setPadding(ui.dp(6), ui.dp(8), ui.dp(6), ui.dp(8));
            if (i > 0) {
                row.addView(ui.hspace(6));
            }
            row.addView(chip, ui.weight(1f));
        }
        return row;
    }

    /** Two columns of counters; zero counters other than the first are left out to keep the card short. */
    private void addTiles(LinearLayout card, StatsView.Totals t) {
        int[] labels = {R.string.stats_opened, R.string.stats_threats, R.string.stats_trackers,
            R.string.stats_rejected, R.string.stats_unverified, R.string.stats_sterile, R.string.stats_pictures};
        long[] values = {t.opened, t.active, t.trackers, t.rejected, t.unverified, t.sterile, t.pictures};
        LinearLayout row = null;
        int inRow = 0;
        for (int i = 0; i < labels.length; i++) {
            if (i > 0 && values[i] == 0 && i != 1 && i != 2) {
                continue; // rejected / unverified / sterile / pictures appear when there is something to say
            }
            if (row == null || inRow == 2) {
                if (row != null) {
                    card.addView(ui.space(10));
                }
                row = ui.hbox();
                card.addView(row, ui.fillWidth());
                inRow = 0;
            }
            if (inRow == 1) {
                row.addView(ui.hspace(12));
            }
            row.addView(tile(values[i], labels[i]), ui.weight(1f));
            inRow++;
        }
        if (inRow == 1 && row != null) {
            row.addView(ui.hspace(12));
            row.addView(new View(act), ui.weight(1f));
        }
    }

    private View tile(long value, int labelRes) {
        LinearLayout l = ui.vbox();
        String number = StatsView.count(value);
        String label = ui.str(labelRes);
        l.addView(ui.text(number, Ui.TITLE));
        l.addView(ui.text(label, Ui.CAPTION));
        // One spoken item per counter ("Documents opened: 12"), not two loose texts.
        l.setFocusable(true);
        l.setContentDescription(ui.str(R.string.stats_item_description, label, number));
        for (int i = 0; i < l.getChildCount(); i++) {
            l.getChildAt(i).setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_NO);
        }
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
                        // Only a document handed over by another app (content://); a dragged web
                        // address or anything else is not a file and is never fetched.
                        if (uri == null || !"content".equals(uri.getScheme())) {
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
