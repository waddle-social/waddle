//! Inbound/outbound XMPP messaging, MUC, and presence operations.
//!
//! Exposes a [`parse`] function for runtime dispatch and typed outbound stanza
//! builders. With the `native` feature enabled, also exposes a convenience
//! trait for sending those stanzas through the native client handle.

mod builders;
mod call;
mod link_preview_lookup;
pub(crate) mod namespaces;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod native;
mod parsing;
mod presence;
#[cfg(test)]
mod tests;
mod types;

pub use builders::{
    build_chat_state_message, build_correction_message, build_displayed_message,
    build_file_sharing_element, build_in_call_reaction_message, build_moderation_message,
    build_muc_join_presence, build_outbound_message, build_pinned_chat_message,
    build_pinned_message, build_reaction_message, build_retraction_message,
    build_unpinned_chat_message, build_unpinned_message,
};
pub use call::{
    build_finish, build_finish_migrated, build_finish_with_options, build_finish_with_reason,
    build_muji_jingle_content, build_muji_session_initiate, build_muji_session_terminate,
    build_proceed, build_propose, build_reject, build_reject_with_options, build_retract,
    build_retract_with_options, build_ringing, build_session_accept, build_session_initiate,
    build_session_terminate, jingle_reason_from_wire_name, jingle_reason_wire_name,
    parse_call_event, parse_jingle_iq, parse_jmi_message, wrap_jmi_message, CallEventKind,
    CallMedia, InboundCallEvent, LiveKitJoin,
};
pub use link_preview_lookup::{
    build_link_preview_lookup_iq, eligible_preview_url, parse_link_preview_lookup_response,
    LinkPreviewLookup, LinkPreviewLookupReady, MAX_LINK_PREVIEW_TOKEN_BYTES,
};
pub use namespaces::{
    build_in_call_presence_state_element, build_muji_element, NS_CHAT_MARKERS, NS_CHAT_STATES,
    NS_CLIENT, NS_JINGLE_RTP, NS_MESSAGE_CORRECT, NS_MESSAGE_MODERATE, NS_MESSAGE_RETRACT, NS_MUJI,
    NS_REACTIONS, NS_STANZA_ID, NS_WADDLE_IN_CALL, NS_WADDLE_LINK_PREVIEW, NS_WADDLE_PIN_V0,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use native::MessagingExt;
pub use parsing::{
    parse, parse_chat_state_payload, parse_correction_payload, parse_displayed_marker_payload,
    parse_markable_marker_payload, parse_moderation_payload, parse_reaction_payload,
    parse_retraction_payload,
};
pub use presence::{build_presence_stanza, parse_show};
pub use types::{
    CarbonDirection, ChatStatePayload, CorrectionPayload, DisplayedMarkerPayload,
    ExtensionCapabilityData, ExtensionCommandNode, ExtensionDisplayText, ExtensionEnrichmentData,
    ExtensionEnvelopeData, ExtensionLaunchContextData, ExtensionLaunchData, ExtensionNamespace,
    ExtensionPayloadAttributeData, ExtensionPayloadElementData, ExtensionPluginId,
    ExtensionRoomJid, ExtensionSourceData, ExtensionTextId, ExtensionTimestamp, ExtensionXmlName,
    InCallReactionEmoji, InCallReactionPayload, InCallReactionRecipient, InCallSessionId,
    InCallSignal, InboundMessage, InboundPresence, InvalidLinkPreviewToken, LinkPreviewData,
    LinkPreviewPlayer, LinkPreviewToken, MarkableMarkerPayload, MarkupSpan, MarkupSpanData,
    MarkupSpanType, MdsDisplayedEntry, MessagingEvent, ModerationPayload, MucAffiliation, MucRole,
    MucStatus, MujiPresence, PresenceHat, ReactionPayload, ReferenceData, RetractionPayload,
    SendMessageOptions, SharedFile, SharedFileDisposition,
};
/// Re-export the xmpp-parsers Jingle types Waddle's call surface
/// owns so downstream crates (notably the wasm bindings, which
/// don't depend on xmpp-parsers directly) can use the typed
/// `SessionId` / `Reason` without re-vendoring them.
pub use xmpp_parsers::jingle::{Reason as JingleReason, SessionId};
