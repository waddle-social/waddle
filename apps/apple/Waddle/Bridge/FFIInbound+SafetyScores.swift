import Foundation
import WaddleKit

extension FFIInbound {
    /// XEP-0422 `urn:waddle:safety-scores:1`. The core already dropped
    /// unknown categories and out-of-range probabilities; the checks here
    /// only keep an invalid record from ever becoming a typed value.
    static func safetyScoresFastening(_ fastening: WaddleSafetyScoresFastening) -> WireMessage.SafetyScoresFastening? {
        guard !fastening.targetOriginId.isEmpty,
              !fastening.targetStanzaId.isEmpty,
              !fastening.sourceRevisionId.isEmpty,
              let targetStanzaBy = bareJID(fastening.targetStanzaBy),
              let parsed = safetyScores(fastening.scores) else { return nil }
        return WireMessage.SafetyScoresFastening(
            targetOriginID: fastening.targetOriginId,
            targetStanzaID: fastening.targetStanzaId,
            targetStanzaBy: targetStanzaBy,
            sourceRevisionID: fastening.sourceRevisionId,
            scores: parsed
        )
    }

    static func safetyScores(_ scores: WaddleSafetyScores) -> SafetyScores? {
        guard !scores.modelVersion.isEmpty else { return nil }
        return SafetyScores(modelVersion: scores.modelVersion, scores: scores.scores.compactMap(safetyScore))
    }

    static func safetyScore(_ score: WaddleSafetyScore) -> SafetyScore? {
        guard let probability = SafetyProbability(score.probability), !score.taxonomyVersion.isEmpty else {
            return nil
        }
        return SafetyScore(
            category: safetyCategory(score.category),
            probability: probability,
            taxonomyVersion: score.taxonomyVersion
        )
    }

    static func safetyCategory(_ category: WaddleSafetyCategory) -> SafetyCategory {
        switch category {
        case .isQuestion: return .isQuestion
        case .hateSpeech: return .hateSpeech
        case .explicit: return .explicit
        case .harassment: return .harassment
        case .violence: return .violence
        case .selfHarm: return .selfHarm
        case .spam: return .spam
        case .scam: return .scam
        }
    }
}
