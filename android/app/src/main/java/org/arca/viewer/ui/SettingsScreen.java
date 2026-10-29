// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.app.AlertDialog;
import android.content.DialogInterface;
import android.view.View;
import android.widget.CompoundButton;
import android.widget.HorizontalScrollView;
import android.widget.LinearLayout;
import android.widget.Switch;
import android.widget.TextView;

import org.arca.viewer.MainActivity;
import org.arca.viewer.Prefs;
import org.arca.viewer.R;
import org.arca.viewer.Stats;

/** Appearance, language and privacy options. Nothing here leaves the device. */
public final class SettingsScreen extends Screen {
    public SettingsScreen(MainActivity act) {
        super(act);
    }

    @Override
    public View build() {
        LinearLayout col = ui.vbox();

        // ---- appearance ----
        LinearLayout look = ui.card();
        look.addView(ui.heading(ui.str(R.string.settings_appearance), Ui.LABEL));
        look.addView(ui.space(8));
        look.addView(ui.text(R.string.settings_theme, Ui.SMALL));
        look.addView(chips(new int[] {R.string.theme_system, R.string.theme_light, R.string.theme_dark},
            Prefs.theme(act), new Choice() {
                @Override
                public void chosen(int index) {
                    if (index != Prefs.theme(act)) {
                        Prefs.setTheme(act, index);
                        act.recreate(); // the night / day resources are picked when the activity is created
                    }
                }
            }));
        look.addView(ui.space(12));
        look.addView(ui.text(R.string.settings_language, Ui.SMALL));
        look.addView(chips(new int[] {R.string.lang_system, R.string.lang_ru, R.string.lang_en},
            Prefs.lang(act), new Choice() {
                @Override
                public void chosen(int index) {
                    if (index != Prefs.lang(act)) {
                        Prefs.setLang(act, index);
                        act.recreate();
                    }
                }
            }));
        look.addView(ui.space(12));
        look.addView(ui.text(R.string.settings_text_size, Ui.SMALL));
        look.addView(chips(new int[] {R.string.size_small, R.string.size_normal, R.string.size_large, R.string.size_huge},
            Prefs.textSize(act), new Choice() {
                @Override
                public void chosen(int index) {
                    Prefs.setTextSize(act, index);
                    act.refresh();
                }
            }));
        look.addView(ui.space(8));
        TextView sample = ui.text(R.string.settings_text_sample, Ui.BODY);
        sample.setTextSize(android.util.TypedValue.COMPLEX_UNIT_SP, 16 * Prefs.textScale(act));
        look.addView(sample, ui.fillWidth());
        col.addView(look, ui.fillWidth());
        col.addView(ui.space(12));

        // ---- privacy ----
        LinearLayout privacy = ui.card();
        privacy.addView(ui.heading(ui.str(R.string.settings_privacy), Ui.LABEL));
        privacy.addView(ui.space(4));
        privacy.addView(toggle(R.string.settings_secure, R.string.settings_secure_desc, Prefs.secureScreen(act),
            new CompoundButton.OnCheckedChangeListener() {
                @Override
                public void onCheckedChanged(CompoundButton b, boolean on) {
                    Prefs.setSecureScreen(act, on);
                    Compat.setSecure(act, on);
                }
            }));
        privacy.addView(toggle(R.string.settings_reveal, R.string.settings_reveal_desc, Prefs.revealHidden(act),
            new CompoundButton.OnCheckedChangeListener() {
                @Override
                public void onCheckedChanged(CompoundButton b, boolean on) {
                    Prefs.setRevealHidden(act, on);
                }
            }));
        privacy.addView(toggle(R.string.settings_images, R.string.settings_images_desc, Prefs.autoImages(act),
            new CompoundButton.OnCheckedChangeListener() {
                @Override
                public void onCheckedChanged(CompoundButton b, boolean on) {
                    Prefs.setAutoImages(act, on);
                }
            }));
        col.addView(privacy, ui.fillWidth());
        col.addView(ui.space(12));

        // ---- statistics (counted by the core, kept only on this device) ----
        LinearLayout statistics = ui.card();
        statistics.addView(ui.heading(ui.str(R.string.settings_stats_title), Ui.LABEL));
        statistics.addView(ui.space(4));
        statistics.addView(toggle(R.string.settings_stats, R.string.settings_stats_desc, Stats.enabled(act),
            new CompoundButton.OnCheckedChangeListener() {
                @Override
                public void onCheckedChanged(CompoundButton b, boolean on) {
                    if (on != Stats.enabled(act)) {
                        // Off deletes everything that was counted and counts nothing from now on.
                        Stats.setEnabled(act, on);
                        act.refresh();
                    }
                }
            }));
        statistics.addView(ui.space(8));
        statistics.addView(ui.button(R.string.settings_stats_reset, Ui.OUTLINE, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                confirmReset();
            }
        }), ui.fillWidth());
        col.addView(statistics, ui.fillWidth());
        col.addView(ui.space(12));

        // ---- reminders ----
        LinearLayout notif = ui.card();
        notif.addView(ui.heading(ui.str(R.string.settings_reminders_title), Ui.LABEL));
        notif.addView(ui.space(4));
        notif.addView(toggle(R.string.settings_reminders, R.string.settings_reminders_desc, Prefs.reminders(act),
            new CompoundButton.OnCheckedChangeListener() {
                @Override
                public void onCheckedChanged(CompoundButton b, boolean on) {
                    Prefs.setReminders(act, on);
                }
            }));
        col.addView(notif, ui.fillWidth());
        col.addView(ui.space(12));

        TextView net = ui.text(R.string.home_no_network, Ui.CAPTION);
        col.addView(net, ui.fillWidth());
        return page(header(ui.str(R.string.settings), true), scrollColumn(col, 640, Adaptive.padding(act)));
    }

    /** Asks before deleting the statistics; nothing happens on Cancel. */
    private void confirmReset() {
        Ui.showHardened(act, new AlertDialog.Builder(act)
            .setTitle(R.string.settings_stats_reset_title)
            .setMessage(R.string.settings_stats_reset_message)
            .setPositiveButton(R.string.settings_stats_reset_confirm, new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface d, int w) {
                    Stats.reset(act);
                    android.widget.Toast.makeText(act, R.string.settings_stats_reset_done,
                        android.widget.Toast.LENGTH_SHORT).show();
                    act.refresh();
                }
            })
            .setNegativeButton(R.string.cancel, null));
    }

    private interface Choice {
        void chosen(int index);
    }

    private View chips(int[] labels, int selected, final Choice choice) {
        HorizontalScrollView hs = new HorizontalScrollView(act);
        hs.setHorizontalScrollBarEnabled(false);
        LinearLayout row = ui.hbox();
        row.setPadding(0, ui.dp(4), 0, ui.dp(4));
        for (int i = 0; i < labels.length; i++) {
            final int index = i;
            row.addView(ui.chip(ui.str(labels[i]), i == selected, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    choice.chosen(index);
                    if (!act.isFinishing()) {
                        act.refresh();
                    }
                }
            }));
            row.addView(ui.hspace(8));
        }
        hs.addView(row);
        return hs;
    }

    private View toggle(int titleRes, int descRes, boolean on, CompoundButton.OnCheckedChangeListener l) {
        LinearLayout box = ui.vbox();
        box.setPadding(0, ui.dp(4), 0, ui.dp(4));
        Switch sw = new Switch(act);
        sw.setText(titleRes);
        sw.setTextColor(ui.color(R.color.arca_on_surface));
        sw.setMinHeight(ui.dp(Ui.TOUCH));
        sw.setChecked(on);
        sw.setOnCheckedChangeListener(l);
        box.addView(sw, ui.fillWidth());
        box.addView(ui.text(descRes, Ui.CAPTION), ui.fillWidth());
        return box;
    }
}
