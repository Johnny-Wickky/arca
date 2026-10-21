// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.app.Application;

/** Starts the engine as early as possible so that it is ready when the first file arrives. */
public final class App extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        Engine.start(this);
    }
}
