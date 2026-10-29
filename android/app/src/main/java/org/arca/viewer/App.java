// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.app.Application;

/** Starts the engine as early as possible so that it is ready when the first file arrives. */
public final class App extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        if (Isolation.isIsolatedProcess()) {
            // The sandbox service runs the engine and nothing else: no storage, no settings, no UI.
            return;
        }
        Isolation.attach(this);
        // Whatever an older version kept (a counter blob while the counter is off, first-use and
        // reminder times) is deleted before anything else runs.
        Prefs.purgeLegacy(this);
        Engine.start(this);
        try {
            Stats.purgeIfOff(this);
        } catch (RuntimeException | LinkageError ignored) {
            // the counter is a courtesy; the app starts without it
        }
    }
}
