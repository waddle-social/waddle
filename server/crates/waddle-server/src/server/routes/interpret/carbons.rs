use super::*;
use waddle_xmpp::ingress::IngressEffectIntent;

pub(crate) struct CarbonRegistryDeps<'a> {
    pub ingress_effect_capture: Option<&'a crate::ingress_shadow::IngressEffectCapture>,
    pub sm_session_registry: Option<&'a Arc<InMemorySmSessionRegistry>>,
    pub web_socket_state: Option<&'a WebSocketState>,
}

pub(super) async fn send_carbons(
    registry: &ConnectionRegistry,
    deps: &Deps<'_>,
    owner: BareJid,
    message: Box<Message>,
    kind: CarbonKind,
    exclude: Vec<FullJid>,
) {
    #[cfg(feature = "clustering")]
    if let Some(state) = deps.web_socket_state {
        if let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        {
            for source_jid in &exclude {
                if let Some(outcome) = bridge
                    .try_fanout_remote_user_carbons(
                        source_jid,
                        &owner,
                        &message,
                        kind,
                        exclude.clone(),
                    )
                    .await
                {
                    match outcome {
                        crate::clustering::route_bridge::RemoteCarbonFanout::Applied(
                            carbon_recipients,
                        ) => {
                            if let (Some(capture), Some(excluded_source)) = (
                                deps.ingress_effect_capture.as_ref(),
                                exclude.first().cloned(),
                            ) {
                                capture.record_intent(IngressEffectIntent::Carbons {
                                    carbon_recipients,
                                    excluded_source,
                                });
                            }
                        }
                        crate::clustering::route_bridge::RemoteCarbonFanout::MaybeCommitted => {}
                    }
                    return;
                }
            }
        }
    }
    send_carbons_to_registry(
        registry,
        CarbonRegistryDeps {
            ingress_effect_capture: deps.ingress_effect_capture.as_ref(),
            sm_session_registry: deps.sm_session_registry,
            web_socket_state: deps.web_socket_state,
        },
        owner,
        message,
        kind,
        exclude,
    )
    .await;
}

pub(crate) async fn send_carbons_to_registry(
    registry: &ConnectionRegistry,
    deps: CarbonRegistryDeps<'_>,
    owner: BareJid,
    message: Box<Message>,
    kind: CarbonKind,
    exclude: Vec<FullJid>,
) -> Vec<FullJid> {
    // Per XEP-0280 §5, a carbon copy is the original
    // <message/> wrapped in <sent>/<received> →
    // <forwarded xmlns='urn:xmpp:forward:0'> → original.
    // The outer envelope is addressed FROM the user's
    // bare JID TO the receiving resource. We fan out only
    // to other resources of `owner` that have explicitly
    // opted in via XEP-0280 enable.
    //
    // `exclude` is the original stanza's delivery set —
    // XEP-0280 §6.3: the receiving server MUST NOT send a
    // forwarded copy to the client(s) the original
    // <message/> stanza was addressed to. For the shared
    // bare-JID recipient pass (#1106) that is every
    // same-priority resource; for the sender pass it is
    // the single originating resource.
    //
    // Suppression rules (groupchat, <private/>, no-copy,
    // body-less) are enforced by `CarbonsMessageHandler`
    // before emitting this event; the interpreter does
    // not re-check them — but it DOES per-target filter
    // through `get_other_carbon_resources_for_user` so a
    // resource that disabled carbons after the message
    // entered the pipeline still gets skipped.
    let owner_str = owner.to_string();
    let live_targets = registry.get_other_carbon_resources_for_user(&owner, &exclude);
    // Detached-but-resumable resources (XEP-0198 stream
    // management) — without this fan-out arm, briefly
    // disconnected secondary devices would silently lose
    // carbon copies during their detached window. The
    // legacy `message.rs` path queues carbons on detached
    // resources via
    // `record_stanza_for_detached_bound_resource`; the
    // interpreter does the same here.
    let detached_targets: Vec<jid::FullJid> = match deps.sm_session_registry {
        Some(sm) => sm
            .detached_carbon_resources_for_user(&owner, &exclude)
            .await
            .unwrap_or_else(|error| {
                warn!(
                    owner = %owner,
                    %error,
                    "SendCarbons: failed to enumerate detached SM resources; \
                     falling back to live-only fan-out"
                );
                Vec::new()
            }),
        None => Vec::new(),
    };
    let mut carbon_recipients = live_targets.clone();
    let detached_only = detached_targets
        .iter()
        .filter(|target| !carbon_recipients.contains(*target))
        .cloned()
        .collect::<Vec<_>>();
    carbon_recipients.extend(detached_only);
    if let Some(excluded_source) = exclude.first().cloned() {
        if !carbon_recipients.is_empty() {
            if let Some(capture) = deps.ingress_effect_capture {
                capture.record_intent(IngressEffectIntent::Carbons {
                    carbon_recipients: carbon_recipients.clone(),
                    excluded_source,
                });
            }
        }
    }
    if live_targets.is_empty() && detached_targets.is_empty() {
        debug!(
            owner = %owner,
            kind = ?kind,
            "SendCarbons: no carbon-enabled resources to fan out to"
        );
        return Vec::new();
    }
    for target in live_targets {
        let envelope = match build_carbon_envelope(kind, &message, &owner_str, &target) {
            Ok(env) => env,
            Err(error) => {
                warn!(
                    target = %target,
                    kind = ?kind,
                    %error,
                    "SendCarbons: failed to build envelope; skipping target"
                );
                continue;
            }
        };
        let stanza = Stanza::Message(envelope);
        if let Some(outcome) =
            try_deliver_registered_remote_resource(deps.web_socket_state, &target, &stanza).await
        {
            match outcome {
                FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                    debug!(target = %target, kind = ?kind, "SendCarbons: delivered to remote resource");
                }
                FullJidDeliveryOutcome::Unavailable => {
                    debug!(
                        target = %target,
                        kind = ?kind,
                        "SendCarbons: remote target unavailable at fan-out time, dropping"
                    );
                }
                FullJidDeliveryOutcome::Dropped => {
                    warn!(
                        target = %target,
                        kind = ?kind,
                        "SendCarbons: remote target backpressured or relay failed, dropping"
                    );
                }
                #[cfg(feature = "clustering")]
                FullJidDeliveryOutcome::MaybeCommitted => {
                    debug!(
                        target = %target,
                        kind = ?kind,
                        "SendCarbons: remote delivery maybe committed; suppressing local fallback"
                    );
                }
            }
            continue;
        }
        match registry.send_to(&target, stanza).await {
            waddle_xmpp::registry::SendResult::Sent => {
                debug!(target = %target, kind = ?kind, "SendCarbons: delivered");
            }
            waddle_xmpp::registry::SendResult::NotConnected => {
                // Race between get_other_carbon_resources and
                // send_to — the resource transitioned to
                // detached. Benign: if it's resumable the
                // detached pass below picks it up;
                // otherwise the carbon is dropped per
                // standard offline-delivery semantics.
                debug!(
                    target = %target,
                    kind = ?kind,
                    "SendCarbons: target offline at fan-out time, dropping"
                );
            }
            waddle_xmpp::registry::SendResult::ChannelClosed => {
                warn!(
                    target = %target,
                    kind = ?kind,
                    "SendCarbons: target channel closed, dropping"
                );
            }
        }
    }
    // Detached pass — queue the same envelope for replay
    // when the resource resumes its XEP-0198 session.
    if let Some(sm) = deps.sm_session_registry {
        for target in detached_targets {
            let envelope = match build_carbon_envelope(kind, &message, &owner_str, &target) {
                Ok(env) => env,
                Err(error) => {
                    warn!(
                        target = %target,
                        kind = ?kind,
                        %error,
                        "SendCarbons: failed to build detached envelope; skipping"
                    );
                    continue;
                }
            };
            let stanza = Stanza::Message(envelope);
            match sm
                .record_stanza_for_detached_bound_resource(&target, &stanza, chrono::Utc::now())
                .await
            {
                Ok(true) => {
                    debug!(
                        target = %target,
                        kind = ?kind,
                        "SendCarbons: queued for detached XEP-0198 resume"
                    );
                }
                Ok(false) => {
                    debug!(
                        target = %target,
                        kind = ?kind,
                        "SendCarbons: detached session expired between enumeration \
                         and queue; dropping"
                    );
                }
                Err(error) => {
                    warn!(
                        target = %target,
                        kind = ?kind,
                        %error,
                        "SendCarbons: failed to queue carbon for detached resource"
                    );
                }
            }
        }
    }
    carbon_recipients
}

async fn try_deliver_registered_remote_resource(
    web_socket_state: Option<&WebSocketState>,
    target: &FullJid,
    stanza: &Stanza,
) -> Option<FullJidDeliveryOutcome> {
    #[cfg(feature = "clustering")]
    {
        let state = web_socket_state?;
        let bridge = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()?;
        bridge
            .try_deliver_registered_remote_resource(
                target,
                stanza,
                waddle_xmpp::registry::DeliveryKind::DirectFrame,
            )
            .await
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (web_socket_state, target, stanza);
        None
    }
}
