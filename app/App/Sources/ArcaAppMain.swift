// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI
import ArcaKit

/// The entry point of the iPhone and iPad app. Everything it shows lives in `ArcaKit`
/// (`ArcaScene`); the app target only owns `@main`, the Info.plist and the signing.
@main
struct ArcaAppMain: App {
    var body: some Scene {
        ArcaScene()
    }
}
