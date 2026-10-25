// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Context;
import android.graphics.Paint;
import android.graphics.Typeface;
import android.graphics.drawable.ColorDrawable;
import android.text.Editable;
import android.text.InputFilter;
import android.text.InputType;
import android.text.Layout;
import android.text.SpannableStringBuilder;
import android.text.Spanned;
import android.text.TextUtils;
import android.text.TextWatcher;
import android.text.method.LinkMovementMethod;
import android.text.style.BackgroundColorSpan;
import android.text.style.ClickableSpan;
import android.text.style.ForegroundColorSpan;
import android.text.style.RelativeSizeSpan;
import android.text.style.StrikethroughSpan;
import android.text.style.StyleSpan;
import android.text.style.SubscriptSpan;
import android.text.style.SuperscriptSpan;
import android.text.style.TypefaceSpan;
import android.text.style.UnderlineSpan;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.widget.AbsListView;
import android.widget.BaseAdapter;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.HorizontalScrollView;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.ProgressBar;
import android.widget.ScrollView;
import android.widget.TextView;

import org.arca.viewer.MainActivity;
import org.arca.viewer.Native;
import org.arca.viewer.Prefs;
import org.arca.viewer.R;
import org.arca.viewer.Session;
import org.arca.viewer.core.Bookmarks;
import org.arca.viewer.core.DocOutline;
import org.arca.viewer.core.DocSearch;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Model;
import org.arca.viewer.core.PageCache;
import org.arca.viewer.core.Zoom;

import java.util.List;

/**
 * The safe preview: text, tables, spreadsheet grids and slides extracted inside the sandbox and
 * shown as plain styled text. Nothing in a document is executed, nothing is fetched, links are
 * only <em>shown</em> (a tap offers to copy the address), and pictures are not decoded at all.
 *
 * <p>Items arrive in pages as the list is scrolled, so even a spreadsheet with a million rows
 * costs memory only for the rows near the screen.
 */
public final class PreviewScreen extends Screen {
    private static final int TYPE_PARA = 0;
    private static final int TYPE_ROW = 1;
    private static final int TYPE_GRID_HEAD = 2;
    private static final int TYPE_GRID_ROW = 3;
    private static final int TYPE_IMAGE = 4;
    private static final int TYPE_RULE = 5;
    private static final int TYPE_PENDING = 6;
    private static final int TYPES = 7;

    /** Spreadsheet columns shown at most (a sheet may declare thousands). */
    private static final int MAX_GRID_COLS = 200;

    // Survive rebuilds when the window changes shape.
    private static long shownOutcomeKey;
    private static int section;
    private static int savedPosition;
    private static boolean noticesOpen;

    // The search survives rebuilds too (the window changes shape, a match is in another section).
    private static boolean searchOpen;
    private static boolean focusSearch;
    private static String searchQuery = "";
    private static DocSearch search;
    private static int hitIndex = -1;
    /** The screen on display: the one the search tells about new matches. */
    private static PreviewScreen live;

    // The headings and bookmarks of the document on display (memory only, dropped with it).
    private static DocOutline outline;
    private static final Bookmarks BOOKMARKS = new Bookmarks();
    /** Redraws the lists of the open contents dialog, if there is one. */
    private static Runnable contentsRefresh;
    private static AlertDialog contentsDialog;

    private final float scale;
    private Model.PreviewMeta metaShown;
    private long outcomeShown;
    private TextView countView;
    /** The row being built holds the match the person is on (drawn stronger). */
    private boolean currentHitRow;
    private PageCache cache;
    private ItemAdapter adapter;
    private ListView list;
    private int[] gridWidths;
    private float charWidth;

    public PreviewScreen(MainActivity act) {
        super(act);
        this.scale = Prefs.textScale(act) * Prefs.zoom(act);
    }

    @Override
    public View build() {
        final Session s = Session.get();
        final Session.PreviewState st = s.preview();
        if (s.phase() != Session.Phase.READY) {
            act.getWindow().getDecorView().post(new Runnable() {
                @Override
                public void run() {
                    act.home();
                }
            });
            return new View(act);
        }
        if (st.cancelled) {
            // The person pressed Cancel while it was building: nothing to show.
            act.getWindow().getDecorView().post(new Runnable() {
                @Override
                public void run() {
                    act.back();
                }
            });
            return new View(act);
        }
        if (st.running) {
            return working();
        }
        if (st.failure != null) {
            return failureView(ui.str(R.string.preview_title), st.failure, new Runnable() {
                @Override
                public void run() {
                    Session.get().startPreview(Prefs.revealHidden(act));
                }
            });
        }
        if (!st.ready()) {
            // First visit: start the run once the view is on screen (never from inside build()).
            act.getWindow().getDecorView().post(new Runnable() {
                @Override
                public void run() {
                    Session.PreviewState now = Session.get().preview();
                    if (!now.running && !now.ready() && now.failure == null && !now.cancelled) {
                        Session.get().startPreview(Prefs.revealHidden(act));
                    }
                }
            });
            return working();
        }
        return content(s, st);
    }

    private View working() {
        LinearLayout col = ui.vbox();
        col.setGravity(Gravity.CENTER_HORIZONTAL);
        col.setPadding(ui.dp(24), ui.dp(24), ui.dp(24), ui.dp(24));
        ProgressBar bar = new ProgressBar(act);
        bar.setIndeterminate(true);
        bar.setIndeterminateTintList(android.content.res.ColorStateList.valueOf(ui.color(R.color.arca_primary)));
        col.addView(bar, new LinearLayout.LayoutParams(ui.dp(56), ui.dp(56)));
        col.addView(ui.space(16));
        TextView t = ui.heading(ui.str(R.string.preview_building), Ui.TITLE);
        t.setGravity(Gravity.CENTER);
        col.addView(t, ui.fillWidth());
        col.addView(ui.space(6));
        TextView note = ui.text(R.string.scan_note, Ui.CAPTION);
        note.setGravity(Gravity.CENTER);
        col.addView(note, ui.fillWidth());
        col.addView(ui.space(20));
        col.addView(ui.button(R.string.cancel, Ui.OUTLINE, R.drawable.ic_close, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Session.get().cancel();
                act.back();
            }
        }));
        return page(header(ui.str(R.string.preview_title), true), centered(col, 480));
    }

    /** The section chips, when the window is folded and they have a part of it to themselves (see {@link #dock()}). */
    private View dockView;

    @Override
    public View dock() {
        return dockView;
    }

    // ---- content ------------------------------------------------------------------------------

    private View content(final Session s, final Session.PreviewState st) {
        final Model.PreviewMeta meta = st.meta;
        final long outcome = s.previewOutcome();
        if (shownOutcomeKey != outcome) {
            shownOutcomeKey = outcome;
            section = 0;
            savedPosition = 0;
            resetSearch();
            resetOutline();
        }
        metaShown = meta;
        outcomeShown = outcome;
        live = this;
        if (section >= meta.sections.size()) {
            section = 0;
        }

        View reveal = ui.iconButton(st.reveal ? R.drawable.ic_eye_off : R.drawable.ic_eye,
            st.reveal ? R.string.hide_hidden : R.string.show_hidden, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    // The run replaces the preview, and the matches belong to the old one.
                    resetSearch();
                    resetOutline();
                    s.startPreview(!st.reveal);
                }
            });
        String title = Fmt.clip(Fmt.safe(s.name().isEmpty() ? ui.str(R.string.preview_title) : s.name()), 60);
        View find = ui.iconButton(R.drawable.ic_search, R.string.search_open, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                searchOpen = true;
                focusSearch = true;
                act.refresh();
            }
        });
        View contents = ui.iconButton(R.drawable.ic_list, R.string.contents_open, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showContents();
            }
        });
        View head = meta.sections.isEmpty() ? header(title, true, reveal)
            : header(title, true, find, contents, reveal);

        LinearLayout body = ui.vbox();
        if (searchOpen && !meta.sections.isEmpty()) {
            body.addView(searchBar(), ui.fillWidth());
        }
        View notices = notices(meta, st);
        if (notices != null) {
            body.addView(notices, ui.fillWidth());
        }
        if (meta.sections.isEmpty()) {
            // An empty file, or one whose parts are all hidden and were not asked for: there is no
            // section to open, so say so (the notices above explain what was left out).
            TextView none = ui.text(R.string.preview_nothing, Ui.BODY);
            none.setGravity(Gravity.CENTER);
            none.setPadding(ui.dp(24), ui.dp(48), ui.dp(24), ui.dp(24));
            body.addView(none, ui.fillWidth());
            return page(head, body);
        }

        // Folded window with room for controls apart from the document: the sections go there.
        boolean docked = Hinge.hasDock() && meta.sections.size() > 1;
        boolean rail = Adaptive.twoPane(act) && meta.sections.size() > 1 && !docked;
        LinearLayout main = ui.hbox();
        main.setGravity(Gravity.TOP);
        if (docked) {
            dockView = dockPane(sectionColumn(meta));
        } else if (meta.sections.size() > 1) {
            if (rail) {
                main.addView(sectionRail(meta), new LinearLayout.LayoutParams(ui.dp(240), ViewGroup.LayoutParams.MATCH_PARENT));
                main.addView(ui.divider(), new LinearLayout.LayoutParams(Math.max(1, ui.dp(1)), ViewGroup.LayoutParams.MATCH_PARENT));
            } else {
                body.addView(sectionChips(meta), ui.fillWidth());
            }
        }
        main.addView(withLegend(sectionContent(s, meta, outcome)), new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 1f));
        body.addView(main, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));
        return page(head, body);
    }

    // ---- search -----------------------------------------------------------------------------

    private View searchBar() {
        LinearLayout bar = ui.hbox();
        int pad = ui.dp(Adaptive.padding(act));
        bar.setPadding(pad, ui.dp(2), ui.dp(4), ui.dp(6));
        final EditText edit = new EditText(act);
        edit.setSingleLine(true);
        edit.setHint(R.string.search_hint);
        edit.setHintTextColor(ui.color(R.color.arca_on_surface_variant));
        edit.setTextColor(ui.color(R.color.arca_on_surface));
        edit.setTextSize(TypedValue.COMPLEX_UNIT_SP, 16);
        edit.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        // What is searched for is nobody else's business: no learning, no autofill.
        edit.setImeOptions(EditorInfo.IME_ACTION_SEARCH | EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
            | EditorInfo.IME_FLAG_NO_EXTRACT_UI);
        edit.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO);
        edit.setFilters(new InputFilter[] {new InputFilter.LengthFilter(DocSearch.MAX_QUERY)});
        edit.setBackground(ui.rounded(ui.color(R.color.arca_surface_variant), 0, 22));
        edit.setPadding(ui.dp(16), ui.dp(8), ui.dp(16), ui.dp(8));
        edit.setMinHeight(ui.dp(44));
        edit.setText(searchQuery);
        edit.setSelection(edit.getText().length());
        edit.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence c, int a, int b, int d) {}

            @Override
            public void onTextChanged(CharSequence c, int a, int b, int d) {}

            @Override
            public void afterTextChanged(Editable e) {
                searchQuery = e.toString();
            }
        });
        edit.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                boolean enter = event != null && event.getKeyCode() == KeyEvent.KEYCODE_ENTER
                    && event.getAction() == KeyEvent.ACTION_DOWN;
                if (action == EditorInfo.IME_ACTION_SEARCH || enter) {
                    runSearch(v.getText().toString());
                    hideKeyboard(v);
                    return true;
                }
                return false;
            }
        });
        bar.addView(edit, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));

        countView = ui.text("", Ui.SMALL);
        countView.setPadding(ui.dp(8), 0, ui.dp(2), 0);
        countView.setGravity(Gravity.CENTER);
        bar.addView(countView, ui.wrapLp());
        bar.addView(ui.iconButton(R.drawable.ic_chevron_up, R.string.search_prev, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                if (search != null && search.hitCount() > 0) {
                    goTo(hitIndex < 0 ? search.hitCount() - 1 : hitIndex - 1);
                }
            }
        }));
        bar.addView(ui.iconButton(R.drawable.ic_chevron_down, R.string.search_next, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                if (search != null && search.hitCount() > 0) {
                    goTo(hitIndex < 0 ? 0 : hitIndex + 1);
                }
            }
        }));
        bar.addView(ui.iconButton(R.drawable.ic_close, R.string.search_close, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                closeSearch();
            }
        }));
        updateCount();
        if (focusSearch) {
            focusSearch = false;
            edit.post(new Runnable() {
                @Override
                public void run() {
                    edit.requestFocus();
                    InputMethodManager imm = (InputMethodManager) act.getSystemService(Context.INPUT_METHOD_SERVICE);
                    if (imm != null) {
                        imm.showSoftInput(edit, InputMethodManager.SHOW_IMPLICIT);
                    }
                }
            });
        }
        return bar;
    }

    private void hideKeyboard(View v) {
        InputMethodManager imm = (InputMethodManager) act.getSystemService(Context.INPUT_METHOD_SERVICE);
        if (imm != null) {
            imm.hideSoftInputFromWindow(v.getWindowToken(), 0);
        }
    }

    /** Forgets the search (a new document or preview, or the bar was closed). */
    private static void resetSearch() {
        if (search != null) {
            search.cancel();
        }
        search = null;
        searchOpen = false;
        focusSearch = false;
        searchQuery = "";
        hitIndex = -1;
    }

    private void closeSearch() {
        View focus = act.getCurrentFocus();
        if (focus != null) {
            hideKeyboard(focus);
        }
        resetSearch();
        act.refresh();
    }

    /** Looks for {@code raw} in every section, in the background. */
    private void runSearch(String raw) {
        searchQuery = raw;
        if (search != null) {
            search.cancel();
            search = null;
        }
        hitIndex = -1;
        String q = raw.trim();
        if (DocSearch.usable(q) && metaShown != null) {
            final Session s = Session.get();
            final long outcome = outcomeShown;
            long[] sizes = new long[metaShown.sections.size()];
            for (int i = 0; i < sizes.length; i++) {
                sizes[i] = metaShown.sections.get(i).items;
            }
            search = new DocSearch(q, sizes, new DocSearch.Loader() {
                @Override
                public List<Model.Item> load(int sec, long from, int count) {
                    return s.loadItems(outcome, sec, from, count);
                }
            }, s.pageExecutor(), s.mainExecutor(), new DocSearch.Listener() {
                @Override
                public void onChanged() {
                    PreviewScreen now = live;
                    if (now != null) {
                        now.onSearchChanged();
                    }
                }
            });
            search.start();
        }
        updateCount();
        if (adapter != null) {
            adapter.notifyDataSetChanged();
        }
    }

    /** More matches arrived, or the scan ended. */
    private void onSearchChanged() {
        if (search == null) {
            return;
        }
        if (hitIndex < 0 && search.hitCount() > 0) {
            // The first match at or after where the person is reading.
            int first = search.firstAtOrAfter(section, savedPosition);
            goTo(first < 0 ? 0 : first);
            return;
        }
        updateCount();
    }

    /** Shows match {@code index} (wrapping round): switches section if it has to, then scrolls. */
    private void goTo(int index) {
        if (search == null || search.hitCount() == 0) {
            return;
        }
        int n = search.hitCount();
        hitIndex = ((index % n) + n) % n;
        int sec = search.hitSection(hitIndex);
        long item = search.hitItem(hitIndex);
        if (sec != section) {
            section = sec;
            savedPosition = (int) Math.min(item, Integer.MAX_VALUE);
            act.refresh();
            return;
        }
        if (list != null) {
            list.setSelectionFromTop((int) Math.min(item, Integer.MAX_VALUE), ui.dp(24));
        }
        if (adapter != null) {
            adapter.notifyDataSetChanged();
        }
        updateCount();
    }

    private void updateCount() {
        if (countView == null) {
            return;
        }
        String text = "";
        if (search != null) {
            int n = search.hitCount();
            if (n > 0) {
                text = ui.str(search.capped() ? R.string.search_count_capped : R.string.search_count,
                    hitIndex + 1, n);
                if (!search.done()) {
                    text += " …";
                }
            } else if (!search.done()) {
                long total = Math.max(1, search.total());
                text = ui.str(R.string.search_progress, (int) Math.min(100, search.scanned() * 100 / total));
            } else if (!search.failed()) {
                text = ui.str(R.string.search_none);
            }
            if (search.done() && search.failed()) {
                text = (text.isEmpty() ? "" : text + "\n") + ui.str(R.string.search_incomplete);
            }
        }
        countView.setText(text);
    }

    /** {@code text} with every match of the search marked. */
    private CharSequence highlighted(CharSequence text) {
        if (search == null || text.length() == 0) {
            return text;
        }
        int[] at = DocSearch.occurrences(text.toString(), search.needle());
        if (at.length == 0) {
            return text;
        }
        SpannableStringBuilder sb = text instanceof SpannableStringBuilder
            ? (SpannableStringBuilder) text : new SpannableStringBuilder(text);
        int color = ui.color(currentHitRow ? R.color.arca_search_current : R.color.arca_search_hit);
        int len = search.needle().length();
        for (int start : at) {
            sb.setSpan(new BackgroundColorSpan(color), start, Math.min(sb.length(), start + len),
                Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
        }
        return sb;
    }

    // ---- contents, bookmarks, zoom ----------------------------------------------------------

    private static void resetOutline() {
        if (outline != null) {
            outline.cancel();
        }
        outline = null;
        BOOKMARKS.clear();
        contentsRefresh = null;
        if (contentsDialog != null) {
            contentsDialog.dismiss();
            contentsDialog = null;
        }
    }

    private long[] sectionSizes() {
        long[] sizes = new long[metaShown.sections.size()];
        for (int i = 0; i < sizes.length; i++) {
            sizes[i] = metaShown.sections.get(i).items;
        }
        return sizes;
    }

    /** Starts collecting the headings (once per preview). */
    private void ensureOutline() {
        if (outline != null || metaShown == null) {
            return;
        }
        final Session s = Session.get();
        final long outcome = outcomeShown;
        outline = new DocOutline(sectionSizes(), new DocOutline.Loader() {
            @Override
            public List<Model.Item> load(int sec, long from, int count) {
                return s.loadItems(outcome, sec, from, count);
            }
        }, s.pageExecutor(), s.mainExecutor(), new DocOutline.Listener() {
            @Override
            public void onChanged() {
                Runnable refresh = contentsRefresh;
                if (refresh != null) {
                    refresh.run();
                }
            }
        });
        outline.start();
    }

    /** A line of the contents dialog. */
    private View contentsRow(CharSequence text, int indentDp, View.OnClickListener open, int trailingIcon,
                             int trailingDesc, View.OnClickListener trailing) {
        LinearLayout row = ui.hbox();
        row.setMinimumHeight(ui.dp(44));
        TextView t = ui.text(text, Ui.BODY);
        t.setPadding(ui.dp(indentDp), ui.dp(8), ui.dp(8), ui.dp(8));
        t.setClickable(true);
        t.setFocusable(true);
        t.setOnClickListener(open);
        row.addView(t, ui.weight(1f));
        if (trailing != null) {
            row.addView(ui.iconButton(trailingIcon, trailingDesc, trailing));
        }
        return row;
    }

    /**
     * The screen that is on display. The dialogs outlive a rebuild of the screen (zooming rebuilds
     * it under the open dialog), so their buttons must act on the current screen, not on the one
     * that created them.
     */
    private PreviewScreen current() {
        return live != null ? live : this;
    }

    private String labelOf(int sec, long item) {
        String text = "";
        PreviewScreen me = current();
        if (me.cache != null && metaShown != null && sec < metaShown.sections.size()) {
            text = DocSearch.textOf(me.cache.get(sec, item, metaShown.sections.get(sec).items)).trim();
        }
        if (text.isEmpty() && metaShown != null && sec < metaShown.sections.size()) {
            text = ui.str(R.string.bookmark_place, sectionName(sec, metaShown.sections.get(sec)), item + 1);
        }
        return text;
    }

    /** Takes the reader to a place of the document. */
    private void goToPlace(int sec, long item) {
        if (contentsDialog != null) {
            contentsDialog.dismiss();
        }
        PreviewScreen me = current();
        if (sec == section && me.list != null) {
            me.list.setSelectionFromTop((int) Math.min(item, Integer.MAX_VALUE), ui.dp(24));
            return;
        }
        section = sec;
        savedPosition = (int) Math.min(item, Integer.MAX_VALUE);
        act.refresh();
    }

    private void showContents() {
        if (metaShown == null) {
            return;
        }
        ensureOutline();
        final LinearLayout root = ui.vbox();
        int p = ui.dp(16);
        root.setPadding(p, ui.dp(4), p, ui.dp(8));

        // Zoom: buttons for those who cannot pinch.
        root.addView(ui.text(R.string.zoom_label, Ui.CAPTION));
        LinearLayout zoomRow = ui.hbox();
        final TextView zoomValue = ui.heading(ui.str(R.string.zoom_value, Zoom.percent(Prefs.zoom(act))), Ui.TITLE);
        zoomValue.setGravity(Gravity.CENTER);
        zoomRow.addView(ui.iconButton(R.drawable.ic_minus, R.string.zoom_out, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                applyZoom(Zoom.step(Prefs.zoom(act), -1), zoomValue);
            }
        }));
        zoomRow.addView(zoomValue, new LinearLayout.LayoutParams(ui.dp(80), ViewGroup.LayoutParams.WRAP_CONTENT));
        zoomRow.addView(ui.iconButton(R.drawable.ic_plus, R.string.zoom_in, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                applyZoom(Zoom.step(Prefs.zoom(act), 1), zoomValue);
            }
        }));
        zoomRow.addView(ui.hspace(8));
        zoomRow.addView(ui.button(R.string.zoom_reset, Ui.TEXT_ONLY, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                applyZoom(Zoom.DEFAULT, zoomValue);
            }
        }));
        root.addView(zoomRow);
        root.addView(ui.space(8));

        // Bookmarks.
        root.addView(ui.button(R.string.bookmark_add, Ui.TONAL, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                if (!BOOKMARKS.add(section, savedPosition, labelOf(section, savedPosition))) {
                    android.widget.Toast.makeText(act, R.string.bookmark_full, android.widget.Toast.LENGTH_SHORT).show();
                }
                if (contentsRefresh != null) {
                    contentsRefresh.run();
                }
            }
        }), ui.fillWidth());
        root.addView(ui.space(8));
        root.addView(ui.text(R.string.bookmarks_title, Ui.LABEL));
        final LinearLayout marks = ui.vbox();
        root.addView(marks, ui.fillWidth());
        root.addView(ui.space(8));
        root.addView(ui.text(R.string.headings_title, Ui.LABEL));
        final LinearLayout heads = ui.vbox();
        root.addView(heads, ui.fillWidth());

        contentsRefresh = new Runnable() {
            @Override
            public void run() {
                marks.removeAllViews();
                if (BOOKMARKS.size() == 0) {
                    marks.addView(ui.text(R.string.bookmark_none, Ui.SMALL));
                }
                for (int i = 0; i < BOOKMARKS.size(); i++) {
                    final int index = i;
                    final Bookmarks.Mark m = BOOKMARKS.list().get(i);
                    marks.addView(contentsRow(Fmt.safe(m.label), 0, new View.OnClickListener() {
                        @Override
                        public void onClick(View v) {
                            goToPlace(m.section, m.item);
                        }
                    }, R.drawable.ic_close, R.string.bookmark_remove, new View.OnClickListener() {
                        @Override
                        public void onClick(View v) {
                            BOOKMARKS.remove(index);
                            if (contentsRefresh != null) {
                                contentsRefresh.run();
                            }
                        }
                    }));
                }
                heads.removeAllViews();
                DocOutline o = outline;
                if (o == null || (o.entries().isEmpty() && !o.done())) {
                    heads.addView(ui.text(R.string.headings_scanning, Ui.SMALL));
                } else if (o.entries().isEmpty()) {
                    heads.addView(ui.text(R.string.headings_none, Ui.SMALL));
                }
                if (o != null) {
                    for (final DocOutline.Entry e : o.entries()) {
                        heads.addView(contentsRow(e.text, 12 * Math.max(0, e.level - 1), new View.OnClickListener() {
                            @Override
                            public void onClick(View v) {
                                goToPlace(e.section, e.item);
                            }
                        }, 0, 0, null));
                    }
                    if (!o.done() && !o.entries().isEmpty()) {
                        heads.addView(ui.text(R.string.headings_scanning, Ui.SMALL));
                    }
                    if (o.capped()) {
                        heads.addView(ui.text(ui.str(R.string.headings_capped, DocOutline.MAX_ENTRIES), Ui.SMALL));
                    }
                }
            }
        };
        contentsRefresh.run();

        android.widget.ScrollView sv = new android.widget.ScrollView(act);
        sv.addView(root);
        contentsDialog = new AlertDialog.Builder(act)
            .setTitle(R.string.contents_title)
            .setView(sv)
            .setNegativeButton(R.string.close, null)
            .create();
        Ui.harden(act, contentsDialog);
        contentsDialog.setOnDismissListener(new DialogInterface.OnDismissListener() {
            @Override
            public void onDismiss(DialogInterface d) {
                contentsRefresh = null;
                contentsDialog = null;
            }
        });
        contentsDialog.show();
    }

    private void applyZoom(float zoom, TextView label) {
        Prefs.setZoom(act, zoom);
        label.setText(ui.str(R.string.zoom_value, Zoom.percent(zoom)));
        act.refresh();
    }

    // ---- notices ----------------------------------------------------------------------------

    private View notices(Model.PreviewMeta meta, Session.PreviewState st) {
        if (meta.notices.isEmpty() && !meta.truncated) {
            return null;
        }
        // With nothing else on the screen the explanation is shown open.
        final boolean[] open = {noticesOpen || meta.sections.isEmpty()};
        final LinearLayout box = ui.vbox();
        box.setBackground(ui.rounded(ui.color(R.color.arca_unknown_container), 0, 12));
        int p = ui.dp(12);
        box.setPadding(p, ui.dp(8), p, ui.dp(8));
        LinearLayout.LayoutParams lp = ui.fillWidth();
        lp.setMargins(ui.dp(Adaptive.padding(act)), ui.dp(4), ui.dp(Adaptive.padding(act)), ui.dp(4));
        box.setLayoutParams(lp);

        final LinearLayout head = ui.hbox();
        TextView summary = ui.text(ui.str(R.string.notices_summary, meta.notices.size() + (meta.truncated ? 1 : 0)), Ui.LABEL);
        summary.setTextColor(ui.color(R.color.arca_unknown));
        head.addView(summary, ui.weight(1f));
        final ImageView chevron = ui.icon(open[0] ? R.drawable.ic_chevron_up : R.drawable.ic_chevron_down, R.color.arca_unknown, 24);
        head.addView(chevron);
        box.addView(head);

        final LinearLayout list = ui.vbox();
        for (Model.Notice n : meta.notices) {
            list.addView(ui.text("• " + noticeText(n), Ui.SMALL));
        }
        if (meta.truncated) {
            list.addView(ui.text("• " + ui.str(R.string.notice_truncated_preview), Ui.SMALL));
        }
        list.setVisibility(open[0] ? View.VISIBLE : View.GONE);
        box.addView(list);
        box.setClickable(true);
        box.setFocusable(true);
        box.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                open[0] = !open[0];
                noticesOpen = open[0];
                list.setVisibility(open[0] ? View.VISIBLE : View.GONE);
                chevron.setImageDrawable(act.getDrawable(open[0] ? R.drawable.ic_chevron_up : R.drawable.ic_chevron_down));
            }
        });
        return box;
    }

    private String noticeText(Model.Notice n) {
        return Labels.noticeText(act, n.kind, n.count, n.detail);
    }

    // ---- sections ---------------------------------------------------------------------------

    private String sectionName(int index, Model.SectionInfo sec) {
        String base;
        if (!sec.name.isEmpty()) {
            base = Fmt.clip(Fmt.safe(sec.name), 40);
        } else {
            int res;
            switch (sec.kind) {
                case "Sheet": res = R.string.section_sheet; break;
                case "Slide": res = R.string.section_slide; break;
                case "Page": res = R.string.section_page; break;
                case "Comments": res = R.string.section_comments; break;
                case "Notes": res = R.string.section_notes; break;
                case "Headers": res = R.string.section_headers; break;
                default: res = R.string.section_body; break;
            }
            base = ui.str(res, index + 1);
        }
        return sec.hidden ? ui.str(R.string.section_hidden, base) : base;
    }

    private View sectionChips(final Model.PreviewMeta meta) {
        HorizontalScrollView hs = new HorizontalScrollView(act);
        hs.setHorizontalScrollBarEnabled(false);
        LinearLayout row = ui.hbox();
        int pad = ui.dp(Adaptive.padding(act));
        row.setPadding(pad, ui.dp(4), pad, ui.dp(8));
        for (int i = 0; i < meta.sections.size(); i++) {
            final int index = i;
            row.addView(ui.chip(sectionName(i, meta.sections.get(i)), i == section, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    section = index;
                    savedPosition = 0;
                    act.refresh();
                }
            }));
            row.addView(ui.hspace(8));
        }
        hs.addView(row);
        return hs;
    }

    private View sectionRail(final Model.PreviewMeta meta) {
        ScrollView sv = new ScrollView(act);
        sv.addView(sectionColumn(meta));
        return sv;
    }

    /** The sections as a column of chips. */
    private LinearLayout sectionColumn(final Model.PreviewMeta meta) {
        LinearLayout col = ui.vbox();
        col.setPadding(ui.dp(12), ui.dp(8), ui.dp(12), ui.dp(8));
        for (int i = 0; i < meta.sections.size(); i++) {
            final int index = i;
            TextView c = ui.chip(sectionName(i, meta.sections.get(i)), i == section, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    section = index;
                    savedPosition = 0;
                    act.refresh();
                }
            });
            c.setGravity(Gravity.CENTER_VERTICAL);
            LinearLayout.LayoutParams lp = ui.fillWidth();
            lp.bottomMargin = ui.dp(6);
            col.addView(c, lp);
        }
        return col;
    }

    // ---- the item list ----------------------------------------------------------------------

    private View sectionContent(final Session s, final Model.PreviewMeta meta, final long outcome) {
        final Model.SectionInfo sec = meta.sections.get(section);
        if (sec.items == 0) {
            TextView empty = ui.text(R.string.section_empty, Ui.BODY);
            empty.setGravity(Gravity.CENTER);
            empty.setPadding(ui.dp(24), ui.dp(48), ui.dp(24), ui.dp(24));
            return empty;
        }
        final int sectionIndex = section;
        final long total = sec.items;

        Paint mono = new Paint();
        mono.setTypeface(Typeface.MONOSPACE);
        mono.setTextSize(TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, 13 * scale, act.getResources().getDisplayMetrics()));
        charWidth = mono.measureText("0");

        // A spreadsheet section starts with a grid header, which carries the column widths.
        gridWidths = null;
        try {
            List<Model.Item> first = s.loadItems(outcome, sectionIndex, 0, 1);
            if (!first.isEmpty() && first.get(0) instanceof Model.GridHeader) {
                gridWidths = ((Model.GridHeader) first.get(0)).widths;
            }
        } catch (RuntimeException e) {
            gridWidths = null;
        }

        cache = new PageCache(new PageCache.Loader() {
            @Override
            public List<Model.Item> load(int sec2, long from, int count) {
                return s.loadItems(outcome, sec2, from, count);
            }
        }, s.pageExecutor(), s.mainExecutor(), new PageCache.Listener() {
            @Override
            public void onChanged() {
                if (adapter != null) {
                    adapter.notifyDataSetChanged();
                }
            }

            @Override
            public void onFailed(Exception e) {
                // The row stays a placeholder; the page is not retried on every draw.
            }
        });
        adapter = new ItemAdapter(sectionIndex, total);

        list = new ListView(act);
        list.setDivider(null);
        list.setDividerHeight(0);
        list.setSelector(new ColorDrawable(0));
        list.setCacheColorHint(0);
        list.setClipToPadding(false);
        list.setScrollBarStyle(View.SCROLLBARS_OUTSIDE_OVERLAY);
        list.setAdapter(adapter);
        if (savedPosition > 0 && savedPosition < total) {
            list.setSelectionFromTop(savedPosition, 0);
        }
        list.setOnScrollListener(new AbsListView.OnScrollListener() {
            @Override
            public void onScrollStateChanged(AbsListView view, int scrollState) {}

            @Override
            public void onScroll(AbsListView view, int firstVisibleItem, int visibleItemCount, int totalItemCount) {
                savedPosition = firstVisibleItem;
            }
        });

        if (gridWidths != null) {
            int cols = Math.min(gridWidths.length, MAX_GRID_COLS);
            int width = rowHeaderPx(total) + 1;
            for (int i = 0; i < cols; i++) {
                width += columnPx(i);
            }
            HorizontalScrollView hs = new HorizontalScrollView(act);
            hs.setFillViewport(true);
            hs.addView(list, new FrameLayout.LayoutParams(width, ViewGroup.LayoutParams.MATCH_PARENT));
            return pinchable(hs);
        }
        int pad = ui.dp(Adaptive.padding(act));
        list.setPadding(pad, ui.dp(8), pad, ui.dp(32));
        Ui.MaxWidthFrame frame = new Ui.MaxWidthFrame(act, ui.dp(Adaptive.twoPane(act) ? 860 : Adaptive.readingWidth(act)));
        frame.addView(list, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        FrameLayout outer = new FrameLayout(act);
        outer.addView(frame, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.MATCH_PARENT, Gravity.CENTER_HORIZONTAL));
        return pinchable(outer);
    }

    /** The one-line explanation of text the viewer composed; it appears (and stays) once such a line has been drawn. */
    private TextView syntheticLegend;

    private View withLegend(View content) {
        LinearLayout col = ui.vbox();
        TextView legend = ui.text(R.string.preview_synthetic_legend, Ui.CAPTION);
        int pad = ui.dp(Adaptive.padding(act));
        legend.setPadding(pad, ui.dp(4), pad, ui.dp(4));
        legend.setVisibility(View.GONE);
        syntheticLegend = legend;
        col.addView(legend, ui.fillWidth());
        col.addView(content, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));
        return col;
    }

    private void showSyntheticLegend() {
        final TextView legend = syntheticLegend;
        if (legend != null && legend.getVisibility() != View.VISIBLE) {
            // Not during the layout pass that is binding the row.
            legend.post(new Runnable() {
                @Override
                public void run() {
                    legend.setVisibility(View.VISIBLE);
                }
            });
        }
    }

    /** Makes a two-finger pinch on {@code content} change the text zoom. */
    private View pinchable(View content) {
        PinchFrame frame = new PinchFrame(act, Prefs.zoom(act), new PinchFrame.Listener() {
            @Override
            public void onPinch(float factor) {
                Prefs.setZoom(act, Zoom.pinch(Prefs.zoom(act), factor));
                act.refresh();
            }
        });
        frame.addView(content, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.MATCH_PARENT));
        return frame;
    }

    private int columnPx(int col) {
        int chars = col < gridWidths.length ? gridWidths[col] : 4;
        return Math.round((Math.max(4, Math.min(40, chars)) + 2) * charWidth);
    }

    private int rowHeaderPx(long rows) {
        int digits = Long.toString(Math.max(1, rows)).length() + 1;
        return Math.round((digits + 1) * charWidth);
    }

    // ---- adapter ----------------------------------------------------------------------------

    private final class ItemAdapter extends BaseAdapter {
        private final int sectionIndex;
        private final long total;

        ItemAdapter(int sectionIndex, long total) {
            this.sectionIndex = sectionIndex;
            this.total = total;
        }

        @Override
        public int getCount() {
            return (int) Math.min(total, Integer.MAX_VALUE);
        }

        @Override
        public Object getItem(int position) {
            return cache.get(sectionIndex, position, total);
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
        public int getViewTypeCount() {
            return TYPES;
        }

        @Override
        public int getItemViewType(int position) {
            Model.Item item = (Model.Item) getItem(position);
            if (item instanceof Model.ParaItem) {
                return TYPE_PARA;
            }
            if (item instanceof Model.TableRow) {
                return TYPE_ROW;
            }
            if (item instanceof Model.GridHeader) {
                return TYPE_GRID_HEAD;
            }
            if (item instanceof Model.GridRow) {
                return TYPE_GRID_ROW;
            }
            if (item instanceof Model.ImageItem) {
                return TYPE_IMAGE;
            }
            if (item instanceof Model.RuleItem) {
                return TYPE_RULE;
            }
            return TYPE_PENDING;
        }

        @Override
        public View getView(int position, View convertView, ViewGroup parent) {
            currentHitRow = search != null && search.isHit(hitIndex, sectionIndex, position);
            try {
                return buildView(position, convertView);
            } finally {
                currentHitRow = false;
            }
        }

        private View buildView(int position, View convertView) {
            Model.Item item = (Model.Item) getItem(position);
            switch (getItemViewType(position)) {
                case TYPE_PARA:
                    return paragraphView(convertView, ((Model.ParaItem) item).para);
                case TYPE_ROW:
                    return tableRow((Model.TableRow) item);
                case TYPE_GRID_HEAD:
                    return gridHeader((Model.GridHeader) item);
                case TYPE_GRID_ROW:
                    return gridRow(convertView, (Model.GridRow) item);
                case TYPE_IMAGE:
                    return imageRow((Model.ImageItem) item);
                case TYPE_RULE:
                    return ruleRow();
                default:
                    return pendingRow(convertView);
            }
        }
    }

    // ---- row views ----------------------------------------------------------------------------

    private View pendingRow(View reuse) {
        if (reuse instanceof TextView && "pending".equals(reuse.getTag())) {
            return reuse;
        }
        TextView t = ui.text("…", Ui.SMALL);
        t.setTag("pending");
        t.setPadding(0, ui.dp(10), 0, ui.dp(10));
        t.setLayoutParams(new AbsListView.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ui.dp(40)));
        return t;
    }

    private View ruleRow() {
        FrameLayout f = new FrameLayout(act);
        View line = ui.divider();
        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Math.max(1, ui.dp(1)));
        lp.topMargin = ui.dp(10);
        lp.bottomMargin = ui.dp(10);
        f.addView(line, lp);
        f.setLayoutParams(new AbsListView.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        return f;
    }

    /** Pictures the reader folded away, so that "show pictures at once" does not bring them back. */
    private static final java.util.Set<Integer> FOLDED = new java.util.HashSet<>();

    private View imageRow(final Model.ImageItem img) {
        final FrameLayout holder = new FrameLayout(act);
        holder.setLayoutParams(new AbsListView.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        fillImageRow(holder, img);
        return holder;
    }

    /**
     * A picture of the document in the text: nothing is decoded until the reader asks for it (or
     * has asked for pictures at once, or opened a picture file). The pixels come from the sandbox.
     */
    private void fillImageRow(final FrameLayout holder, final Model.ImageItem img) {
        final Session s = Session.get();
        Model.PreviewMeta meta = s.preview().meta;
        boolean file = meta != null && Model.isPictureFormat(meta.format);
        boolean can = img.id >= 0 && !img.external;
        boolean folded = FOLDED.contains(img.id);
        Session.ImageResult have = can ? s.cachedImage(img.id) : null;
        holder.removeAllViews();
        if (have != null && have.bitmap != null && !folded) {
            holder.addView(pictureView(holder, img, have), new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
            return;
        }
        boolean auto = can && !folded && (file || Prefs.autoImages(act));
        if (can && (s.imageLoading(img.id) || auto)) {
            startImage(holder, img);
            return;
        }
        holder.addView(imageCard(holder, img, can ? 0 : -1, 0), new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
    }

    /** Asks the sandbox for the picture and shows the result in {@code holder} when it arrives. */
    private void startImage(final FrameLayout holder, final Model.ImageItem img) {
        FOLDED.remove(img.id);
        holder.removeAllViews();
        holder.addView(imageCard(holder, img, 1, 0), new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        final Session.ImageCallback[] callback = new Session.ImageCallback[1];
        final View.OnAttachStateChangeListener[] detach = new View.OnAttachStateChangeListener[1];
        callback[0] = new Session.ImageCallback() {
            @Override
            public void onImage(int id, Session.ImageResult r) {
                holder.removeOnAttachStateChangeListener(detach[0]);
                holder.removeAllViews();
                if (r.bitmap != null) {
                    holder.addView(pictureView(holder, img, r), new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
                } else if (r.failure != null && r.failure.status == Native.CANCELLED) {
                    // Dropped from a full queue: the reader may ask again.
                    holder.addView(imageCard(holder, img, 0, 0), new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
                } else {
                    holder.addView(imageCard(holder, img, 2, r.failure == null ? 0 : r.failure.status), new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
                }
            }
        };
        // A row that scrolls away (or is rebuilt) withdraws its request, so that a fast scroll
        // through a long document does not leave a queue of pictures nobody looks at.
        detach[0] = new View.OnAttachStateChangeListener() {
            @Override
            public void onViewAttachedToWindow(View v) {}

            @Override
            public void onViewDetachedFromWindow(View v) {
                holder.removeOnAttachStateChangeListener(this);
                if (Session.get().cancelImage(img.id, callback[0])) {
                    holder.removeAllViews();
                    holder.addView(imageCard(holder, img, 0, 0), new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
                }
            }
        };
        holder.addOnAttachStateChangeListener(detach[0]);
        Session.get().loadImage(img.id, callback[0]);
    }

    /**
     * The card that stands for a picture. {@code state}: -1 the picture cannot be opened, 0 it can
     * (a button asks for it), 1 it is being decoded, 2 it could not be shown (with {@code status}).
     */
    private View imageCard(final FrameLayout holder, final Model.ImageItem img, int state, int status) {
        LinearLayout card = ui.card();
        card.setPadding(ui.dp(12), ui.dp(10), ui.dp(12), ui.dp(10));
        LinearLayout row = ui.hbox();
        row.addView(ui.icon(R.drawable.ic_file, R.color.arca_on_surface_variant, 24));
        row.addView(ui.hspace(10));
        LinearLayout text = ui.vbox();
        String kind = img.media.isEmpty() ? "" : " (" + Fmt.clip(Fmt.safe(img.media), 16) + (img.bytes > 0 ? ", " + Fmt.size(img.bytes) : "") + ")";
        text.addView(ui.text(ui.str(state == -1 ? R.string.image_placeholder : R.string.image_title) + kind, Ui.SMALL));
        if (!img.alt.isEmpty()) {
            text.addView(ui.text(Fmt.clip(Fmt.safe(img.alt), 200), Ui.BODY));
        }
        if (img.external) {
            text.addView(ui.space(4));
            text.addView(ui.pill(ui.str(R.string.image_remote), R.color.arca_warn, R.color.arca_warn_container));
        }
        if (state == 1) {
            text.addView(ui.space(4));
            text.addView(ui.text(R.string.image_loading, Ui.SMALL));
        } else if (state == 2) {
            text.addView(ui.space(4));
            text.addView(ui.text(ui.str(Errors.pictureText(status)), Ui.SMALL));
        }
        row.addView(text, ui.weight(1f));
        card.addView(row, ui.fillWidth());
        if (state == 0 || state == 2) {
            card.addView(ui.space(6));
            LinearLayout.LayoutParams blp = ui.wrapLp();
            card.addView(ui.button(R.string.image_show, Ui.TONAL, 0, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    startImage(holder, img);
                }
            }), blp);
        }
        FrameLayout f = new FrameLayout(act);
        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.topMargin = ui.dp(6);
        lp.bottomMargin = ui.dp(6);
        f.addView(card, lp);
        return f;
    }

    /** The picture itself in the text; a tap opens it full screen, the second button folds it away. */
    private View pictureView(final FrameLayout holder, final Model.ImageItem img, Session.ImageResult r) {
        LinearLayout col = ui.vbox();
        col.setPadding(0, ui.dp(6), 0, ui.dp(6));
        ImageView iv = new ImageView(act);
        iv.setImageBitmap(r.bitmap);
        iv.setAdjustViewBounds(true);
        iv.setScaleType(ImageView.ScaleType.FIT_CENTER);
        iv.setMaxHeight(ui.dp(460));
        iv.setContentDescription(ui.str(R.string.image_description, img.alt.isEmpty() ? "" : Fmt.clip(Fmt.safe(img.alt), 80)));
        iv.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                ImageViewer.show(act, img.id, img.alt);
            }
        });
        // At its own size when it is small, reduced to the width of the text when it is large.
        col.addView(iv, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        LinearLayout bar = ui.hbox();
        List<String> lines = ImageViewer.processing(ui, r.facts);
        TextView caption = ui.text(Fmt.clip(android.text.TextUtils.join(" · ", lines), 200), Ui.CAPTION);
        bar.addView(caption, ui.weight(1f));
        bar.addView(ui.button(R.string.image_hide, Ui.TEXT_ONLY, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                FOLDED.add(img.id);
                fillImageRow(holder, img);
            }
        }), ui.wrapLp());
        col.addView(bar, ui.fillWidth());
        if (r.facts.hasHiddenParts()) {
            col.addView(ui.pill(ui.str(R.string.image_info_hidden_title), R.color.arca_warn, R.color.arca_warn_container));
        }
        return col;
    }

    // ---- paragraphs -------------------------------------------------------------------------

    private View paragraphView(View reuse, Model.Para p) {
        TextView t;
        if (reuse instanceof TextView && "para".equals(reuse.getTag())) {
            t = (TextView) reuse;
        } else {
            t = new TextView(act);
            t.setTag("para");
            t.setLayoutParams(new AbsListView.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
            t.setMovementMethod(LinkMovementMethod.getInstance());
            Compat.noTextClassifier(t);
        }
        styleParagraph(t, p);
        return t;
    }

    private void styleParagraph(final TextView t, final Model.Para p) {
        float size = 16;
        Typeface face = Typeface.DEFAULT;
        int color = ui.color(R.color.arca_on_surface);
        int top = 6;
        int bottom = 6;
        int left = p.level * 20;
        boolean italic = false;
        switch (p.style) {
            case "Title": size = 28; face = Typeface.create("sans-serif-medium", Typeface.NORMAL); top = 14; bottom = 10; break;
            case "Subtitle": size = 20; color = ui.color(R.color.arca_on_surface_variant); bottom = 10; break;
            case "Heading1": size = 24; face = Typeface.create("sans-serif-medium", Typeface.NORMAL); top = 16; bottom = 8; break;
            case "Heading2": size = 21; face = Typeface.create("sans-serif-medium", Typeface.NORMAL); top = 14; bottom = 6; break;
            case "Heading3": size = 19; face = Typeface.create("sans-serif-medium", Typeface.NORMAL); top = 12; break;
            case "Heading4": size = 17.5f; face = Typeface.create("sans-serif-medium", Typeface.NORMAL); top = 10; break;
            case "Heading5": size = 16; face = Typeface.create("sans-serif-medium", Typeface.NORMAL); top = 10; break;
            case "Heading6": size = 15; face = Typeface.create("sans-serif-medium", Typeface.NORMAL); top = 10; break;
            case "Quote": italic = true; left += 16; color = ui.color(R.color.arca_on_surface_variant); break;
            case "Code": size = 14; face = Typeface.MONOSPACE; break;
            case "Note":
            case "Comment":
            case "Footnote":
            case "HeaderFooter":
                size = 13; color = ui.color(R.color.arca_on_surface_variant); break;
            default: break;
        }
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, size * scale);
        t.setTypeface(face, italic ? Typeface.ITALIC : Typeface.NORMAL);
        t.setTextColor(color);
        t.setLineSpacing(0f, 1.18f);
        t.setPadding(ui.dp(left), ui.dp(top), 0, ui.dp(bottom));
        t.setBackground(p.style.equals("Code") ? ui.rounded(ui.color(R.color.arca_code_bg), 0, 6) : null);
        switch (p.align) {
            case 1: t.setGravity(Gravity.CENTER_HORIZONTAL); break;
            case 2: t.setGravity(Gravity.END); break;
            default: t.setGravity(Gravity.START); break;
        }
        t.setJustificationMode(p.align == 3 ? Layout.JUSTIFICATION_MODE_INTER_WORD : Layout.JUSTIFICATION_MODE_NONE);
        t.setText(render(p), TextView.BufferType.SPANNABLE);
        t.setOnLongClickListener(new View.OnLongClickListener() {
            @Override
            public boolean onLongClick(View v) {
                ui.copy("text", Fmt.safe(p.plain()));
                return true;
            }
        });
    }

    /** Builds the styled text of a paragraph; links are underlined and open a copy dialog. */
    private CharSequence render(Model.Para p) {
        boolean synthetic = false;
        SpannableStringBuilder sb = new SpannableStringBuilder();
        if (!p.marker.isEmpty()) {
            sb.append(Fmt.clip(Fmt.safe(p.marker), 16)).append(' ');
        }
        for (Model.Run r : p.runs) {
            int start = sb.length();
            sb.append(Fmt.safe(r.text));
            int end = sb.length();
            if (end == start) {
                continue;
            }
            int st = r.style;
            if ((st & Model.BOLD) != 0) {
                sb.setSpan(new StyleSpan(Typeface.BOLD), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.ITALIC) != 0) {
                sb.setSpan(new StyleSpan(Typeface.ITALIC), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.UNDERLINE) != 0) {
                sb.setSpan(new UnderlineSpan(), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.STRIKE) != 0) {
                sb.setSpan(new StrikethroughSpan(), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.MONO) != 0) {
                sb.setSpan(new TypefaceSpan("monospace"), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                sb.setSpan(new BackgroundColorSpan(ui.color(R.color.arca_code_bg)), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.SUPER) != 0) {
                sb.setSpan(new SuperscriptSpan(), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                sb.setSpan(new RelativeSizeSpan(0.75f), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.SUB) != 0) {
                sb.setSpan(new SubscriptSpan(), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                sb.setSpan(new RelativeSizeSpan(0.75f), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.HIDDEN) != 0) {
                sb.setSpan(new ForegroundColorSpan(ui.color(R.color.arca_hidden_text)), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                sb.setSpan(new StyleSpan(Typeface.ITALIC), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.SYNTHETIC) != 0) {
                // The viewer's own words (a rebuilt query, a diagram link): never in the colour or face of the document's text.
                synthetic = true;
                sb.setSpan(new ForegroundColorSpan(ui.color(R.color.arca_on_surface_variant)), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                sb.setSpan(new StyleSpan(Typeface.ITALIC), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.DELETED) != 0) {
                sb.setSpan(new ForegroundColorSpan(ui.color(R.color.arca_deleted)), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                sb.setSpan(new StrikethroughSpan(), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if ((st & Model.INSERTED) != 0) {
                sb.setSpan(new ForegroundColorSpan(ui.color(R.color.arca_inserted)), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                sb.setSpan(new UnderlineSpan(), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
            if (!r.href.isEmpty()) {
                final String href = r.href;
                sb.setSpan(new ForegroundColorSpan(ui.color(R.color.arca_link)), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                sb.setSpan(new UnderlineSpan(), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                sb.setSpan(new ClickableSpan() {
                    @Override
                    public void onClick(View widget) {
                        // The link is never opened from here: the person decides, outside the sandbox.
                        ui.showAddress(act, ui.str(R.string.link_dialog_title), href);
                    }

                    @Override
                    public void updateDrawState(android.text.TextPaint ds) {
                        // keep the span colours set above
                    }
                }, start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
        }
        if (synthetic) {
            showSyntheticLegend();
        }
        return highlighted(sb);
    }

    // ---- tables -----------------------------------------------------------------------------

    private View tableRow(Model.TableRow row) {
        LinearLayout line = new LinearLayout(act);
        line.setOrientation(LinearLayout.HORIZONTAL);
        line.setLayoutParams(new AbsListView.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        float total = Math.max(1, row.columns);
        for (Model.Cell cell : row.cells) {
            LinearLayout box = ui.vbox();
            int fill = cell.header ? ui.color(R.color.arca_surface_variant) : ui.color(R.color.arca_surface);
            box.setBackground(ui.rounded(fill, ui.color(R.color.arca_outline), 0));
            box.setPadding(ui.dp(8), ui.dp(6), ui.dp(8), ui.dp(6));
            if (cell.paras.isEmpty()) {
                box.addView(new View(act), new LinearLayout.LayoutParams(1, ui.dp(20)));
            }
            for (Model.Para p : cell.paras) {
                TextView t = new TextView(act);
                t.setMovementMethod(LinkMovementMethod.getInstance());
                Compat.noTextClassifier(t);
                styleParagraph(t, p);
                t.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14 * scale);
                t.setPadding(0, ui.dp(2), 0, ui.dp(2));
                if (cell.header) {
                    t.setTypeface(Typeface.create("sans-serif-medium", Typeface.NORMAL));
                }
                box.addView(t, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
            }
            line.addView(box, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, cell.span / total));
        }
        return line;
    }

    // ---- spreadsheet grids ------------------------------------------------------------------

    private static String columnName(int index) {
        StringBuilder sb = new StringBuilder();
        int n = index;
        do {
            sb.insert(0, (char) ('A' + n % 26));
            n = n / 26 - 1;
        } while (n >= 0);
        return sb.toString();
    }

    private TextView gridCell(int widthPx, int gravity, boolean header) {
        TextView t = new TextView(act);
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13 * scale);
        t.setTypeface(Typeface.MONOSPACE);
        t.setTextColor(ui.color(R.color.arca_on_surface));
        t.setSingleLine(true);
        t.setEllipsize(TextUtils.TruncateAt.END);
        t.setGravity(gravity | Gravity.CENTER_VERTICAL);
        t.setPadding(ui.dp(4), ui.dp(4), ui.dp(4), ui.dp(4));
        t.setBackground(ui.rounded(header ? ui.color(R.color.arca_surface_variant) : ui.color(R.color.arca_surface),
            ui.color(R.color.arca_outline), 0));
        t.setLayoutParams(new LinearLayout.LayoutParams(widthPx, ViewGroup.LayoutParams.MATCH_PARENT));
        return t;
    }

    private View gridHeader(Model.GridHeader h) {
        LinearLayout row = new LinearLayout(act);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setMinimumHeight(ui.dp(32));
        row.setLayoutParams(new AbsListView.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ui.dp(32)));
        row.addView(gridCell(rowHeaderPx(adapter.total), Gravity.CENTER, true));
        int cols = Math.min(h.widths.length, MAX_GRID_COLS);
        for (int i = 0; i < cols; i++) {
            TextView c = gridCell(columnPx(i), Gravity.CENTER, true);
            c.setText(columnName(i));
            c.setTypeface(Typeface.create(Typeface.MONOSPACE, Typeface.BOLD));
            row.addView(c);
        }
        return row;
    }

    private View gridRow(View reuse, Model.GridRow r) {
        int cols = gridWidths == null ? 0 : Math.min(gridWidths.length, MAX_GRID_COLS);
        LinearLayout row;
        if (reuse instanceof LinearLayout && "grid".equals(reuse.getTag()) && ((LinearLayout) reuse).getChildCount() == cols + 1) {
            row = (LinearLayout) reuse;
        } else {
            row = new LinearLayout(act);
            row.setOrientation(LinearLayout.HORIZONTAL);
            row.setTag("grid");
            row.setLayoutParams(new AbsListView.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ui.dp(32)));
            row.addView(gridCell(rowHeaderPx(adapter.total), Gravity.CENTER, true));
            for (int i = 0; i < cols; i++) {
                row.addView(gridCell(columnPx(i), Gravity.START, false));
            }
        }
        TextView label = (TextView) row.getChildAt(0);
        label.setText(Long.toString(r.index + 1));
        label.setAlpha(r.hidden ? 0.5f : 1f);
        for (int i = 0; i < cols; i++) {
            TextView c = (TextView) row.getChildAt(i + 1);
            c.setText("");
            c.setOnClickListener(null);
            c.setClickable(false);
            c.setPaintFlags(c.getPaintFlags() & ~Paint.UNDERLINE_TEXT_FLAG);
            c.setAlpha(r.hidden ? 0.5f : 1f);
            c.setGravity(Gravity.START | Gravity.CENTER_VERTICAL);
            c.setTypeface(Typeface.MONOSPACE);
            c.setTextColor(ui.color(R.color.arca_on_surface));
        }
        for (Model.GridCell cell : r.cells) {
            if (cell.col < 0 || cell.col >= cols) {
                continue;
            }
            TextView c = (TextView) row.getChildAt(cell.col + 1);
            c.setText(highlighted(Fmt.safe(cell.text)));
            switch (cell.kind) {
                case Model.CELL_NUMBER:
                case Model.CELL_DATE:
                    c.setGravity(Gravity.END | Gravity.CENTER_VERTICAL);
                    break;
                case Model.CELL_BOOL:
                    c.setGravity(Gravity.CENTER);
                    break;
                case Model.CELL_ERROR:
                    c.setGravity(Gravity.CENTER);
                    c.setTextColor(ui.color(R.color.arca_danger));
                    break;
                case Model.CELL_FORMULA:
                    c.setTypeface(Typeface.MONOSPACE, Typeface.ITALIC);
                    c.setTextColor(ui.color(R.color.arca_link));
                    break;
                case Model.CELL_CALCULATED:
                    // Calculated by Arca, not stored in the file: italic, and numbers stay on the right.
                    c.setTypeface(Typeface.DEFAULT, Typeface.ITALIC);
                    c.setTextColor(ui.color(R.color.arca_link));
                    if (Fmt.looksNumeric(cell.text)) {
                        c.setGravity(Gravity.END | Gravity.CENTER_VERTICAL);
                    }
                    break;
                default:
                    break;
            }
            final String text = cell.text;
            if (!cell.href.isEmpty()) {
                // A link of the document is only shown: the address, with a button to copy it.
                final String href = cell.href;
                c.setTextColor(ui.color(R.color.arca_link));
                c.setPaintFlags(c.getPaintFlags() | Paint.UNDERLINE_TEXT_FLAG);
                c.setOnClickListener(new View.OnClickListener() {
                    @Override
                    public void onClick(View v) {
                        ui.showAddress(act, ui.str(R.string.link_dialog_title), href);
                    }
                });
            }
            c.setOnLongClickListener(new View.OnLongClickListener() {
                @Override
                public boolean onLongClick(View v) {
                    ui.showText(act, ui.str(R.string.cell_dialog_title), Fmt.safe(text), "cell");
                    return true;
                }
            });
        }
        return row;
    }

    @Override
    public void onSessionChanged() {
        Session s = Session.get();
        if (s.phase() != Session.Phase.READY) {
            act.refresh();
            return;
        }
        // Running, finished, failed or cancelled: build() shows the right thing for each.
        act.refresh();
    }

    @Override
    public boolean onBack() {
        if (searchOpen) {
            closeSearch();
            return true;
        }
        return false;
    }

    @Override
    public void onLeave() {
        resetSearch();
        resetOutline();
        if (live == this) {
            live = null;
        }
        Session.get().closePreview();
        shownOutcomeKey = 0;
    }
}
