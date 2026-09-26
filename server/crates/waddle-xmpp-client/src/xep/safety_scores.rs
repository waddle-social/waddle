//! Waddle per-message safety scores, fastened to an already-sent message
//! with XEP-0422 Message Fastening.
//!
//! ```xml
//! <apply-to xmlns='urn:xmpp:fasten:0' id='SOURCE_ORIGIN_ID'>
//!   <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='…'
//!       target-stanza-id='ROOM_ID' target-stanza-by='room@conference.example.org'
//!       source-revision-id='ROOM_REVISION_ID'>
//!     <score category='is_question' probability='0.92' taxonomy-version='is-question-v1'/>
//!   </safety-scores>
//! </apply-to>
//! ```
//!
//! No XEP defines a moderation-score payload, so the payload lives in a
//! `urn:waddle:*` namespace; only the `<apply-to/>` wrapper is XEP-0422.
//!
//! XEP-0422 rules this parser applies:
//! - a message carries at most one `<apply-to/>` (§Business Rules);
//! - an `<apply-to shell='true'/>` has no content and is ignored
//!   (§Interaction with stanza encryption);
//! - `clear='true'` is ignored by this result-only profile;
//! - unknown children of `<apply-to/>` are ignored.
//!
//! Waddle rules for the payload:
//! - one `<safety-scores/>` per `<apply-to/>`;
//! - `model-version` is required and batch-level;
//! - room stanza identity and accepted body revision are required;
//! - unknown `category` values are skipped, so the server can add a
//!   category without breaking older clients; a malformed or out-of-range
//!   `<score/>` is skipped as well; a repeated category keeps its first
//!   occurrence.
//!
//! [`parse_safety_scores_fastening`] itself does not decide who may send
//! scores — it is message-type agnostic, so a direct message parses with
//! the same shape as a room broadcast. [`parse_room_safety_scores_child`]
//! is the trusted entry point every consumer of this shared crate (the
//! messaging parser, and through it every FFI/wasm client) actually
//! calls: it applies the one authority rule this payload gets, so the
//! decision is made once, here, rather than separately — and possibly
//! inconsistently — by each client.

use minidom::Element;

use waddle_xmpp_core::xep0359::{OriginId, StanzaId};

/// `urn:xmpp:fasten:0` — XEP-0422 Message Fastening.
pub const NS_FASTEN: &str = "urn:xmpp:fasten:0";

/// `urn:waddle:safety-scores:1` — the fastened safety-score payload.
pub const NS_WADDLE_SAFETY_SCORES: &str = "urn:waddle:safety-scores:1";

/// A category in the room extension safety-score payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SafetyCategory {
    /// `is_question`: a community-enrichment signal, not a safety one.
    IsQuestion,
    /// `safety:hate_speech`
    HateSpeech,
    /// `safety:explicit`
    Explicit,
    /// `safety:harassment`
    Harassment,
    /// `safety:violence`
    Violence,
    /// `safety:self_harm`
    SelfHarm,
    /// `safety:spam`
    Spam,
    /// `safety:scam`
    Scam,
}

impl SafetyCategory {
    /// The wire token, as sent in `<score category='…'/>`.
    pub fn as_wire(self) -> &'static str {
        match self {
            SafetyCategory::IsQuestion => "is_question",
            SafetyCategory::HateSpeech => "safety:hate_speech",
            SafetyCategory::Explicit => "safety:explicit",
            SafetyCategory::Harassment => "safety:harassment",
            SafetyCategory::Violence => "safety:violence",
            SafetyCategory::SelfHarm => "safety:self_harm",
            SafetyCategory::Spam => "safety:spam",
            SafetyCategory::Scam => "safety:scam",
        }
    }

    /// Parses a wire token; `None` for a category this client does not know.
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "is_question" => Some(SafetyCategory::IsQuestion),
            "safety:hate_speech" => Some(SafetyCategory::HateSpeech),
            "safety:explicit" => Some(SafetyCategory::Explicit),
            "safety:harassment" => Some(SafetyCategory::Harassment),
            "safety:violence" => Some(SafetyCategory::Violence),
            "safety:self_harm" => Some(SafetyCategory::SelfHarm),
            "safety:spam" => Some(SafetyCategory::Spam),
            "safety:scam" => Some(SafetyCategory::Scam),
            _ => None,
        }
    }
}

/// A probability in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SafetyProbability(f64);

impl SafetyProbability {
    /// `None` unless `value` is finite and within `0.0..=1.0`.
    pub fn new(value: f64) -> Option<Self> {
        (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(Self(value))
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

/// Opaque, non-empty identifier of the model run that produced a batch
/// (for example `typesafe/jev-1.13-20260917`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JudgmentModelVersion(String);

impl JudgmentModelVersion {
    pub fn new(value: &str) -> Option<Self> {
        non_empty(value).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque, non-empty identifier of the wording a category was judged
/// against (for example `safety-hate-speech-v1`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaxonomyVersion(String);

impl TaxonomyVersion {
    pub fn new(value: &str) -> Option<Self> {
        non_empty(value).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One `<score/>`.
#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScore {
    pub category: SafetyCategory,
    pub probability: SafetyProbability,
    pub taxonomy_version: TaxonomyVersion,
}

/// One `<safety-scores/>` batch: every score one model call produced.
#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScores {
    pub model_version: JudgmentModelVersion,
    /// Known categories only, in document order, one per category.
    pub scores: Vec<SafetyScore>,
}

/// A parsed `<apply-to/>` carrying `<safety-scores/>`.
#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScoresFastening {
    /// XEP-0422 target: the original source's XEP-0359 origin-id.
    pub target_origin_id: OriginId,
    /// Room-assigned identity that disambiguates sender-chosen origin IDs.
    pub target_stanza_id: StanzaId,
    /// Room-assigned identity of the body revision that was judged.
    pub source_revision_id: StanzaId,
    pub scores: SafetyScores,
}

/// Extracts a safety-scores fastening from a `<message/>`, or `None` when
/// the message carries none or carries a malformed one.
pub fn parse_safety_scores_fastening(message: &Element) -> Option<SafetyScoresFastening> {
    let apply_to = single_apply_to(message)?;
    if is_true(apply_to.attr("clear")) {
        return None;
    }
    let target_origin_id = OriginId::new(non_empty(apply_to.attr("id")?)?);
    let mut payloads = apply_to
        .children()
        .filter(|child| child.is("safety-scores", NS_WADDLE_SAFETY_SCORES));
    let payload = payloads.next()?;
    if payloads.next().is_some() {
        return None;
    }
    let by: jid::Jid = payload.attr("target-stanza-by")?.parse().ok()?;
    if by.to_string().contains('/') {
        return None;
    }
    let target_stanza_id = StanzaId::new(non_empty(payload.attr("target-stanza-id")?)?, by.clone());
    let source_revision_id = StanzaId::new(non_empty(payload.attr("source-revision-id")?)?, by);
    let scores = parse_safety_scores(payload)?;
    Some(SafetyScoresFastening {
        target_origin_id,
        target_stanza_id,
        source_revision_id,
        scores,
    })
}

/// Extracts a safety-scores fastening from a room broadcast, the only
/// sender this payload trusts.
///
/// Only the MUC service itself may attach scores: the stanza must be
/// `type='groupchat'` from the bare room JID. An occupant's own message is
/// also reflected with `type='groupchat'`, but always carries a `/nick`
/// resource, so this is the same authenticity rule XEP-0425 moderation
/// uses to reject an occupant's own claim of moderation. A direct-message
/// fastening is not accepted: no trusted 1:1 sender is defined for this
/// payload.
pub fn parse_room_safety_scores_child(message: &Element) -> Option<SafetyScoresFastening> {
    let from: jid::BareJid = message.attr("from")?.parse().ok()?;
    if message.attr("type") != Some("groupchat") {
        return None;
    }
    let fastening = parse_safety_scores_fastening(message)?;
    (fastening.target_stanza_id.by == from).then_some(fastening)
}

/// The message's one non-shell `<apply-to/>`. Two or more is a XEP-0422
/// business-rule violation and yields nothing.
fn single_apply_to(message: &Element) -> Option<&Element> {
    let mut candidates = message
        .children()
        .filter(|child| child.is("apply-to", NS_FASTEN) && !is_true(child.attr("shell")));
    let apply_to = candidates.next()?;
    candidates.next().is_none().then_some(apply_to)
}

fn parse_safety_scores(payload: &Element) -> Option<SafetyScores> {
    let model_version = JudgmentModelVersion::new(payload.attr("model-version")?)?;
    let mut scores: Vec<SafetyScore> = Vec::new();
    for score in payload
        .children()
        .filter(|child| child.is("score", NS_WADDLE_SAFETY_SCORES))
        .filter_map(parse_score)
    {
        if scores.iter().all(|kept| kept.category != score.category) {
            scores.push(score);
        }
    }
    Some(SafetyScores {
        model_version,
        scores,
    })
}

fn parse_score(element: &Element) -> Option<SafetyScore> {
    let category = SafetyCategory::from_wire(element.attr("category")?)?;
    let probability = element
        .attr("probability")?
        .trim()
        .parse::<f64>()
        .ok()
        .and_then(SafetyProbability::new)?;
    let taxonomy_version = TaxonomyVersion::new(element.attr("taxonomy-version")?)?;
    Some(SafetyScore {
        category,
        probability,
        taxonomy_version,
    })
}

/// XML Schema boolean truth (`true` or `1`).
fn is_true(value: Option<&str>) -> bool {
    matches!(value, Some("true" | "1"))
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}
