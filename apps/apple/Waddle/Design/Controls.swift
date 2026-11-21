import SwiftUI

// MARK: - Button

/// Polar Huddle's button (web `btn`). One primary per view.
struct HuddleButtonStyle: ButtonStyle {
    enum Variant {
        /// Outlined: any action that is not the main one.
        case secondary
        /// The main action of the view.
        case primary
        /// Joining something that is happening now.
        case live
        /// Leave, remove, end.
        case danger
    }

    enum Size {
        case small
        case regular
        case large
    }

    var variant: Variant = .secondary
    var size: Size = .regular

    func makeBody(configuration: Configuration) -> some View {
        HuddleButtonBody(configuration: configuration, variant: variant, size: size)
    }
}

extension ButtonStyle where Self == HuddleButtonStyle {
    static func huddle(
        _ variant: HuddleButtonStyle.Variant = .secondary,
        size: HuddleButtonStyle.Size = .regular
    ) -> HuddleButtonStyle {
        HuddleButtonStyle(variant: variant, size: size)
    }
}

private struct HuddleButtonBody: View {
    let configuration: ButtonStyleConfiguration
    let variant: HuddleButtonStyle.Variant
    let size: HuddleButtonStyle.Size
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let shape = variant == .live
            ? AnyInsettableShape(Capsule())
            : AnyInsettableShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
        configuration.label
            .huddleText(textStyle)
            .lineLimit(1)
            .foregroundStyle(foreground)
            .padding(.horizontal, horizontalPadding)
            .frame(minHeight: height)
            .background(shape.fill(background))
            .overlay(shape.fill(Theme.Colors.fg.opacity(configuration.isPressed ? 0.1 : 0)))
            .overlay(shape.strokeBorder(border, lineWidth: WaddleTokens.BorderWidth.hair))
            .scaleEffect(configuration.isPressed && !reduceMotion ? HuddleMotion.pressScale : 1)
            .opacity(isEnabled ? 1 : 0.5)
            .huddleTouchTarget()
            .huddleAnimation(.press, value: configuration.isPressed)
    }

    private var textStyle: HuddleTextStyle {
        let base = size == .large
            ? Theme.Typography.button.sized(WaddleTokens.FontSize.md)
            : Theme.Typography.button
        return variant == .secondary ? base : base.weight(700)
    }

    private var height: CGFloat {
        switch size {
        case .small: return WaddleTokens.ControlHeight.md
        case .regular: return Self.regularHeight
        case .large: return 48
        }
    }

    /// Touch gets the 44pt target; a pointer gets the 40pt control.
    private static var regularHeight: CGFloat {
        #if os(iOS)
        WaddleTokens.ControlHeight.touch
        #else
        WaddleTokens.ControlHeight.lg
        #endif
    }

    private var horizontalPadding: CGFloat {
        switch size {
        case .small: return WaddleTokens.Spacing.s3
        case .regular: return WaddleTokens.Spacing.s4
        case .large: return WaddleTokens.Spacing.s5
        }
    }

    private var background: Color {
        switch variant {
        case .secondary: return Theme.Colors.surface
        case .primary: return Theme.Colors.action
        case .live: return Theme.Colors.live
        case .danger: return Theme.Colors.danger
        }
    }

    private var foreground: Color {
        switch variant {
        case .secondary: return Theme.Colors.fg
        case .primary: return Theme.Colors.actionFg
        case .live: return Theme.Colors.liveFg
        case .danger: return Theme.Colors.dangerFg
        }
    }

    private var border: Color {
        variant == .secondary ? Theme.Colors.border : .clear
    }
}

extension View {
    /// Gives a control the 44pt touch target on iOS without changing how
    /// tall it looks; a pointer keeps the control's own bounds.
    func huddleTouchTarget() -> some View {
        #if os(iOS)
        frame(minHeight: Theme.Size.minHitTarget).contentShape(Rectangle())
        #else
        contentShape(Rectangle())
        #endif
    }
}

/// Type-erased insettable shape, so one view can pick a capsule or a
/// rounded rectangle.
struct AnyInsettableShape: InsettableShape {
    private let pathIn: @Sendable (CGRect) -> Path
    private let insetBy: @Sendable (CGFloat) -> AnyInsettableShape

    init<S: InsettableShape>(_ shape: S) {
        pathIn = { shape.path(in: $0) }
        insetBy = { AnyInsettableShape(shape.inset(by: $0)) }
    }

    func path(in rect: CGRect) -> Path { pathIn(rect) }
    func inset(by amount: CGFloat) -> AnyInsettableShape { insetBy(amount) }
}

// MARK: - Chip

/// A pill that toggles or filters (web `chip`): a reaction, a filter.
struct HuddleChipStyle: ButtonStyle {
    var isSelected = false

    func makeBody(configuration: Configuration) -> some View {
        HuddleChipBody(configuration: configuration, isSelected: isSelected)
    }
}

extension ButtonStyle where Self == HuddleChipStyle {
    static func huddleChip(isSelected: Bool = false) -> HuddleChipStyle {
        HuddleChipStyle(isSelected: isSelected)
    }
}

private struct HuddleChipBody: View {
    let configuration: ButtonStyleConfiguration
    let isSelected: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        configuration.label
            .huddleText(Theme.Typography.caption.weight(isSelected ? 700 : 400))
            .monospacedDigit()
            .lineLimit(1)
            .foregroundStyle(isSelected ? Theme.Colors.fg : Theme.Colors.fgMuted)
            .padding(.horizontal, WaddleTokens.Spacing.s2Half)
            .frame(minHeight: WaddleTokens.ControlHeight.sm)
            // The outline is a capsule filled behind the chip's own fill, not
            // a stroke: in a window, an animated view's stroked capsule is
            // drawn as a layer of its own, which leaves a mark at each end.
            .background(
                Capsule()
                    .fill(isSelected ? Theme.Colors.actionSubtle : Theme.Colors.surface)
                    .padding(isSelected ? WaddleTokens.BorderWidth.strong : WaddleTokens.BorderWidth.hair)
                    .background(Capsule().fill(isSelected ? Theme.Colors.actionText : Theme.Colors.border))
            )
            .overlay(Capsule().fill(Theme.Colors.fg.opacity(configuration.isPressed ? 0.1 : 0)))
            .scaleEffect(configuration.isPressed && !reduceMotion ? HuddleMotion.pressScale : 1)
            .huddleTouchTarget()
            .huddleAnimation(.press, value: configuration.isPressed)
            .huddleAnimation(.pop, value: isSelected)
            .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}

// MARK: - Tag

/// A small outlined label for a role or state (web `tag`).
struct HuddleTag: View {
    let title: String

    init(_ title: String) {
        self.title = title
    }

    var body: some View {
        Text(title)
            .huddleText(Theme.Typography.tag)
            .lineLimit(1)
            .foregroundStyle(Theme.Colors.fgMuted)
            .padding(.horizontal, WaddleTokens.Spacing.s2)
            .frame(minHeight: 22)
            .overlay(
                RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous)
                    .strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair)
            )
    }
}

// MARK: - Toggle

/// Polar Huddle's switch (web `toggle`): the track fills with the action
/// colour when on, and the thumb settles across with a spring.
struct HuddleToggleStyle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        // A button, so the row takes keyboard focus and Space like any
        // other control; a tap gesture would be pointer- and touch-only.
        Button {
            configuration.isOn.toggle()
        } label: {
            HStack(spacing: Theme.Spacing.m) {
                configuration.label
                Spacer(minLength: 0)
                HuddleSwitch(isOn: configuration.isOn)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityValue(Text(configuration.isOn ? "On" : "Off"))
        .accessibilityAddTraits(.isToggle)
    }
}

extension ToggleStyle where Self == HuddleToggleStyle {
    static var huddle: HuddleToggleStyle { HuddleToggleStyle() }
}

private struct HuddleSwitch: View {
    let isOn: Bool
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private static let size = CGSize(width: 44, height: 26)
    private static let thumb: CGFloat = 20

    var body: some View {
        Capsule()
            .fill(isOn ? Theme.Colors.action : Theme.Colors.surface2)
            .overlay(
                Capsule().strokeBorder(
                    isOn ? Theme.Colors.action : Theme.Colors.borderStrong,
                    lineWidth: WaddleTokens.BorderWidth.hair
                )
            )
            .frame(width: Self.size.width, height: Self.size.height)
            .overlay { thumbs }
            .opacity(isEnabled ? 1 : 0.5)
            .huddleAnimation(.settle, value: isOn)
    }

    /// One thumb that slides across; with Reduce Motion, a thumb at each
    /// end that cross-fade, so nothing moves.
    @ViewBuilder
    private var thumbs: some View {
        if reduceMotion {
            thumb(isOn: false).opacity(isOn ? 0 : 1)
            thumb(isOn: true).opacity(isOn ? 1 : 0)
        } else {
            thumb(isOn: isOn)
        }
    }

    private func thumb(isOn: Bool) -> some View {
        Circle()
            .fill(isOn ? Theme.Colors.actionFg : Theme.Colors.fgMuted)
            .frame(width: Self.thumb, height: Self.thumb)
            .padding(.horizontal, (Self.size.height - Self.thumb) / 2)
            .frame(maxWidth: .infinity, alignment: isOn ? .trailing : .leading)
    }
}

// MARK: - Segmented control

/// A pill of mutually exclusive choices (web `seg`): Gallery / Speaker.
struct HuddleSegmentedControl<Value: Hashable>: View {
    struct Segment: Identifiable {
        let value: Value
        let title: String
        var systemImage: String?

        var id: Value { value }
    }

    @Binding var selection: Value
    let segments: [Segment]
    @Namespace private var thumb
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// With the 3pt inset, touch gets a 44pt control (web `seg` size `lg`).
    private static var segmentHeight: CGFloat {
        #if os(iOS)
        38
        #else
        WaddleTokens.ControlHeight.sm
        #endif
    }

    var body: some View {
        HStack(spacing: WaddleTokens.Spacing.s0Half) {
            ForEach(segments) { segment in
                segmentButton(segment)
            }
        }
        .padding(3)
        .background(Capsule().fill(Theme.Colors.surface2))
        .huddleAnimation(.settle, value: selection)
        .accessibilityElement(children: .contain)
    }

    private func segmentButton(_ segment: Segment) -> some View {
        let isSelected = segment.value == selection
        return Button {
            selection = segment.value
        } label: {
            HStack(spacing: WaddleTokens.Spacing.s1Half) {
                if let systemImage = segment.systemImage {
                    Image(systemName: systemImage)
                }
                Text(segment.title)
            }
            .huddleText(Theme.Typography.caption.weight(600))
            .lineLimit(1)
            .foregroundStyle(isSelected ? Theme.Colors.fg : Theme.Colors.fgMuted)
            .padding(.horizontal, WaddleTokens.Spacing.s3)
            .frame(minHeight: Self.segmentHeight)
            .background {
                if isSelected {
                    selectionThumb
                }
            }
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    /// The raised pill behind the selected segment. It slides between
    /// segments; with Reduce Motion the old one fades out and the new one
    /// fades in, so nothing moves.
    @ViewBuilder
    private var selectionThumb: some View {
        let pill = Color.clear
            .huddleSurface(.card, in: Capsule())
            .overlay(Capsule().strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair))
        if reduceMotion {
            pill.transition(.opacity)
        } else {
            pill.matchedGeometryEffect(id: "thumb", in: thumb)
        }
    }
}
