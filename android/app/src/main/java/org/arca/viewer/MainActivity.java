// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.res.Configuration;
import android.net.Uri;
import android.os.Bundle;
import android.view.View;
import android.view.WindowInsets;
import android.widget.FrameLayout;
import android.widget.Toast;

import org.arca.viewer.ui.AboutScreen;
import org.arca.viewer.ui.Compat;
import org.arca.viewer.ui.DiagnosticsScreen;
import org.arca.viewer.ui.Hinge;
import org.arca.viewer.ui.HomeScreen;
import org.arca.viewer.ui.LicensesScreen;
import org.arca.viewer.ui.PagesScreen;
import org.arca.viewer.ui.PreviewScreen;
import org.arca.viewer.ui.ResultScreen;
import org.arca.viewer.ui.SanitizeScreen;
import org.arca.viewer.ui.ScanScreen;
import org.arca.viewer.ui.Screen;
import org.arca.viewer.ui.SettingsScreen;

import java.util.ArrayList;
import java.util.List;

/**
 * The only activity. It shows one {@link Screen} at a time from a back stack of {@link Route}s,
 * rebuilds the visible screen when the window changes shape (it handles rotation, folding and
 * resizing itself, so a scan is never interrupted by them), and receives files from other apps
 * ({@code VIEW}/{@code SEND}) and from the system file picker.
 */
public final class MainActivity extends Activity implements Session.Listener, Engine.Listener {
    /** The screens of the app. */
    public enum Route { HOME, SCAN, RESULT, PREVIEW, PAGES, SANITIZE, ABOUT, LICENSES, SETTINGS, DIAGNOSTICS }

    public static final String ACTION_SHOW_ABOUT = "org.arca.viewer.action.ABOUT";
    public static final int REQ_NOTIF_PERMISSION = 101;
    private static final int REQ_OPEN = 1;
    private static final int REQ_SAVE = 2;
    private static final String STATE_ROUTES = "routes";

    /** Survives re-creation of the activity (theme or language change, system dark mode). */
    private static final List<Route> ROUTES = new ArrayList<>();

    private FrameLayout root;
    private Screen top;
    private boolean building;
    private boolean rebuildRequested;
    private final Compat.Back back = new Compat.Back(this);
    private boolean started;

    @Override
    protected void attachBaseContext(Context base) {
        super.attachBaseContext(Prefs.wrap(base));
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        boolean night = (getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK)
            == Configuration.UI_MODE_NIGHT_YES;
        Compat.edgeToEdge(this, night);
        Compat.setSecure(this, Prefs.secureScreen(this));
        Compat.noAutofill(this);
        Reminders.init(this);

        root = new FrameLayout(this);
        root.setBackgroundColor(getColor(R.color.arca_bg));
        root.setOnApplyWindowInsetsListener(new View.OnApplyWindowInsetsListener() {
            @Override
            public WindowInsets onApplyWindowInsets(View v, WindowInsets insets) {
                int[] i = Compat.insets(insets);
                v.setPadding(i[0], i[1], i[2], i[3]);
                return insets;
            }
        });
        setContentView(root);

        if (ROUTES.isEmpty()) {
            ROUTES.add(Route.HOME);
        }
        sanitizeRoutes();
        if (savedInstanceState == null) {
            handleIntent(getIntent());
        }
        show();
    }

    /** Drops routes that make no sense without their document (after the session was closed). */
    private void sanitizeRoutes() {
        Session s = Session.get();
        boolean hasDoc = s.phase() != Session.Phase.IDLE;
        List<Route> keep = new ArrayList<>();
        for (Route r : ROUTES) {
            boolean needsDoc = r == Route.SCAN || r == Route.RESULT || r == Route.PREVIEW || r == Route.PAGES || r == Route.SANITIZE;
            if (!needsDoc || hasDoc) {
                keep.add(r);
            }
        }
        if (keep.isEmpty() || keep.get(0) != Route.HOME) {
            keep.add(0, Route.HOME);
        }
        ROUTES.clear();
        ROUTES.addAll(keep);
        // A finished scan belongs on the result screen, not the progress screen.
        if (s.phase() == Session.Phase.READY && ROUTES.get(ROUTES.size() - 1) == Route.SCAN) {
            ROUTES.set(ROUTES.size() - 1, Route.RESULT);
        }
    }

    @Override
    protected void onSaveInstanceState(Bundle out) {
        super.onSaveInstanceState(out);
        int[] ids = new int[ROUTES.size()];
        for (int i = 0; i < ids.length; i++) {
            ids[i] = ROUTES.get(i).ordinal();
        }
        out.putIntArray(STATE_ROUTES, ids);
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleIntent(intent);
        show();
    }

    @Override
    protected void onStart() {
        super.onStart();
        started = true;
        Hinge.start(this, new Runnable() {
            @Override
            public void run() {
                show();
            }
        });
        Session.get().addListener(this);
        Engine.addListener(this);
        // Anything that happened while stopped (a scan finishing in the background).
        onSessionChanged();
    }

    @Override
    protected void onStop() {
        started = false;
        Hinge.stop();
        Session.get().removeListener(this);
        Engine.removeListener(this);
        super.onStop();
    }

    @Override
    protected void onDestroy() {
        back.unregister();
        super.onDestroy();
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        // Rotation, folding, resizing: the window changed shape, the document did not.
        show();
    }

    @Override
    public void onSessionChanged() {
        if (top != null && started) {
            top.onSessionChanged();
        }
    }

    @Override
    public void onEngineChanged() {
        onSessionChanged();
    }

    // ---- navigation -------------------------------------------------------------------------

    private Screen create(Route r) {
        switch (r) {
            case SCAN: return new ScanScreen(this);
            case RESULT: return new ResultScreen(this);
            case PREVIEW: return new PreviewScreen(this);
            case PAGES: return new PagesScreen(this);
            case SANITIZE: return new SanitizeScreen(this);
            case ABOUT: return new AboutScreen(this);
            case LICENSES: return new LicensesScreen(this);
            case SETTINGS: return new SettingsScreen(this);
            case DIAGNOSTICS: return new DiagnosticsScreen(this);
            default: return new HomeScreen(this);
        }
    }

    /** Shows the route on top of the stack, building a fresh screen for it. */
    public void show() {
        if (root == null) {
            return;
        }
        if (building) {
            // A screen changed state while building (it should not): redo once it returns.
            rebuildRequested = true;
            return;
        }
        building = true;
        try {
            do {
                rebuildRequested = false;
                Route r = ROUTES.get(ROUTES.size() - 1);
                top = create(r);
                View view = top.build();
                root.removeAllViews();
                // Across the fold of a half-open foldable the screen takes one half of the window.
                root.addView(view, Hinge.layout(this));
            } while (rebuildRequested);
        } finally {
            building = false;
        }
        updateBack();
    }

    /** Re-renders the visible screen (its state changed). */
    public void refresh() {
        show();
    }

    public void push(Route r) {
        leaveTop();
        ROUTES.add(r);
        show();
    }

    /** Replaces the top screen with {@code r}. */
    public void replace(Route r) {
        leaveTop();
        ROUTES.set(ROUTES.size() - 1, r);
        show();
    }

    /** Goes back to the home screen, closing the document (and everything derived from it). */
    public void home() {
        leaveTop();
        Session.get().close();
        ROUTES.clear();
        ROUTES.add(Route.HOME);
        show();
    }

    private void leaveTop() {
        if (top != null) {
            Screen leaving = top;
            top = null; // notifications during the transition are ignored
            leaving.onLeave();
        }
    }

    /** One step back; at the root the activity finishes. */
    public void back() {
        if (org.arca.viewer.ui.ImageViewer.dismiss()) {
            return;
        }
        if (top != null && top.onBack()) {
            return;
        }
        if (ROUTES.size() <= 1) {
            finish();
            return;
        }
        Route leaving = ROUTES.get(ROUTES.size() - 1);
        // Leaving the result screen (or the scan) means closing the document.
        if (leaving == Route.RESULT || leaving == Route.SCAN) {
            home();
            return;
        }
        leaveTop();
        ROUTES.remove(ROUTES.size() - 1);
        show();
    }

    @Override
    public void onBackPressed() {
        // Android 12 and older, and 13+ when no predictive-back callback is registered.
        if (ROUTES.size() <= 1 && !org.arca.viewer.ui.ImageViewer.dismiss() && !(top != null && top.onBack())) {
            super.onBackPressed();
            return;
        }
        back();
    }

    private void updateBack() {
        if (ROUTES.size() > 1) {
            back.register(new Runnable() {
                @Override
                public void run() {
                    back();
                }
            });
        } else {
            back.unregister();
        }
    }

    // ---- files ------------------------------------------------------------------------------

    /** Asks the system file picker for a document. */
    public void pickFile() {
        Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        i.addCategory(Intent.CATEGORY_OPENABLE);
        i.setType("*/*");
        try {
            startActivityForResult(i, REQ_OPEN);
        } catch (RuntimeException e) {
            Toast.makeText(this, R.string.no_file_picker, Toast.LENGTH_LONG).show();
        }
    }

    /** Asks the system file picker where to save the sterile copy. */
    public void pickSaveLocation(String suggestedName, String mime) {
        Intent i = new Intent(Intent.ACTION_CREATE_DOCUMENT);
        i.addCategory(Intent.CATEGORY_OPENABLE);
        i.setType(mime == null || mime.isEmpty() ? "application/octet-stream" : mime);
        i.putExtra(Intent.EXTRA_TITLE, suggestedName);
        try {
            startActivityForResult(i, REQ_SAVE);
        } catch (RuntimeException e) {
            Toast.makeText(this, R.string.no_file_picker, Toast.LENGTH_LONG).show();
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (resultCode != RESULT_OK || data == null || data.getData() == null) {
            return;
        }
        if (requestCode == REQ_OPEN) {
            openDocument(data.getData(), data.getType());
        } else if (requestCode == REQ_SAVE) {
            Session.get().saveSanitized(this, data.getData());
        }
    }

    /** Starts scanning {@code uri}: the stack becomes Home, Scan. */
    public void openDocument(Uri uri, String mime) {
        leaveTop();
        Session.get().open(this, uri, mime);
        ROUTES.clear();
        ROUTES.add(Route.HOME);
        ROUTES.add(Route.SCAN);
        show();
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == REQ_NOTIF_PERMISSION) {
            boolean granted = grantResults.length > 0 && grantResults[0] == android.content.pm.PackageManager.PERMISSION_GRANTED;
            Prefs.setReminders(this, granted);
            if (granted) {
                Reminders.schedule(this);
            } else {
                Reminders.cancel(this);
                Toast.makeText(this, R.string.settings_reminders_perm_denied, Toast.LENGTH_SHORT).show();
            }
            refresh();
        }
    }

    private void handleIntent(Intent intent) {
        if (intent == null) {
            return;
        }
        String action = intent.getAction();
        if (ACTION_SHOW_ABOUT.equals(action)) {
            setIntent(new Intent());
            if (ROUTES.isEmpty()) {
                ROUTES.add(Route.HOME);
            }
            if (ROUTES.get(ROUTES.size() - 1) != Route.ABOUT) {
                push(Route.ABOUT);
            }
            return;
        }
        Uri uri = null;
        try {
            if (Intent.ACTION_VIEW.equals(action)) {
                uri = intent.getData();
            } else if (Intent.ACTION_SEND.equals(action)) {
                // Any app can send this activity an intent: a hostile extra must not crash it. The
                // Class-typed overload (API 33+) refuses to unparcel anything but a Uri.
                uri = Compat.parcelableExtra(intent, Intent.EXTRA_STREAM, Uri.class);
                if (uri == null) {
                    Toast.makeText(this, R.string.share_without_file, Toast.LENGTH_LONG).show();
                }
            }
        } catch (RuntimeException e) {
            // BadParcelableException, ClassCastException, ...: treated as "nothing usable was sent".
            uri = null;
            Toast.makeText(this, R.string.unsupported_source, Toast.LENGTH_LONG).show();
        }
        if (uri == null) {
            return;
        }
        String scheme = uri.getScheme();
        if (!"content".equals(scheme) && !"file".equals(scheme)) {
            Toast.makeText(this, R.string.unsupported_source, Toast.LENGTH_LONG).show();
            return;
        }
        if ("file".equals(scheme) && !isReadableDocumentFile(uri)) {
            Toast.makeText(this, R.string.unsupported_source, Toast.LENGTH_LONG).show();
            return;
        }
        // The intent must not start the same scan again after a configuration-driven re-creation.
        setIntent(new Intent());
        openDocument(uri, intent.getType());
    }

    /**
     * A file: address is accepted only for a regular file outside the app's own storage and the
     * kernel's pseudo file systems. The manifest does not even advertise file:, but this activity
     * is exported, so any app can send it one; the canonical path defeats ../ and symlink tricks.
     */
    private boolean isReadableDocumentFile(Uri uri) {
        String path = uri.getPath();
        if (path == null || path.isEmpty()) {
            return false;
        }
        try {
            java.io.File file = new java.io.File(path).getCanonicalFile();
            String canonical = file.getPath();
            String[] blocked = {
                getDataDir().getCanonicalPath(),
                createDeviceProtectedStorageContext().getDataDir().getCanonicalPath(),
                "/proc", "/sys", "/dev"
            };
            for (String b : blocked) {
                if (canonical.equals(b) || canonical.startsWith(b + "/")) {
                    return false;
                }
            }
            return file.isFile();
        } catch (java.io.IOException | RuntimeException e) {
            return false;
        }
    }
}
