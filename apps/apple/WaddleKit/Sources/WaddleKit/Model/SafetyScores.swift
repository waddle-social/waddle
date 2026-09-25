import Foundation

/// A judgment category. Raw values are the server's
/// `message_judgments.judgment_name` tokens. Categories this client does
/// not know are dropped by the parser, never mapped here.
public enum SafetyCategory: String, CaseIterable, Hashable, Sendable {
    case isQuestion = "is_question"
    case hateSpeech = "safety:hate_speech"
    case explicit = "safety:explicit"
    case harassment = "safety:harassment"
    case violence = "safety:violence"
    case selfHarm = "safety:self_harm"
}

/// A probability in `0...1`.
public struct SafetyProbability: Hashable, Sendable, Comparable {
    public let value: Double

    /// Nil unless `value` is finite and within `0...1`.
    public init?(_ value: Double) {
        guard value.isFinite, (0.0 ... 1.0).contains(value) else { return nil }
        self.value = value
    }

    public static func < (lhs: SafetyProbability, rhs: SafetyProbability) -> Bool {
        lhs.value < rhs.value
    }
}

/// One category's score from a judgment batch.
public struct SafetyScore: Hashable, Sendable {
    public let category: SafetyCategory
    public let probability: SafetyProbability
    /// The wording the category was judged against (e.g.
    /// `safety-hate-speech-v1`); categories are revised independently.
    public let taxonomyVersion: String

    public init(category: SafetyCategory, probability: SafetyProbability, taxonomyVersion: String) {
        self.category = category
        self.probability = probability
        self.taxonomyVersion = taxonomyVersion
    }
}

/// Every score one model call produced for a message
/// (`urn:waddle:safety-scores:1`).
public struct SafetyScores: Hashable, Sendable {
    /// Batch-level model identifier (e.g. `typesafe/jev-1.13-20260917`).
    public let modelVersion: String
    /// At most one score per category.
    public let scores: [SafetyScore]

    /// Keeps the first score of each category.
    public init(modelVersion: String, scores: [SafetyScore]) {
        self.modelVersion = modelVersion
        var seen = Set<SafetyCategory>()
        self.scores = scores.filter { seen.insert($0.category).inserted }
    }

    public func score(for category: SafetyCategory) -> SafetyScore? {
        scores.first { $0.category == category }
    }
}

extension WireMessage {
    /// A XEP-0422 fastening that sets or clears the safety scores of the
    /// message named by `targetID`. The sender is unverified here; only
    /// the room itself may set scores on a room message.
    public struct SafetyScoresFastening: Hashable, Sendable {
        public enum Action: Hashable, Sendable {
            /// Replaces the sender's previous scores (XEP-0422 §Replacing).
            case apply(SafetyScores)
            /// Removes them (XEP-0422 §Removing, `clear='true'`).
            case clear
        }

        public let targetID: String
        public let action: Action

        public init(targetID: String, action: Action) {
            self.targetID = targetID
            self.action = action
        }

        /// The scores after this fastening: nil for a clear.
        public var scores: SafetyScores? {
            guard case let .apply(scores) = action else { return nil }
            return scores
        }
    }
}
