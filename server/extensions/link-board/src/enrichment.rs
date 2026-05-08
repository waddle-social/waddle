use super::*;

pub(super) fn link_enrichments(hook: &types::MessageHook) -> Vec<types::ExtensionEffect> {
    let Some(room) = hook.context.room.clone() else {
        return vec![];
    };
    if hook.links.is_empty() {
        return vec![];
    }
    vec![types::ExtensionEffect::EnrichMessage(
        types::ExtensionEnvelope {
            version: 1,
            enrichments: hook
                .links
                .iter()
                .enumerate()
                .map(|(index, link)| link_enrichment(hook, &room, index, link))
                .collect(),
        },
    )]
}

pub(super) fn link_enrichment(
    hook: &types::MessageHook,
    room: &types::RoomJid,
    index: usize,
    link: &types::LinkTarget,
) -> types::MessageEnrichment {
    let url = link.url.value.clone();
    let launch_id_value = format!("save-link-{index}");
    types::MessageEnrichment {
        id: enrichment_id(&format!("link-{index}")),
        plugin: plugin_id(),
        capability: types::ExtensionCapability::MessageEnrich,
        payload_namespace: payload_namespace(),
        created_at: timestamp(),
        source: hook
            .context
            .stanza_id
            .clone()
            .map(|stanza_id| types::MessageSource {
                stanza_id,
                body_range: Some(link.range),
            }),
        ui: vec![view(
            &format!("link-card-{index}"),
            PLUGIN_NAME,
            &format!("Save {url}"),
            &launch_id_value,
        )],
        payloads: vec![link_payload(
            &url,
            &normalized_url(&url),
            hook.context.stanza_id.as_ref(),
            Some(&link.range),
        )],
        launches: vec![launch(
            &launch_id_value,
            "save-link",
            "Save",
            &hook.context,
            room,
            &url,
            link.range,
        )],
    }
}

pub(super) fn save_link(launch: types::LaunchInvocation) -> Vec<types::ExtensionEffect> {
    let Some(room) = launch.context.room.clone() else {
        return vec![types::ExtensionEffect::HostWarning(display(
            "Link Board saves require a channel context.",
        ))];
    };
    let Some(url) = field_value(&launch.fields, "payload#link#url") else {
        return vec![types::ExtensionEffect::HostWarning(display(
            "Link Board save action is missing a URL.",
        ))];
    };
    let normalized = field_value(&launch.fields, "payload#link#normalized-url")
        .unwrap_or_else(|| normalized_url(&url));
    vec![types::ExtensionEffect::PublishPubsub(
        types::PubsubPublish {
            node: links_node(&room),
            item_id: Some(types::PubsubItemId {
                value: link_item_id(&normalized),
            }),
            payload: saved_link_extension_item(&url, &normalized, timestamp().value.as_str()),
        },
    )]
}
