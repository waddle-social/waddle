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
    element
        .get_child("retract", NS_MESSAGE_RETRACT)
        .and_then(|child| child.attr("id"))
        .or_else(|| {
            element
                .get_child("retracted", NS_MESSAGE_RETRACT)
                .and_then(|child| child.attr("id"))
        })
        .filter(|id| !id.is_empty())
        .map(|id| RetractionPayload {
            target_id: id.to_string(),
        })
}

pub fn parse_moderation_payload(element: &Element) -> Option<ModerationPayload> {
    let apply_to = element.get_child("apply-to", NS_FASTEN)?;
    let target_id = apply_to.attr("id")?.trim();
    if target_id.is_empty() {
        return None;
    }
    let moderated = apply_to.get_child("moderated", NS_MESSAGE_MODERATE)?;
    moderated.get_child("retract", NS_MESSAGE_RETRACT)?;
    let moderated_by = moderated.attr("by").unwrap_or_default().to_string();
    let reason = moderated
        .get_child("reason", NS_MESSAGE_MODERATE)
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
