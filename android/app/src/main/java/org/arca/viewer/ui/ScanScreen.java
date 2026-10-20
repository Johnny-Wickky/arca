// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.view.Gravity;
import android.view.View;
import android.widget.LinearLayout;
import android.widget.ProgressBar;
import android.widget.TextView;

import org.arca.viewer.MainActivity;
import org.arca.viewer.Native;
import org.arca.viewer.R;
import org.arca.viewer.Session;
import org.arca.viewer.core.Fmt;

import java.util.Locale;

/** Progress while the file is read and X-rayed in the sandbox; the error page if that failed. */
public final class ScanScreen extends Screen {
    private final Handler ticker = new Handler(Looper.getMainLooper());
    private TextView elapsed;
    private final Runnable tick = new Runnable() {
        @Override
        public void run() {
            if (elapsed == null) {
                return;
            }
            long s = Math.max(0, SystemClock.elapsedRealtime() - Session.get().startedAt()) / 1000;
            elapsed.setText(String.format(Locale.getDefault(), "%02d:%02d", s / 60, s % 60));
            ticker.postDelayed(this, 500);
        }
    };

    public ScanScreen(MainActivity act) {
        super(act);
    }

    @Override
    public View build() {
        Session s = Session.get();
        switch (s.phase()) {
            case READY:
                // Defer: replacing the route from inside build() would re-enter the activity.
                ticker.post(new Runnable() {
                    @Override
                    public void run() {
                        if (Session.get().phase() == Session.Phase.READY) {
                            act.replace(MainActivity.Route.RESULT);
                        }
                    }
                });
                return page(header(ui.str(R.string.scan_title), false), new View(act));
            case FAILED:
                return failed(s);
            case IDLE:
                ticker.post(new Runnable() {
                    @Override
                    public void run() {
                        act.home();
                    }
                });
                return new View(act);
            default:
                return working(s);
        }
    }

    private View working(Session s) {
        LinearLayout col = ui.vbox();
        col.setGravity(Gravity.CENTER_HORIZONTAL);
        col.setPadding(ui.dp(24), ui.dp(24), ui.dp(24), ui.dp(24));

        ProgressBar bar = new ProgressBar(act);
        bar.setIndeterminate(true);
        bar.setIndeterminateTintList(android.content.res.ColorStateList.valueOf(ui.color(R.color.arca_primary)));
        col.addView(bar, new LinearLayout.LayoutParams(ui.dp(56), ui.dp(56)));
        col.addView(ui.space(20));

        boolean reading = s.phase() == Session.Phase.READING;
        TextView title = ui.heading(ui.str(reading ? R.string.scan_reading : R.string.scan_inspecting), Ui.TITLE);
        title.setGravity(Gravity.CENTER);
        col.addView(title, ui.fillWidth());
        col.addView(ui.space(6));

        String name = s.name().isEmpty() ? "…" : Fmt.safe(s.name());
        TextView file = ui.text(name, Ui.BODY);
        file.setGravity(Gravity.CENTER);
        col.addView(file, ui.fillWidth());
        if (s.size() >= 0) {
            TextView size = ui.text(Fmt.size(s.size()), Ui.SMALL);
            size.setGravity(Gravity.CENTER);
            col.addView(size, ui.fillWidth());
        }
        col.addView(ui.space(12));

        elapsed = ui.text("00:00", Ui.MONO);
        elapsed.setGravity(Gravity.CENTER);
        // The clock runs only while this very view is on screen: a screen that is rebuilt
        // (rotation, folding, a new phase) or left must not leave its ticker behind.
        elapsed.addOnAttachStateChangeListener(new View.OnAttachStateChangeListener() {
            @Override
            public void onViewAttachedToWindow(View v) {
                ticker.removeCallbacks(tick);
                ticker.post(tick);
            }

            @Override
            public void onViewDetachedFromWindow(View v) {
                ticker.removeCallbacks(tick);
            }
        });
        col.addView(elapsed, ui.fillWidth());
        col.addView(ui.space(12));

        TextView note = ui.text(R.string.scan_note, Ui.CAPTION);
        note.setGravity(Gravity.CENTER);
        col.addView(note, ui.fillWidth());
        col.addView(ui.space(24));
        col.addView(ui.button(R.string.cancel, Ui.OUTLINE, R.drawable.ic_close, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Session.get().cancel();
                act.home();
            }
        }));
        return page(header(ui.str(R.string.scan_title), false), centered(col, 480));
    }

    private View failed(Session s) {
        Session.Failure f = s.failure();
        int status = f == null ? Native.FAILED : f.status;
        LinearLayout col = ui.vbox();
        col.setPadding(ui.dp(24), ui.dp(16), ui.dp(24), ui.dp(24));

        LinearLayout banner = ui.banner(status == Native.ENCRYPTED || status == Native.UNSUPPORTED
            ? R.color.arca_unknown_container : R.color.arca_danger_container);
        LinearLayout head = ui.hbox();
        int fg = status == Native.ENCRYPTED || status == Native.UNSUPPORTED ? R.color.arca_unknown : R.color.arca_danger;
        head.addView(ui.icon(status == Native.ENCRYPTED ? R.drawable.ic_lock
            : status == Native.UNSUPPORTED ? R.drawable.ic_verdict_unknown : R.drawable.ic_verdict_danger, fg, 32));
        head.addView(ui.hspace(12));
        TextView title = ui.heading(ui.str(Errors.title(status)), Ui.TITLE);
        title.setTextColor(ui.color(fg));
        head.addView(title, ui.weight(1f));
        banner.addView(head);
        banner.addView(ui.space(8));
        banner.addView(ui.text(Errors.text(status), Ui.BODY));
        if (status == Native.TOO_LARGE) {
            banner.addView(ui.space(6));
            banner.addView(ui.text(ui.str(R.string.err_too_large_limit, org.arca.viewer.Engine.maxInputMiB()), Ui.SMALL));
        }
        col.addView(banner, ui.fillWidth());

        if (!s.name().isEmpty()) {
            col.addView(ui.space(12));
            col.addView(fact(ui.str(R.string.file_label), Fmt.safe(s.name()), false));
        }
        col.addView(ui.space(8));
        String technical = Errors.code(status) + (f == null || f.technical.isEmpty() ? "" : ": " + Fmt.safe(f.technical));
        col.addView(fact(ui.str(R.string.technical_label), technical, true));
        col.addView(ui.space(20));
        col.addView(ui.button(R.string.back_home, Ui.PRIMARY, 0, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                act.home();
            }
        }), ui.fillWidth());
        return page(header(ui.str(R.string.scan_title), true), scrollColumn(col, 560, 8));
    }

    @Override
    public void onSessionChanged() {
        act.refresh();
    }

    @Override
    public void onLeave() {
        ticker.removeCallbacks(tick);
    }
}
