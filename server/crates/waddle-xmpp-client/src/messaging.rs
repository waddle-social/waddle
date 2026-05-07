//! Inbound/outbound XMPP messaging, MUC, and presence operations.
//!
//! Exposes a [`parse`] function for runtime dispatch and typed outbound stanza
//! builders. With the `native` feature enabled, also exposes a convenience
//! trait for sending those stanzas through the native client handle.

mod builders;
mod namespaces;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod native;
mod parsing;
mod presence;
#[cfg(test)]
mod tests;
mod types;

pub use builders::{
    build_chat_state_message, build_correction_message, build_displayed_message,
    build_file_sharing_element, build_moderation_message, build_outbound_message,
    build_reaction_message, build_retraction_message,
};
pub use namespaces::{
    NS_CHAT_MARKERS, NS_CHAT_STATES, NS_MESSAGE_CORRECT, NS_MESSAGE_MODERATE, NS_MESSAGE_RETRACT,
    NS_REACTIONS,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use native::MessagingExt;
pub use parsing::{
    parse, parse_chat_state_payload, parse_correction_payload, parse_displayed_marker_payload,
    parse_moderation_payload, parse_reaction_payload, parse_retraction_payload,
};
pub use types::{
    ChatStatePayload, CorrectionPayload, DisplayedMarkerPayload, InboundMessage, InboundPresence,
    MarkupSpan, MarkupSpanData, MarkupSpanType, MessagingEvent, ModerationPayload, MucAffiliation,
    MucRole, PresenceHat, ReactionPayload, ReferenceData, RetractionPayload, SendMessageOptions,
    SharedFile, SharedFileDisposition,
};
