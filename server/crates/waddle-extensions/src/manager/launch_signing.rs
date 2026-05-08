use super::*;

pub(super) fn sign_launch_token(
    key: &[u8],
    plugin: &PluginId,
    action: &crate::types::ActionId,
    launch_id: &LaunchId,
    context: &LaunchContext,
    expires_at: Option<&crate::types::Timestamp>,
    payload_digest: &str,
) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(plugin.as_str().as_bytes());
    mac.update(b"\0");
    mac.update(action.as_str().as_bytes());
    mac.update(b"\0");
    mac.update(launch_id.as_str().as_bytes());
    mac.update(b"\0");
    mac.update(context.waddle_id.as_str().as_bytes());
    mac.update(b"\0");
    if let Some(room) = &context.room {
        mac.update(room.as_str().as_bytes());
    }
    mac.update(b"\0");
    if let Some(stanza_id) = &context.source_stanza_id {
        mac.update(stanza_id.as_str().as_bytes());
    }
    mac.update(b"\0");
    mac.update(payload_digest.as_bytes());
    mac.update(b"\0");
    if let Some(expires_at) = expires_at {
        mac.update(expires_at.as_str().as_bytes());
    }
    hex::encode(mac.finalize().into_bytes())
}

pub(super) fn launch_payload_digest(payloads: &[crate::types::ExtensionPayload]) -> String {
    digest_launch_payload_fields(launch_payload_fields(payloads))
}

pub(super) fn submitted_launch_payload_digest(fields: &[crate::types::FormFieldValue]) -> String {
    let pairs = fields
        .iter()
        .filter(|field| field.name.as_str().starts_with("payload#"))
        .map(|field| {
            (
                field.name.as_str().to_string(),
                field
                    .values
                    .iter()
                    .map(|value| value.as_str().to_string())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    digest_launch_payload_fields(pairs)
}

pub(super) fn launch_payload_fields(
    payloads: &[crate::types::ExtensionPayload],
) -> Vec<(String, Vec<String>)> {
    let mut fields = Vec::new();
    for payload in payloads {
        let prefix = format!("payload#{}", payload.root.local_name);
        for attribute in &payload.root.attributes {
            if attribute.namespace.is_none() && attribute.local_name != "xmlns" {
                fields.push((
                    format!("{prefix}#{}", attribute.local_name),
                    vec![attribute.value.clone()],
                ));
            }
        }
        let text = payload
            .root
            .children
            .iter()
            .filter_map(|child| match child {
                crate::types::XmlNode::Text(text) => Some(text.as_str()),
                crate::types::XmlNode::Element(_) => None,
            })
            .collect::<String>()
            .trim()
            .to_string();
        if !text.is_empty() {
            fields.push((prefix, vec![text]));
        }
    }
    fields
}

pub(super) fn digest_launch_payload_fields(mut fields: Vec<(String, Vec<String>)>) -> String {
    fields.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let mut hasher = Sha256::new();
    for (name, values) in fields {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        for value in values {
            hasher.update(value.as_bytes());
            hasher.update([0]);
        }
        hasher.update([0xff]);
    }
    hex::encode(hasher.finalize())
}

pub(super) fn pubsub_node_placeholder_value(
    pattern: &str,
    candidate: &str,
    placeholder: &str,
) -> Option<String> {
    let pattern_parts = pattern.split(':').collect::<Vec<_>>();
    let candidate_parts = candidate.split(':').collect::<Vec<_>>();
    if pattern_parts.len() != candidate_parts.len() {
        return None;
    }
    let mut value = None;
    for (pattern_part, candidate_part) in pattern_parts.iter().zip(candidate_parts) {
        if pattern_part.starts_with('{') && pattern_part.ends_with('}') {
            if &pattern_part[1..pattern_part.len() - 1] == placeholder {
                value = Some(candidate_part.to_string());
            }
        } else if *pattern_part != candidate_part {
            return None;
        }
    }
    value
}

pub(super) fn default_launch_expiry() -> Option<crate::types::Timestamp> {
    crate::types::Timestamp::new((Utc::now() + chrono::Duration::hours(1)).to_rfc3339()).ok()
}

pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}
