import SwiftUI

/// A borderless icon button in the composer (web `wd-iconbtn`). `isOn`
/// marks toggles such as the formatting bar.
struct ComposerIconButton: View {
    let symbol: String
    let label: String
    var isOn = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            ComposerIconLabel(symbol: symbol, isOn: isOn)
        }
        .buttonStyle(ComposerIconButtonStyle(isOn: isOn))
        .help(label)
        .accessibilityLabel(Text(label))
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }
}

/// The icon inside a composer button or menu label, sized to the platform
/// target.
struct ComposerIconLabel: View {
    let symbol: String
    var isOn = false
    @Environment(\.isEnabled) private var isEnabled
    @ScaledMetric(relativeTo: .body) private var glyph = ComposerMetrics.iconSize

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: glyph, weight: .medium))
            .foregroundStyle(isOn ? Theme.Colors.fg : Theme.Colors.fgMuted)
            .opacity(isEnabled ? 1 : 0.5)
            .frame(width: ComposerMetrics.button, height: ComposerMetrics.button)
            .contentShape(RoundedRectangle(cornerRadius: ComposerMetrics.buttonRadius, style: .continuous))
    }
}

/// Hover, press and on tints behind a composer icon.
struct ComposerIconButtonStyle: ButtonStyle {
    var isOn = false

    func makeBody(configuration: Configuration) -> some View {
        ComposerIconButtonBody(configuration: configuration, isOn: isOn)
    }
}

private struct ComposerIconButtonBody: View {
    let configuration: ButtonStyleConfiguration
    let isOn: Bool
    @State private var isHovering = false

    var body: some View {
        configuration.label
            .background(
                RoundedRectangle(cornerRadius: ComposerMetrics.buttonRadius, style: .continuous)
                    .fill(tint)
            )
            .onHover { isHovering = $0 }
    }

    private var tint: Color {
        if configuration.isPressed || isOn { return Theme.State.selected }
        return isHovering ? Theme.State.hover : .clear
    }
}
