// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI

/// Available color themes defined in the Arca design system specification.
public enum ArcaTheme: String, CaseIterable, Identifiable, Sendable {
    case alabaster = "Alabaster Studio"
    case obsidian = "Obsidian Vault"
    case emeraldSecOps = "Emerald SecOps"
    case sierraBlue = "Sierra Blue & Slate"
    case desertTitanium = "Desert Titanium"
    case cashmere = "Cashmere & Cappuccino"

    public var id: String { rawValue }

    /// Background color of the main interface canvas.
    public var background: Color {
        switch self {
        case .alabaster:
            return Color(red: 0.98, green: 0.98, blue: 0.99)
        case .obsidian:
            return Color(red: 0.00, green: 0.00, blue: 0.00) // Pure OLED Black
        case .emeraldSecOps:
            return Color(red: 0.04, green: 0.07, blue: 0.06)
        case .sierraBlue:
            return Color(red: 0.07, green: 0.09, blue: 0.12)
        case .desertTitanium:
            return Color(red: 0.12, green: 0.11, blue: 0.10)
        case .cashmere:
            return Color(red: 0.96, green: 0.94, blue: 0.91)
        }
    }

    /// Card, cell and sheet surface color.
    public var surface: Color {
        switch self {
        case .alabaster:
            return Color(red: 1.00, green: 1.00, blue: 1.00)
        case .obsidian:
            return Color(red: 0.08, green: 0.08, blue: 0.09)
        case .emeraldSecOps:
            return Color(red: 0.07, green: 0.13, blue: 0.11)
        case .sierraBlue:
            return Color(red: 0.12, green: 0.15, blue: 0.19)
        case .desertTitanium:
            return Color(red: 0.18, green: 0.16, blue: 0.15)
        case .cashmere:
            return Color(red: 1.00, green: 0.99, blue: 0.97)
        }
    }

    /// Secondary elevated surface (e.g. tag chips, button backgrounds).
    public var surfaceSecondary: Color {
        switch self {
        case .alabaster:
            return Color(red: 0.93, green: 0.93, blue: 0.95)
        case .obsidian:
            return Color(red: 0.14, green: 0.14, blue: 0.16)
        case .emeraldSecOps:
            return Color(red: 0.11, green: 0.19, blue: 0.16)
        case .sierraBlue:
            return Color(red: 0.17, green: 0.22, blue: 0.27)
        case .desertTitanium:
            return Color(red: 0.25, green: 0.23, blue: 0.21)
        case .cashmere:
            return Color(red: 0.90, green: 0.87, blue: 0.83)
        }
    }

    /// Primary high-contrast text color.
    public var textPrimary: Color {
        switch self {
        case .alabaster, .cashmere:
            return Color(red: 0.10, green: 0.10, blue: 0.12)
        default:
            return Color(red: 0.96, green: 0.96, blue: 0.97)
        }
    }

    /// Secondary muted text color for captions and metadata.
    public var textSecondary: Color {
        switch self {
        case .alabaster, .cashmere:
            return Color(red: 0.45, green: 0.45, blue: 0.48)
        default:
            return Color(red: 0.60, green: 0.60, blue: 0.64)
        }
    }

    /// Primary accent tint.
    public var accent: Color {
        switch self {
        case .alabaster:
            return Color(red: 0.15, green: 0.45, blue: 0.85)
        case .obsidian:
            return Color(red: 0.90, green: 0.90, blue: 0.92)
        case .emeraldSecOps:
            return Color(red: 0.15, green: 0.82, blue: 0.55)
        case .sierraBlue:
            return Color(red: 0.35, green: 0.65, blue: 0.95)
        case .desertTitanium:
            return Color(red: 0.85, green: 0.65, blue: 0.45)
        case .cashmere:
            return Color(red: 0.55, green: 0.35, blue: 0.25)
        }
    }

    /// Clean verdict color (Shield green).
    public static let cleanGreen = Color(red: 0.13, green: 0.75, blue: 0.42)
    /// Warning verdict color (Trackers amber).
    public static let trackerAmber = Color(red: 0.95, green: 0.65, blue: 0.15)
    /// Active payload neutralized color (Active red).
    public static let activeRed = Color(red: 0.92, green: 0.28, blue: 0.25)
    /// Rejected threat color (Crimson).
    public static let rejectedCrimson = Color(red: 0.78, green: 0.10, blue: 0.15)
    /// Unverified status color (Muted slate).
    public static let unverifiedSlate = Color(red: 0.55, green: 0.60, blue: 0.65)
}

/// Dynamic spring animation parameters mandated by the Arca specification:
/// `mass: 1.0, stiffness: 480, damping: 34` (critically damped, interruption-friendly).
public extension Animation {
    static var arcaSpring: Animation {
        .interpolatingSpring(mass: 1.0, stiffness: 480, damping: 34)
    }

    static var arcaBouncy: Animation {
        .interpolatingSpring(mass: 0.9, stiffness: 350, damping: 25)
    }
}
