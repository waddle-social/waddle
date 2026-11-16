import SwiftUI

/// How far a surface sits above the page. Night shows depth with a light
/// top edge and a border; Daylight with a soft shadow.
enum HuddleElevation: CaseIterable {
    /// Cards and other resting surfaces.
    case card
    /// Menus, popovers and hovered cards.
    case raised
    /// Dialogs and sheets.
    case dialog
}

private struct ElevationShadow {
    let opacity: Double
    let radius: CGFloat
    let y: CGFloat
}

private extension HuddleElevation {
    /// Web `shadows.elev.N`. SwiftUI has no shadow spread, so a negative
    /// spread becomes a smaller, fainter shadow.
    func shadows(_ scheme: ColorScheme) -> [ElevationShadow] {
        switch (self, scheme) {
        case (.card, .dark):
            return [ElevationShadow(opacity: 0.4, radius: 1, y: 1)]
        case (.raised, .dark):
            return [ElevationShadow(opacity: 0.5, radius: 9, y: 12), ElevationShadow(opacity: 0.35, radius: 3, y: 2)]
        case (.dialog, .dark):
            return [ElevationShadow(opacity: 0.5, radius: 22, y: 24)]
        case (.card, _):
            return [ElevationShadow(opacity: 0.06, radius: 1, y: 1)]
        case (.raised, _):
            return [ElevationShadow(opacity: 0.14, radius: 8, y: 12), ElevationShadow(opacity: 0.06, radius: 3, y: 2)]
        case (.dialog, _):
            return [ElevationShadow(opacity: 0.2, radius: 20, y: 24), ElevationShadow(opacity: 0.08, radius: 4, y: 2)]
        }
    }

    /// Web `colors.elev.N`: the border that pairs with each elevation.
    func border(_ scheme: ColorScheme) -> Color {
        switch (self, scheme) {
        case (.card, .dark), (.dialog, .light): return .clear
        case (.raised, .dark), (.dialog, .dark): return Theme.Colors.border
        default: return Theme.Colors.borderSoft
        }
    }
}

private struct HuddleSurface<S: InsettableShape>: ViewModifier {
    let elevation: HuddleElevation
    let shape: S
    let fill: Color
    @Environment(\.colorScheme) private var scheme

    func body(content: Content) -> some View {
        content
            .background(ground)
            .overlay(shape.strokeBorder(elevation.border(scheme), lineWidth: WaddleTokens.BorderWidth.hair))
            .overlay(lightEdge)
    }

    private var ground: some View {
        let shadowColor = scheme == .dark ? Color.black : Color(red: 11 / 255, green: 14 / 255, blue: 19 / 255)
        return elevation.shadows(scheme).reduce(AnyView(shape.fill(fill))) { view, shadow in
            AnyView(view.shadow(color: shadowColor.opacity(shadow.opacity), radius: shadow.radius, y: shadow.y))
        }
    }

    /// Night's light top edge (web `shadows.edgeLight`).
    @ViewBuilder
    private var lightEdge: some View {
        if scheme == .dark {
            shape
                .strokeBorder(
                    LinearGradient(colors: [.white.opacity(0.06), .clear], startPoint: .top, endPoint: .center),
                    lineWidth: WaddleTokens.BorderWidth.hair
                )
                .allowsHitTesting(false)
        }
    }
}

extension View {
    /// Puts this view on a Polar Huddle surface: a filled `shape` with the
    /// border, light edge and shadow of `elevation`.
    func huddleSurface<S: InsettableShape>(
        _ elevation: HuddleElevation = .card,
        in shape: S,
        fill: Color = Theme.Colors.surface
    ) -> some View {
        modifier(HuddleSurface(elevation: elevation, shape: shape, fill: fill))
    }

    /// Puts this view on a surface with continuous rounded corners.
    func huddleSurface(
        _ elevation: HuddleElevation = .card,
        radius: CGFloat = Theme.Radius.large,
        fill: Color = Theme.Colors.surface
    ) -> some View {
        huddleSurface(elevation, in: RoundedRectangle(cornerRadius: radius, style: .continuous), fill: fill)
    }
}

/// A card: padded content on a resting surface. `isLive` gives it the
/// Aurora border, for a huddle that is happening now and nothing else.
struct HuddleCard<Content: View>: View {
    var isLive = false
    @ViewBuilder let content: Content

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) { content }
            .padding(Theme.Spacing.l)
            .frame(maxWidth: .infinity, alignment: .leading)
            .huddleSurface(.card, in: shape)
            .overlay {
                if isLive {
                    shape.strokeBorder(Theme.Colors.live, lineWidth: WaddleTokens.BorderWidth.strong)
                }
            }
    }
}
