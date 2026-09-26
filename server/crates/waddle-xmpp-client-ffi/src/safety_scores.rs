//! XEP-0422 `urn:waddle:safety-scores:1` fastening → UniFFI records.

use waddle_xmpp_client::xep::safety_scores::{
    SafetyCategory, SafetyScore, SafetyScores, SafetyScoresFastening,
};

use crate::{
    WaddleSafetyCategory, WaddleSafetyScore, WaddleSafetyScores, WaddleSafetyScoresFastening,
};

pub(crate) fn safety_scores_fastening_to_ffi(
    fastening: SafetyScoresFastening,
) -> WaddleSafetyScoresFastening {
    WaddleSafetyScoresFastening {
        target_origin_id: fastening.target_origin_id.id,
        target_stanza_id: fastening.target_stanza_id.id,
        target_stanza_by: fastening.target_stanza_id.by.to_string(),
        source_revision_id: fastening.source_revision_id.id,
        scores: safety_scores_to_ffi(fastening.scores),
    }
}

fn safety_scores_to_ffi(scores: SafetyScores) -> WaddleSafetyScores {
    WaddleSafetyScores {
        model_version: scores.model_version.as_str().to_owned(),
        scores: scores.scores.into_iter().map(safety_score_to_ffi).collect(),
    }
}

fn safety_score_to_ffi(score: SafetyScore) -> WaddleSafetyScore {
    WaddleSafetyScore {
        category: safety_category_to_ffi(score.category),
        probability: score.probability.value(),
        taxonomy_version: score.taxonomy_version.as_str().to_owned(),
    }
}

fn safety_category_to_ffi(category: SafetyCategory) -> WaddleSafetyCategory {
    match category {
        SafetyCategory::IsQuestion => WaddleSafetyCategory::IsQuestion,
        SafetyCategory::HateSpeech => WaddleSafetyCategory::HateSpeech,
        SafetyCategory::Explicit => WaddleSafetyCategory::Explicit,
        SafetyCategory::Harassment => WaddleSafetyCategory::Harassment,
        SafetyCategory::Violence => WaddleSafetyCategory::Violence,
        SafetyCategory::SelfHarm => WaddleSafetyCategory::SelfHarm,
    }
}
