import SwiftUI

/// A running command's progress, or a command notice with dismiss.
struct ComposerNoticeLine: View {
    let notice: ComposerNotice?
    /// The name of the command still running, if any.
    let runningCommand: String?
    let onDismiss: () -> Void

    var body: some View {
        if let runningCommand {
            HStack(spacing: Theme.Spacing.s) {
                ProgressView()
                    .controlSize(.small)
                Text("Running \(runningCommand)…")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .padding(.horizontal, WaddleTokens.Spacing.s2Half)
            .accessibilityElement(children: .combine)
        } else if let notice {
            HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
                Image(systemName: symbol(for: notice.severity))
                    .accessibilityHidden(true)
                Text(notice.text)
                    .huddleText(Theme.Typography.caption)
                    .lineLimit(4)
                    .foregroundStyle(notice.severity == .info ? Theme.Colors.fg : color(for: notice.severity))
                Spacer(minLength: Theme.Spacing.s)
                Button(action: onDismiss) {
                    Image(systemName: "xmark")
                        .huddleText(Theme.Typography.caption)
                }
                .buttonStyle(.plain)
                .help("Dismiss")
                .accessibilityLabel(Text("Dismiss"))
            }
            .foregroundStyle(color(for: notice.severity))
            .padding(.horizontal, WaddleTokens.Spacing.s2Half)
        }
    }

    private func symbol(for severity: ComposerNotice.Severity) -> String {
        switch severity {
        case .info: return "info.circle"
        case .warning: return "exclamationmark.triangle"
        case .error: return "exclamationmark.octagon"
        }
    }

    private func color(for severity: ComposerNotice.Severity) -> Color {
        switch severity {
        case .info: return Theme.Colors.fgMuted
        case .warning: return Theme.Colors.warningText
        case .error: return Theme.Colors.dangerText
        }
    }
}
