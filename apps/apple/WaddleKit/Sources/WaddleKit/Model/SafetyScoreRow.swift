import Foundation

extension SafetyCategory {
    /// Display name for the breakdown.
    public var title: String {
        switch self {
        case .isQuestion: return "Question"
        case .hateSpeech: return "Hate speech"
        case .explicit: return "Sexually explicit"
        case .harassment: return "Harassment"
        case .violence: return "Violence"
        case .selfHarm: return "Self-harm"
        case .spamScam: return "Spam or scam"
        }
    }

    /// Community signals are listed apart from safety categories.
    public var isSafety: Bool { self != .isQuestion }
}

/// How loudly a score is surfaced. Below `noticeThreshold` a score is
/// not surfaced at all; the row marker is amber from `noticeThreshold`
/// and red from `alertThreshold`.
public enum SafetyScoreSeverity: Comparable, Hashable, Sendable {
    case notice
    case alert

    public static let noticeThreshold = 0.5
    public static let alertThreshold = 0.8

    /// Nil below the notice threshold.
    public init?(probability: Double) {
        if probability >= Self.alertThreshold {
            self = .alert
        } else if probability >= Self.noticeThreshold {
            self = .notice
        } else {
            return nil
        }
    }
}

/// One line of the per-category breakdown.
public struct SafetyScoreRow: Hashable, Sendable, Identifiable {
    public var id: SafetyCategory { category }
    public let category: SafetyCategory
    public let probability: Double
    public let taxonomyVersion: String

    public var title: String { category.title }

    /// This row's own severity; nil below the notice threshold.
    public var severity: SafetyScoreSeverity? { SafetyScoreSeverity(probability: probability) }

    /// Whole-percent text: "92%". Anything above zero but under 1% reads
    /// "<1%" so a small nonzero score is not shown as none.
    public var percentText: String {
        let percent = Int((probability * 100).rounded())
        if percent == 0, probability > 0 { return "<1%" }
        return "\(percent)%"
    }
}

extension SafetyScores {
    /// Rows in the fixed `SafetyCategory` order, so every message's
    /// breakdown reads the same way regardless of wire order.
    public var rows: [SafetyScoreRow] {
        SafetyCategory.allCases.compactMap { category in
            score(for: category).map {
                SafetyScoreRow(category: category, probability: $0.probability.value, taxonomyVersion: $0.taxonomyVersion)
            }
        }
    }

    /// The community-signal rows (currently the question estimate).
    public var signalRows: [SafetyScoreRow] { rows.filter { !$0.category.isSafety } }

    /// The content-safety rows.
    public var safetyRows: [SafetyScoreRow] { rows.filter(\.category.isSafety) }

    /// The severity of the highest content-safety score, or nil when no
    /// safety category reaches the notice threshold. The question signal
    /// never counts: a near-certain question is not a warning.
    public var severity: SafetyScoreSeverity? {
        safetyRows.compactMap(\.severity).max()
    }

    /// Rows at or above the notice threshold, in category order. What the
    /// breakdown shows; everything below the threshold stays hidden.
    public var notableRows: [SafetyScoreRow] { rows.filter { $0.severity != nil } }

    public var notableSignalRows: [SafetyScoreRow] { notableRows.filter { !$0.category.isSafety } }

    public var notableSafetyRows: [SafetyScoreRow] { notableRows.filter(\.category.isSafety) }

    /// "Question 92%, Hate speech 3%, …" for VoiceOver.
    public var spokenSummary: String {
        rows.map { "\($0.title) \($0.percentText)" }.joined(separator: ", ")
    }
}
