//! Waddle-specific MAM filter: filter an archive by XEP-0359 stanza-id.
//!
//! Form-field var is namespaced under `urn:waddle:mam-stanza-id:0`
//! per CLAUDE.md's "official XEP namespaces must conform exactly;
//! Waddle-specific semantics use `urn:waddle:*`" rule. No XEP defines
//! "filter MAM by XEP-0359 stanza-id" — XEP-0313 supports custom
//! data-form fields (§4.2), XEP-0068 defines Clark-notation field
//! var naming, and XEP-0359 only defines the stanza-id wire protocol
//! itself.
//!
//! The chat client uses this to materialize pinned-message bodies in
//! the pinned-panel right rail by sending a single batched MAM IQ
//! filtered to the room-canonical stanza-ids it cares about.

/// Form-field var for the Waddle-specific stanza-id MAM filter.
///
/// Uses the `urn:waddle:mam-stanza-id:0` namespace (not `urn:xmpp:sid:0`)
/// because this is a Waddle extension, not a shape defined by XEP-0359.
/// XEP-0068 Clark-notation field vars allow custom namespaces in
/// XEP-0313 data forms (§4.2).
///
/// Wire shape (text-multi):
///
/// ```xml
/// <field var="{urn:waddle:mam-stanza-id:0}stanza-id" type="text-multi">
///   <value>STANZA-ID-1</value>
///   <value>STANZA-ID-2</value>
/// </field>
/// ```
pub const STANZA_ID_FILTER_FIELD: &str = "{urn:waddle:mam-stanza-id:0}stanza-id";

/// Maximum length of a single stanza-id value, in bytes.
///
/// Matches `waddle_xmpp::xep::xep_waddle_pin::MAX_TARGET_STANZA_ID_LEN`
/// — the pin protocol already constrains the ids the chat client will
/// ever ask for, and reusing the same cap keeps validation symmetric.
pub const MAX_FILTER_STANZA_ID_LEN: usize = 256;

/// Maximum number of stanza-ids accepted in a single MAM query.
///
/// Matches the MAM storage hard cap (`min(query.max, 500)` in both the
/// in-memory and SQLx backends). Sending more stanza-ids than this cap
/// would result in a truncated result set with `is_complete=false` and
/// no client-side pagination, causing the excess pins to silently fall
/// back to "Original message no longer available." Cap the filter batch
/// here to make the constraint explicit and symmetric.
pub const MAX_FILTER_STANZA_IDS: usize = 500;

/// A validated stanza-id used as a value in the
/// `{urn:waddle:mam-stanza-id:0}stanza-id` MAM filter.
///
/// Distinct from `xep0359::StanzaId` because the filter carries only
/// the opaque id token (no `by` JID context), and from
/// `MamArchivedMessage.id` because those are archive primary keys.
///
/// The invariants — non-empty and at most [`MAX_FILTER_STANZA_ID_LEN`]
/// bytes — are checked once at the wire/data-form parse boundary in
/// [`crate::mam::parse_mam_query`]. Raw token access via [`as_str`]
/// is reserved for the SQL/storage boundary only; the typed value
/// flows through the rest of the routing and handler chain.
///
/// [`as_str`]: MamFilterStanzaId::as_str
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MamFilterStanzaId(String);

impl MamFilterStanzaId {
    /// Construct a validated stanza-id token. Returns `None` for an
    /// empty token or one exceeding [`MAX_FILTER_STANZA_ID_LEN`].
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_FILTER_STANZA_ID_LEN {
            return None;
        }
        Some(Self(value))
    }

    /// The raw token string. Use this only at the SQL/storage boundary
    /// and at the wire/data-form boundary — keep `MamFilterStanzaId`
    /// throughout the routing/handler chain.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for MamFilterStanzaId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
