// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.Intent;
import android.text.TextUtils;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.widget.HorizontalScrollView;
import android.widget.LinearLayout;
import android.widget.TextView;

import org.arca.viewer.MainActivity;
import org.arca.viewer.R;
import org.arca.viewer.Session;
import org.arca.viewer.core.Capabilities;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Model;

import java.lang.ref.WeakReference;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/**
 * The Threat X-Ray: the verdict, what was found and blocked, the macros, links, metadata and
 * embedded objects, and the ways forward (safe preview, sterile copy).
 */
public final class ResultScreen extends Screen {
    private static final int TAB_FINDINGS = 0;
    private static final int TAB_MACROS = 1;
    private static final int TAB_LINKS = 2;
    private static final int TAB_META = 3;
    private static final int TAB_EMBEDDED = 4;
    private static final int TAB_DETAILS = 5;

    /** Rows added at a time in long lists. */
    private static final int CHUNK = 60;
    /** Longest macro source shown in full, in characters. */
    private static final int MAX_SOURCE = 12_000;

    private static final String[] CATEGORY_ORDER = {
        "ActiveContent", "RemoteContent", "Concealment", "Structural", "Encryption", "PrivacyLeak"};

    // State that survives the screen being rebuilt when the window changes shape.
    // (a weak reference: a closed document's report must not be kept alive by this screen)
    private static WeakReference<Model.Report> lastReport = new WeakReference<>(null);
    private static int tab;
    private static final Set<Integer> OPEN = new HashSet<>();

    private FrameLayout tabBody;
    private LinearLayout chipRow;
    private List<Integer> tabs;

    public ResultScreen(MainActivity act) {
        super(act);
    }

    @Override
    public View build() {
        Session s = Session.get();
        final Model.Report r = s.report();
        if (r == null || s.phase() != Session.Phase.READY) {
            act.getWindow().getDecorView().post(new Runnable() {
                @Override
                public void run() {
                    act.home();
                }
            });
            return new View(act);
        }
        if (r != lastReport.get()) {
            lastReport = new WeakReference<>(r);
            OPEN.clear();
            tab = -1;
        }
        tabs = tabsOf(r);
        if (!tabs.contains(tab)) {
            tab = tabs.get(0);
        }

        View share = ui.iconButton(R.drawable.ic_share, R.string.share_report, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                shareReport(r);
            }
        });
        View head = header(ui.str(R.string.result_title), true, share);

        int pad = Adaptive.padding(act);
        View body;
        if (Adaptive.twoPane(act)) {
            LinearLayout cols = ui.hbox();
            cols.setGravity(Gravity.TOP);
            LinearLayout.LayoutParams left = new LinearLayout.LayoutParams(ui.dp(Adaptive.leftPane(act) + pad * 2),
                ViewGroup.LayoutParams.MATCH_PARENT);
            cols.addView(scrollColumn(summary(r), Adaptive.leftPane(act) + pad * 2, pad), left);
            View divider = ui.divider();
            cols.addView(divider, new LinearLayout.LayoutParams(Math.max(1, ui.dp(1)), ViewGroup.LayoutParams.MATCH_PARENT));
            cols.addView(scrollColumn(details(r), 860, pad), new LinearLayout.LayoutParams(0,
                ViewGroup.LayoutParams.MATCH_PARENT, 1f));
            body = cols;
        } else {
            LinearLayout col = ui.vbox();
            col.addView(summary(r));
            col.addView(ui.space(20));
            col.addView(details(r));
            body = scrollColumn(col, Adaptive.readingWidth(act), pad);
        }
        return page(head, body);
    }

    // ---- summary: verdict, file, counters, actions ------------------------------------------

    private View summary(final Model.Report r) {
        LinearLayout col = ui.vbox();
        col.addView(verdictBanner(r));
        col.addView(ui.space(12));
        col.addView(fileCard(r));
        col.addView(ui.space(12));
        col.addView(counters(r));
        col.addView(ui.space(16));
        col.addView(actions(r));
        return col;
    }

    private View verdictBanner(Model.Report r) {
        int icon;
        int fg;
        int bg;
        int title;
        int text;
        switch (r.verdict) {
            case CLEAN:
                icon = R.drawable.ic_verdict_clean;
                fg = R.color.arca_safe;
                bg = R.color.arca_safe_container;
                title = R.string.verdict_clean_title;
                text = R.string.verdict_clean_text;
                break;
            case HIDDEN_TRACKERS:
                icon = R.drawable.ic_verdict_warn;
                fg = R.color.arca_warn;
                bg = R.color.arca_warn_container;
                title = R.string.verdict_trackers_title;
                text = R.string.verdict_trackers_text;
                break;
            case ACTIVE_NEUTRALIZED:
                icon = R.drawable.ic_verdict_danger;
                fg = R.color.arca_danger;
                bg = R.color.arca_danger_container;
                title = R.string.verdict_active_title;
                text = R.string.verdict_active_text;
                break;
            case REJECTED:
                icon = R.drawable.ic_verdict_danger;
                fg = R.color.arca_danger;
                bg = R.color.arca_danger_container;
                title = R.string.verdict_rejected_title;
                text = R.string.verdict_rejected_text;
                break;
            default:
                icon = R.drawable.ic_verdict_unknown;
                fg = R.color.arca_unknown;
                bg = R.color.arca_unknown_container;
                title = R.string.verdict_unverified_title;
                text = R.string.verdict_unverified_text;
                break;
        }
        LinearLayout banner = ui.banner(bg);
        LinearLayout head = ui.hbox();
        head.addView(ui.icon(icon, fg, 36));
        head.addView(ui.hspace(12));
        TextView t = ui.heading(ui.str(title), Ui.TITLE);
        t.setTextColor(ui.color(fg));
        head.addView(t, ui.weight(1f));
        banner.addView(head);
        banner.addView(ui.space(8));
        banner.addView(ui.text(text, Ui.BODY));
        if (r.verdict == Model.Verdict.REJECTED && !r.rejection.isEmpty()) {
            banner.addView(ui.space(8));
            banner.addView(ui.text(ui.str(R.string.verdict_rejected_reason, Fmt.safe(r.rejection)), Ui.SMALL));
        }
        banner.setContentDescription(ui.str(title) + ". " + ui.str(text));
        return banner;
    }

    private View fileCard(final Model.Report r) {
        Session s = Session.get();
        LinearLayout card = ui.card();
        card.addView(ui.text(R.string.file_label, Ui.CAPTION));
        TextView name = ui.text(Fmt.safe(s.name().isEmpty() ? "?" : s.name()), Ui.BODY);
        name.setEllipsize(TextUtils.TruncateAt.MIDDLE);
        name.setMaxLines(2);
        card.addView(name);
        card.addView(ui.space(8));
        card.addView(ui.text(R.string.format_label, Ui.CAPTION));
        card.addView(ui.text(Labels.format(act, r.format) + (s.size() >= 0 ? " · " + Fmt.size(s.size()) : ""), Ui.BODY));
        card.addView(ui.space(8));
        LinearLayout hashRow = ui.hbox();
        LinearLayout hashCol = ui.vbox();
        hashCol.addView(ui.text(R.string.sha256_label, Ui.CAPTION));
        hashCol.addView(ui.text(Fmt.grouped(r.sha256), Ui.MONO));
        hashRow.addView(hashCol, ui.weight(1f));
        hashRow.addView(ui.iconButton(R.drawable.ic_copy, R.string.copy_hash, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                ui.copy("SHA-256", r.sha256);
            }
        }));
        card.addView(hashRow, ui.fillWidth());
        return card;
    }

    private View counters(Model.Report r) {
        LinearLayout row = ui.hbox();
        row.setGravity(Gravity.TOP);
        row.addView(tile(r.trackers, R.string.stat_trackers, r.trackers > 0 ? R.color.arca_warn : R.color.arca_on_surface), ui.weight(1f));
        row.addView(ui.hspace(8));
        row.addView(tile(r.active, R.string.stat_active, r.active > 0 ? R.color.arca_danger : R.color.arca_on_surface), ui.weight(1f));
        row.addView(ui.hspace(8));
        row.addView(tile(r.findings.size(), R.string.stat_findings, R.color.arca_on_surface), ui.weight(1f));
        return row;
    }

    private View tile(long value, int labelRes, int colorRes) {
        LinearLayout l = ui.card();
        l.setGravity(Gravity.CENTER_HORIZONTAL);
        l.setPadding(ui.dp(8), ui.dp(12), ui.dp(8), ui.dp(12));
        TextView v = ui.text(Long.toString(value), Ui.HEADLINE);
        v.setTextColor(ui.color(colorRes));
        l.addView(v);
        TextView label = ui.text(labelRes, Ui.CAPTION);
        label.setGravity(Gravity.CENTER);
        l.addView(label);
        l.setContentDescription(value + " " + ui.str(labelRes));
        return l;
    }

    private View actions(final Model.Report r) {
        LinearLayout col = ui.vbox();
        boolean rejected = r.verdict == Model.Verdict.REJECTED;
        boolean preview = !rejected && Capabilities.canPreview(r.format);
        boolean sanitize = !rejected && Capabilities.canSanitize(r.format);

        col.addView(ui.button(R.string.action_preview, Ui.PRIMARY, R.drawable.ic_eye, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.push(MainActivity.Route.PREVIEW);
            }
        }), ui.fillWidth());
        col.getChildAt(0).setEnabled(preview);
        col.getChildAt(0).setAlpha(preview ? 1f : 0.4f);
        if (!preview) {
            col.addView(ui.text(rejected ? R.string.preview_blocked_rejected : R.string.preview_not_available, Ui.CAPTION));
        }
        col.addView(ui.space(8));
        View sanitizeButton = ui.button(R.string.action_sanitize, Ui.TONAL, R.drawable.ic_sanitize, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.push(MainActivity.Route.SANITIZE);
            }
        });
        sanitizeButton.setEnabled(sanitize);
        sanitizeButton.setAlpha(sanitize ? 1f : 0.4f);
        col.addView(sanitizeButton, ui.fillWidth());
        if (!sanitize) {
            col.addView(ui.text(rejected ? R.string.sanitize_blocked_rejected : R.string.sanitize_not_available, Ui.CAPTION));
        }
        return col;
    }

    // ---- details: tabs ------------------------------------------------------------------------

    private List<Integer> tabsOf(Model.Report r) {
        List<Integer> t = new ArrayList<>();
        if (!r.findings.isEmpty()) {
            t.add(TAB_FINDINGS);
        }
        if (!r.macros.isEmpty()) {
            t.add(TAB_MACROS);
        }
        if (!r.refs.isEmpty()) {
            t.add(TAB_LINKS);
        }
        if (!r.meta.isEmpty()) {
            t.add(TAB_META);
        }
        if (!r.embedded.isEmpty()) {
            t.add(TAB_EMBEDDED);
        }
        t.add(TAB_DETAILS);
        return t;
    }

    private String tabLabel(int id, Model.Report r) {
        switch (id) {
            case TAB_FINDINGS: return ui.str(R.string.tab_findings) + " " + r.findings.size();
            case TAB_MACROS: return ui.str(R.string.tab_macros) + " " + r.macros.size();
            case TAB_LINKS: return ui.str(R.string.tab_links) + " " + r.refs.size();
            case TAB_META: return ui.str(R.string.tab_meta) + " " + r.meta.size();
            case TAB_EMBEDDED: return ui.str(R.string.tab_embedded) + " " + r.embedded.size();
            default: return ui.str(R.string.tab_details);
        }
    }

    private View details(final Model.Report r) {
        LinearLayout col = ui.vbox();
        if (r.verdict == Model.Verdict.CLEAN && r.findings.isEmpty()) {
            col.addView(ui.text(R.string.nothing_found, Ui.SMALL));
            col.addView(ui.space(12));
        }
        HorizontalScrollView hs = new HorizontalScrollView(act);
        hs.setHorizontalScrollBarEnabled(false);
        chipRow = ui.hbox();
        hs.addView(chipRow);
        col.addView(hs, ui.fillWidth());
        col.addView(ui.space(12));
        tabBody = new FrameLayout(act);
        col.addView(tabBody, ui.fillWidth());
        renderTabs(r);
        return col;
    }

    private void renderTabs(final Model.Report r) {
        chipRow.removeAllViews();
        for (final int id : tabs) {
            chipRow.addView(ui.chip(tabLabel(id, r), id == tab, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    tab = id;
                    renderTabs(r);
                }
            }));
            chipRow.addView(ui.hspace(8));
        }
        tabBody.removeAllViews();
        switch (tab) {
            case TAB_FINDINGS: tabBody.addView(findingsTab(r)); break;
            case TAB_MACROS: tabBody.addView(macrosTab(r)); break;
            case TAB_LINKS: tabBody.addView(linksTab(r)); break;
            case TAB_META: tabBody.addView(metaTab(r)); break;
            case TAB_EMBEDDED: tabBody.addView(embeddedTab(r)); break;
            default: tabBody.addView(detailsTab(r)); break;
        }
    }

    /** Adds rows to {@code col} {@link #CHUNK} at a time, with a "show more" button in between. */
    private void appendChunked(final LinearLayout col, final List<View> rows) {
        int shown = 0;
        for (; shown < rows.size() && shown < CHUNK; shown++) {
            col.addView(rows.get(shown));
        }
        if (shown < rows.size()) {
            addMore(col, rows, shown);
        }
    }

    private void addMore(final LinearLayout col, final List<View> rows, final int from) {
        final int left = rows.size() - from;
        final View[] more = new View[1];
        more[0] = ui.button(ui.str(R.string.show_more, left), Ui.OUTLINE, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                col.removeView(more[0]);
                int end = Math.min(rows.size(), from + CHUNK);
                for (int i = from; i < end; i++) {
                    col.addView(rows.get(i));
                }
                if (end < rows.size()) {
                    addMore(col, rows, end);
                }
            }
        });
        col.addView(more[0], ui.fillWidth());
    }

    // ---- findings ---------------------------------------------------------------------------

    private View findingsTab(Model.Report r) {
        LinearLayout col = ui.vbox();
        List<View> rows = new ArrayList<>();
        Set<Integer> placed = new HashSet<>();
        for (String cat : CATEGORY_ORDER) {
            List<Integer> idx = new ArrayList<>();
            for (int i = 0; i < r.findings.size(); i++) {
                if (r.findings.get(i).category.equals(cat)) {
                    idx.add(i);
                }
            }
            if (idx.isEmpty()) {
                continue;
            }
            rows.add(groupHeader(Labels.category(act, cat) + " · " + idx.size()));
            for (int i : idx) {
                rows.add(findingCard(r.findings.get(i), i));
                placed.add(i);
            }
        }
        List<Integer> other = new ArrayList<>();
        for (int i = 0; i < r.findings.size(); i++) {
            if (!placed.contains(i)) {
                other.add(i);
            }
        }
        if (!other.isEmpty()) {
            rows.add(groupHeader(ui.str(R.string.category_other) + " · " + other.size()));
            for (int i : other) {
                rows.add(findingCard(r.findings.get(i), i));
            }
        }
        appendChunked(col, rows);
        return col;
    }

    private View groupHeader(String text) {
        TextView t = ui.heading(text, Ui.LABEL);
        t.setPadding(0, ui.dp(12), 0, ui.dp(6));
        return t;
    }

    private View findingCard(final Model.Finding f, final int index) {
        final LinearLayout card = ui.card();
        card.setPadding(ui.dp(14), ui.dp(12), ui.dp(14), ui.dp(12));
        LinearLayout.LayoutParams lp = ui.fillWidth();
        lp.bottomMargin = ui.dp(8);
        card.setLayoutParams(lp);

        LinearLayout head = ui.hbox();
        head.addView(ui.pill(Labels.severity(act, f.severity), Labels.severityColor(f.severity), R.color.arca_surface_variant));
        head.addView(ui.hspace(10));
        TextView title = ui.text(Labels.threatTitle(act, f.kind), Ui.LABEL);
        title.setTextSize(15);
        head.addView(title, ui.weight(1f));
        card.addView(head);

        if (f.blocked) {
            card.addView(ui.space(6));
            card.addView(ui.pill(ui.str(R.string.blocked), R.color.arca_safe, R.color.arca_safe_container));
        }
        if (!f.detail.isEmpty()) {
            card.addView(ui.space(6));
            TextView d = ui.text(Fmt.clip(Fmt.safe(f.detail), 600), Ui.MONO);
            d.setTextIsSelectable(true);
            card.addView(d);
        }
        if (!f.location.isEmpty()) {
            card.addView(ui.space(4));
            TextView l = ui.text(Fmt.clip(Fmt.safe(f.location), 300), Ui.CAPTION);
            l.setEllipsize(TextUtils.TruncateAt.MIDDLE);
            l.setMaxLines(2);
            card.addView(l);
        }
        final TextView explain = ui.text(Labels.threatText(act, f.kind), Ui.SMALL);
        explain.setPadding(0, ui.dp(8), 0, 0);
        explain.setVisibility(OPEN.contains(index) ? View.VISIBLE : View.GONE);
        card.addView(explain);
        final TextView hint = ui.text(OPEN.contains(index) ? R.string.hide_explanation : R.string.show_explanation, Ui.CAPTION);
        hint.setTextColor(ui.color(R.color.arca_primary));
        hint.setPadding(0, ui.dp(8), 0, 0);
        card.addView(hint);

        card.setClickable(true);
        card.setFocusable(true);
        card.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                boolean open = !OPEN.contains(index);
                if (open) {
                    OPEN.add(index);
                } else {
                    OPEN.remove(index);
                }
                explain.setVisibility(open ? View.VISIBLE : View.GONE);
                hint.setText(open ? R.string.hide_explanation : R.string.show_explanation);
            }
        });
        return card;
    }

    // ---- macros -----------------------------------------------------------------------------

    private View macrosTab(Model.Report r) {
        LinearLayout col = ui.vbox();
        col.addView(ui.text(R.string.macros_note, Ui.SMALL));
        col.addView(ui.space(12));
        List<View> rows = new ArrayList<>();
        for (Model.MacroModule m : r.macros) {
            rows.add(macroCard(m));
        }
        appendChunked(col, rows);
        return col;
    }

    private View macroCard(final Model.MacroModule m) {
        LinearLayout card = ui.card();
        LinearLayout.LayoutParams lp = ui.fillWidth();
        lp.bottomMargin = ui.dp(10);
        card.setLayoutParams(lp);
        card.addView(ui.text(Fmt.clip(Fmt.safe(m.name.isEmpty() ? "(unnamed)" : m.name), 120), Ui.TITLE));
        card.addView(ui.text(Labels.macroLang(act, m.language) + " · " + Fmt.clip(Fmt.safe(m.location), 160), Ui.CAPTION));
        if (!m.autoExec.isEmpty()) {
            card.addView(ui.space(8));
            card.addView(ui.text(R.string.macro_autoexec, Ui.LABEL));
            card.addView(pills(m.autoExec, R.color.arca_danger, R.color.arca_danger_container));
        }
        if (!m.suspicious.isEmpty()) {
            card.addView(ui.space(8));
            card.addView(ui.text(R.string.macro_suspicious, Ui.LABEL));
            card.addView(pills(m.suspicious, R.color.arca_warn, R.color.arca_warn_container));
        }
        if (!m.iocs.isEmpty()) {
            card.addView(ui.space(8));
            card.addView(ui.text(R.string.macro_iocs, Ui.LABEL));
            for (String ioc : m.iocs) {
                TextView t = ui.text(Fmt.clip(Fmt.safe(ioc), 300), Ui.MONO);
                t.setTextIsSelectable(true);
                card.addView(t);
            }
        }
        if (!m.source.isEmpty()) {
            card.addView(ui.space(10));
            card.addView(ui.text(R.string.macro_source, Ui.LABEL));
            String shown = m.source.length() > MAX_SOURCE ? m.source.substring(0, MAX_SOURCE) : m.source;
            TextView code = ui.text(Fmt.safe(shown), Ui.MONO);
            code.setBackground(ui.rounded(ui.color(R.color.arca_code_bg), 0, 8));
            code.setPadding(ui.dp(10), ui.dp(8), ui.dp(10), ui.dp(8));
            code.setTextIsSelectable(true);
            HorizontalScrollView hs = new HorizontalScrollView(act);
            hs.addView(code);
            card.addView(hs, ui.fillWidth());
            if (m.truncated || m.source.length() > MAX_SOURCE) {
                card.addView(ui.text(R.string.macro_truncated, Ui.CAPTION));
            }
            final String full = m.source;
            card.addView(ui.space(6));
            card.addView(ui.button(R.string.copy_source, Ui.TEXT_ONLY, R.drawable.ic_copy, new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    ui.copy("macro", full);
                }
            }));
        }
        return card;
    }

    private View pills(List<String> items, int fg, int bg) {
        // A simple wrapping row: pills are added to rows of at most the window width.
        LinearLayout wrap = ui.vbox();
        LinearLayout line = ui.hbox();
        int width = 0;
        int limit = ui.dp(Math.max(240, Adaptive.widthDp(act) - 120));
        for (String s : items) {
            TextView p = ui.pill(Fmt.clip(Fmt.safe(s), 40), fg, bg);
            p.measure(View.MeasureSpec.UNSPECIFIED, View.MeasureSpec.UNSPECIFIED);
            int w = p.getMeasuredWidth() + ui.dp(6);
            if (width + w > limit && line.getChildCount() > 0) {
                wrap.addView(line);
                line = ui.hbox();
                width = 0;
            }
            LinearLayout.LayoutParams lp = ui.wrapLp();
            lp.topMargin = ui.dp(4);
            lp.rightMargin = ui.dp(6);
            line.addView(p, lp);
            width += w;
        }
        wrap.addView(line);
        return wrap;
    }

    // ---- links ------------------------------------------------------------------------------

    private View linksTab(Model.Report r) {
        LinearLayout col = ui.vbox();
        col.addView(ui.text(R.string.links_note, Ui.SMALL));
        col.addView(ui.space(12));
        List<View> rows = new ArrayList<>();
        for (final Model.Ref ref : r.refs) {
            LinearLayout card = ui.card();
            card.setPadding(ui.dp(14), ui.dp(12), ui.dp(14), ui.dp(12));
            LinearLayout.LayoutParams lp = ui.fillWidth();
            lp.bottomMargin = ui.dp(8);
            card.setLayoutParams(lp);
            TextView target = ui.text(Fmt.clip(Fmt.safe(ref.target), 240), Ui.MONO);
            card.addView(target);
            card.addView(ui.space(6));
            LinearLayout tags = ui.hbox();
            tags.addView(ui.pill(Labels.refClass(act, ref.cls), R.color.arca_on_surface, R.color.arca_surface_variant));
            tags.addView(ui.hspace(6));
            tags.addView(ui.pill(Labels.refKind(act, ref.kind), R.color.arca_on_surface, R.color.arca_surface_variant));
            if (ref.auto) {
                tags.addView(ui.hspace(6));
                tags.addView(ui.pill(ui.str(R.string.ref_auto), R.color.arca_danger, R.color.arca_danger_container));
            }
            card.addView(tags);
            if (!ref.location.isEmpty()) {
                card.addView(ui.space(4));
                TextView loc = ui.text(Fmt.clip(Fmt.safe(ref.location), 200), Ui.CAPTION);
                loc.setEllipsize(TextUtils.TruncateAt.MIDDLE);
                loc.setMaxLines(1);
                card.addView(loc);
            }
            card.setClickable(true);
            card.setFocusable(true);
            card.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    ui.showText(act, ui.str(R.string.link_dialog_title), Fmt.safe(ref.target), "link");
                }
            });
            rows.add(card);
        }
        appendChunked(col, rows);
        return col;
    }

    // ---- metadata ---------------------------------------------------------------------------

    private View metaTab(Model.Report r) {
        LinearLayout col = ui.vbox();
        col.addView(ui.text(R.string.meta_note, Ui.SMALL));
        col.addView(ui.space(8));
        List<View> rows = new ArrayList<>();
        for (Model.Meta m : r.meta) {
            LinearLayout card = ui.card();
            card.setPadding(ui.dp(14), ui.dp(10), ui.dp(14), ui.dp(10));
            LinearLayout.LayoutParams lp = ui.fillWidth();
            lp.bottomMargin = ui.dp(6);
            card.setLayoutParams(lp);
            String label = Labels.metaKey(act, m.key);
            if (!m.name.isEmpty()) {
                label = label + ": " + Fmt.clip(Fmt.safe(m.name), 80);
            }
            card.addView(ui.text(label, Ui.CAPTION));
            TextView v = ui.text(Fmt.clip(Fmt.safe(m.value), 800), Ui.BODY);
            v.setTextIsSelectable(true);
            card.addView(v);
            if (!m.source.isEmpty()) {
                TextView src = ui.text(Fmt.clip(Fmt.safe(m.source), 160), Ui.CAPTION);
                src.setEllipsize(TextUtils.TruncateAt.MIDDLE);
                src.setMaxLines(1);
                card.addView(src);
            }
            rows.add(card);
        }
        appendChunked(col, rows);
        return col;
    }

    // ---- embedded objects -------------------------------------------------------------------

    private View embeddedTab(Model.Report r) {
        LinearLayout col = ui.vbox();
        col.addView(ui.text(R.string.embedded_note, Ui.SMALL));
        col.addView(ui.space(12));
        List<View> rows = new ArrayList<>();
        for (final Model.Embedded e : r.embedded) {
            LinearLayout card = ui.card();
            LinearLayout.LayoutParams lp = ui.fillWidth();
            lp.bottomMargin = ui.dp(8);
            card.setLayoutParams(lp);
            card.addView(ui.text(Labels.embeddedKind(act, e.kind), Ui.TITLE));
            if (!e.file.isEmpty()) {
                card.addView(fact(ui.str(R.string.emb_file), Fmt.clip(Fmt.safe(e.file), 160), true));
            }
            if (!e.className.isEmpty()) {
                card.addView(fact(ui.str(R.string.emb_class), Fmt.clip(Fmt.safe(e.className), 120), true));
            }
            if (!e.clsid.isEmpty()) {
                card.addView(fact("CLSID", Fmt.safe(e.clsid), true));
            }
            card.addView(fact(ui.str(R.string.emb_size), Fmt.size(e.size), false));
            card.addView(fact(ui.str(R.string.emb_location), Fmt.clip(Fmt.safe(e.location), 200), true));
            if (!e.sha256.isEmpty()) {
                LinearLayout row = ui.hbox();
                row.addView(fact(ui.str(R.string.sha256_label), Fmt.grouped(e.sha256), true), ui.weight(1f));
                row.addView(ui.iconButton(R.drawable.ic_copy, R.string.copy_hash, new View.OnClickListener() {
                    @Override
                    public void onClick(View v) {
                        ui.copy("SHA-256", e.sha256);
                    }
                }));
                card.addView(row, ui.fillWidth());
            }
            rows.add(card);
        }
        appendChunked(col, rows);
        return col;
    }

    // ---- technical details ------------------------------------------------------------------

    private View detailsTab(Model.Report r) {
        LinearLayout col = ui.vbox();
        LinearLayout card = ui.card();
        card.addView(fact(ui.str(R.string.format_label), Labels.format(act, r.format) + " (" + Fmt.safe(r.format) + ")", false));
        card.addView(fact(ui.str(R.string.stat_parts), Long.toString(r.parts), false));
        card.addView(fact(ui.str(R.string.stat_xml), Long.toString(r.xmlEvents), false));
        card.addView(fact(ui.str(R.string.stat_inflated), Fmt.size(r.inflated), false));
        card.addView(fact(ui.str(R.string.stat_nesting), Long.toString(r.nesting), false));
        col.addView(card, ui.fillWidth());
        col.addView(ui.space(12));
        col.addView(ui.text(R.string.details_note, Ui.CAPTION));
        return col;
    }

    // ---- sharing ----------------------------------------------------------------------------

    private void shareReport(Model.Report r) {
        Session s = Session.get();
        StringBuilder sb = new StringBuilder();
        sb.append(ui.str(R.string.share_report_header)).append('\n');
        sb.append(ui.str(R.string.file_label)).append(": ").append(Fmt.safe(s.name())).append('\n');
        sb.append(ui.str(R.string.format_label)).append(": ").append(Labels.format(act, r.format)).append('\n');
        sb.append("SHA-256: ").append(r.sha256).append('\n');
        sb.append(ui.str(R.string.verdict_label)).append(": ").append(verdictName(r.verdict)).append('\n');
        sb.append(ui.str(R.string.stat_trackers)).append(": ").append(r.trackers).append('\n');
        sb.append(ui.str(R.string.stat_active)).append(": ").append(r.active).append('\n');
        sb.append(ui.str(R.string.stat_findings)).append(": ").append(r.findings.size()).append("\n\n");
        int n = 0;
        for (Model.Finding f : r.findings) {
            if (n++ >= 40) {
                sb.append("…\n");
                break;
            }
            sb.append("- [").append(Labels.severity(act, f.severity)).append("] ")
                .append(Labels.threatTitle(act, f.kind));
            if (!f.detail.isEmpty()) {
                sb.append(": ").append(Fmt.clip(Fmt.safe(f.detail), 200));
            }
            sb.append('\n');
        }
        sb.append('\n').append(ui.str(R.string.share_report_footer));
        Intent send = new Intent(Intent.ACTION_SEND);
        send.setType("text/plain");
        send.putExtra(Intent.EXTRA_SUBJECT, ui.str(R.string.share_report_subject));
        send.putExtra(Intent.EXTRA_TEXT, sb.toString());
        try {
            act.startActivity(Intent.createChooser(send, ui.str(R.string.share_report)));
        } catch (RuntimeException ignored) {
            // no app can take it; nothing to do
        }
    }

    private String verdictName(Model.Verdict v) {
        switch (v) {
            case CLEAN: return ui.str(R.string.verdict_clean_title);
            case HIDDEN_TRACKERS: return ui.str(R.string.verdict_trackers_title);
            case ACTIVE_NEUTRALIZED: return ui.str(R.string.verdict_active_title);
            case REJECTED: return ui.str(R.string.verdict_rejected_title);
            default: return ui.str(R.string.verdict_unverified_title);
        }
    }

    @Override
    public void onSessionChanged() {
        if (Session.get().phase() != Session.Phase.READY) {
            act.refresh(); // the document was closed: build() sends us home
        }
    }
}
