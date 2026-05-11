//! Shared Message Archive Management (MAM) primitives and helpers.
//!
//! These types and builders are safe to share across server and client code.

mod query;
mod response;
mod stanza_id_filter;
#[cfg(test)]
mod tests;
mod types;

pub use query::{build_query_form_iq, is_mam_query, is_mam_query_form_request, parse_mam_query};
pub use response::{build_fin_iq, build_result_messages, message_type_wire_str};
pub use stanza_id_filter::{
    MamFilterStanzaId, MAX_FILTER_STANZA_IDS, MAX_FILTER_STANZA_ID_LEN, STANZA_ID_FILTER_FIELD,
};
pub use types::{
    ArchivedMention, ArchivedMessage, ArchivedModeration, ArchivedReactionSet, ArchivedReference,
    ArchivedReply, ArchivedRetraction, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone,
    MamQuery, MamResult, RichMessageId, RichText, ThreadId,
};

/// MAM XML namespace (XEP-0313 v2).
pub const MAM_NS: &str = "urn:xmpp:mam:2";

/// Waddle MAM thread filter namespace.
///
/// XEP-0313 permits extension data form fields, but `{urn:xmpp:mam:2}thread`
/// is not a standard MAM field. Keep Waddle-specific filtering in a Waddle
/// namespace so official MAM semantics stay conformant.
pub const WADDLE_MAM_THREAD_NS: &str = "urn:waddle:mam-thread:0";
pub const WADDLE_MAM_THREAD_FIELD: &str = "{urn:waddle:mam-thread:0}thread";

/// Full Text Search in MAM namespace (XEP-0431).
pub const FULLTEXT_MAM_NS: &str = "urn:xmpp:fulltext:0";
pub const FULLTEXT_MAM_FIELD: &str = "{urn:xmpp:fulltext:0}fulltext";

/// Result Set Management namespace (XEP-0059).
pub const RSM_NS: &str = "http://jabber.org/protocol/rsm";

/// Data Forms namespace.
pub const DATA_FORMS_NS: &str = "jabber:x:data";

/// Stanza ID namespace (XEP-0359).
pub const STANZA_ID_NS: &str = "urn:xmpp:sid:0";

/// Forward namespace (XEP-0297).
pub const FORWARD_NS: &str = "urn:xmpp:forward:0";

/// Delay namespace (XEP-0203).
pub const DELAY_NS: &str = "urn:xmpp:delay";

const CLIENT_NS: &str = "jabber:client";
const REPLY_NS: &str = "urn:xmpp:reply:0";
const MESSAGE_CORRECT_NS: &str = "urn:xmpp:message-correct:0";
const MESSAGE_RETRACT_NS: &str = "urn:xmpp:message-retract:1";
const MESSAGE_MODERATE_NS: &str = "urn:xmpp:message-moderate:1";
const REACTIONS_NS: &str = "urn:xmpp:reactions:0";
const REFERENCE_NS: &str = "urn:xmpp:reference:0";
const MENTIONS_NS: &str = "urn:xmpp:mentions:0";
const XDATA_VALIDATE_NS: &str = "http://jabber.org/protocol/xdata-validate";
