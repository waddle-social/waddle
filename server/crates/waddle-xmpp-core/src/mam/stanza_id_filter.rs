//! XEP-0359 §3 — filter a MAM archive by stanza-id.
//!
//! Exposes the form-field name and per-id length cap. The field is
//! advertised by `build_query_form_iq` and parsed in `parse_mam_query`.

/// Form-field var for XEP-0359 §3 stanza-id filter.
///
/// Wire shape (text-multi):
///
/// ```xml
/// <field var="{urn:xmpp:sid:0}stanza-id" type="text-multi">
///   <value>STANZA-ID-1</value>
///   <value>STANZA-ID-2</value>
/// </field>
/// ```
pub const STANZA_ID_FILTER_FIELD: &str = "{urn:xmpp:sid:0}stanza-id";

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
