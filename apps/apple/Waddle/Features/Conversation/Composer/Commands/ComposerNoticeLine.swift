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
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, Theme.Spacing.s + 2)
            .accessibilityElement(children: .combine)
        } else if let notice {
            HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
                Image(systemName: symbol(for: notice.severity))
                    .accessibilityHidden(true)
                Text(notice.text)
                    .font(.caption)
                    .lineLimit(4)
                    .foregroundStyle(notice.severity == .info ? Color.primary : color(for: notice.severity))
                Spacer(minLength: Theme.Spacing.s)
                Button(action: onDismiss) {
                    Image(systemName: "xmark")
                        .font(.caption)
                }
                .buttonStyle(.plain)
                .help("Dismiss")
                .accessibilityLabel(Text("Dismiss"))
            }
            .foregroundStyle(color(for: notice.severity))
            .padding(.horizontal, Theme.Spacing.s + 2)
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
        case .info: return Color.secondary
        case .warning: return Color.orange
        case .error: return Color.red
        }
    }
}
