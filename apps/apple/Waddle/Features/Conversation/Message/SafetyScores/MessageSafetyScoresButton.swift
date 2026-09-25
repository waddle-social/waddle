import SwiftUI

/// The quiet marker on a row the room has scored. Every participant sees
/// it; it opens the per-category breakdown. It is an annotation, not a
/// moderation action, so it stays small and uncolored.
struct MessageSafetyScoresButton: View {
    static let title = "Content signals"
    static let symbol = "gauge.with.dots.needle.33percent"

    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: Self.symbol)
                .font(.caption2)
                .foregroundStyle(.tertiary)
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
