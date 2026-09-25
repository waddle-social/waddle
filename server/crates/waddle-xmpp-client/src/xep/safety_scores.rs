//! Waddle safety-scores fastening (`urn:waddle:safety-scores:1`).
//!
//! The server judges an already-delivered message asynchronously and
//! attaches the per-category probabilities to it with XEP-0422 Message
//! Fastening:
//!
//! ```xml
//! <message from='room@muc.example' type='groupchat'>
//!   <apply-to xmlns='urn:xmpp:fasten:0' id='TARGET_ID'>
//!     <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='typesafe/jev-1.13-20260917'>
//!       <score category='is_question' probability='0.92' taxonomy-version='is-question-v1'/>
//!       <score category='safety:hate_speech' probability='0.03' taxonomy-version='safety-hate-speech-v1'/>
//!     </safety-scores>
//!   </apply-to>
//! </message>
//! ```
//!
//! No XEP defines a judgment-score payload, so the payload is a Waddle
//! namespace; the envelope is XEP-0422. XEP-0422 §Business Rules leaves
//! "who may fasten this" to the payload spec: this parser is
//! sender-agnostic, and consumers MUST gate on the sender (the room bare
//! JID for groupchat) before trusting the scores.
//!
//! Forward compatibility: the category set is expected to grow (the server
//! adds a judgment with one table entry), so unknown categories and
//! individually malformed `<score/>` elements are skipped rather than
//! failing the whole payload.

use minidom::Element;

/// XEP-0422 Message Fastening namespace.
pub const NS_FASTEN: &str = "urn:xmpp:fasten:0";
/// Waddle safety-scores fastening payload namespace.
pub const NS_WADDLE_SAFETY_SCORES: &str = "urn:waddle:safety-scores:1";

const APPLY_TO: &str = "apply-to";
const SAFETY_SCORES: &str = "safety-scores";
const SCORE: &str = "score";

/// A judgment category this client understands. Wire tokens are the
/// server's `message_judgments.judgment_name` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JudgmentCategory {
    IsQuestion,
    HateSpeech,
    Explicit,
    Harassment,
    Violence,
    SelfHarm,
}

impl JudgmentCategory {
    /// Every known category, in canonical display order.
    pub const ALL: [JudgmentCategory; 6] = [
        JudgmentCategory::IsQuestion,
        JudgmentCategory::HateSpeech,
        JudgmentCategory::Explicit,
        JudgmentCategory::Harassment,
        JudgmentCategory::Violence,
        JudgmentCategory::SelfHarm,
    ];

    /// Canonical wire token (`category` attribute value).
    pub fn as_wire(self) -> &'static str {
        match self {
            JudgmentCategory::IsQuestion => "is_question",
            JudgmentCategory::HateSpeech => "safety:hate_speech",
            JudgmentCategory::Explicit => "safety:explicit",
            JudgmentCategory::Harassment => "safety:harassment",
            JudgmentCategory::Violence => "safety:violence",
            JudgmentCategory::SelfHarm => "safety:self_harm",
        }
    }

    /// Parse a wire token; `None` for a category this client does not know.
    pub fn from_wire(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_wire() == token)
    }
}

/// A finite probability in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Probability(f64);

impl Probability {
    pub fn parse(value: &str) -> Result<Self, SafetyScoresParseError> {
        let parsed = value
            .trim()
            .parse::<f64>()
            .map_err(|_| SafetyScoresParseError::InvalidProbability)?;
        if parsed.is_finite() && (0.0..=1.0).contains(&parsed) {
            Ok(Self(parsed))
        } else {
            Err(SafetyScoresParseError::InvalidProbability)
        }
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

/// Opaque, non-empty identifier of the model that produced a batch
/// (`model-version`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVersion(String);

impl ModelVersion {
    pub fn parse(value: &str) -> Result<Self, SafetyScoresParseError> {
        non_empty(value)
            .map(Self)
            .ok_or(SafetyScoresParseError::MissingAttribute("model-version"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque, non-empty identifier of a category's wording
/// (`taxonomy-version`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxonomyVersion(String);

impl TaxonomyVersion {
    pub fn parse(value: &str) -> Result<Self, SafetyScoresParseError> {
        non_empty(value)
            .map(Self)
            .ok_or(SafetyScoresParseError::MissingAttribute("taxonomy-version"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScore {
    pub category: JudgmentCategory,
    pub probability: Probability,
    pub taxonomy_version: TaxonomyVersion,
}

/// One judgment batch: every score was produced by one model call.
#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScores {
    pub model_version: ModelVersion,
    /// Known categories only; the first occurrence of a category wins and
    /// wire order is preserved.
    pub scores: Vec<SafetyScore>,
}

/// XEP-0422 semantics of one safety-scores fastening.
#[derive(Debug, Clone, PartialEq)]
pub enum SafetyScoresUpdate {
    /// §Replacing fastenings: replaces every earlier safety-scores
    /// fastening from the same sender on the target.
    Replace(SafetyScores),
    /// §Removing fastenings: `<apply-to clear='true'>`.
    Clear,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScoresFastening {
    /// XEP-0422 `apply-to@id`: the XEP-0359 id of the judged message.
    pub target_id: String,
    pub update: SafetyScoresUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SafetyScoresParseError {
    #[error("element is not a safety-scores fastening")]
    NotSafetyScores,
    #[error("missing or empty attribute `{0}`")]
    MissingAttribute(&'static str),
    #[error("probability is not a finite number in 0..=1")]
    InvalidProbability,
    #[error("unknown judgment category")]
    UnknownCategory,
}

/// Extract the safety-scores fastening from a `<message/>`, if any.
///
/// `None` when the message has no XEP-0422 `<apply-to/>`, the apply-to is
/// an encryption `shell='true'` placeholder, it carries a different
/// fastening type, or the payload is unusable.
pub fn parse_safety_scores_fastening_child(message: &Element) -> Option<SafetyScoresFastening> {
    // XEP-0422 §Business Rules: a message carries a single apply-to.
    let apply_to = message.get_child(APPLY_TO, NS_FASTEN)?;
    parse_safety_scores_fastening(apply_to).ok()
}

/// Parse one XEP-0422 `<apply-to/>` as a safety-scores fastening.
pub fn parse_safety_scores_fastening(
    apply_to: &Element,
) -> Result<SafetyScoresFastening, SafetyScoresParseError> {
    if apply_to.name() != APPLY_TO || apply_to.ns() != NS_FASTEN {
        return Err(SafetyScoresParseError::NotSafetyScores);
    }
    // §Interaction with stanza encryption: a shell carries no content.
    if is_xs_true(apply_to.attr("shell")) {
        return Err(SafetyScoresParseError::NotSafetyScores);
    }
    let payload = apply_to
        .get_child(SAFETY_SCORES, NS_WADDLE_SAFETY_SCORES)
        .ok_or(SafetyScoresParseError::NotSafetyScores)?;
    let target_id = apply_to
        .attr("id")
        .and_then(non_empty)
        .ok_or(SafetyScoresParseError::MissingAttribute("id"))?;
    let update = if is_xs_true(apply_to.attr("clear")) {
        SafetyScoresUpdate::Clear
    } else {
        SafetyScoresUpdate::Replace(parse_safety_scores(payload)?)
    };
    Ok(SafetyScoresFastening { target_id, update })
}

/// Parse a `<safety-scores/>` payload, skipping unusable `<score/>`s.
pub fn parse_safety_scores(element: &Element) -> Result<SafetyScores, SafetyScoresParseError> {
    if element.name() != SAFETY_SCORES || element.ns() != NS_WADDLE_SAFETY_SCORES {
        return Err(SafetyScoresParseError::NotSafetyScores);
    }
    let model_version = ModelVersion::parse(element.attr("model-version").unwrap_or_default())?;
    let scores = element
        .children()
        .filter(|child| child.name() == SCORE && child.ns() == NS_WADDLE_SAFETY_SCORES)
        .filter_map(|child| parse_score(child).ok())
        .fold(Vec::<SafetyScore>::new(), |mut kept, score| {
            if !kept.iter().any(|seen| seen.category == score.category) {
                kept.push(score);
            }
            kept
        });
    Ok(SafetyScores {
        model_version,
        scores,
    })
}

fn parse_score(element: &Element) -> Result<SafetyScore, SafetyScoresParseError> {
    let category = JudgmentCategory::from_wire(
        element
            .attr("category")
            .ok_or(SafetyScoresParseError::MissingAttribute("category"))?,
    )
    .ok_or(SafetyScoresParseError::UnknownCategory)?;
    let probability = Probability::parse(
        element
            .attr("probability")
            .ok_or(SafetyScoresParseError::MissingAttribute("probability"))?,
    )?;
    let taxonomy_version =
        TaxonomyVersion::parse(element.attr("taxonomy-version").unwrap_or_default())?;
    Ok(SafetyScore {
        category,
        probability,
        taxonomy_version,
    })
}

/// XML Schema `xs:boolean` truth (`true` / `1`).
fn is_xs_true(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("true" | "1"))
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}
