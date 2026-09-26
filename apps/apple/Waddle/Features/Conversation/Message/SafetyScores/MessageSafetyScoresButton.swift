import SwiftUI
import WaddleKit

/// The marker on a row whose scores crossed the notice threshold. Every
/// participant sees it; it opens the per-category breakdown. It is an
/// annotation, not a moderation action, so it stays small: amber for a
/// notice, red for an alert, and absent below the notice threshold.
struct MessageSafetyScoresButton: View {
    static let title = "Content signals"
    static let symbol = "gauge.with.dots.needle.33percent"

    let severity: SafetyScoreSeverity
    let action: () -> Void

    static func tint(for severity: SafetyScoreSeverity) -> Color {
        switch severity {
        case .notice: return .orange
        case .alert: return .red
        }
    }

    var body: some View {
        Button(action: action) {
            Image(systemName: Self.symbol)
                .font(.caption2)
                .foregroundStyle(Self.tint(for: severity))
                .padding(Theme.Spacing.xs)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(Self.title)
        // The row is one combined accessibility element; VoiceOver reaches
        // the breakdown through the row's custom action instead.
        .accessibilityHidden(true)
    }
}
