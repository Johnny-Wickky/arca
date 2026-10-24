// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import UIKit

/// Tactile haptic feedback coordinator mandated by the Arca specification.
@MainActor
public enum ArcaHaptics {
    /// Security shield verdict feedback.
    public static func shieldVerdict(_ verdict: Verdict) {
        switch verdict {
        case .clean:
            let g = UINotificationFeedbackGenerator()
            g.prepare()
            g.notificationOccurred(.success)
        case .hiddenTrackers, .unverified:
            let g = UINotificationFeedbackGenerator()
            g.prepare()
            g.notificationOccurred(.warning)
        case .activeNeutralized, .rejected:
            let g = UINotificationFeedbackGenerator()
            g.prepare()
            g.notificationOccurred(.error)
        }
    }

    /// Zoom scale snap to 100%.
    public static func scaleSnap() {
        let g = UIImpactFeedbackGenerator(style: .rigid)
        g.prepare()
        g.impactOccurred(intensity: 0.8)
    }

    /// General UI selection / button tap.
    public static func selection() {
        let g = UISelectionFeedbackGenerator()
        g.prepare()
        g.selectionChanged()
    }

    /// One-tap sanitize completion.
    public static func sanitizeSuccess() {
        let g = UINotificationFeedbackGenerator()
        g.prepare()
        g.notificationOccurred(.success)
    }
}
