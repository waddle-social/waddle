use minidom::Element;

use crate::error::{ClientError, ClientResult};

use super::super::namespaces::*;
use super::super::types::*;

pub(crate) fn validate_chat_state(state: &str) -> ClientResult<&str> {
    match state {
        "active" | "composing" | "paused" | "inactive" | "gone" => Ok(state),
        _ => Err(ClientError::Core(waddle_xmpp_core::CoreError::bad_request(
            Some(format!("invalid chat state `{state}`")),
        ))),
    }
}

pub fn parse_chat_state_payload(element: &Element) -> Option<ChatStatePayload> {
    element
        .children()
        .find(|child| child.ns() == NS_CHAT_STATES)
        .and_then(|child| validate_chat_state(child.name()).ok())
        .map(|state| ChatStatePayload {
            state: state.to_string(),
        })
}

pub fn parse_displayed_marker_payload(element: &Element) -> Option<DisplayedMarkerPayload> {
    element
        .get_child("displayed", NS_CHAT_MARKERS)
        .and_then(|child| child.attr("id"))
        .filter(|id| !id.is_empty())
        .map(|id| DisplayedMarkerPayload { id: id.to_string() })
}

pub fn parse_reaction_payload(element: &Element) -> Option<ReactionPayload> {
    let reactions = element.get_child("reactions", NS_REACTIONS)?;
    let target_id = reactions.attr("id")?.trim();
    if target_id.is_empty() {
        return None;
    }
    let emojis = reactions
        .children()
        .filter(|child| child.name() == "reaction" && child.ns() == NS_REACTIONS)
        .map(|child| child.text())
        .filter(|emoji| !emoji.is_empty())
        .collect();
    Some(ReactionPayload {
        target_id: target_id.to_string(),
        emojis,
    })
}

pub fn parse_retraction_payload(element: &Element) -> Option<RetractionPayload> {
    let retract = element.get_child("retract", NS_MESSAGE_RETRACT)?;
    if retract
        .get_child("moderated", NS_MESSAGE_MODERATE)
        .is_some()
    {
        return None;
    }
    retract
        .attr("id")
        .filter(|id| !id.is_empty())
        .map(|id| RetractionPayload {
            target_id: id.to_string(),
        })
}

pub fn parse_retraction_tombstone_payload(element: &Element) -> Option<RetractionTombstonePayload> {
    let retracted = element.get_child("retracted", NS_MESSAGE_RETRACT)?;
    let retraction_id = retracted
        .attr("id")
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let moderated = retracted.get_child("moderated", NS_MESSAGE_MODERATE);
    let moderated_by = moderated
        .and_then(|child| child.attr("by"))
        .and_then(|by| by.parse::<jid::Jid>().ok());
    let reason = retracted
        .get_child("reason", NS_MESSAGE_RETRACT)
        .map(|child| child.text())
        .filter(|text| !text.trim().is_empty());
    Some(RetractionTombstonePayload {
        retraction_id,
        moderated_by,
        reason,
    })
}

fn is_bare_jid(value: &str) -> bool {
    !value.contains('/')
}

pub fn parse_moderation_payload(element: &Element) -> Option<ModerationPayload> {
    let from = element.attr("from")?;
    if element.attr("type") != Some("groupchat") || !is_bare_jid(from) {
        return None;
    }
    let retract = element.get_child("retract", NS_MESSAGE_RETRACT)?;
    let target_id = retract.attr("id")?.trim();
    if target_id.is_empty() {
        return None;
    }
    let moderated = retract.get_child("moderated", NS_MESSAGE_MODERATE)?;
    let moderated_by = moderated
        .attr("by")
        .and_then(|by| by.parse::<jid::Jid>().ok());
    let reason = retract
        .get_child("reason", NS_MESSAGE_RETRACT)
        .map(|child| child.text())
        .filter(|text| !text.trim().is_empty());
    Some(ModerationPayload {
        target_id: target_id.to_string(),
        moderated_by,
        reason,
    })
}

pub fn parse_correction_payload(element: &Element) -> Option<CorrectionPayload> {
    element
        .get_child("replace", NS_MESSAGE_CORRECT)
        .and_then(|child| child.attr("id"))
        .filter(|id| !id.is_empty())
        .map(|id| CorrectionPayload {
            replaces_id: id.to_string(),
        })
}
