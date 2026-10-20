// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.graphics.Typeface;
import android.graphics.drawable.ColorDrawable;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.AbsListView;
import android.widget.BaseAdapter;
import android.widget.FrameLayout;
import android.widget.HorizontalScrollView;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.TextView;

import org.arca.viewer.Engine;
import org.arca.viewer.MainActivity;
import org.arca.viewer.Native;
import org.arca.viewer.Prefs;
import org.arca.viewer.R;
import org.arca.viewer.core.Fmt;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.List;

/**
 * The licence texts: the AGPLv3, the additional terms (Russian original and English
 * translation) and the notices of the third-party components built into the app. The first
 * three live inside the native library, so the text shown is the text that was compiled in.
 */
public final class LicensesScreen extends Screen {
    private static final int TAB_AGPL = 0;
    private static final int TAB_TERMS_RU = 1;
    private static final int TAB_TERMS_EN = 2;
    private static final int TAB_THIRD_PARTY = 3;
    /** Longest chunk of text per list row (long rows are slow to lay out). */
    private static final int CHUNK = 3000;
    private static final String THIRD_PARTY_ASSET = "THIRD_PARTY_NOTICES.txt";
    private static final int MAX_ASSET_BYTES = 8 << 20;

    private static int tab = -1;

    public LicensesScreen(MainActivity act) {
        super(act);
        if (tab < 0) {
            tab = TAB_AGPL;
        }
    }

    @Override
    public View build() {
        LinearLayout body = ui.vbox();
        HorizontalScrollView hs = new HorizontalScrollView(act);
        hs.setHorizontalScrollBarEnabled(false);
        LinearLayout chips = ui.hbox();
        int pad = ui.dp(Adaptive.padding(act));
        chips.setPadding(pad, ui.dp(4), pad, ui.dp(8));
        int[] order = Prefs.engineLang(act) == 0
            ? new int[] {TAB_AGPL, TAB_TERMS_RU, TAB_TERMS_EN, TAB_THIRD_PARTY}
            : new int[] {TAB_AGPL, TAB_TERMS_EN, TAB_TERMS_RU, TAB_THIRD_PARTY};
        for (final int t : order) {
            chips.addView(ui.chip(ui.str(label(t)), t == tab, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    tab = t;
                    act.refresh();
                }
            }));
            chips.addView(ui.hspace(8));
        }
        hs.addView(chips);
        body.addView(hs, ui.fillWidth());
        body.addView(text(), new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));
        return page(header(ui.str(R.string.licenses_title), true), body);
    }

    private static int label(int t) {
        switch (t) {
            case TAB_TERMS_RU: return R.string.licenses_terms_ru;
            case TAB_TERMS_EN: return R.string.licenses_terms_en;
            case TAB_THIRD_PARTY: return R.string.licenses_third_party;
            default: return R.string.licenses_agpl;
        }
    }

    private View text() {
        String content = load(tab);
        if (content == null || content.isEmpty()) {
            TextView t = ui.text(R.string.licenses_unavailable, Ui.BODY);
            t.setGravity(Gravity.CENTER);
            t.setPadding(ui.dp(24), ui.dp(48), ui.dp(24), ui.dp(24));
            return t;
        }
        final List<String> chunks = Fmt.chunks(content, CHUNK);
        final float scale = Prefs.textScale(act);
        ListView list = new ListView(act);
        list.setDivider(null);
        list.setDividerHeight(0);
        list.setSelector(new ColorDrawable(0));
        list.setClipToPadding(false);
        int pad = ui.dp(Adaptive.padding(act));
        list.setPadding(pad, ui.dp(8), pad, ui.dp(32));
        list.setAdapter(new BaseAdapter() {
            @Override
            public int getCount() {
                return chunks.size();
            }

            @Override
            public Object getItem(int position) {
                return chunks.get(position);
            }

            @Override
            public long getItemId(int position) {
                return position;
            }

            @Override
            public boolean isEnabled(int position) {
                return false;
            }

            @Override
            public View getView(int position, View convert, ViewGroup parent) {
                TextView t;
                if (convert instanceof TextView) {
                    t = (TextView) convert;
                } else {
                    t = new TextView(act);
                    t.setTypeface(Typeface.MONOSPACE);
                    t.setTextColor(ui.color(R.color.arca_on_surface));
                    t.setLayoutParams(new AbsListView.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.WRAP_CONTENT));
                }
                t.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f * scale);
                t.setText(chunks.get(position));
                return t;
            }
        });
        Ui.MaxWidthFrame frame = new Ui.MaxWidthFrame(act, ui.dp(820));
        frame.addView(list, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        FrameLayout outer = new FrameLayout(act);
        outer.addView(frame, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.MATCH_PARENT, Gravity.CENTER_HORIZONTAL));
        return outer;
    }

    private String load(int which) {
        try {
            if (which == TAB_THIRD_PARTY) {
                return readAsset();
            }
            if (Engine.state() != Engine.State.READY) {
                return null;
            }
            int id = which == TAB_TERMS_RU ? Native.LICENSE_TERMS_RU
                : which == TAB_TERMS_EN ? Native.LICENSE_TERMS_EN : Native.LICENSE_AGPL;
            byte[] bytes = Native.licenseText(id);
            return bytes == null ? null : new String(bytes, StandardCharsets.UTF_8);
        } catch (RuntimeException e) {
            return null;
        }
    }

    private String readAsset() {
        try (InputStream in = act.getAssets().open(THIRD_PARTY_ASSET)) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buf = new byte[16384];
            int n;
            while ((n = in.read(buf)) > 0) {
                out.write(buf, 0, n);
                if (out.size() > MAX_ASSET_BYTES) {
                    break;
                }
            }
            return new String(out.toByteArray(), StandardCharsets.UTF_8);
        } catch (IOException | RuntimeException e) {
            return null;
        }
    }

}
