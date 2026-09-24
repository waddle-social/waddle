import SwiftUI

/// Send (or save, while editing): muted until there is something to send,
/// then filled with the accent color. Command-Return sends on every
/// platform.
struct ComposerSendButton: View {
    let isEditing: Bool
    let isEnabled: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: isEditing ? "checkmark" : "paperplane.fill")
                .font(.system(size: ComposerMetrics.sendSymbolSize, weight: .semibold))
                .foregroundStyle(isEnabled ? Color.white : Color.secondary)
                .frame(width: ComposerMetrics.sendWidth, height: ComposerMetrics.sendHeight)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.small + 2, style: .continuous)
                        .fill(isEnabled ? Color.accentColor : Color.secondary.opacity(0.12))
                )
                .frame(minWidth: ComposerMetrics.touchTarget, minHeight: ComposerMetrics.touchTarget)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!isEnabled)
        .keyboardShortcut(.return, modifiers: .command)
        .help(isEditing ? "Save edit" : "Send")
        .accessibilityLabel(Text(isEditing ? "Save edit" : "Send"))
    }
}
