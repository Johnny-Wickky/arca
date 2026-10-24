// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI

/// Original vector iconography drawn from pure geometry.
///
/// In strict compliance with Arca Rule 6: "Никаких SF Symbols и Material Icons".
public struct ArcaShieldShape: Shape {
    public func path(in rect: CGRect) -> Path {
        var path = Path()
        let w = rect.width
        let h = rect.height

        path.move(to: CGPoint(x: w * 0.5, y: 0))
        path.addLine(to: CGPoint(x: w, y: h * 0.15))
        path.addQuadCurve(
            to: CGPoint(x: w * 0.5, y: h),
            control: CGPoint(x: w * 0.95, y: h * 0.65)
        )
        path.addQuadCurve(
            to: CGPoint(x: 0, y: h * 0.15),
            control: CGPoint(x: w * 0.05, y: h * 0.65)
        )
        path.closeSubpath()
        return path
    }
}

/// Geometric glyph kinds supported across the interface.
public enum ArcaGlyphKind: Sendable {
    case shield
    case shieldCheck
    case shieldAlert
    case shieldCross
    case document
    case macro
    case externalLink
    case metadata
    case table
    case image
    case sanitize
    case passport
    case lock
    case unlock
    case close
    case chevronRight
    case chevronDown
    case arrowLeft
    case share
    case check
}

/// Rendered geometric glyph view.
public struct ArcaGlyph: View {
    public let kind: ArcaGlyphKind
    public var size: CGFloat = 20
    public var color: Color = .primary

    public init(_ kind: ArcaGlyphKind, size: CGFloat = 20, color: Color = .primary) {
        self.kind = kind
        self.size = size
        self.color = color
    }

    public var body: some View {
        Canvas { context, canvasSize in
            let w = canvasSize.width
            let h = canvasSize.height
            let strokeStyle = StrokeStyle(lineWidth: max(1.5, size * 0.09), lineCap: .round, lineJoin: .round)

            switch kind {
            case .shield:
                let rect = CGRect(origin: .zero, size: canvasSize)
                context.stroke(ArcaShieldShape().path(in: rect), with: .color(color), style: strokeStyle)

            case .shieldCheck:
                let rect = CGRect(origin: .zero, size: canvasSize)
                context.stroke(ArcaShieldShape().path(in: rect), with: .color(color), style: strokeStyle)
                var p = Path()
                p.move(to: CGPoint(x: w * 0.32, y: h * 0.52))
                p.addLine(to: CGPoint(x: w * 0.46, y: h * 0.66))
                p.addLine(to: CGPoint(x: w * 0.68, y: h * 0.38))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .shieldAlert:
                let rect = CGRect(origin: .zero, size: canvasSize)
                context.stroke(ArcaShieldShape().path(in: rect), with: .color(color), style: strokeStyle)
                var p = Path()
                p.move(to: CGPoint(x: w * 0.5, y: h * 0.32))
                p.addLine(to: CGPoint(x: w * 0.5, y: h * 0.56))
                context.stroke(p, with: .color(color), style: strokeStyle)
                let dotRect = CGRect(x: w * 0.44, y: h * 0.66, width: w * 0.12, height: w * 0.12)
                context.fill(Path(ellipseIn: dotRect), with: .color(color))

            case .shieldCross:
                let rect = CGRect(origin: .zero, size: canvasSize)
                context.stroke(ArcaShieldShape().path(in: rect), with: .color(color), style: strokeStyle)
                var p = Path()
                p.move(to: CGPoint(x: w * 0.36, y: h * 0.40))
                p.addLine(to: CGPoint(x: w * 0.64, y: h * 0.64))
                p.move(to: CGPoint(x: w * 0.64, y: h * 0.40))
                p.addLine(to: CGPoint(x: w * 0.36, y: h * 0.64))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .document:
                var p = Path()
                p.move(to: CGPoint(x: w * 0.20, y: h * 0.10))
                p.addLine(to: CGPoint(x: w * 0.60, y: h * 0.10))
                p.addLine(to: CGPoint(x: w * 0.80, y: h * 0.30))
                p.addLine(to: CGPoint(x: w * 0.80, y: h * 0.90))
                p.addLine(to: CGPoint(x: w * 0.20, y: h * 0.90))
                p.closeSubpath()
                // fold
                p.move(to: CGPoint(x: w * 0.60, y: h * 0.10))
                p.addLine(to: CGPoint(x: w * 0.60, y: h * 0.30))
                p.addLine(to: CGPoint(x: w * 0.80, y: h * 0.30))
                // lines
                p.move(to: CGPoint(x: w * 0.35, y: h * 0.50))
                p.addLine(to: CGPoint(x: w * 0.65, y: h * 0.50))
                p.move(to: CGPoint(x: w * 0.35, y: h * 0.68))
                p.addLine(to: CGPoint(x: w * 0.65, y: h * 0.68))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .macro:
                var p = Path()
                // Left angle bracket
                p.move(to: CGPoint(x: w * 0.38, y: h * 0.30))
                p.addLine(to: CGPoint(x: w * 0.20, y: h * 0.50))
                p.addLine(to: CGPoint(x: w * 0.38, y: h * 0.70))
                // Right angle bracket
                p.move(to: CGPoint(x: w * 0.62, y: h * 0.30))
                p.addLine(to: CGPoint(x: w * 0.80, y: h * 0.50))
                p.addLine(to: CGPoint(x: w * 0.62, y: h * 0.70))
                // Slash
                p.move(to: CGPoint(x: w * 0.56, y: h * 0.24))
                p.addLine(to: CGPoint(x: w * 0.44, y: h * 0.76))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .externalLink:
                var p = Path()
                p.move(to: CGPoint(x: w * 0.45, y: h * 0.25))
                p.addLine(to: CGPoint(x: w * 0.75, y: h * 0.25))
                p.addLine(to: CGPoint(x: w * 0.75, y: h * 0.55))
                p.move(to: CGPoint(x: w * 0.75, y: h * 0.25))
                p.addLine(to: CGPoint(x: w * 0.35, y: h * 0.65))
                // Box outline
                p.move(to: CGPoint(x: w * 0.55, y: h * 0.45))
                p.addLine(to: CGPoint(x: w * 0.55, y: h * 0.75))
                p.addLine(to: CGPoint(x: w * 0.25, y: h * 0.75))
                p.addLine(to: CGPoint(x: w * 0.25, y: h * 0.45))
                p.addLine(to: CGPoint(x: w * 0.45, y: h * 0.45))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .metadata:
                var p = Path()
                p.move(to: CGPoint(x: w * 0.20, y: h * 0.30))
                p.addLine(to: CGPoint(x: w * 0.80, y: h * 0.30))
                p.move(to: CGPoint(x: w * 0.20, y: h * 0.50))
                p.addLine(to: CGPoint(x: w * 0.80, y: h * 0.50))
                p.move(to: CGPoint(x: w * 0.20, y: h * 0.70))
                p.addLine(to: CGPoint(x: w * 0.55, y: h * 0.70))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .table:
                var p = Path()
                p.addRect(CGRect(x: w * 0.15, y: h * 0.20, width: w * 0.70, height: h * 0.60))
                p.move(to: CGPoint(x: w * 0.15, y: h * 0.45))
                p.addLine(to: CGPoint(x: w * 0.85, y: h * 0.45))
                p.move(to: CGPoint(x: w * 0.50, y: h * 0.20))
                p.addLine(to: CGPoint(x: w * 0.50, y: h * 0.80))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .image:
                var p = Path()
                p.addRect(CGRect(x: w * 0.15, y: h * 0.20, width: w * 0.70, height: h * 0.60))
                p.move(to: CGPoint(x: w * 0.25, y: h * 0.65))
                p.addLine(to: CGPoint(x: w * 0.45, y: h * 0.45))
                p.addLine(to: CGPoint(x: w * 0.60, y: h * 0.55))
                p.addLine(to: CGPoint(x: w * 0.75, y: h * 0.40))
                p.addLine(to: CGPoint(x: w * 0.80, y: h * 0.65))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .sanitize:
                var p = Path()
                // Sparkle / Sterilize star
                p.move(to: CGPoint(x: w * 0.50, y: h * 0.15))
                p.addQuadCurve(to: CGPoint(x: w * 0.85, y: h * 0.50), control: CGPoint(x: w * 0.50, y: h * 0.50))
                p.addQuadCurve(to: CGPoint(x: w * 0.50, y: h * 0.85), control: CGPoint(x: w * 0.50, y: h * 0.50))
                p.addQuadCurve(to: CGPoint(x: w * 0.15, y: h * 0.50), control: CGPoint(x: w * 0.50, y: h * 0.50))
                p.addQuadCurve(to: CGPoint(x: w * 0.50, y: h * 0.15), control: CGPoint(x: w * 0.50, y: h * 0.50))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .passport:
                var p = Path()
                p.addRoundedRect(in: CGRect(x: w * 0.20, y: h * 0.15, width: w * 0.60, height: h * 0.70), cornerSize: CGSize(width: 4, height: 4))
                p.addEllipse(in: CGRect(x: w * 0.38, y: h * 0.35, width: w * 0.24, height: w * 0.24))
                p.move(to: CGPoint(x: w * 0.35, y: h * 0.70))
                p.addLine(to: CGPoint(x: w * 0.65, y: h * 0.70))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .lock:
                var p = Path()
                p.addRoundedRect(in: CGRect(x: w * 0.25, y: h * 0.45, width: w * 0.50, height: h * 0.45), cornerSize: CGSize(width: 3, height: 3))
                p.move(to: CGPoint(x: w * 0.35, y: h * 0.45))
                p.addLine(to: CGPoint(x: w * 0.35, y: h * 0.30))
                p.addQuadCurve(to: CGPoint(x: w * 0.65, y: h * 0.30), control: CGPoint(x: w * 0.50, y: h * 0.15))
                p.addLine(to: CGPoint(x: w * 0.65, y: h * 0.45))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .unlock:
                var p = Path()
                p.addRoundedRect(in: CGRect(x: w * 0.25, y: h * 0.45, width: w * 0.50, height: h * 0.45), cornerSize: CGSize(width: 3, height: 3))
                p.move(to: CGPoint(x: w * 0.35, y: h * 0.45))
                p.addLine(to: CGPoint(x: w * 0.35, y: h * 0.30))
                p.addQuadCurve(to: CGPoint(x: w * 0.65, y: h * 0.30), control: CGPoint(x: w * 0.50, y: h * 0.15))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .close:
                var p = Path()
                p.move(to: CGPoint(x: w * 0.25, y: h * 0.25))
                p.addLine(to: CGPoint(x: w * 0.75, y: h * 0.75))
                p.move(to: CGPoint(x: w * 0.75, y: h * 0.25))
                p.addLine(to: CGPoint(x: w * 0.25, y: h * 0.75))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .chevronRight:
                var p = Path()
                p.move(to: CGPoint(x: w * 0.35, y: h * 0.25))
                p.addLine(to: CGPoint(x: w * 0.65, y: h * 0.50))
                p.addLine(to: CGPoint(x: w * 0.35, y: h * 0.75))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .chevronDown:
                var p = Path()
                p.move(to: CGPoint(x: w * 0.25, y: h * 0.35))
                p.addLine(to: CGPoint(x: w * 0.50, y: h * 0.65))
                p.addLine(to: CGPoint(x: w * 0.75, y: h * 0.35))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .arrowLeft:
                var p = Path()
                p.move(to: CGPoint(x: w * 0.45, y: h * 0.25))
                p.addLine(to: CGPoint(x: w * 0.20, y: h * 0.50))
                p.addLine(to: CGPoint(x: w * 0.45, y: h * 0.75))
                p.move(to: CGPoint(x: w * 0.20, y: h * 0.50))
                p.addLine(to: CGPoint(x: w * 0.80, y: h * 0.50))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .share:
                var p = Path()
                p.move(to: CGPoint(x: w * 0.50, y: h * 0.15))
                p.addLine(to: CGPoint(x: w * 0.50, y: h * 0.60))
                p.move(to: CGPoint(x: w * 0.35, y: h * 0.30))
                p.addLine(to: CGPoint(x: w * 0.50, y: h * 0.15))
                p.addLine(to: CGPoint(x: w * 0.65, y: h * 0.30))
                p.move(to: CGPoint(x: w * 0.25, y: h * 0.45))
                p.addLine(to: CGPoint(x: w * 0.25, y: h * 0.85))
                p.addLine(to: CGPoint(x: w * 0.75, y: h * 0.85))
                p.addLine(to: CGPoint(x: w * 0.75, y: h * 0.45))
                context.stroke(p, with: .color(color), style: strokeStyle)

            case .check:
                var p = Path()
                p.move(to: CGPoint(x: w * 0.20, y: h * 0.50))
                p.addLine(to: CGPoint(x: w * 0.42, y: h * 0.72))
                p.addLine(to: CGPoint(x: w * 0.80, y: h * 0.28))
                context.stroke(p, with: .color(color), style: strokeStyle)
            }
        }
        .frame(width: size, height: size)
    }
}
