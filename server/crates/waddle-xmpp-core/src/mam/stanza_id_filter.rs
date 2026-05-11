//! Waddle-specific MAM filter: filter an archive by XEP-0359 stanza-id.
//!
//! Form-field var is namespaced under `urn:waddle:mam:stanza-id:0`
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
/// Uses the `urn:waddle:mam:stanza-id:0` namespace (not `urn:xmpp:sid:0`)
/// because this is a Waddle extension, not a shape defined by XEP-0359.
/// XEP-0068 Clark-notation field vars allow custom namespaces in
/// XEP-0313 data forms (§4.2).
///
/// Wire shape (text-multi):
///
/// ```xml
/// <field var="{urn:waddle:mam:stanza-id:0}stanza-id" type="text-multi">
///   <value>STANZA-ID-1</value>
///   <value>STANZA-ID-2</value>
/// </field>
/// ```
pub const STANZA_ID_FILTER_FIELD: &str = "{urn:waddle:mam:stanza-id:0}stanza-id";

/// Maximum length of a single stanza-id value, in bytes.
///
/// Matches `waddle_xmpp::xep::xep_waddle_pin::MAX_TARGET_STANZA_ID_LEN`
/// — the pin protocol already constrains the ids the chat client will
/// ever ask for, and reusing the same cap keeps validation symmetric.
pub const MAX_FILTER_STANZA_ID_LEN: usize = 256;

/// Maximum number of stanza-ids accepted in a single MAM query. A
/// well-formed pinned panel asks for at most `MAX_PINNED_ENTRIES`
/// (1_000) ids in one batch; cap matches.
pub const MAX_FILTER_STANZA_IDS: usize = 1_000;
