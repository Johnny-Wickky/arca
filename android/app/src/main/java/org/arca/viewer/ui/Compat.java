// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.os.Build;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;

import org.arca.viewer.Prefs;
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

    /**
     * Binds the sandbox service. From Android 10 each distinct {@code instance} gets a process of its
     * own, so every document runs in a fresh one; before that, unbinding the previous document has
     * already ended the old process and the plain bind starts a new one.
     */
    public static boolean bindIsolated(Context c, Intent intent, String instance, ServiceConnection conn) {
        if (Build.VERSION.SDK_INT >= 29) {
            return Api29.bindIsolated(c, intent, instance, conn);
        }
        return c.bindService(intent, conn, Context.BIND_AUTO_CREATE);
    }

    /**
     * Blurs what is behind {@code w} (Liquid Glass dialogs and floating panels) where the system
     * offers it: Android 12+ with cross-window blur enabled (some devices switch it off in battery
     * saver or developer options). Returns whether a blur is in effect; when it is not, the caller's
     * translucent panel already reads well without it.
     */
    public static boolean blurBehind(Window w, int radiusPx) {
        return Build.VERSION.SDK_INT >= 31 && Api31.blurBehind(w, radiusPx);
    }

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

    /**
     * The bounding rectangles of the display cutouts in these insets, {@code {left, top, right,
     * bottom}} in pixels, in window coordinates (API 28+; none before). A dual-screen device
     * reports its hinge as one of them; see {@link org.arca.viewer.core.Fold}.
     */
    public static int[][] cutoutRects(WindowInsets in) {
        if (Build.VERSION.SDK_INT >= 28) {
            return Api28.cutoutRects(in);
        }
        return new int[0][];
    }

    /**
     * The size in pixels of this window and of the largest window the display allows,
     * {@code {width, height, maxWidth, maxHeight}} (API 30+); {@code null} before.
     */
    public static int[] windowSizePx(Activity a) {
        return Build.VERSION.SDK_INT >= 30 ? Api30.windowSize(a) : null;
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
        if (Build.VERSION.SDK_INT >= 29) {
            Api29.noContentCapture(a.getWindow());
        }
    }

    /**
     * Gives a dialog the protection the activity's own window has. Dialogs are separate windows:
     * FLAG_SECURE, the autofill opt-out and the content-capture opt-out set on the activity do not
     * cover them, and they show document text (links, contents) and the password field.
     */
    public static void hardenDialog(Activity owner, android.app.Dialog dialog) {
        Window w = dialog.getWindow();
        if (w == null) {
            return;
        }
        int flags = owner.getWindow().getAttributes().flags;
        if ((flags & android.view.WindowManager.LayoutParams.FLAG_SECURE) != 0) {
            w.addFlags(android.view.WindowManager.LayoutParams.FLAG_SECURE);
        }
        w.getDecorView().setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS);
        if (Build.VERSION.SDK_INT >= 29) {
            Api29.noContentCapture(w);
        }
        if (Theme.glass(owner)) {
            // Liquid Glass: a glass panel for the dialog, blurred behind where the system allows it.
            float density = owner.getResources().getDisplayMetrics().density;
            boolean blurred = blurBehind(w, Math.round(24f * density));
            Boolean palette = Theme.dark(Prefs.palette(owner));
            boolean dark = palette != null ? palette : (owner.getResources().getConfiguration().uiMode
                & android.content.res.Configuration.UI_MODE_NIGHT_MASK) == android.content.res.Configuration.UI_MODE_NIGHT_YES;
            int surface = Theme.color(owner, org.arca.viewer.R.color.arca_surface);
            // Without a blur the panel is made more opaque, so that whatever shows through stays quiet.
            int body = Theme.withAlpha(surface, blurred ? (dark ? 0xD0 : 0xCC) : 0xF2);
            android.graphics.drawable.GradientDrawable g = new android.graphics.drawable.GradientDrawable();
            g.setColor(body);
            g.setCornerRadius(24f * density);
            w.setBackgroundDrawable(g);
        }
    }

    /** Keeps the system text classifier (smart selection, link actions) away from untrusted text. */
    public static void noTextClassifier(android.widget.TextView t) {
        // TextClassifier exists since API 26, the minimum, so no guard is needed.
        t.setTextClassifier(android.view.textclassifier.TextClassifier.NO_OP);
    }

    /**
     * Puts text on the clipboard flagged as sensitive (so Android 13+ hides it in the preview) and
     * clears it again after {@code clearAfterMs} if it is still ours (API 28+).
     */
    public static void copySensitive(final android.content.Context c, String label, String text, long clearAfterMs) {
        final android.content.ClipboardManager cm =
            (android.content.ClipboardManager) c.getSystemService(android.content.Context.CLIPBOARD_SERVICE);
        if (cm == null) {
            return;
        }
        final android.content.ClipData clip = android.content.ClipData.newPlainText(label, text);
        if (Build.VERSION.SDK_INT >= 33) {
            Api33.markSensitive(clip);
        }
        cm.setPrimaryClip(clip);
        if (Build.VERSION.SDK_INT >= 28) {
            new android.os.Handler(android.os.Looper.getMainLooper()).postDelayed(new Runnable() {
                @Override
                public void run() {
                    Api28.clearClipIfOurs(cm, clip);
                }
            }, clearAfterMs);
        }
    }

    /** The Intent extra for a parcelable, with the Class-typed overload where the platform has it. */
    public static <T extends android.os.Parcelable> T parcelableExtra(android.content.Intent i, String key, Class<T> type) {
        if (Build.VERSION.SDK_INT >= 33) {
            return Api33.parcelableExtra(i, key, type);
        }
        return legacyParcelableExtra(i, key, type);
    }

    @SuppressWarnings({"deprecation", "unchecked"})
    private static <T extends android.os.Parcelable> T legacyParcelableExtra(android.content.Intent i, String key, Class<T> type) {
        android.os.Parcelable p = i.getParcelableExtra(key);
        return type.isInstance(p) ? (T) p : null;
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

    /**
     * The hinge angle of a foldable (a platform sensor, Android 11+). {@link #start()} returns
     * {@code false} where there is none, which is every device that does not fold.
     */
    public static final class HingeSensor {
        /** Receives the angle in degrees (0 closed, 180 flat). */
        public interface Listener {
            void onAngle(float degrees);
        }

        private final Activity activity;
        private final Listener listener;
        private Object registration;

        public HingeSensor(Activity activity, Listener listener) {
            this.activity = activity;
            this.listener = listener;
        }

        public boolean start() {
            if (Build.VERSION.SDK_INT < 30 || registration != null) {
                return false;
            }
            registration = Api30.hinge(activity, listener);
            return registration != null;
        }

        public void stop() {
            if (Build.VERSION.SDK_INT >= 30 && registration != null) {
                Api30.unhinge(activity, registration);
                registration = null;
            }
        }
    }

    /** API 28+ calls, isolated so that older devices never load them. */
    private static final class Api28 {
        static void clearClipIfOurs(android.content.ClipboardManager cm, android.content.ClipData ours) {
            // Only clear what we put there: the reader may have copied something else since.
            // A clip that cannot be read (background app on API 29+) is cleared too: best effort.
            android.content.ClipData now = cm.getPrimaryClip();
            if (now == null || (now.getItemCount() > 0 && ours.getItemCount() > 0
                && String.valueOf(now.getItemAt(0).getText()).equals(String.valueOf(ours.getItemAt(0).getText())))) {
                cm.clearPrimaryClip();
            }
        }

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

        static int[][] cutoutRects(WindowInsets in) {
            android.view.DisplayCutout cut = in.getDisplayCutout();
            if (cut == null) {
                return new int[0][];
            }
            java.util.List<android.graphics.Rect> rects = cut.getBoundingRects();
            int[][] out = new int[rects.size()][];
            for (int i = 0; i < out.length; i++) {
                android.graphics.Rect r = rects.get(i);
                out[i] = new int[] {r.left, r.top, r.right, r.bottom};
            }
            return out;
        }
    }

    /** API 29+ calls, isolated so that older devices never load them. */
    private static final class Api29 {
        static boolean bindIsolated(Context c, Intent intent, String instance, ServiceConnection conn) {
            // Callbacks run on the binder thread that delivers them: the waiting thread must not
            // depend on the main looper.
            return c.bindIsolatedService(intent, Context.BIND_AUTO_CREATE, instance, new java.util.concurrent.Executor() {
                @Override
                public void execute(Runnable r) {
                    r.run();
                }
            }, conn);
        }

        /**
         * The app zygote's preload (named in the manifest): loads the engine library and has the
         * guest module checked against its build-time digest and validated in memory, so the
         * process of each document starts from a zygote that already has both. Parsing a module is
         * plain single-threaded computation; the sandbox starts a thread only to run a guest,
         * which never happens here, so the zygote stays single-threaded.
         */
        public static final class Preload implements android.app.ZygotePreload {
            @Override
            public void doPreload(android.content.pm.ApplicationInfo info) {
                if (org.arca.viewer.Native.isLoaded()) {
                    try {
                        org.arca.viewer.Native.preloadGuest();
                    } catch (RuntimeException | LinkageError ignored) {
                        // not worth a failed zygote: every run validates the module itself
                    }
                }
            }
        }

        static void noContrastScrim(Window w) {
            // The app draws its own backgrounds behind the bars; no system scrim on top of them.
            w.setNavigationBarContrastEnforced(false);
            w.setStatusBarContrastEnforced(false);
        }

        static void noContentCapture(Window w) {
            w.getDecorView().setImportantForContentCapture(View.IMPORTANT_FOR_CONTENT_CAPTURE_NO_EXCLUDE_DESCENDANTS);
        }
    }

    /** API 31+ calls, isolated so that older devices never load them. */
    private static final class Api31 {
        static boolean blurBehind(Window w, int radiusPx) {
            android.view.WindowManager wm = w.getWindowManager();
            if (wm == null || !wm.isCrossWindowBlurEnabled()) {
                return false;
            }
            w.addFlags(android.view.WindowManager.LayoutParams.FLAG_BLUR_BEHIND);
            android.view.WindowManager.LayoutParams lp = w.getAttributes();
            lp.setBlurBehindRadius(radiusPx);
            w.setAttributes(lp);
            w.setBackgroundBlurRadius(radiusPx);
            return true;
        }
    }

    /** API 30+ calls, isolated so that older devices never load them. */
    private static final class Api30 {
        static void edgeToEdge(Window w, boolean night) {
            w.setDecorFitsSystemWindows(false);
            // Installs the decor view: without it getInsetsController() is null-dereferenced
            // when this runs in onCreate before setContentView.
            w.getDecorView();
            WindowInsetsController c = w.getInsetsController();
            if (c != null) {
                int mask = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                    | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                c.setSystemBarsAppearance(night ? 0 : mask, mask);
            }
        }

        static Object hinge(Activity a, final HingeSensor.Listener listener) {
            android.hardware.SensorManager sm =
                (android.hardware.SensorManager) a.getSystemService(android.content.Context.SENSOR_SERVICE);
            if (sm == null) {
                return null;
            }
            android.hardware.Sensor sensor = sm.getDefaultSensor(android.hardware.Sensor.TYPE_HINGE_ANGLE);
            if (sensor == null) {
                return null;
            }
            android.hardware.SensorEventListener l = new android.hardware.SensorEventListener() {
                @Override
                public void onSensorChanged(android.hardware.SensorEvent e) {
                    if (e.values.length > 0) {
                        listener.onAngle(e.values[0]);
                    }
                }

                @Override
                public void onAccuracyChanged(android.hardware.Sensor s, int accuracy) {}
            };
            return sm.registerListener(l, sensor, android.hardware.SensorManager.SENSOR_DELAY_NORMAL) ? l : null;
        }

        static void unhinge(Activity a, Object registration) {
            android.hardware.SensorManager sm =
                (android.hardware.SensorManager) a.getSystemService(android.content.Context.SENSOR_SERVICE);
            if (sm != null) {
                sm.unregisterListener((android.hardware.SensorEventListener) registration);
            }
        }

        static int[] insets(WindowInsets in) {
            android.graphics.Insets i = in.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            return new int[] {i.left, i.top, i.right, i.bottom};
        }

        static int[] windowSize(Activity a) {
            android.view.WindowManager wm = a.getWindowManager();
            android.graphics.Rect now = wm.getCurrentWindowMetrics().getBounds();
            android.graphics.Rect max = wm.getMaximumWindowMetrics().getBounds();
            return new int[] {now.width(), now.height(), max.width(), max.height()};
        }
    }

    /** API 33+ calls. */
    private static final class Api33 {
        static void markSensitive(android.content.ClipData clip) {
            android.os.PersistableBundle extras = new android.os.PersistableBundle();
            extras.putBoolean("android.content.extra.IS_SENSITIVE", true);
            clip.getDescription().setExtras(extras);
        }

        static <T extends android.os.Parcelable> T parcelableExtra(android.content.Intent i, String key, Class<T> type) {
            return i.getParcelableExtra(key, type);
        }

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
