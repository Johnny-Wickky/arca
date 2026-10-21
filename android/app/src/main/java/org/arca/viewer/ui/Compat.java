// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.app.Activity;
import android.os.Build;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.window.OnBackInvokedCallback;
import android.window.OnBackInvokedDispatcher;

/**
 * Everything that depends on the Android version lives here, behind {@code SDK_INT} checks, so
 * that the rest of the code never names a class or method newer than API 26 (the minimum). The
 * calls are made only on devices that have them; on older devices this class's other methods
 * still load and verify, because newer types appear only inside the guarded branches' callees.
 */
public final class Compat {
    private Compat() {}

    /** Marks a view as a heading for screen readers (API 28+). */
    public static void markHeading(View v) {
        if (Build.VERSION.SDK_INT >= 28) {
            Api28.markHeading(v);
        }
    }

    /** Tells screen readers a chip is selected (API 30+ has state descriptions). */
    public static void markSelected(View v, boolean selected) {
        v.setSelected(selected);
    }

    /** Draws the window behind the system bars on every version; the app pads for the bars. */
    public static void edgeToEdge(Activity a, boolean night) {
        Window w = a.getWindow();
        if (Build.VERSION.SDK_INT >= 30) {
            Api30.edgeToEdge(w, night);
        } else {
            int flags = View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION;
            if (!night) {
                flags |= View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
            }
            w.getDecorView().setSystemUiVisibility(flags);
        }
        if (Build.VERSION.SDK_INT >= 28) {
            Api28.useCutoutArea(w);
        }
        if (Build.VERSION.SDK_INT >= 29) {
            Api29.noContrastScrim(w);
        }
    }

    /** Insets to pad the content by: system bars plus display cutout, in pixels. */
    public static int[] insets(WindowInsets in) {
        if (Build.VERSION.SDK_INT >= 30) {
            return Api30.insets(in);
        }
        int[] bars = {in.getSystemWindowInsetLeft(), in.getSystemWindowInsetTop(),
            in.getSystemWindowInsetRight(), in.getSystemWindowInsetBottom()};
        if (Build.VERSION.SDK_INT >= 28) {
            Api28.mergeCutout(in, bars);
        }
        return bars;
    }

    /** Keeps the window out of screenshots, screen recordings and the recents thumbnail. */
    public static void setSecure(Activity a, boolean secure) {
        if (secure) {
            a.getWindow().addFlags(android.view.WindowManager.LayoutParams.FLAG_SECURE);
        } else {
            a.getWindow().clearFlags(android.view.WindowManager.LayoutParams.FLAG_SECURE);
        }
        if (Build.VERSION.SDK_INT >= 33) {
            Api33.recentsScreenshot(a, !secure);
        }
    }

    /** Opts the window out of autofill and content capture: no document text leaves via them. */
    public static void noAutofill(Activity a) {
        a.getWindow().getDecorView().setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS);
    }

    /** A registered predictive-back callback; {@link #unregister()} removes it. */
    public static final class Back {
        private final Activity activity;
        private Object callback;

        public Back(Activity activity) {
            this.activity = activity;
        }

        /** Registers {@code action} to run on a back gesture (Android 13+; older versions use onBackPressed). */
        public void register(Runnable action) {
            if (Build.VERSION.SDK_INT >= 33 && callback == null) {
                callback = Api33.register(activity, action);
            }
        }

        public void unregister() {
            if (Build.VERSION.SDK_INT >= 33 && callback != null) {
                Api33.unregister(activity, callback);
                callback = null;
            }
        }
    }

    /** API 28+ calls, isolated so that older devices never load them. */
    private static final class Api28 {
        static void markHeading(View v) {
            v.setAccessibilityHeading(true);
        }

        static void mergeCutout(WindowInsets in, int[] bars) {
            android.view.DisplayCutout cut = in.getDisplayCutout();
            if (cut != null) {
                bars[0] = Math.max(bars[0], cut.getSafeInsetLeft());
                bars[1] = Math.max(bars[1], cut.getSafeInsetTop());
                bars[2] = Math.max(bars[2], cut.getSafeInsetRight());
                bars[3] = Math.max(bars[3], cut.getSafeInsetBottom());
            }
        }

        static void useCutoutArea(Window w) {
            // Use the notch area in landscape too; the content is padded by the cutout insets.
            android.view.WindowManager.LayoutParams lp = w.getAttributes();
            lp.layoutInDisplayCutoutMode = android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            w.setAttributes(lp);
        }
    }

    /** API 29+ calls, isolated so that older devices never load them. */
    private static final class Api29 {
        static void noContrastScrim(Window w) {
            // The app draws its own backgrounds behind the bars; no system scrim on top of them.
            w.setNavigationBarContrastEnforced(false);
            w.setStatusBarContrastEnforced(false);
        }
    }

    /** API 30+ calls, isolated so that older devices never load them. */
    private static final class Api30 {
        static void edgeToEdge(Window w, boolean night) {
            w.setDecorFitsSystemWindows(false);
            WindowInsetsController c = w.getInsetsController();
            if (c != null) {
                int mask = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                    | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                c.setSystemBarsAppearance(night ? 0 : mask, mask);
            }
        }

        static int[] insets(WindowInsets in) {
            android.graphics.Insets i = in.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            return new int[] {i.left, i.top, i.right, i.bottom};
        }
    }

    /** API 33+ calls. */
    private static final class Api33 {
        static void recentsScreenshot(Activity a, boolean enabled) {
            a.setRecentsScreenshotEnabled(enabled);
        }

        static Object register(Activity a, final Runnable action) {
            OnBackInvokedCallback cb = new OnBackInvokedCallback() {
                @Override
                public void onBackInvoked() {
                    action.run();
                }
            };
            a.getOnBackInvokedDispatcher().registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, cb);
            return cb;
        }

        static void unregister(Activity a, Object cb) {
            a.getOnBackInvokedDispatcher().unregisterOnBackInvokedCallback((OnBackInvokedCallback) cb);
        }
    }
}
