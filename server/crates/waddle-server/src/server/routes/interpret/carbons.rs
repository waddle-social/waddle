use super::*;
use waddle_xmpp::ingress::IngressEffectIntent;

pub(crate) struct CarbonRegistryDeps<'a> {
    pub ingress_effect_capture: Option<&'a crate::ingress::IngressEffectCapture>,
    pub sm_session_registry: Option<&'a Arc<InMemorySmSessionRegistry>>,
    pub web_socket_state: Option<&'a WebSocketState>,
}

/// Why an owner could not finish its frozen carbon obligation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error,
)]
pub enum CarbonFanoutFailure {
    #[error("detached carbon inventory unavailable")]
    DetachedInventory,
    #[error("detached carbon append failed")]
    DetachedAppend,
    #[error("carbon envelope construction failed")]
    Envelope,
    #[error("carbon resource delivery failed")]
    Delivery,
}

pub(crate) struct CarbonRegistryFanoutOutcome {
    pub(crate) carbon_recipients: Vec<FullJid>,
    // Re-exported to the origin node through the clustering relay reply; the
    // local capture already recorded each stream, so non-clustering builds
    // construct but never re-read this field.
    #[cfg_attr(not(feature = "clustering"), allow(dead_code))]
    pub(crate) recipient_sm_append_streams: Vec<waddle_xmpp::pending_delivery::SmSessionId>,
}

pub(super) async fn send_carbons(
    registry: &ConnectionRegistry,
    deps: &Deps<'_>,
    owner: BareJid,
    message: Box<Message>,
    kind: CarbonKind,
    exclude: Vec<FullJid>,
) {
    if deps.effects.is_planning() {
        if super::route_to_connection::plan::remote_owner(deps, &owner).await {
            deps.capture_intent(IngressEffectIntent::RelayCarbons {
                owner: owner.clone(),
                exclude: exclude.clone(),
                kind,
            });
            super::effects::delivery::record(
                deps,
                super::effects::delivery::ExternalDeliveryEffect::RelayCarbons {
                    origin: deps.ordered_relay_origin.clone(),
                    owner,
                    exclude,
                    message,
                    kind,
                },
            );
            return;
        }
        let mut carbon_recipients = registry.get_other_carbon_resources_for_user(&owner, &exclude);
        if let Some(sm) = deps.sm_session_registry {
            if let Ok(detached) = sm
                .detached_carbon_resources_for_user(&owner, &exclude)
                .await
            {
                carbon_recipients.extend(detached);
            }
        }
        carbon_recipients.sort();
        carbon_recipients.dedup();
        if let Some(excluded_source) = exclude.first().cloned() {
            deps.capture_intent(IngressEffectIntent::Carbons {
                carbon_recipients,
                excluded_source,
                kind,
            });
        }
        super::effects::delivery::record(
            deps,
            super::effects::delivery::ExternalDeliveryEffect::Carbons {
                owner,
                exclude,
                message,
                kind,
            },
        );
        return;
    }
    if relay_carbons_only(deps, &owner, &message, kind, &exclude)
        .await
        .is_some()
    {
        return;
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

/// Executes only the frozen remote carbon obligation. A changed owner must be
/// reported to the ingress executor, never silently expanded to local fanout.
pub(super) async fn relay_carbons_only(
    deps: &Deps<'_>,
    owner: &BareJid,
    message: &Message,
    kind: CarbonKind,
    exclude: &[FullJid],
) -> Option<FullJidDeliveryOutcome> {
    #[cfg(feature = "clustering")]
    if let Some(state) = deps.web_socket_state {
        if let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        {
            for source_jid in exclude {
                if let Some(outcome) = bridge
                    .try_fanout_remote_user_carbons(
                        source_jid,
                        owner,
                        message,
                        kind,
                        exclude.to_vec(),
                    )
                    .await
                {
                    return Some(remote_carbon_delivery(outcome, deps, owner, exclude, kind));
                }
            }
        }
    }
    #[cfg(not(feature = "clustering"))]
    let _ = (deps, owner, message, kind, exclude);
    None
}

#[cfg(feature = "clustering")]
pub(crate) fn remote_carbon_delivery(
    outcome: crate::clustering::route_bridge::RemoteCarbonFanout,
    deps: &Deps<'_>,
    owner: &BareJid,
    exclude: &[FullJid],
    kind: CarbonKind,
) -> FullJidDeliveryOutcome {
    match outcome {
        crate::clustering::route_bridge::RemoteCarbonFanout::Applied {
            carbon_recipients,
            recipient_sm_append_streams,
        } => {
            if let Some(capture) = deps.ingress_effect_capture.as_ref() {
                for stream in recipient_sm_append_streams {
                    capture.record_recipient_sm_append(stream);
                }
            }
            if let (Some(capture), Some(excluded_source)) = (
                deps.ingress_effect_capture.as_ref(),
                exclude.first().cloned(),
            ) {
                capture.record_intent(IngressEffectIntent::Carbons {
                    carbon_recipients,
                    excluded_source,
                    kind,
                });
            }
            FullJidDeliveryOutcome::Delivered
        }
        crate::clustering::route_bridge::RemoteCarbonFanout::Incomplete { reason } => {
            warn!(%owner, %reason, "remote carbon fanout incomplete");
            FullJidDeliveryOutcome::Unavailable
        }
        crate::clustering::route_bridge::RemoteCarbonFanout::MaybeCommitted => {
            FullJidDeliveryOutcome::MaybeCommitted
        }
    }
}

pub(crate) async fn send_carbons_to_registry(
    registry: &ConnectionRegistry,
    deps: CarbonRegistryDeps<'_>,
    owner: BareJid,
    message: Box<Message>,
    kind: CarbonKind,
    exclude: Vec<FullJid>,
) -> Vec<FullJid> {
    match send_carbons_to_registry_with_capture(registry, deps, owner, message, kind, exclude).await
    {
        Ok(outcome) => outcome.carbon_recipients,
        Err(reason) => {
            warn!(%reason, "carbon fanout incomplete");
            Vec::new()
        }
    }
}

pub(crate) async fn send_carbons_to_registry_with_capture(
    registry: &ConnectionRegistry,
    deps: CarbonRegistryDeps<'_>,
    owner: BareJid,
    message: Box<Message>,
    kind: CarbonKind,
    exclude: Vec<FullJid>,
) -> Result<CarbonRegistryFanoutOutcome, CarbonFanoutFailure> {
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
            .map_err(|error| {
                warn!(%owner, %error, "SendCarbons: detached inventory failed");
                CarbonFanoutFailure::DetachedInventory
            })?,
        None => Vec::new(),
    };
    if live_targets.is_empty() && detached_targets.is_empty() {
        debug!(
            owner = %owner,
            kind = ?kind,
            "SendCarbons: no carbon-enabled resources to fan out to"
        );
        return Ok(CarbonRegistryFanoutOutcome {
            carbon_recipients: Vec::new(),
            recipient_sm_append_streams: Vec::new(),
        });
    }
    let mut carbon_recipients = Vec::new();
    let mut recipient_sm_append_streams = Vec::new();
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
                return Err(CarbonFanoutFailure::Envelope);
            }
        };
        let stanza = Stanza::Message(envelope);
        if let Some(outcome) =
            try_deliver_registered_remote_resource(deps.web_socket_state, &target, &stanza).await
        {
            match outcome {
                FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                    carbon_recipients.push(target.clone());
                    debug!(target = %target, kind = ?kind, "SendCarbons: delivered to remote resource");
                }
                FullJidDeliveryOutcome::Unavailable => {
                    debug!(
                        target = %target,
                        kind = ?kind,
                        "SendCarbons: remote target unavailable at fan-out time, dropping"
                    );
                    return Err(CarbonFanoutFailure::Delivery);
                }
                FullJidDeliveryOutcome::Dropped => {
                    warn!(
                        target = %target,
                        kind = ?kind,
                        "SendCarbons: remote target backpressured or relay failed, dropping"
                    );
                    return Err(CarbonFanoutFailure::Delivery);
                }
                #[cfg(feature = "clustering")]
                FullJidDeliveryOutcome::MaybeCommitted => {
                    debug!(
                        target = %target,
                        kind = ?kind,
                        "SendCarbons: remote delivery maybe committed; suppressing local fallback without recording a definitive carbon recipient"
                    );
                    return Err(CarbonFanoutFailure::Delivery);
                }
            }
            continue;
        }
        match registry.send_to(&target, stanza).await {
            waddle_xmpp::registry::SendResult::Sent => {
                carbon_recipients.push(target.clone());
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
                return Err(CarbonFanoutFailure::Delivery);
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
                    return Err(CarbonFanoutFailure::Envelope);
                }
            };
            let stanza = Stanza::Message(envelope);
            match sm
                .record_stanza_for_detached_bound_resource_with_stream(
                    &target,
                    &stanza,
                    chrono::Utc::now(),
                )
                .await
            {
                Ok(Some(stream)) => {
                    if let Some(capture) = deps.ingress_effect_capture {
                        capture.record_recipient_sm_append(stream.clone());
                    }
                    recipient_sm_append_streams.push(stream);
                    carbon_recipients.push(target.clone());
                    debug!(
                        target = %target,
                        kind = ?kind,
                        "SendCarbons: queued for detached XEP-0198 resume"
                    );
                }
                Ok(None) => {
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
                    return Err(CarbonFanoutFailure::DetachedAppend);
                }
            }
        }
    }
    carbon_recipients.sort_by_key(ToString::to_string);
    carbon_recipients.dedup();
    if let (Some(capture), Some(excluded_source)) =
        (deps.ingress_effect_capture, exclude.first().cloned())
    {
        if !carbon_recipients.is_empty() {
            capture.record_intent(IngressEffectIntent::Carbons {
                carbon_recipients: carbon_recipients.clone(),
                excluded_source,
                kind,
            });
        }
    }
    Ok(CarbonRegistryFanoutOutcome {
        carbon_recipients,
        recipient_sm_append_streams,
    })
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
