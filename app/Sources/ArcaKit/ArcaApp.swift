// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI

/// The scene of the Arca document viewer.
///
/// The app target (`app/App/ArcaAppMain.swift`) owns the `@main` entry point and shows this scene;
/// the entry point is not in this library so that it stays linkable into tests and other hosts.
@MainActor
public struct ArcaScene: Scene {
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
