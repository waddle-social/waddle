use super::*;

pub(super) fn room_stanza_id_from_payloads(msg: &Message, room: &str) -> Option<StanzaId> {
    msg.payloads
        .iter()
        .find(|payload| {
            payload.name() == "stanza-id"
                && payload.ns() == XEP_0359_STANZA_ID_NS
                && payload.attr("by") == Some(room)
        })
        .and_then(|payload| payload.attr("id"))
        .and_then(|id| StanzaId::new(id.to_string()).ok())
}

pub(super) fn push_feature_namespace(
    module: &ExtensionModuleConfig,
    feature_namespaces: &mut Vec<String>,
    namespace: &str,
) {
    if namespace.trim().is_empty() {
        return;
    }
    if is_official_namespace(namespace) {
        warn!(
            extension = %module.name,
            namespace,
            "extension attempted to advertise an official XMPP namespace; ignoring"
        );
        return;
    }
    if !namespace.starts_with("urn:") && !namespace.starts_with("https://") {
        warn!(
            extension = %module.name,
            namespace,
            "extension attempted to advertise a non-absolute namespace; ignoring"
        );
        return;
    }
    if !feature_namespaces.iter().any(|value| value == namespace) {
        feature_namespaces.push(namespace.to_string());
    }
}

pub(super) fn remove_invalid_cached_extension(module: &ExtensionModuleConfig, wasm_path: &Path) {
    match std::fs::remove_file(wasm_path) {
        Ok(()) => {
            warn!(
                extension = %module.name,
                cache_path = %wasm_path.display(),
                "removed cached extension after component load failure"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            warn!(
                extension = %module.name,
                cache_path = %wasm_path.display(),
                %error,
                "failed to remove cached extension after component load failure"
            );
        }
    }
}

pub(super) fn effective_module_config_json(
    module: &ExtensionModuleConfig,
) -> Result<String, EffectiveModuleConfigError> {
    effective_module_config_with_reader(module, |path| std::fs::read_to_string(path))
        .map(|value| value.to_string())
}

pub(super) fn effective_module_config_with_reader<F>(
    module: &ExtensionModuleConfig,
    mut read_to_string: F,
) -> Result<Value, EffectiveModuleConfigError>
where
    F: FnMut(&Path) -> std::io::Result<String>,
{
    if module.config_secret_files.is_empty() {
        return Ok(module.config.clone());
    }

    let mut config = match module.config.clone() {
        Value::Object(config) => config,
        _ => {
            return Err(EffectiveModuleConfigError::NonObjectBaseConfig {
                extension: module.name.clone(),
            });
        }
    };

    for (key, path) in &module.config_secret_files {
        let contents = read_to_string(Path::new(path)).map_err(|source| {
            EffectiveModuleConfigError::ReadSecretFile {
                extension: module.name.clone(),
                key: key.clone(),
                path: path.clone(),
                source,
            }
        })?;
        config.insert(key.clone(), Value::String(contents));
    }

    Ok(Value::Object(config))
}

pub(super) fn reply_target_from_payloads(payloads: &[minidom::Element]) -> Option<ReplyTarget> {
    payloads
        .iter()
        .find(|payload| payload.name() == "reply" && payload.ns() == "urn:xmpp:reply:0")
        .and_then(|payload| {
            let id = payload.attr("id").and_then(|id| StanzaId::new(id).ok())?;
            let to = payload.attr("to").and_then(|to| FullJidValue::new(to).ok());
            Some(ReplyTarget { id, to })
        })
}

pub(super) fn thread_id_from_message(msg: &Message) -> Option<ThreadId> {
    msg.thread
        .as_ref()
        .and_then(|thread| ThreadId::new(thread.id.clone()).ok())
        .or_else(|| {
            msg.payloads
                .iter()
                .find(|payload| {
                    payload.name() == "thread-reply" && payload.ns() == "urn:waddle:forums:0"
                })
                .and_then(|payload| payload.attr("thread-id"))
                .and_then(|thread_id| ThreadId::new(thread_id).ok())
        })
}

pub(super) fn detect_links(body: &str) -> Vec<DetectedLink> {
    static FENCED_CODE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static INLINE_CODE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static URL_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let fenced_re =
        FENCED_CODE_RE.get_or_init(|| Regex::new(r"(?s)```.*?```").expect("valid fenced regex"));
    let inline_re =
        INLINE_CODE_RE.get_or_init(|| Regex::new(r"`[^`\n]*`").expect("valid inline regex"));
    let re =
        URL_RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"'`]+"#).expect("valid link regex"));

    let mut ignored_ranges: Vec<(usize, usize)> = fenced_re
        .find_iter(body)
        .map(|m| (m.start(), m.end()))
        .collect();
    ignored_ranges.extend(inline_re.find_iter(body).map(|m| (m.start(), m.end())));
    ignored_ranges.sort_unstable_by_key(|(start, _)| *start);

    let mut seen_urls = HashSet::new();
    let mut links = Vec::new();

    for m in re.find_iter(body) {
        if links.len() >= MAX_DETECTED_LINKS {
            break;
        }
        if ignored_ranges
            .iter()
            .any(|(start, end)| m.start() >= *start && m.start() < *end)
        {
            continue;
        }

        let trimmed = m
            .as_str()
            .trim_end_matches(['.', ',', '!', '?', ';', ':', ')', ']']);
        if trimmed.is_empty() || seen_urls.contains(trimmed) {
            continue;
        }
        seen_urls.insert(trimmed.to_string());

        links.push(DetectedLink {
            url: trimmed.to_string(),
            start_offset: m.start() as u32,
            end_offset: (m.start() + trimmed.len()) as u32,
        });
    }

    links
}
