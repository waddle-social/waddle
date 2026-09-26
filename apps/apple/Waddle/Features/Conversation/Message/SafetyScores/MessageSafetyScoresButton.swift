import SwiftUI
import WaddleKit

/// The marker on a row whose scores crossed the notice threshold. Every
/// participant sees it; it opens the per-category breakdown. It is an
/// annotation, not a moderation action, so it stays small: amber for a
/// notice, red for an alert, and absent below the notice threshold.
struct MessageSafetyScoresButton: View {
    static let title = "Content signals"
    static let symbol = "gauge.with.dots.needle.33percent"
    static let questionSymbol = "questionmark.bubble"

    let severity: SafetyScoreSeverity?
    let hasQuestionSignal: Bool
    let action: () -> Void

    static func isVisible(for scores: SafetyScores?) -> Bool {
        guard let scores else { return false }
        return !scores.notableRows.isEmpty
    }

    static func hasQuestionSignal(_ scores: SafetyScores?) -> Bool {
        scores?.notableSignalRows.contains { $0.category == .isQuestion } == true
    }

    static func symbol(for scores: SafetyScores?) -> String {
        guard scores?.severity == nil, hasQuestionSignal(scores) else { return symbol }
        return questionSymbol
    }

    static func tint(for severity: SafetyScoreSeverity) -> Color {
        switch severity {
        case .notice: return .orange
        case .alert: return .red
        }
    }

    var body: some View {
        Button(action: action) {
            Image(systemName: severity == nil && hasQuestionSignal ? Self.questionSymbol : Self.symbol)
                .font(.caption2)
                .foregroundStyle(markerTint)
                .padding(Theme.Spacing.xs)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(Self.title)
        // The row is one combined accessibility element; VoiceOver reaches
        // the breakdown through the row's custom action instead.
        .accessibilityHidden(true)
    }

    private var markerTint: Color {
        if let severity { return Self.tint(for: severity) }
        return hasQuestionSignal ? .accentColor : .secondary
    }
}
