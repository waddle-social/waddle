import SwiftUI

/// Send (or save, while editing). It sleeps, a bare muted glyph, until
/// there is something to send, then fills with the action colour (web
/// `iconbtn` send). Command-Return sends on every platform.
struct ComposerSendButton: View {
    let isEditing: Bool
    let isEnabled: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: isEditing ? "checkmark" : "paperplane.fill")
                .font(.system(size: ComposerMetrics.iconSize, weight: .semibold))
                .foregroundStyle(isEnabled ? Theme.Colors.actionFg : Theme.Colors.fgSoft)
                .frame(width: ComposerMetrics.button, height: ComposerMetrics.button)
                .background(
                    RoundedRectangle(cornerRadius: ComposerMetrics.buttonRadius, style: .continuous)
                        .fill(isEnabled ? Theme.Colors.action : .clear)
                )
                .contentShape(RoundedRectangle(cornerRadius: ComposerMetrics.buttonRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(!isEnabled)
        .huddleAnimation(.settle, value: isEnabled)
        .keyboardShortcut(.return, modifiers: .command)
        .help(isEditing ? "Save edit" : "Send")
        .accessibilityLabel(Text(isEditing ? "Save edit" : "Send"))
    }
}
