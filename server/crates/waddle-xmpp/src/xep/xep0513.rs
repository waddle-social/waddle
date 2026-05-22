//! XEP-0513: Explicit Mentions.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::message::Message;

use super::xep0372::{extract_references_from_message, Reference};

/// Namespace for XEP-0513 Explicit Mentions.
pub const NS_EXPLICIT_MENTIONS: &str = "urn:xmpp:mentions:0";

/// XEP-0513 channel-wide mention URI.
pub const CHANNEL_MENTION: &str = "urn:xmpp:mentions:0#channel";

/// XEP-0513 §301 example value for the `mentions#count` form field.
///
/// XEP-0513 §304: "Receiving entities SHOULD ignore all mentions if
/// the message contains more mentions than the threshold specified by
/// `mentions#count`." Used as the server-internal default until the
/// per-room override IQ (XEP-0513 §295) lands in a follow-up slice.
pub const DEFAULT_MENTIONS_COUNT: u32 = 5;

/// Returns `true` when `mention` carries an actual XEP-0513 §3
/// mention TARGET — i.e. it identifies someone or something to
/// notify. Per XEP-0513 §3 the targeting axes are exactly:
///
///   - `jid` (the bare JID of the mentioned entity),
///   - `occupantid` (the XEP-0421 stable occupant identifier),
///   - `mentions` (a URI identifying a special group, e.g.
///     `urn:xmpp:mentions:0#channel`).
///
/// [`parse_mention_element`] is deliberately MORE permissive: it
/// also accepts `<mention/>` elements whose only payload is a
/// `uri='…'` attribute or a `<active/>` / `<noping/>` child. Per
/// §3, `uri` is documented as a SCOPE qualifier for channel
/// mentions (or a hint for clients to dereference), NOT as a
/// targeting axis; `<active/>` and `<noping/>` are §"Active
/// Mentions" / §"No Ping" qualifiers that modify a target but
/// don't introduce one. The parser/counter asymmetry is
/// intentional: the parser preserves round-trip wire shape, while
/// §304's count gate operates on logical TARGETS only. Counting
/// non-targeting `<mention/>` elements would let an attacker pad
/// the §304 cap from a message that names nobody (wire-shape
/// review on PR #741).
///
/// Anchor-only (`begin`/`end`-only) `<mention/>` elements are
/// rejected by [`parse_mention_element`] outright — those don't
/// reach this predicate.
///
/// Empty-string `occupantid=''` and `mentions=''` are also
/// excluded: the parser preserves them as `Some("".to_string())`
/// but an empty occupant-id targets no XEP-0421 occupant and an
/// empty `mentions=` URI identifies no group per §3.
fn is_mention_target(mention: &ExplicitMention) -> bool {
    mention.jid.is_some()
        || mention
            .occupant_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
        || mention
            .mentions
            .as_deref()
            .is_some_and(|uri| !uri.is_empty())
}

/// Returns `true` when an XEP-0372 `<reference/>` carries a real
/// mention TARGET — `type='mention'` AND a parseable XMPP bare JID
/// in the `uri`. Per XEP-0372 §"Reference type 'mention'" the URI
/// MUST be the XMPP URI of the mentioned entity; counting
/// `<reference type='mention' uri=''/>` or
/// `<reference type='mention' uri='https://attacker.example/'/>`
/// would let an attacker pad the §304 count from a message that
/// targets nobody — the same padding vector closed for XEP-0513
/// by [`is_mention_target`] (PR #741 wire-shape review).
fn is_reference_mention_target(reference: &Reference) -> bool {
    reference.is_mention() && reference.bare_jid().is_some()
}

/// Counts the mention TARGETS on `message`. Includes:
///
/// - every parsed XEP-0513 `<mention/>` payload whose attributes
///   actually identify a target (jid / occupantid / mentions URI) —
///   anchor-only and `<active/>` / `<noping/>` -only elements MUST
///   NOT contribute, per XEP-0513 §3's definition of a mention
///   target;
/// - every XEP-0372 `<reference type='mention'/>` element — XEP-0513
///   §304's "more mentions than the threshold" cap defensively
///   extends to the XEP-0372 fallback path, otherwise an attacker
///   bypasses the cap by encoding the spam via XEP-0372 references
///   instead of XEP-0513 mentions (XEP-0513 §526 authorises
///   server-internal filtering "according to their own rules").
///
/// The XEP-0513 / XEP-0372 sides are NOT deduplicated — a well-
/// behaved dual-encoded message (per XEP-0513 §3 example showing
/// both `<mention jid='X'/>` and `<reference uri='xmpp:X'/>`) is
/// counted as two targets. This is a documented anti-abuse choice:
/// the cap deliberately over-counts to avoid bypass via mixed
/// encoding, and the §301 default of `5` leaves comfortable
/// headroom for dual-encoded messages naming up to ~2 distinct
/// targets. Per-XEP precise dedup is a follow-up if reports
/// indicate false-positives in practice.
///
/// The result is clamped to `u32::MAX`; in practice a single message
/// would never legitimately approach that count.
pub fn mention_target_count(explicit_mentions: &[ExplicitMention], message: &Message) -> u32 {
    let references = extract_references_from_message(message);
    mention_target_count_from_parts(explicit_mentions, &references)
}

/// Parts-based version of [`mention_target_count`]. Use this when
/// the caller has already parsed both XEP-0513 mentions and XEP-0372
/// references — typically on a hot path where the same message is
/// inspected multiple times (e.g. T0 candidate emission across many
/// recipients) — so the payload sweep happens once per message
/// instead of once per inspection (XEP review on PR #741).
pub fn mention_target_count_from_parts(
    explicit_mentions: &[ExplicitMention],
    references: &[Reference],
) -> u32 {
    let xep0513 = explicit_mentions
        .iter()
        .filter(|mention| is_mention_target(mention))
        .count();
    let xep0372 = references
        .iter()
        .filter(|reference| is_reference_mention_target(reference))
        .count();
    u32::try_from(xep0513.saturating_add(xep0372)).unwrap_or(u32::MAX)
}

/// Returns `true` when the mention payloads on `message` exceed the
/// configured `threshold` (XEP-0513 §304). Callers handle the
/// SHOULD-ignore-all-mentions consequence themselves.
pub fn mentions_exceed_threshold(
    explicit_mentions: &[ExplicitMention],
    message: &Message,
    threshold: u32,
) -> bool {
    mention_target_count(explicit_mentions, message) > threshold
}

/// Parts-based version of [`mentions_exceed_threshold`].
pub fn mentions_exceed_threshold_from_parts(
    explicit_mentions: &[ExplicitMention],
    references: &[Reference],
    threshold: u32,
) -> bool {
    mention_target_count_from_parts(explicit_mentions, references) > threshold
}

/// A single top-level `<mention/>` payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExplicitMention {
    pub begin: Option<u32>,
    pub end: Option<u32>,
    pub jid: Option<BareJid>,
    pub occupant_id: Option<String>,
    pub mentions: Option<String>,
    pub uri: Option<String>,
    pub active: bool,
    pub noping: bool,
}

impl ExplicitMention {
    pub fn jid(jid: BareJid) -> Self {
        Self {
            jid: Some(jid),
            ..Self::default()
        }
    }

    pub fn occupant_id(occupant_id: impl Into<String>) -> Self {
        Self {
            occupant_id: Some(occupant_id.into()),
            ..Self::default()
        }
    }

    pub fn channel() -> Self {
        Self {
            mentions: Some(CHANNEL_MENTION.to_string()),
            ..Self::default()
        }
    }

    pub fn active_channel() -> Self {
        Self {
            active: true,
            ..Self::channel()
        }
    }

    pub fn is_channel(&self) -> bool {
        self.mentions.as_deref() == Some(CHANNEL_MENTION)
    }

    pub fn is_individual(&self) -> bool {
        self.jid.is_some() || self.occupant_id.is_some()
    }
}

/// A set of explicit mentions in a message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExplicitMentions {
    pub mentions: Vec<ExplicitMention>,
}

impl ExplicitMentions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mention(mut self, mention: ExplicitMention) -> Self {
        self.mentions.push(mention);
        self
    }

    pub fn with_channel(self) -> Self {
        self.with_mention(ExplicitMention::channel())
    }

    pub fn with_active_channel(self) -> Self {
        self.with_mention(ExplicitMention::active_channel())
    }

    pub fn has_channel(&self) -> bool {
        self.mentions.iter().any(ExplicitMention::is_channel)
    }

    pub fn mentions_jid(&self, jid: &BareJid) -> bool {
        self.mentions.iter().any(|mention| {
            mention
                .jid
                .as_ref()
                .is_some_and(|mentioned| mentioned == jid)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.mentions.is_empty()
    }
}

/// Trait for types that can carry explicit mentions.
pub trait ExplicitMentionCarrier {
    fn explicit_mentions(&self) -> Option<ExplicitMentions>;

    fn has_explicit_mentions(&self) -> bool {
        self.explicit_mentions().is_some_and(|m| !m.is_empty())
    }
}

impl ExplicitMentionCarrier for Message {
    fn explicit_mentions(&self) -> Option<ExplicitMentions> {
        extract_explicit_mentions(self)
    }
}

pub fn is_mention_element(elem: &Element) -> bool {
    elem.is("mention", NS_EXPLICIT_MENTIONS)
}

pub fn has_explicit_mentions(msg: &Message) -> bool {
    msg.payloads.iter().any(is_mention_element)
}

pub fn extract_explicit_mentions(msg: &Message) -> Option<ExplicitMentions> {
    let mentions: Vec<ExplicitMention> = msg
        .payloads
        .iter()
        .filter(|elem| is_mention_element(elem))
        .filter_map(parse_mention_element)
        .collect();

    if mentions.is_empty() {
        None
    } else {
        Some(ExplicitMentions { mentions })
    }
}

pub fn parse_mention_element(elem: &Element) -> Option<ExplicitMention> {
    let begin = elem.attr("begin").and_then(|value| value.parse().ok());
    let end = elem.attr("end").and_then(|value| value.parse().ok());
    let jid = elem.attr("jid").and_then(|value| value.parse().ok());
    let occupant_id = elem.attr("occupantid").map(str::to_string);
    let mentions = elem.attr("mentions").map(str::to_string);
    let uri = elem.attr("uri").map(str::to_string);
    let active = elem.get_child("active", NS_EXPLICIT_MENTIONS).is_some();
    let noping = elem.get_child("noping", NS_EXPLICIT_MENTIONS).is_some();

    if jid.is_none()
        && occupant_id.is_none()
        && mentions.is_none()
        && uri.is_none()
        && !active
        && !noping
    {
        return None;
    }

    Some(ExplicitMention {
        begin,
        end,
        jid,
        occupant_id,
        mentions,
        uri,
        active,
        noping,
    })
}

pub fn build_mention_element(mention: &ExplicitMention) -> Element {
    let mut elem = Element::builder("mention", NS_EXPLICIT_MENTIONS).build();

    if let Some(begin) = mention.begin {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("begin").to_owned(),
            begin.to_string(),
        );
    }
    if let Some(end) = mention.end {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("end").to_owned(),
            end.to_string(),
        );
    }
    if let Some(jid) = &mention.jid {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("jid").to_owned(),
            jid.to_string(),
        );
    }
    if let Some(occupant_id) = &mention.occupant_id {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("occupantid").to_owned(),
            occupant_id,
        );
    }
    if let Some(mentions) = &mention.mentions {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("mentions").to_owned(),
            mentions,
        );
    }
    if let Some(uri) = &mention.uri {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("uri").to_owned(),
            uri,
        );
    }
    if mention.active {
        elem.append_child(Element::builder("active", NS_EXPLICIT_MENTIONS).build());
    }
    if mention.noping {
        elem.append_child(Element::builder("noping", NS_EXPLICIT_MENTIONS).build());
    }

    elem
}

pub fn build_mentions_elements(mentions: &ExplicitMentions) -> Vec<Element> {
    mentions
        .mentions
        .iter()
        .map(build_mention_element)
        .collect()
}

pub fn set_explicit_mentions(msg: &mut Message, mentions: &ExplicitMentions) {
    strip_explicit_mentions(msg);
    msg.payloads.extend(build_mentions_elements(mentions));
}

pub fn strip_explicit_mentions(msg: &mut Message) {
    msg.payloads
        .retain(|elem| elem.ns() != NS_EXPLICIT_MENTIONS);
}

// ── §295 / §303: Permissions Query ──────────────────────────────────

/// XEP-0513 §303 form field `var` for the per-message mention-count
/// threshold. Always required when permissions are advertised.
pub const FIELD_MENTIONS_COUNT: &str = "mentions#count";

/// XEP-0513 §303 form field `var` for the individual-mention policy.
/// Always required when permissions are advertised.
pub const FIELD_MENTIONS_INDIVIDUAL: &str = "mentions#individual";

/// XEP-0513 §303 form field `var` for the channel-mention policy.
/// Present iff the room advertises `urn:xmpp:mentions:0#channel`.
pub const FIELD_MENTIONS_CHANNEL: &str = "mentions#channel";

/// XEP-0513 §303 policy enum for a `list-single` permissions field.
///
/// Three exhaustive values map directly to the spec's option labels.
/// Modelled as an enum (not a string) so call sites and callers across
/// the typed-payload boundary can match exhaustively rather than
/// shuffling `&str` literals (typed-payloads hard rule, CLAUDE.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionsPermission {
    /// Any room participant may use this mention type.
    Participants,
    /// Only moderators may use this mention type.
    Moderators,
    /// No participant may use this mention type.
    Nobody,
}

impl MentionsPermission {
    /// Wire value emitted in the data-form `<value/>` child and in
    /// `<option>` values.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Participants => "participants",
            Self::Moderators => "moderators",
            Self::Nobody => "none",
        }
    }

    /// Human-readable label emitted in `<option label='…'/>`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Participants => "Participants",
            Self::Moderators => "Moderators Only",
            Self::Nobody => "Nobody",
        }
    }
}

/// Typed view of a XEP-0513 §303 permissions form. Only the fields
/// Waddle currently advertises (`mentions#count`, `mentions#individual`,
/// `mentions#channel`) are modelled; the optional `#space` / `#server`
/// / `#associations` / `#hats` fields are deliberately omitted until
/// the matching disco feature is advertised (#525 — "Do not advertise
/// `#space`, `#server`, `#associations`, or `#hats` until recipient
/// resolution and permissions are implemented for them").
///
/// The `channel` field is `Option<…>` because §303 ties its presence
/// to the `…#channel` feature advert: "All other fields are OPTIONAL,
/// but they MUST be present if and only if the corresponding feature
/// is advertised in service discovery." A future room that doesn't
/// advertise channel mentions would carry `channel: None` and we'd
/// omit the field on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionsPermissions {
    /// `mentions#count` — XEP-0513 §304 threshold.
    pub count: u32,
    /// `mentions#individual` — gate for per-recipient mentions.
    pub individual: MentionsPermission,
    /// `mentions#channel` — gate for `urn:xmpp:mentions:0#channel`.
    /// `None` when the room does not advertise channel mentions; the
    /// form field is then omitted entirely.
    pub channel: Option<MentionsPermission>,
}

impl MentionsPermissions {
    /// Waddle's server-internal default policy. Mirrors the hardcoded
    /// gates enforced at T0 candidate classification (PR #738 slice 3a
    /// and #741 slice 3b): the threshold is the XEP-0513 §301 example
    /// value (`5`), individual mentions are open to all participants,
    /// and channel mentions are restricted to moderators.
    pub fn server_default() -> Self {
        Self {
            count: DEFAULT_MENTIONS_COUNT,
            individual: MentionsPermission::Participants,
            channel: Some(MentionsPermission::Moderators),
        }
    }
}

/// Returns `true` when `elem` is the root of a XEP-0513 §295
/// permissions query — a `<query xmlns='urn:xmpp:mentions:0'/>`.
pub fn is_mentions_permissions_query(elem: &Element) -> bool {
    elem.name() == "query" && elem.ns() == NS_EXPLICIT_MENTIONS
}

/// Build the §303 result payload — `<query xmlns='urn:xmpp:mentions:0'>`
/// wrapping a `<x xmlns='jabber:x:data' type='form'>` with the typed
/// `permissions`. The room's IQ handler wraps this in an `<iq
/// type='result'/>` envelope.
///
/// Field shape per §303:
///
/// - `FORM_TYPE` hidden = `urn:xmpp:mentions:0`,
/// - `mentions#count` text-single, required, value = `permissions.count`,
/// - `mentions#individual` list-single, required, value =
///   `permissions.individual`, options = participants/moderators/none,
/// - `mentions#channel` list-single, required, value =
///   `permissions.channel` (only when `permissions.channel.is_some()`).
pub fn build_mentions_permissions_query(permissions: &MentionsPermissions) -> Element {
    use crate::xep::xep0004::{DataForm, Field, FormType, IntoElement};

    let mut form = DataForm::new(FormType::Form)
        .with_title("Permissions for Explicit Mentions")
        .add_instructions(concat!(
            "Complete this form to inform entities about who can mention whom. ",
            "The count is the maximum number of mentions allowed per message. ",
            "For each mention type supported, the users allowed to use the type ",
            "may be set to all participants, moderators only, or nobody."
        ))
        .add_field(Field::form_type(NS_EXPLICIT_MENTIONS))
        .add_field(
            Field::text_single(FIELD_MENTIONS_COUNT, permissions.count.to_string())
                .with_label("How many mentions are allowed in a message?")
                .with_required(),
        );

    form = form.add_field(policy_field(
        FIELD_MENTIONS_INDIVIDUAL,
        "Who can mention individual users?",
        permissions.individual,
    ));

    if let Some(channel) = permissions.channel {
        form = form.add_field(policy_field(
            FIELD_MENTIONS_CHANNEL,
            "Who can mention rooms?",
            channel,
        ));
    }

    let mut query = Element::builder("query", NS_EXPLICIT_MENTIONS).build();
    query.append_child(form.into_element());
    query
}

/// Build a §303 `list-single` permissions field — the same shape
/// reused for `mentions#individual` / `mentions#channel` / (future)
/// `mentions#space` / `#server` / `#associations` / `#hats`. Single
/// helper keeps the option set and ordering in lockstep across every
/// permissions field; if a future XEP-0513 revision adds a fourth
/// policy value, only one constructor changes.
fn policy_field(
    var: &'static str,
    label: &'static str,
    value: MentionsPermission,
) -> crate::xep::xep0004::Field {
    use crate::xep::xep0004::{Field, FieldOption, FieldType};

    [
        MentionsPermission::Participants,
        MentionsPermission::Moderators,
        MentionsPermission::Nobody,
    ]
    .into_iter()
    .fold(
        Field::new(var, FieldType::ListSingle)
            .with_label(label)
            .with_required()
            .with_value(value.as_wire()),
        |field, option| field.add_option(FieldOption::with_label(option.label(), option.as_wire())),
    )
}

#[cfg(test)]
mod count_tests {
    use super::*;
    use crate::xep::xep0372::{add_reference, Reference};
    use xmpp_parsers::message::Message;

    fn empty_message() -> Message {
        Message::new(None::<jid::Jid>)
    }

    fn with_xep0513_mention(msg: &mut Message, mention: ExplicitMention) {
        msg.payloads.push(build_mention_element(&mention));
    }

    /// XEP-0513 `<mention/>` payloads with a real targeting axis
    /// (jid, occupantid, mentions URI) contribute one mention
    /// TARGET each. The parser is more permissive — it also
    /// accepts `uri`-only or modifier-only mentions per the §3
    /// round-trip contract — but the counter filters those out
    /// via [`is_mention_target`]; the cases that DO contribute are
    /// exactly the ones exercised by this test. Other tests pin
    /// the exclusion path (`*_ignores_*`).
    #[test]
    fn mention_target_count_counts_xep0513_mentions() {
        let mut msg = empty_message();
        with_xep0513_mention(
            &mut msg,
            ExplicitMention::jid("alice@example.com".parse().expect("alice bare")),
        );
        with_xep0513_mention(&mut msg, ExplicitMention::occupant_id("room-stable-bob"));
        with_xep0513_mention(&mut msg, ExplicitMention::channel());
        let mentions = extract_explicit_mentions(&msg)
            .map(|m| m.mentions)
            .unwrap_or_default();
        assert_eq!(mention_target_count(&mentions, &msg), 3);
    }

    /// XEP-0372 `<reference type='mention'/>` elements ALSO contribute
    /// to the per-message mention count — without this an attacker
    /// bypasses the XEP-0513 §304 cap by encoding spam as XEP-0372
    /// references instead.
    #[test]
    fn mention_target_count_counts_xep0372_mention_references() {
        let mut msg = empty_message();
        add_reference(&mut msg, &Reference::mention("xmpp:alice@example.com"));
        add_reference(&mut msg, &Reference::mention("xmpp:bob@example.com"));
        // XEP-0372 references with type='data' MUST NOT be counted —
        // they're file attachments, not mentions.
        add_reference(
            &mut msg,
            &Reference::data("https://files.example.com/cat.jpg"),
        );
        assert_eq!(mention_target_count(&[], &msg), 2);
    }

    /// Sum of both XEPs is reported.
    #[test]
    fn mention_target_count_sums_xep0513_and_xep0372() {
        let mut msg = empty_message();
        with_xep0513_mention(
            &mut msg,
            ExplicitMention::jid("alice@example.com".parse().expect("alice bare")),
        );
        add_reference(&mut msg, &Reference::mention("xmpp:bob@example.com"));
        let mentions = extract_explicit_mentions(&msg)
            .map(|m| m.mentions)
            .unwrap_or_default();
        assert_eq!(mention_target_count(&mentions, &msg), 2);
    }

    /// Zero mentions → zero count. The threshold check via
    /// `mentions_exceed_threshold` is `false` regardless of threshold.
    #[test]
    fn mention_target_count_is_zero_for_unmentioned_message() {
        let msg = empty_message();
        assert_eq!(mention_target_count(&[], &msg), 0);
        assert!(!mentions_exceed_threshold(&[], &msg, 0));
    }

    /// XEP-0513 §3 + XEP-0372 §"Reference type 'mention'": a mention
    /// reference whose `uri` doesn't resolve to an XMPP bare JID
    /// targets nobody and MUST NOT contribute to the §304 count.
    /// Closes the symmetric padding attack on the XEP-0372 fallback
    /// path (an attacker injecting 6× `<reference type='mention'
    /// uri=''/>` or `<reference type='mention' uri='https://x/'/>`
    /// would otherwise pad the cap from a no-target message —
    /// wire-shape review on PR #741).
    #[test]
    fn mention_target_count_ignores_xep0372_references_with_unparseable_uris() {
        let mut msg = empty_message();
        // 6 mention references whose URIs cannot resolve to an XMPP
        // bare JID. The first five fail the `xmpp:` scheme check;
        // the sixth fails JID parsing because spaces are invalid in
        // a JID localpart / domain.
        add_reference(&mut msg, &Reference::mention("xmpp:"));
        add_reference(&mut msg, &Reference::mention(""));
        add_reference(&mut msg, &Reference::mention("https://attacker.example/"));
        add_reference(&mut msg, &Reference::mention("mailto:foo@example.com"));
        add_reference(&mut msg, &Reference::mention("not-an-xmpp-uri"));
        add_reference(&mut msg, &Reference::mention("xmpp:bad jid with spaces"));
        assert_eq!(
            mention_target_count(&[], &msg),
            0,
            "XEP-0372 mention references whose URI doesn't parse to an \
             XMPP bare JID MUST NOT contribute to the §304 count"
        );
    }

    /// XEP-0513 §3 + parser tolerance: empty-string `occupantid=''`
    /// and `mentions=''` parse as `Some("".to_string())` but target
    /// nobody. They MUST NOT contribute to the §304 count.
    #[test]
    fn mention_target_count_ignores_empty_string_targets() {
        let mut msg = empty_message();
        let empty_occupant = ExplicitMention {
            occupant_id: Some(String::new()),
            ..ExplicitMention::default()
        };
        let empty_mentions = ExplicitMention {
            mentions: Some(String::new()),
            ..ExplicitMention::default()
        };
        with_xep0513_mention(&mut msg, empty_occupant);
        with_xep0513_mention(&mut msg, empty_mentions);
        let mentions = extract_explicit_mentions(&msg)
            .map(|m| m.mentions)
            .unwrap_or_default();
        // Both elements parsed (the parser accepts `Some("")`).
        assert_eq!(mentions.len(), 2);
        assert_eq!(
            mention_target_count(&mentions, &msg),
            0,
            "empty-string `occupantid=''` and `mentions=''` MUST NOT \
             contribute to the §304 count — they identify no target"
        );
    }

    /// XEP-0513 §3 defines a mention TARGET as a payload that
    /// identifies someone/something to notify — `jid`, `occupantid`,
    /// or `mentions` URI. A `<mention/>` carrying only `<noping/>`
    /// (or only `<active/>`, or only an anchor) targets nobody and
    /// MUST NOT contribute to the §304 count, otherwise an attacker
    /// pads the cap from messages that don't actually mention anyone
    /// (XEP-0513 review on PR #741).
    #[test]
    fn mention_target_count_ignores_anchor_only_and_payload_only_mentions() {
        let mut msg = empty_message();
        // 6 anchor-less `<noping/>`-only mentions — each is a parse
        // hit (parse_mention_element accepts the `noping` child as a
        // sufficient reason to construct an `ExplicitMention`) but
        // structurally targets nobody.
        for _ in 0..6 {
            let noping_only = ExplicitMention {
                noping: true,
                ..ExplicitMention::default()
            };
            msg.payloads.push(build_mention_element(&noping_only));
        }
        let mentions = extract_explicit_mentions(&msg)
            .map(|m| m.mentions)
            .unwrap_or_default();
        assert_eq!(mentions.len(), 6, "all six payloads must parse");
        assert_eq!(
            mention_target_count(&mentions, &msg),
            0,
            "anchor-less `<noping/>`-only mentions are NOT targets per \
             XEP-0513 §3 and MUST NOT contribute to the §304 count"
        );
        assert!(
            !mentions_exceed_threshold(&mentions, &msg, DEFAULT_MENTIONS_COUNT),
            "six target-less mentions MUST NOT trip the threshold"
        );
    }

    /// XEP-0513 §304 boundary: "more than the threshold" is strict.
    /// Equal count does NOT exceed.
    #[test]
    fn mentions_exceed_threshold_is_strict_inequality() {
        let mut msg = empty_message();
        for i in 0..5 {
            with_xep0513_mention(
                &mut msg,
                ExplicitMention::jid(
                    format!("user{i}@example.com")
                        .parse()
                        .expect("target bare jid"),
                ),
            );
        }
        let mentions = extract_explicit_mentions(&msg)
            .map(|m| m.mentions)
            .unwrap_or_default();
        assert_eq!(mention_target_count(&mentions, &msg), 5);
        assert!(
            !mentions_exceed_threshold(&mentions, &msg, 5),
            "count == threshold MUST NOT exceed; §304 says \"more than\""
        );
        assert!(
            mentions_exceed_threshold(&mentions, &msg, 4),
            "count > threshold MUST exceed"
        );
    }
}
