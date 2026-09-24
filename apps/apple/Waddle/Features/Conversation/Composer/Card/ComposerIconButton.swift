import SwiftUI

/// A borderless icon button in the composer card. `isOn` tints it for
/// toggles such as the formatting bar.
struct ComposerIconButton: View {
    let symbol: String
    let label: String
    var isOn = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            ComposerIconLabel(symbol: symbol, isOn: isOn)
        }
        .buttonStyle(.plain)
        .help(label)
        .accessibilityLabel(Text(label))
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }
}

/// The icon inside a composer button or menu label, sized to the platform
/// touch target.
struct ComposerIconLabel: View {
    let symbol: String
    var isOn = false

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: ComposerMetrics.iconSize, weight: .medium))
            .foregroundStyle(isOn ? Color.accentColor : Color.secondary)
            .frame(width: ComposerMetrics.touchTarget, height: ComposerMetrics.touchTarget)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous)
                    .fill(isOn ? Color.accentColor.opacity(0.12) : Color.clear)
            )
            .contentShape(Rectangle())
    }
}
