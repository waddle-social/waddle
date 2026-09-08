use super::*;

impl ExtensionManager {
    /// Whether this message can invoke at least one granted observer hook.
    /// Keep the body and capability checks aligned with observer execution.
    pub fn has_message_observers(&self, message: &Message) -> bool {
        message_hook_body(message).is_some()
            && self.actors.iter().any(|actor| {
                actor
                    .manifest()
                    .declares_capability(ExtensionCapability::MessageObserve)
                    && actor.has_grant(ExtensionCapability::MessageObserve)
            })
    }

    pub async fn enrich_message(&self, msg: &mut Message) -> usize {
        self.enrich_message_for_waddle(msg, WaddleId::new("local").expect("static waddle id"))
            .await
    }

    pub async fn enrich_message_for_waddle(&self, msg: &mut Message, waddle_id: WaddleId) -> usize {
        self.process_message_for_waddle(msg, waddle_id)
            .await
            .enrichments_added
    }

    pub async fn process_message_for_waddle(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
    ) -> MessageExtensionOutcome {
        self.process_message_for_waddle_with_requester(msg, waddle_id, None)
            .await
    }

    pub async fn process_message_for_waddle_with_requester(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
        requester: Option<BareJid>,
    ) -> MessageExtensionOutcome {
        self.process_message_for_waddle_with_requester_and_mode(
            msg,
            waddle_id,
            requester,
            MessageHookMode::All,
        )
        .await
    }

    pub async fn process_message_enrichments_for_waddle_with_requester(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
        requester: Option<BareJid>,
    ) -> MessageExtensionOutcome {
        self.process_message_for_waddle_with_requester_and_mode(
            msg,
            waddle_id,
            requester,
            MessageHookMode::EnrichOnly,
        )
        .await
    }

    pub async fn process_message_observers_for_waddle_with_requester(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
        requester: Option<BareJid>,
    ) -> MessageExtensionOutcome {
        self.process_message_for_waddle_with_requester_and_mode(
            msg,
            waddle_id,
            requester,
            MessageHookMode::ObserveOnly,
        )
        .await
    }

    async fn process_message_for_waddle_with_requester_and_mode(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
        requester: Option<BareJid>,
        mode: MessageHookMode,
    ) -> MessageExtensionOutcome {
        if mode != MessageHookMode::ObserveOnly && message_has_framework_envelope(msg) {
            return MessageExtensionOutcome::default();
        }

        let Some(body_text) = message_hook_body(msg) else {
            return MessageExtensionOutcome::default();
        };
        let body = body_text.as_str();

        let links = detect_links(body);

        let mut outcome = MessageExtensionOutcome::default();
        if !self.actors.is_empty() {
            let hook_links: Vec<LinkTarget> = links
                .into_iter()
                .filter_map(|link| LinkTarget::try_from(link).ok())
                .collect();
            let room = msg
                .to
                .as_ref()
                .or(msg.from.as_ref())
                .and_then(|jid| RoomJid::new(jid.to_bare().to_string()).ok());
            let hook_room = room.clone();
            let source_stanza_id = room
                .as_ref()
                .and_then(|room| room_stanza_id_from_payloads(msg, room.as_str()))
                .or_else(|| {
                    msg.id
                        .as_ref()
                        .and_then(|id| StanzaId::new(id.0.clone()).ok())
                });
            let sender = msg
                .from
                .as_ref()
                .and_then(|jid| FullJidValue::new(jid.to_string()).ok());
            let event = ExtensionEvent::MessageHook(MessageHook {
                context: MessageContext {
                    waddle_id: waddle_id.clone(),
                    stanza_id: source_stanza_id.clone(),
                    room,
                    sender,
                    thread_id: thread_id_from_message(msg),
                    reply_to: reply_target_from_payloads(&msg.payloads),
                },
                body: body_text,
                links: hook_links,
            });
            let enrich_futures = self.actors.iter().filter_map(|actor| {
                let manifest = actor.manifest();
                let declares_enrich =
                    manifest.declares_capability(crate::types::ExtensionCapability::MessageEnrich);
                let declares_observe =
                    manifest.declares_capability(crate::types::ExtensionCapability::MessageObserve);
                let grants_enrich =
                    actor.has_grant(crate::types::ExtensionCapability::MessageEnrich);
                let grants_observe =
                    actor.has_grant(crate::types::ExtensionCapability::MessageObserve);
                let selected = match mode {
                    MessageHookMode::All => {
                        (declares_enrich && grants_enrich) || (declares_observe && grants_observe)
                    }
                    MessageHookMode::EnrichOnly => {
                        declares_enrich && grants_enrich && !(declares_observe && grants_observe)
                    }
                    MessageHookMode::ObserveOnly => declares_observe && grants_observe,
                };
                if !selected {
                    return None;
                }
                let actor_name = actor.manifest().id.to_string();
                let manifest = actor.manifest();
                let actor = Arc::clone(actor);
                let event = event.clone();
                let waddle_id = waddle_id.clone();
                let requester = requester.clone();
                Some(async move {
                    let timeout_duration = if manifest
                        .declares_capability(crate::types::ExtensionCapability::MessageObserve)
                    {
                        EXTENSION_OBSERVE_TIMEOUT
                    } else {
                        EXTENSION_ENRICH_TIMEOUT
                    };
                    let effects = bounded_message_hook(
                        &manifest.id,
                        mode,
                        timeout_duration,
                        actor.handle_event_for_waddle_with_requester(event, waddle_id, requester),
                    )
                    .await;
                    (actor_name, manifest, effects)
                })
            });
            let results = join_all(enrich_futures).await;

            let mut enrichments = Vec::new();
            let mut emitted_effects = Vec::new();
            for (actor_name, manifest, effects) in results {
                for effect in self.sign_effects(effects) {
                    if !message_hook_effect_launches_match_room(&effect, hook_room.as_ref()) {
                        warn!(
                            extension = %actor_name,
                            "extension emitted a message-hook launch outside the hook room; dropping"
                        );
                        continue;
                    }
                    if !effect.validate_for_manifest(&manifest) {
                        warn!(
                            extension = %actor_name,
                            "extension emitted undeclared or invalid message effect; dropping"
                        );
                        continue;
                    }
                    match effect {
                        ExtensionEffect::EnrichMessage(envelope) => {
                            enrichments.extend(envelope.enrichments);
                        }
                        ExtensionEffect::PublishPubSub(_)
                        | ExtensionEffect::ReferenceArtifact(_)
                        | ExtensionEffect::CommandForm(_) => {}
                        ExtensionEffect::HostWarning(warning) => {
                            emitted_effects.push(ExtensionEffect::HostWarning(warning));
                        }
                        ExtensionEffect::Noop => {}
                    }
                }
            }
            let count = enrichments.len();
            if !enrichments.is_empty() {
                msg.payloads
                    .push(ExtensionEnvelope::new(enrichments).to_minidom());
            }
            outcome.enrichments_added = count;
            outcome.effects = emitted_effects;
            if outcome.enrichments_added > 0 {
                debug!(
                    embeds_added = outcome.enrichments_added,
                    "message enriched by extensions"
                );
            }
        }
        outcome
    }
}

fn message_hook_body(message: &Message) -> Option<DisplayText> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .and_then(|body| DisplayText::new(body.clone()).ok())
}

/// Preserve observer failures for the caller's durable receipt decision.
async fn bounded_message_hook(
    plugin: &PluginId,
    mode: MessageHookMode,
    timeout_duration: Duration,
    invocation: impl std::future::Future<Output = Vec<ExtensionEffect>>,
) -> Vec<ExtensionEffect> {
    match timeout(timeout_duration, invocation).await {
        Ok(effects) => effects,
        Err(_) => {
            warn!(
                extension = %plugin,
                timeout_secs = timeout_duration.as_secs(),
                "extension message hook timed out; continuing fail-open"
            );
            if mode == MessageHookMode::ObserveOnly {
                vec![ExtensionEffect::HostWarning(
                    DisplayText::new("Extension message observer timed out")
                        .expect("static observer timeout warning"),
                )]
            } else {
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod observer_tests {
    use super::*;

    #[tokio::test]
    async fn observer_invocation_success_returns_confirmed_effects() {
        let effects = bounded_message_hook(
            &PluginId::new("observer-test").expect("plugin"),
            MessageHookMode::ObserveOnly,
            Duration::from_secs(1),
            std::future::ready(vec![ExtensionEffect::Noop]),
        )
        .await;
        assert!(matches!(effects.as_slice(), [ExtensionEffect::Noop]));
    }

    #[tokio::test]
    async fn observer_invocation_failure_preserves_host_warning() {
        let warning = DisplayText::new("observer host mutation failed").expect("warning");
        let effects = bounded_message_hook(
            &PluginId::new("observer-test").expect("plugin"),
            MessageHookMode::ObserveOnly,
            Duration::from_secs(1),
            std::future::ready(vec![ExtensionEffect::HostWarning(warning.clone())]),
        )
        .await;
        assert!(
            matches!(effects.as_slice(), [ExtensionEffect::HostWarning(actual)] if actual == &warning)
        );
    }

    #[tokio::test]
    async fn observer_invocation_timeout_remains_unconfirmed() {
        let effects = bounded_message_hook(
            &PluginId::new("observer-test").expect("plugin"),
            MessageHookMode::ObserveOnly,
            Duration::ZERO,
            std::future::pending(),
        )
        .await;
        assert!(matches!(
            effects.as_slice(),
            [ExtensionEffect::HostWarning(_)]
        ));
    }

    #[tokio::test]
    async fn enrichment_invocation_timeout_still_fails_open() {
        let effects = bounded_message_hook(
            &PluginId::new("enrichment-test").expect("plugin"),
            MessageHookMode::EnrichOnly,
            Duration::ZERO,
            std::future::pending(),
        )
        .await;
        assert!(effects.is_empty());
    }
}
