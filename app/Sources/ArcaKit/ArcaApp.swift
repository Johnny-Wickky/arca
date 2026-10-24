// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI

/// Main entry point of the Arca iOS document viewer application.
@main
public struct ArcaApp: App {
    @StateObject private var state = AppState()

    public init() {}

    public var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(state)
                .onOpenURL { url in
                    // Handles "Open with Arca" from Files, Mail, AirDrop, Telegram
                    state.openDocument(from: url)
                }
        }
    }
}
