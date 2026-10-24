//! Trusted room judgments, kept separate from user reactions. These annotate
//! an exact archived body revision; they never perform moderation actions.

use jid::BareJid;

/// Identity assigned to a room message, verified by the platform adapter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScoreTarget {
    pub origin_id: String,
    pub stanza_id: String,
    pub stanza_by: BareJid,
}

/// A finite probability, stored by its IEEE representation so row equality
/// remains exact without permitting NaN to enter timeline state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probability(u64);

impl Probability {
    pub fn try_new(value: f64) -> Option<Self> {
        (value.is_finite() && (0.0..=1.0).contains(&value)).then(|| {
            Self(if value == 0.0 {
                0.0_f64.to_bits()
            } else {
                value.to_bits()
            })
        })
    }

    pub fn value(self) -> f64 {
        f64::from_bits(self.0)
    }
}

/// Canonical display order, matching the web client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SafetyCategory {
    IsQuestion,
    HateSpeech,
    Explicit,
    Harassment,
    Violence,
    SelfHarm,
    Spam,
    Scam,
}

impl SafetyCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::IsQuestion => "Question",
            Self::HateSpeech => "Hate speech",
            Self::Explicit => "Explicit content",
            Self::Harassment => "Harassment",
            Self::Violence => "Violence",
            Self::SelfHarm => "Self-harm",
            Self::Spam => "Spam",
            Self::Scam => "Scam",
        }
    }

    pub fn threshold(self) -> f64 {
        if self == Self::IsQuestion {
            0.75
        } else {
            0.5
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyScore {
    pub category: SafetyCategory,
    pub probability: Probability,
    pub taxonomy_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyScores {
    pub model_version: String,
    pub scores: Vec<SafetyScore>,
}

impl SafetyScores {
    pub fn notable(&self) -> Vec<&SafetyScore> {
        let mut scores: Vec<_> = self
            .scores
            .iter()
            .filter(|score| score.probability.value() >= score.category.threshold())
            .collect();
        scores.sort_by_key(|score| score.category);
        scores
    }

    /// Strongest visible safety score; canonical order resolves ties.
    pub fn leading(&self) -> Option<&SafetyScore> {
        self.notable()
            .into_iter()
            .filter(|score| score.category != SafetyCategory::IsQuestion)
            .fold(None, |held, score| match held {
                Some(held) if held.probability.value() >= score.probability.value() => Some(held),
                _ => Some(score),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyScoresFastening {
    pub target: ScoreTarget,
    pub source_revision_id: String,
    pub scores: SafetyScores,
}

#[derive(Debug)]
pub(crate) struct StampedScores {
    pub at: i64,
    pub scores: SafetyScores,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probabilities_reject_invalid_values_and_normalize_zero() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.1] {
            assert!(Probability::try_new(value).is_none());
        }
        assert_eq!(Probability::try_new(-0.0), Probability::try_new(0.0));
        assert_eq!(Probability::try_new(1.0).unwrap().value(), 1.0);
    }

    #[test]
    fn question_is_independent_and_safety_ties_use_canonical_order() {
        let score = |category, probability| SafetyScore {
            category,
            probability: Probability::try_new(probability).unwrap(),
            taxonomy_version: "v1".into(),
        };
        let scores = SafetyScores {
            model_version: "model".into(),
            scores: vec![
                score(SafetyCategory::Scam, 0.8),
                score(SafetyCategory::IsQuestion, 0.75),
                score(SafetyCategory::HateSpeech, 0.8),
                score(SafetyCategory::Spam, 0.499),
            ],
        };
        assert_eq!(
            scores.leading().unwrap().category,
            SafetyCategory::HateSpeech
        );
        assert_eq!(scores.notable().len(), 3);
        let question_only = SafetyScores {
            model_version: "model".into(),
            scores: vec![score(SafetyCategory::IsQuestion, 0.99)],
        };
        assert!(question_only.leading().is_none());
        assert_eq!(question_only.notable().len(), 1);
    }
}
