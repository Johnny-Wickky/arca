// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.util.DisplayMetrics;
import android.view.View;
import android.widget.LinearLayout;
import android.widget.TextView;

import org.arca.viewer.Engine;
import org.arca.viewer.MainActivity;
import org.arca.viewer.Native;
import org.arca.viewer.R;
import org.arca.viewer.core.Breakpoints;
import org.arca.viewer.core.Fmt;
import org.arca.viewer.core.Model;

import java.util.Locale;

/**
 * Facts for a bug report: the engine and its pinned sandbox module, this device and window,
 * and a self-test that runs a built-in document through the whole sandbox. Nothing is sent
 * anywhere; the report is text that the person copies if they want to.
 */
public final class DiagnosticsScreen extends Screen {
    private static final Handler MAIN = new Handler(Looper.getMainLooper());

    // Survive rebuilds (rotation, folding) while the test runs on its own thread.
    private static boolean testRunning;
    private static Model.SelfTest lastTest;
    private static String lastTestError;
    private static DiagnosticsScreen visible;

    public DiagnosticsScreen(MainActivity act) {
        super(act);
        visible = this;
    }

    @Override
    public View build() {
        LinearLayout col = ui.vbox();
        col.addView(engineCard(), ui.fillWidth());
        col.addView(ui.space(12));
        col.addView(deviceCard(), ui.fillWidth());
        col.addView(ui.space(12));
        col.addView(selfTestCard(), ui.fillWidth());
        col.addView(ui.space(16));
        col.addView(ui.button(R.string.diag_copy_report, Ui.OUTLINE, R.drawable.ic_copy, new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                ui.copy("arca-diagnostics", report());
            }
        }), ui.fillWidth());
        col.addView(ui.space(8));
        col.addView(ui.text(R.string.diag_privacy, Ui.CAPTION), ui.fillWidth());
        return page(header(ui.str(R.string.diagnostics_title), true), scrollColumn(col, 640, Adaptive.padding(act)));
    }

    // ---- cards ------------------------------------------------------------------------------

    private View engineCard() {
        LinearLayout card = ui.card();
        card.addView(ui.heading(ui.str(R.string.diag_engine), Ui.LABEL));
        Engine.State state = Engine.state();
        if (state == Engine.State.READY) {
            Model.EngineInfo i = Engine.info();
            card.addView(fact(ui.str(R.string.diag_engine_version), i.engine + " (ABI " + i.abi + ")", false));
            card.addView(fact(ui.str(R.string.diag_guest_hash), Fmt.grouped(i.guestSha256), true));
            card.addView(fact(ui.str(R.string.diag_limits),
                ui.str(R.string.diag_limits_value, Fmt.size(i.maxInputBytes), Fmt.size(i.maxMemoryBytes)), false));
            Model.ReleaseInfo r = Engine.release();
            card.addView(fact(ui.str(R.string.diag_build_kind),
                ui.str(r.ready ? R.string.diag_build_release : R.string.diag_build_dev), false));
        } else if (state == Engine.State.FAILED) {
            card.addView(ui.space(4));
            TextView t = ui.text(R.string.home_engine_failed, Ui.BODY);
            t.setTextColor(ui.color(R.color.arca_danger));
            card.addView(t);
            card.addView(ui.text(Fmt.safe(Engine.failure()), Ui.MONO));
        } else {
            card.addView(ui.text(R.string.home_engine_starting, Ui.BODY));
        }
        return card;
    }

    private View deviceCard() {
        LinearLayout card = ui.card();
        card.addView(ui.heading(ui.str(R.string.diag_device), Ui.LABEL));
        card.addView(fact(ui.str(R.string.diag_app), appLine(), false));
        card.addView(fact(ui.str(R.string.diag_android), androidLine(), false));
        card.addView(fact(ui.str(R.string.diag_abis), abiLine(), false));
        card.addView(fact(ui.str(R.string.diag_screen), screenLine(), false));
        card.addView(fact(ui.str(R.string.diag_window), windowLine(), false));
        return card;
    }

    private View selfTestCard() {
        LinearLayout card = ui.card();
        card.addView(ui.heading(ui.str(R.string.diag_selftest), Ui.LABEL));
        card.addView(ui.space(4));
        card.addView(ui.text(R.string.diag_selftest_note, Ui.SMALL));
        card.addView(ui.space(8));
        if (testRunning) {
            card.addView(ui.text(R.string.diag_selftest_running, Ui.BODY));
        } else {
            View run = ui.button(lastTest == null ? R.string.diag_selftest_run : R.string.diag_selftest_again,
                Ui.TONAL, 0, new View.OnClickListener() {
                    @Override
                    public void onClick(View v) {
                        runSelfTest();
                    }
                });
            run.setEnabled(Engine.state() == Engine.State.READY);
            card.addView(run, ui.fillWidth());
        }
        if (lastTestError != null) {
            card.addView(ui.space(8));
            TextView t = ui.text(Fmt.safe(lastTestError), Ui.MONO);
            t.setTextColor(ui.color(R.color.arca_danger));
            card.addView(t);
        }
        if (lastTest != null) {
            card.addView(ui.space(8));
            TextView verdict = ui.heading(ui.str(lastTest.ok ? R.string.diag_selftest_ok : R.string.diag_selftest_failed), Ui.TITLE);
            verdict.setTextColor(ui.color(lastTest.ok ? R.color.arca_safe : R.color.arca_danger));
            card.addView(verdict);
            for (Model.SelfTestStep s : lastTest.steps) {
                String line = (s.ok ? "✓ " : "✗ ") + Fmt.clip(Fmt.safe(s.name), 48) + "  " + s.ms + " ms";
                TextView t = ui.text(line, Ui.MONO);
                t.setTextColor(ui.color(s.ok ? R.color.arca_on_surface : R.color.arca_danger));
                card.addView(t);
                if (!s.ok && !s.detail.isEmpty()) {
                    card.addView(ui.text("   " + Fmt.clip(Fmt.safe(s.detail), 200), Ui.CAPTION));
                }
            }
        }
        return card;
    }

    // ---- self-test --------------------------------------------------------------------------

    private void runSelfTest() {
        if (testRunning || Engine.state() != Engine.State.READY) {
            return;
        }
        testRunning = true;
        lastTestError = null;
        act.refresh();
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                Model.SelfTest result = null;
                String error = null;
                try {
                    byte[] json = Native.selfTest();
                    if (json == null) {
                        error = "the self-test returned nothing";
                    } else {
                        result = Model.parseSelfTest(json);
                    }
                } catch (RuntimeException e) {
                    error = e.toString();
                }
                final Model.SelfTest r = result;
                final String err = error;
                MAIN.post(new Runnable() {
                    @Override
                    public void run() {
                        testRunning = false;
                        lastTest = r;
                        lastTestError = err;
                        if (visible != null) {
                            visible.onSessionChanged();
                        }
                    }
                });
            }
        }, "arca-selftest");
        t.start();
    }

    @Override
    public void onSessionChanged() {
        act.refresh();
    }

    @Override
    public void onLeave() {
        if (visible == this) {
            visible = null;
        }
    }

    // ---- the text report --------------------------------------------------------------------

    private String appLine() {
        return "Arca " + versionName() + " (" + versionCode() + ")";
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

    @SuppressWarnings("deprecation")
    private long versionCode() {
        try {
            PackageInfo pi = act.getPackageManager().getPackageInfo(act.getPackageName(), 0);
            return pi.versionCode;
        } catch (PackageManager.NameNotFoundException | RuntimeException e) {
            return -1;
        }
    }

    private String androidLine() {
        return "Android " + Build.VERSION.RELEASE + " (API " + Build.VERSION.SDK_INT + "), "
            + Build.MANUFACTURER + " " + Build.MODEL;
    }

    private String abiLine() {
        StringBuilder sb = new StringBuilder();
        for (String abi : Build.SUPPORTED_ABIS) {
            if (sb.length() > 0) {
                sb.append(", ");
            }
            sb.append(abi);
        }
        String arch = Engine.state() == Engine.State.READY ? " | engine: " + Engine.info().arch : "";
        return sb + arch;
    }

    private String screenLine() {
        DisplayMetrics m = act.getResources().getDisplayMetrics();
        return String.format(Locale.ROOT, "%d × %d px, %.0f dpi (%.2f×)", m.widthPixels, m.heightPixels,
            m.densityDpi * 1f, m.density);
    }

    private String windowLine() {
        int w = Adaptive.widthDp(act);
        int h = Adaptive.heightDp(act);
        String cls;
        switch (Breakpoints.widthClass(w)) {
            case Breakpoints.COMPACT: cls = "compact"; break;
            case Breakpoints.MEDIUM: cls = "medium"; break;
            default: cls = "expanded"; break;
        }
        return w + " × " + h + " dp, " + cls + (Adaptive.isShort(act) ? ", short" : "")
            + (Adaptive.isLandscape(act) ? ", landscape" : ", portrait")
            + (Adaptive.twoPane(act) ? ", two-pane" : "");
    }

    private String report() {
        StringBuilder sb = new StringBuilder();
        sb.append(appLine()).append('\n');
        sb.append(androidLine()).append('\n');
        sb.append("ABI: ").append(abiLine()).append('\n');
        sb.append("Screen: ").append(screenLine()).append('\n');
        sb.append("Window: ").append(windowLine()).append('\n');
        if (Engine.state() == Engine.State.READY) {
            Model.EngineInfo i = Engine.info();
            sb.append("Engine: ").append(i.engine).append(" ABI ").append(i.abi).append('\n');
            sb.append("Guest SHA-256: ").append(i.guestSha256).append('\n');
            sb.append("Release build: ").append(Engine.release().ready ? "yes" : "no").append('\n');
        } else {
            sb.append("Engine: ").append(Engine.state()).append(' ').append(Fmt.safe(Engine.failure())).append('\n');
        }
        if (lastTest != null) {
            sb.append("Self-test: ").append(lastTest.ok ? "ok" : "FAILED").append('\n');
            for (Model.SelfTestStep s : lastTest.steps) {
                sb.append("  ").append(s.ok ? "ok   " : "FAIL ").append(s.name).append(' ').append(s.ms).append(" ms");
                if (!s.ok && !s.detail.isEmpty()) {
                    sb.append(" — ").append(Fmt.clip(Fmt.safe(s.detail), 200));
                }
                sb.append('\n');
            }
        }
        return sb.toString();
    }
}
