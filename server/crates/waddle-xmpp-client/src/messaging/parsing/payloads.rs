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

pub fn parse_extension_envelope(element: &Element) -> Option<ExtensionEnvelopeData> {
    let envelope = element.get_child("extensions", NS_WADDLE_EXTENSION)?;
    let version = envelope
        .attr("version")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);
    let enrichments = envelope
        .children()
        .filter(|child| child.name() == "enrichment" && child.ns() == NS_WADDLE_EXTENSION)
        .filter_map(parse_extension_enrichment)
        .collect::<Vec<_>>();
    if enrichments.is_empty() {
        return None;
    }
    Some(ExtensionEnvelopeData {
        version,
        enrichments,
    })
}

fn parse_extension_enrichment(element: &Element) -> Option<ExtensionEnrichmentData> {
    let id = ExtensionTextId::new(element.attr("id")?)?;
    let plugin = ExtensionPluginId::new(element.attr("plugin")?)?;
    let capability = ExtensionCapabilityData::from_attr(element.attr("capability")?)?;
    if capability != ExtensionCapabilityData::MessageEnrich {
        return None;
    }
    let payload_namespace = ExtensionNamespace::new(element.attr("payload-ns")?)?;
    let created = ExtensionTimestamp::new(element.attr("created")?)?;
    let source = element
        .get_child("source", NS_WADDLE_EXTENSION)
        .and_then(parse_extension_source);
    let payload_container = element.get_child("payload", NS_WADDLE_EXTENSION);
    let title = payload_container.and_then(|payload| {
        payload
            .children()
            .find(|child| child.name() == "view" && child.ns() == NS_WADDLE_EXTENSION)
            .and_then(|view| view.attr("title"))
            .filter(|title| !title.trim().is_empty())
            .and_then(ExtensionDisplayText::new)
    });
    let summary = payload_container.and_then(|payload| {
        payload
            .children()
            .find(|child| child.name() == "view" && child.ns() == NS_WADDLE_EXTENSION)
            .and_then(|view| {
                view.children()
                    .find(|child| child.name() == "text" && child.ns() == NS_WADDLE_EXTENSION)
            })
            .map(|text| text.text().trim().to_string())
            .filter(|text| !text.is_empty())
            .and_then(ExtensionDisplayText::new)
    });
    let payloads = payload_container
        .map(|payload| {
            payload
                .children()
                .filter(|child| child.ns() != NS_WADDLE_EXTENSION)
                .filter_map(parse_extension_payload_element)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let launches = element
        .children()
        .filter(|child| child.name() == "launch" && child.ns() == NS_WADDLE_EXTENSION)
        .filter_map(parse_extension_launch)
        .collect();

    Some(ExtensionEnrichmentData {
        id,
        plugin,
        capability,
        payload_namespace,
        created,
        source,
        title,
        summary,
        payloads,
        launches,
    })
}

fn parse_extension_source(element: &Element) -> Option<ExtensionSourceData> {
    Some(ExtensionSourceData {
        stanza_id: ExtensionTextId::new(element.attr("stanza-id")?)?,
        body_start: element
            .attr("body-start")
            .and_then(|value| value.parse::<u32>().ok()),
        body_end: element
            .attr("body-end")
            .and_then(|value| value.parse::<u32>().ok()),
    })
}

fn parse_extension_launch(element: &Element) -> Option<ExtensionLaunchData> {
    let context = element
        .get_child("context", NS_WADDLE_EXTENSION)
        .and_then(parse_extension_launch_context)?;
    let payloads = element
        .get_child("payload", NS_WADDLE_EXTENSION)
        .map(|payload| {
            payload
                .children()
                .filter(|child| child.ns() != NS_WADDLE_EXTENSION)
                .filter_map(parse_extension_payload_element)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(ExtensionLaunchData {
        id: ExtensionTextId::new(element.attr("id")?)?,
        plugin: ExtensionPluginId::new(element.attr("plugin")?)?,
        action: ExtensionTextId::new(element.attr("action")?)?,
        command_node: ExtensionCommandNode::new(element.attr("command-node")?)?,
        label: ExtensionDisplayText::new(element.attr("label")?.trim())?,
        context,
        payloads,
        expires_at: element.attr("expires-at").and_then(ExtensionTimestamp::new),
        token: element.attr("token").and_then(ExtensionTextId::new),
    })
}

fn parse_extension_launch_context(element: &Element) -> Option<ExtensionLaunchContextData> {
    Some(ExtensionLaunchContextData {
        waddle_id: ExtensionTextId::new(element.attr("waddle-id")?)?,
        room: element.attr("room").and_then(ExtensionRoomJid::new),
        source_stanza_id: element.attr("stanza-id").and_then(ExtensionTextId::new),
    })
}

fn parse_extension_payload_element(element: &Element) -> Option<ExtensionPayloadElementData> {
    let attributes = element
        .attrs()
        .filter_map(|(name, value)| {
            Some(ExtensionPayloadAttributeData {
                name: ExtensionXmlName::new(name)?,
                value: value.to_string(),
            })
        })
        .collect::<Vec<_>>();
    let children = element
        .children()
        .filter(|child| child.ns() == element.ns())
        .filter_map(parse_extension_payload_element)
        .collect();
    let text = element.text().trim().to_string();
    Some(ExtensionPayloadElementData {
        namespace: ExtensionNamespace::new(element.ns())?,
        name: ExtensionXmlName::new(element.name())?,
        attributes,
        text: if text.is_empty() { None } else { Some(text) },
        children,
    })
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
