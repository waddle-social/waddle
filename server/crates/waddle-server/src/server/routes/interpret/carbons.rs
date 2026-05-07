use super::*;

pub(super) async fn send_carbons(
    registry: &ConnectionRegistry,
    deps: &Deps<'_>,
    owner: BareJid,
    message: Box<Message>,
    kind: CarbonKind,
    exclude: FullJid,
) {
    // Per XEP-0280 §5, a carbon copy is the original
    // <message/> wrapped in <sent>/<received> →
    // <forwarded xmlns='urn:xmpp:forward:0'> → original.
    // The outer envelope is addressed FROM the user's
    // bare JID TO the receiving resource. We fan out only
    // to other resources of `owner` that have explicitly
    // opted in via XEP-0280 enable.
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
    if live_targets.is_empty() && detached_targets.is_empty() {
        debug!(
            owner = %owner,
            kind = ?kind,
            "SendCarbons: no carbon-enabled resources to fan out to"
        );
        return;
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
        match registry.send_to(&target, Stanza::Message(envelope)).await {
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
}
