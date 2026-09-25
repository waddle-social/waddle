//! Waddle per-message safety scores, fastened with XEP-0422.
//!
//! The server judges an already-archived message asynchronously (issue
//! #1831) and broadcasts the per-category probabilities to every
//! participant as a XEP-0422 fastening on the judged message:
//!
//! ```xml
//! <message from='room@muc.example' type='groupchat'>
//!   <apply-to xmlns='urn:xmpp:fasten:0' id='judged-stanza-id'>
//!     <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='…'>
//!       <score category='is_question' probability='0.92' taxonomy-version='is-question-v1'/>
//!     </safety-scores>
//!   </apply-to>
//! </message>
//! ```
//!
//! No XEP defines a moderation-score payload, so the payload lives in a
//! Waddle namespace; the wrapper is plain XEP-0422. Per XEP-0422 a new
//! fastening of the same qualified name replaces the sender's previous one,
//! and `clear='true'` removes it.

use minidom::Element;

/// XEP-0422 Message Fastening namespace.
pub const NS_FASTEN: &str = "urn:xmpp:fasten:0";
/// Waddle safety-scores fastening payload namespace.
pub const NS_WADDLE_SAFETY_SCORES: &str = "urn:waddle:safety-scores:1";

const APPLY_TO: &str = "apply-to";
const SAFETY_SCORES: &str = "safety-scores";
const SCORE: &str = "score";

/// One judgment category, keyed by the server's `judgment_name`. Unknown
/// wire categories are skipped (the server adds categories by table entry),
/// so this enum only lists what this client can label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SafetyScoreCategory {
    IsQuestion,
    HateSpeech,
    Explicit,
    Harassment,
    Violence,
    SelfHarm,
}

impl SafetyScoreCategory {
    /// Every known category, in display order.
    pub const ALL: [SafetyScoreCategory; 6] = [
        SafetyScoreCategory::IsQuestion,
        SafetyScoreCategory::HateSpeech,
        SafetyScoreCategory::Explicit,
        SafetyScoreCategory::Harassment,
        SafetyScoreCategory::Violence,
        SafetyScoreCategory::SelfHarm,
    ];

    /// Canonical wire token (the server's `judgment_name`).
    pub fn as_token(self) -> &'static str {
        match self {
            SafetyScoreCategory::IsQuestion => "is_question",
            SafetyScoreCategory::HateSpeech => "safety:hate_speech",
            SafetyScoreCategory::Explicit => "safety:explicit",
            SafetyScoreCategory::Harassment => "safety:harassment",
            SafetyScoreCategory::Violence => "safety:violence",
            SafetyScoreCategory::SelfHarm => "safety:self_harm",
        }
    }

    /// Parse a wire token; `None` for categories this client does not know.
    pub fn parse_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_token() == token)
    }
}

/// A probability in `0.0..=1.0` (finite). Constructed only via [`Self::parse`]
/// or [`Self::new`], so a value in hand is always in range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SafetyProbability(f64);

impl SafetyProbability {
    pub fn new(value: f64) -> Result<Self, SafetyScoresParseError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(SafetyScoresParseError::InvalidProbability)
        }
    }

    pub fn parse(value: &str) -> Result<Self, SafetyScoresParseError> {
        let parsed = value
            .trim()
            .parse::<f64>()
            .map_err(|_| SafetyScoresParseError::InvalidProbability)?;
        Self::new(parsed)
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

/// Opaque, non-empty version label (model or taxonomy revision).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyVersion(String);

impl SafetyVersion {
    pub fn parse(value: &str) -> Result<Self, SafetyScoresParseError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            Err(SafetyScoresParseError::EmptyVersion)
        } else {
            Ok(Self(trimmed.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScore {
    pub category: SafetyScoreCategory,
    pub probability: SafetyProbability,
    pub taxonomy_version: SafetyVersion,
}

/// One judgment batch: every score came from the same model call.
#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScores {
    pub model_version: SafetyVersion,
    /// Known categories only, wire order, first occurrence per category.
    pub scores: Vec<SafetyScore>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SafetyScoresPayload {
    /// Replace the target's scores with these (XEP-0422 replace).
    Scores(SafetyScores),
    /// XEP-0422 `clear='true'`: remove the target's scores.
    Cleared,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScoresFastening {
    /// XEP-0422 `<apply-to id='…'/>`: the judged message's XEP-0359 id.
    pub target_id: String,
    pub payload: SafetyScoresPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyScoresParseError {
    NotApplyTo,
    MissingTargetId,
    NotSafetyScores,
    MissingAttribute(&'static str),
    InvalidProbability,
    EmptyVersion,
}

/// Parse a `<apply-to xmlns='urn:xmpp:fasten:0'>` carrying safety scores.
///
/// Message-type agnostic: authority checks live in the caller (see
/// [`parse_room_safety_scores_child`]). Per XEP-0422 other children of the
/// `<apply-to/>` (e.g. `<external/>`) are ignored.
pub fn parse_safety_scores_fastening(
    apply_to: &Element,
) -> Result<SafetyScoresFastening, SafetyScoresParseError> {
    if !apply_to.is(APPLY_TO, NS_FASTEN) {
        return Err(SafetyScoresParseError::NotApplyTo);
    }
    let target_id = apply_to
        .attr("id")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or(SafetyScoresParseError::MissingTargetId)?
        .to_owned();
    let payload_element = apply_to
        .get_child(SAFETY_SCORES, NS_WADDLE_SAFETY_SCORES)
        .ok_or(SafetyScoresParseError::NotSafetyScores)?;
    let payload = if is_clear(apply_to) {
        SafetyScoresPayload::Cleared
    } else {
        SafetyScoresPayload::Scores(parse_safety_scores(payload_element)?)
    };
    Ok(SafetyScoresFastening { target_id, payload })
}

/// Extract the safety-scores fastening from a room broadcast.
///
/// Only the MUC service itself may attach scores: the stanza must be
/// `type='groupchat'` from the bare room JID (occupants always carry a
/// `/nick` resource), the same authenticity rule as XEP-0425 moderation.
/// Direct-message fastenings are deliberately not accepted yet: no trusted
/// 1:1 sender (and conversation routing) is defined for this payload.
pub fn parse_room_safety_scores_child(message: &Element) -> Option<SafetyScoresFastening> {
    let from = message.attr("from")?;
    if message.attr("type") != Some("groupchat") || from.contains('/') {
        return None;
    }
    let apply_to = message.get_child(APPLY_TO, NS_FASTEN)?;
    parse_safety_scores_fastening(apply_to).ok()
}

/// Parse a `<safety-scores/>` element. Unknown categories, malformed
/// scores, and repeated categories are skipped rather than failing the
/// whole batch; a missing `model-version` fails it.
pub fn parse_safety_scores(element: &Element) -> Result<SafetyScores, SafetyScoresParseError> {
    if !element.is(SAFETY_SCORES, NS_WADDLE_SAFETY_SCORES) {
        return Err(SafetyScoresParseError::NotSafetyScores);
    }
    let model_version = SafetyVersion::parse(required_attr(element, "model-version")?)?;
    let mut scores: Vec<SafetyScore> = Vec::new();
    for child in element.children() {
        let Some(score) = parse_score(child) else {
            continue;
        };
        if scores.iter().all(|seen| seen.category != score.category) {
            scores.push(score);
        }
    }
    Ok(SafetyScores {
        model_version,
        scores,
    })
}

fn parse_score(element: &Element) -> Option<SafetyScore> {
    if !element.is(SCORE, NS_WADDLE_SAFETY_SCORES) {
        return None;
    }
    let category = SafetyScoreCategory::parse_token(element.attr("category")?)?;
    let probability = SafetyProbability::parse(element.attr("probability")?).ok()?;
    let taxonomy_version = SafetyVersion::parse(element.attr("taxonomy-version")?).ok()?;
    Some(SafetyScore {
        category,
        probability,
        taxonomy_version,
    })
}

/// XEP-0422 `clear` is an XML boolean.
fn is_clear(apply_to: &Element) -> bool {
    matches!(apply_to.attr("clear"), Some("true" | "1"))
}

fn required_attr<'a>(
    element: &'a Element,
    name: &'static str,
) -> Result<&'a str, SafetyScoresParseError> {
    element
        .attr(name)
        .ok_or(SafetyScoresParseError::MissingAttribute(name))
}
