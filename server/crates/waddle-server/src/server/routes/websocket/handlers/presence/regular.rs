use super::subscription::{
    blocking_storage, record_stanza_for_detached_available_resources_excluding, roster_storage,
};
use super::{build_available_presence, debug, info, warn, BareJid, FullJid, Jid, WebSocketState};
#[cfg(feature = "clustering")]
use crate::clustering::codec::RemoteElement;
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::RemotePresenceShow;
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::RemotePresenceStateSnapshot;
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::RemoteResourceStateUpdate;
use crate::pending_delivery::FlushContext;
use crate::pending_delivery::MamArchiveResolver;
use crate::server::caps_resolution::{build_caps_disco_info_query, extract_caps_payload};
use crate::server::routes::interpret::OrderedRelayRouteOrigin;
use futures::StreamExt as _;
use tracing::Instrument as _;
use waddle_xmpp::ownership::account_coordinator::Presentation;
use waddle_xmpp::Stanza;

const PRESENCE_FANOUT_CONCURRENCY: usize = 16;

/// Handle a broadcast (undirected) presence update from a live connection.
///
/// `owner` is the connection's registry ownership token
/// (`WsConnState::registry_owner`, the local binding ID returned by
/// `register_*`). Every JID-keyed registry write below is owner-gated
/// (issue #1208): a superseded same-full-JID connection processing a late
/// presence frame must neither stamp stale presence onto the replacement's
/// entry nor consume the replacement's once-per-session claims. `None`
/// means the connection never completed registration (or its registration
/// was rolled back), so it owns no registry slot and is treated as a
/// non-owner: registry writes are skipped entirely.
pub(super) async fn handle_regular_presence_update(
    state: &WebSocketState,
    sender_jid: &FullJid,
    owner: Option<&waddle_xmpp::registry::LocalConnectionBindingId>,
    presence: xmpp_parsers::presence::Presence,
    ordered_relay_origin: Option<&OrderedRelayRouteOrigin>,
) {
    // Defense in depth behind `parse_subscription_presence`: only the normal
    // available/unavailable forms may update state and be relayed. The
    // broadcast below forwards the stanza verbatim, so any other type leaking
    // in here would reach subscribers.
    if !matches!(
        presence.type_,
        xmpp_parsers::presence::Type::None | xmpp_parsers::presence::Type::Unavailable
    ) {
        warn!(
            jid = %sender_jid,
            presence_type = ?presence.type_,
            "Dropping non-broadcastable presence type from regular update path"
        );
        return;
    }
    let available = presence.type_ != xmpp_parsers::presence::Type::Unavailable;
    let priority: i8 = priority_to_i8(&presence.priority);
    // Issue #1208: each write re-verifies ownership INSIDE the registry call
    // (the owner-gated variants hold the connections entry guard across
    // check and write, see `update_presence_state_if_owner`). The gated
    // `update_presence_if_owner_transition` doubles as the ownership probe for
    // the sibling writes in each branch and reports the previous availability
    // atomically so RFC 6121 §4.3.1 initial presence probes run only on an
    // unavailable -> available transition.
    let availability_transition = |available: bool| {
        owner.and_then(|owner| {
            state
                .deps
                .protocol
                .connection_registry
                .update_presence_if_owner_transition(sender_jid, owner, available, priority)
                .map(|was_available| (owner, was_available))
        })
    };
    if available {
        if let Some((owner, was_available)) = availability_transition(true) {
            state
                .deps
                .protocol
                .connection_registry
                .clear_last_activity(&sender_jid.to_bare());
            resolve_caps_for_presence(state, sender_jid, &presence).await;
            // XEP-0160 §3 step 5 (locked Q7a/Q7d): on the first non-negative-
            // priority presence of a fresh session, drain pending_delivery
            // for the recipient. `claim_offline_flush` (a once-per-session
            // CAS) is consumed HERE, on the connection task, so the
            // claim-race semantics are unchanged; the actual row pushing is
            // handed to a spawned task below so a backlog larger than the
            // outbound mpsc can't self-deadlock this connection (issue #1220).
            let flush_plan = if priority >= 0 {
                plan_offline_flush(state, sender_jid, owner).await
            } else {
                None
            };
            let presence_state_written = state
                .deps
                .protocol
                .connection_registry
                .update_presence_state_if_owner(
                    sender_jid,
                    owner,
                    presence
                        .show
                        .as_ref()
                        .map(|show| show_name(show).to_string()),
                    presence.statuses.values().next().cloned(),
                    priority,
                    // The client's extension payloads (XEP-0115 caps, XEP-0319
                    // idle, ...) are stored verbatim so probe/subscription
                    // delivery relays the real advertisements (issue #1101).
                    presence.payloads.clone(),
                );
            mirror_remote_presence_update(
                state,
                sender_jid,
                owner,
                true,
                priority,
                presence_state_written
                    .then(|| remote_presence_state_from_presence(&presence, priority)),
            )
            .await;
            // Queued inbound subscription requests are presented after the
            // frame, once the account authority holds this availability: see
            // `present_pending_subscriptions`.
            spawn_session_recovery_delivery(state, sender_jid, owner, flush_plan, Vec::new());
            if !was_available {
                spawn_initial_presence_probes(state, sender_jid, owner, ordered_relay_origin);
            }
        } else {
            debug!(
                jid = %sender_jid,
                "Skipping presence registry writes: connection does not own its registry slot"
            );
        }
    } else if let Some((owner, _was_available)) = availability_transition(false) {
        state
            .deps
            .protocol
            .connection_registry
            .clear_presence_state_if_owner(sender_jid, owner);
        mirror_remote_presence_update(state, sender_jid, owner, false, priority, None).await;
        state
            .deps
            .protocol
            .connection_registry
            .record_last_activity(
                &sender_jid.to_bare(),
                presence.statuses.values().next().cloned(),
            );
    } else {
        debug!(
            jid = %sender_jid,
            "Skipping unavailable-presence registry writes: connection does not own its registry slot"
        );
    }
    broadcast_presence_to_subscribers(state, sender_jid, &presence, ordered_relay_origin).await;
}

/// Present the subscription requests queued for a resource, once per
/// session, after the account authority has recorded the resource as
/// available (RFC 6121 §3.1.3).
///
/// The connection calls this after each frame, once the frame's resource
/// snapshot has reached the account authority. The order is what keeps a
/// request from being stranded: `handle_subscription_presence` queues a
/// subscribe before it asks the authority for the recipient's available
/// resources, and this reads the queue only after the authority holds the
/// availability. A request is therefore either delivered live by its
/// sender's turn or found in the queue here. Reading the queue on the
/// presence frame itself, before the authority learns the availability,
/// lets a subscribe that saw the resource unavailable land in the queue
/// after the read, where it waits for the next session.
///
/// The per-connection CAS makes this once per session, so later presence
/// updates within a session never re-prompt the user (issue #1104). It is
/// owner-gated, so a superseded connection cannot consume its
/// replacement's claim.
pub(in crate::server::routes::websocket) async fn present_pending_subscriptions(
    state: &WebSocketState,
    resource: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
) {
    let registry = &state.deps.protocol.connection_registry;
    let Some(entry) = registry
        .entry_if_owner(resource, owner)
        .filter(|entry| entry.is_presence_available())
    else {
        return;
    };
    if !entry.claim_pending_subscribes_flush() {
        return;
    }
    // The selected account authority records the presentation before it is
    // made, and a resumed session is not presented to again.
    if !crate::server::routes::websocket::account_binding::mark_presented(
        state,
        resource,
        &entry,
        Presentation::PendingSubscriptions,
    )
    .await
    {
        return;
    }
    let account = resource.to_bare();
    let mut subscribe_stanzas = registry.pending_subscription_stanzas(&account);
    // Requests from remote senders are recorded durably and delivered on
    // every new session until answered (gate G18).
    subscribe_stanzas.extend(super::remote::pending_remote_subscriptions(state, &account).await);
    spawn_session_recovery_delivery(state, resource, owner, None, subscribe_stanzas);
    // RFC 6121 §4.2.2: a new presence session probes the remote contacts
    // whose presence the account is subscribed to; local contacts' presence
    // is known here already.
    super::remote::spawn_probe_remote_contacts(state, &account);
}

#[cfg(feature = "clustering")]
async fn mirror_remote_presence_update(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    available: bool,
    priority: i8,
    presence_state: Option<RemotePresenceStateSnapshot>,
) {
    if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        bridge
            .update_remote_user_resource_if_owner(
                jid,
                owner,
                RemoteResourceStateUpdate::Presence {
                    available,
                    priority,
                    state: presence_state,
                },
            )
            .await;
    }
}

#[cfg(not(feature = "clustering"))]
async fn mirror_remote_presence_update(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    _available: bool,
    _priority: i8,
    _presence_state: Option<()>,
) {
}

#[cfg(feature = "clustering")]
fn remote_presence_state_from_presence(
    presence: &xmpp_parsers::presence::Presence,
    priority: i8,
) -> RemotePresenceStateSnapshot {
    RemotePresenceStateSnapshot {
        show: presence.show.clone().map(RemotePresenceShow::from),
        status: presence.statuses.values().next().cloned(),
        priority,
        payloads: presence
            .payloads
            .iter()
            .cloned()
            .map(RemoteElement)
            .collect(),
    }
}

#[cfg(not(feature = "clustering"))]
fn remote_presence_state_from_presence(
    _presence: &xmpp_parsers::presence::Presence,
    _priority: i8,
) {
}

/// Captured intent to run the XEP-0160 offline flush for a recovering
/// session, produced on the connection task and consumed by the spawned
/// [`spawn_session_recovery_delivery`] task (issue #1220).
struct OfflineFlushPlan {
    /// The recovering connection's XEP-0198 stream id (locked Q7b SM-ack
    /// lifecycle). `None` → the flush uses the non-SM delete-on-push path.
    sm_session: Option<waddle_xmpp::pending_delivery::SmSessionId>,
}

/// XEP-0160 §3 step 5 + locked Q7a / Q7c / Q7d: consume the recovering
/// session's once-per-session offline-flush CAS on the CONNECTION task and
/// capture the intent to flush.
///
/// `ConnectionEntry::claim_offline_flush()` is a CAS that returns `true`
/// exactly once per fresh session — repeated presence updates (priority
/// transitions, status text changes) do not re-flush an already-drained
/// queue. Consuming it here (not in the spawned task) keeps the claim-race
/// semantics identical to the pre-#1220 inline flush.
///
/// Owner-gated (issue #1208): the entry lookup uses `entry_if_owner` so a
/// superseded same-JID connection cannot consume the replacement's
/// once-per-session `claim_offline_flush` CAS.
async fn plan_offline_flush(
    state: &WebSocketState,
    sender_jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
) -> Option<OfflineFlushPlan> {
    let entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(sender_jid, owner)?;
    if !entry.claim_offline_flush() {
        return None;
    }
    // The selected account authority records the flush before it runs. A
    // resumed session, whose earlier binding already received its backlog,
    // is not flushed to again.
    if !crate::server::routes::websocket::account_binding::mark_presented(
        state,
        sender_jid,
        &entry,
        Presentation::OfflineMessages,
    )
    .await
    {
        return None;
    }
    Some(OfflineFlushPlan {
        sm_session: entry.sm_stream_id(),
    })
}

/// Push a recovering session's offline backlog and queued subscription
/// requests to its resource, OFF the connection task (issue #1220).
///
/// Both the XEP-0160 flush and the RFC 6121 §3.1.3 pending-subscribe
/// delivery `.await`-push into the recipient's own 256-slot outbound mpsc,
/// whose only consumer is the connection task. Running them inline (the
/// pre-#1220 behaviour) self-deadlocks that task the moment the backlog
/// exceeds the channel capacity. Spawning with owned handles lets the
/// connection task keep draining its mpsc while this task backpressures on
/// it — and it composes with the #1219 send-window gate, which pauses that
/// drain under load without ever wedging the producer.
///
/// The CASes gating both actions were already consumed on the connection
/// task; only the pushing is deferred here. Subscription requests are sent
/// before the potentially long-running ordered archive drain.
fn spawn_session_recovery_delivery(
    state: &WebSocketState,
    resource: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    flush_plan: Option<OfflineFlushPlan>,
    subscribe_stanzas: Vec<Stanza>,
) {
    if flush_plan.is_none() && subscribe_stanzas.is_empty() {
        return;
    }
    let storage = std::sync::Arc::clone(&state.deps.protocol.pending_delivery_storage);
    let ingress = std::sync::Arc::clone(&state.deps.protocol.ingress);
    let registry = std::sync::Arc::clone(&state.deps.protocol.connection_registry);
    let blocking_storage = std::sync::Arc::clone(&state.deps.protocol.blocking_storage);
    let mam_storage = std::sync::Arc::clone(&state.deps.protocol.mam_storage);
    let server_domain = state.deps.auth_state.xmpp_domain.clone();
    let app_state = std::sync::Arc::clone(&state.deps.app_state);
    let served = state
        .deps
        .service_domains
        .served_domains(server_domain.as_str());
    let recipient = resource.to_bare();
    let resource = resource.clone();
    let owner = *owner;
    // Carry the current tracing span into the spawned task (issue #1220): the
    // flush is logically part of this presence-handling flow, but `tokio::spawn`
    // resets the task-local span context, which would orphan the flush's own
    // `#[instrument]` span (and its OTLP logs/trace) from the connection's
    // trace. Instrumenting the spawned future with `Span::current()` re-parents
    // it — the same idiom `server::http` uses for off-task request work.
    let flush_span = tracing::Span::current();
    tokio::spawn(
        async move {
            // If this session was superseded (same full JID re-registered) before
            // the task ran, skip the flush: the replacement runs its OWN flush via
            // its own once-per-session CAS, and pushing SM-claimed rows to a
            // replacement whose stream id differs would leave them claimed by the
            // now-dead session (self-healed later by the claim-expiry janitor, but
            // redundant and a duplicate-delivery risk). The rows stay unclaimed
            // for the replacement. This narrows the window the off-task spawn
            // opened; a replacement racing in mid-flush is still janitor-healed.
            // Subscription delivery must not wait for archive predecessor acks.
            for stanza in subscribe_stanzas {
                let _ = registry.send_to_if_owner(&resource, &owner, stanza).await;
            }
            let still_owner = registry.entry_if_owner(&resource, &owner).is_some();
            if let Some(plan) = flush_plan.filter(|_| still_owner) {
                // Remote senders' messages are delivered only while their
                // domain is still admitted (ADR-0020 D1; gate G18).
                crate::server::routes::websocket::s2s_accounts::forget_unadmitted_remote_messages(
                    &app_state,
                    served.as_ref(),
                    &storage,
                    &mam_storage,
                    &recipient,
                )
                .await;
                let resolver = MamArchiveResolver { mam_storage };
                let outcome = crate::pending_delivery::flush_for_resource_with_retry(
                    &storage,
                    &registry,
                    &recipient,
                    &resource,
                    FlushContext {
                        server_domain: &server_domain,
                        sm_session: plan.sm_session.as_ref(),
                        // XEP-0191 §2 step 4 flush-time block re-evaluation
                        // (PR #360): pass live blocking storage so a recipient who
                        // blocked a sender AFTER intake doesn't see queued
                        // messages from that sender on reconnect.
                        blocking_storage: Some(&blocking_storage),
                        // Owner-gate SM pushes so a same-full-JID replacement
                        // racing in mid-flush can't receive this session's
                        // SM-claimed rows (issue #1220 review).
                        owner: Some(&owner),
                        archive_resolver: &resolver,
                        dispatch_gate: Some(ingress.as_ref()),
                    },
                )
                .await;
                if outcome.batches > 0 {
                    waddle_xmpp::telemetry::reliability::add_pending_flush_batches(u64::from(
                        outcome.batches,
                    ));
                }
                if outcome.pushed > 0 {
                    waddle_xmpp::telemetry::reliability::add_pending_flush_rows_pushed(u64::from(
                        outcome.pushed,
                    ));
                }
                if outcome.deferred_transient > 0 && outcome.deferred_ordering == 0 {
                    // Issue #1122 follow-up (R2): the flush hit a transient MAM
                    // failure and released the failing row plus the rest of the
                    // batch. `claim_offline_flush` is a once-per-connection CAS,
                    // so without a reset those rows would wait for a full
                    // reconnect. Re-open the CAS (re-acquiring the entry, which
                    // may be gone if the session was superseded) so the client's
                    // next presence update re-attempts the flush.
                    if let Some(entry) = registry.entry_if_owner(&resource, &owner) {
                        entry.reset_offline_flush();
                    }
                }
                if outcome.claimed > 0 {
                    info!(
                        jid = %resource,
                        claimed = outcome.claimed,
                        batches = outcome.batches,
                        pushed = outcome.pushed,
                        unresolved = outcome.unresolved,
                        deferred_transient = outcome.deferred_transient,
                        "XEP-0160 pending_delivery flush completed"
                    );
                }
            }
        }
        .instrument(flush_span),
    );
}

struct InitialProbeDeps {
    app_state: std::sync::Arc<crate::server::AppState>,
    connection_registry: std::sync::Arc<waddle_xmpp::registry::ConnectionRegistry>,
    sm_session_registry: std::sync::Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    served: Option<crate::destination::ServedDomains>,
}

/// RFC 6121 §4.2.2 & §4.3.1: on a resource's initial available presence
/// (transitioning from unavailable to available), deliver the current presence
/// of the user's other available resources and probe subscribed contacts
/// (`subscription` in `to`/`both`) off the connection task so large rosters
/// cannot self-deadlock the connection's 256-slot outbound mpsc (issue #1220).
fn spawn_initial_presence_probes(
    state: &WebSocketState,
    sender_jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    ordered_relay_origin: Option<&OrderedRelayRouteOrigin>,
) {
    let deps = InitialProbeDeps {
        app_state: std::sync::Arc::clone(&state.deps.app_state),
        connection_registry: std::sync::Arc::clone(&state.deps.protocol.connection_registry),
        sm_session_registry: std::sync::Arc::clone(&state.deps.protocol.sm_session_registry),
        served: state
            .deps
            .service_domains
            .served_domains(state.deps.auth_state.xmpp_domain.as_str()),
    };
    let sender_jid = sender_jid.clone();
    let owner = *owner;
    let ordered_relay_origin = ordered_relay_origin.cloned();
    let probe_span = tracing::Span::current();
    tokio::spawn(
        async move {
            run_initial_presence_probes(&deps, &sender_jid, &owner, ordered_relay_origin.as_ref())
                .await;
        }
        .instrument(probe_span),
    );
}

async fn run_initial_presence_probes(
    deps: &InitialProbeDeps,
    sender_jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    ordered_relay_origin: Option<&OrderedRelayRouteOrigin>,
) {
    if deps
        .connection_registry
        .entry_if_owner(sender_jid, owner)
        .is_none()
    {
        return;
    }
    let sender_bare = sender_jid.to_bare();

    // RFC 6121 §4.2.2: deliver current presence of the user's other available
    // resources to the newly available resource.
    deliver_local_contact_presence_to_owner(deps, &sender_bare, sender_jid, owner, true, None)
        .await;

    let storage = deps.app_state.db_pool.roster_storage();
    let contacts = match storage.get_presence_subscription_jids(&sender_bare).await {
        Ok(contacts) => contacts,
        Err(error) => {
            warn!(
                error = %error,
                jid = %sender_jid,
                "Failed to load presence subscriptions for initial probes"
            );
            return;
        }
    };
    if contacts.is_empty() {
        return;
    }

    let blocking = deps.app_state.db_pool.blocking_storage();
    let sender_blocklist = match blocking.load_blocklist(&sender_bare).await {
        Ok(blocklist) => blocklist,
        Err(error) => {
            warn!(
                error = %error,
                jid = %sender_jid,
                "Failed to load sender blocklist for initial presence probes; skipping fail-closed"
            );
            return;
        }
    };
    let candidates: Vec<BareJid> = contacts
        .into_iter()
        .filter(|contact| {
            if *contact == sender_bare {
                return false;
            }
            let contact_jid = Jid::from(contact.clone());
            // Remote contacts are probed over S2S by `spawn_probe_remote_contacts`.
            let is_remote = deps.served.as_ref().is_some_and(|served| {
                matches!(
                    served.classify(Some(&contact_jid)).unserved(),
                    Some(crate::destination::UnservedDestination::Remote(_))
                )
            });
            !is_remote && !sender_blocklist.contains_jid(&contact_jid)
        })
        .collect();
    if candidates.is_empty() {
        return;
    }

    // XEP-0191 §3.3: pass the connecting full JID so exact full-JID blocks
    // (as well as bare-JID and domain blocks) on the contact's blocklist match.
    let sender_full_jid = Jid::from(sender_jid.clone());
    let blocked_by_contacts = match blocking
        .filter_blocking_recipients(&candidates, &sender_full_jid)
        .await
    {
        Ok(blocked) => blocked,
        Err(error) => {
            warn!(
                error = %error,
                jid = %sender_jid,
                "Failed to batch-check contact blocklists for initial presence probes; skipping fail-closed"
            );
            return;
        }
    };

    let targets: Vec<BareJid> = candidates
        .into_iter()
        .filter(|contact| !blocked_by_contacts.contains(contact))
        .collect();
    if targets.is_empty() {
        return;
    }

    let sender_blocklist_ref = &sender_blocklist;
    futures::stream::iter(targets)
        .for_each_concurrent(PRESENCE_FANOUT_CONCURRENCY, |contact| async move {
            if deps
                .connection_registry
                .entry_if_owner(sender_jid, owner)
                .is_none()
            {
                return;
            }
            let mut probe =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Probe);
            probe.from = Some(Jid::from(sender_jid.clone()));
            probe.to = Some(Jid::from(contact.clone()));
            let probe_stanza = Stanza::Presence(probe);
            if super::subscription::try_route_presence_to_bare_remote_for_app_state(
                &deps.app_state,
                &contact,
                &probe_stanza,
                ordered_relay_origin,
            )
            .await
            {
                return;
            }
            deliver_local_contact_presence_to_owner(
                deps,
                &contact,
                sender_jid,
                owner,
                false,
                Some(sender_blocklist_ref),
            )
            .await;
        })
        .await;
}

async fn deliver_local_contact_presence_to_owner(
    deps: &InitialProbeDeps,
    contact: &BareJid,
    recipient_full: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    exclude_recipient: bool,
    recipient_blocklist: Option<&waddle_xmpp::protocol::Blocklist>,
) {
    let available =
        crate::server::routes::websocket::handlers::account_reads::available_resources_with_presence_for_app_state(
            &deps.app_state,
            contact,
        )
        .await;
    let mut detached_available = match deps
        .sm_session_registry
        .available_detached_presence_states_for_user(contact)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(
                error = %error,
                contact = %contact,
                "Failed to list detached resources for initial presence probe"
            );
            Vec::new()
        }
    };
    detached_available.retain(|detached| {
        !(exclude_recipient && detached.resource == *recipient_full)
            && !recipient_blocklist.is_some_and(|blocklist| {
                blocklist.contains_jid(&Jid::from(detached.resource.clone()))
            })
            && !available
                .iter()
                .any(|(live_resource, _)| *live_resource == detached.resource)
    });

    let recipient_bare = recipient_full.to_bare();
    let registry = &deps.connection_registry;

    for detached in detached_available {
        let mut presence = build_available_presence(
            &detached.resource,
            &recipient_bare,
            detached.show.as_ref().map(show_name),
            detached.status.as_deref(),
            detached.priority,
        );
        presence.to = Some(Jid::from(recipient_full.clone()));
        presence.payloads.extend(detached.payloads);
        let _ = registry
            .send_to_if_owner(recipient_full, owner, Stanza::Presence(presence))
            .await;
    }

    for (resource, presence_state) in available {
        if exclude_recipient && resource == *recipient_full {
            continue;
        }
        if recipient_blocklist
            .is_some_and(|blocklist| blocklist.contains_jid(&Jid::from(resource.clone())))
        {
            continue;
        }
        let mut presence = build_available_presence(
            &resource,
            &recipient_bare,
            presence_state
                .as_ref()
                .and_then(|stored| stored.show.as_deref()),
            presence_state
                .as_ref()
                .and_then(|stored| stored.status.as_deref()),
            presence_state
                .as_ref()
                .map(|stored| stored.priority)
                .unwrap_or(0),
        );
        presence.to = Some(Jid::from(recipient_full.clone()));
        if let Some(stored) = &presence_state {
            presence.payloads.extend(stored.payloads.iter().cloned());
        }
        let _ = registry
            .send_to_if_owner(recipient_full, owner, Stanza::Presence(presence))
            .await;
    }
}

async fn broadcast_presence_to_subscribers(
    state: &WebSocketState,
    sender_jid: &FullJid,
    presence: &xmpp_parsers::presence::Presence,
    ordered_relay_origin: Option<&OrderedRelayRouteOrigin>,
) {
    let Some(storage) = roster_storage(state).await else {
        return;
    };
    let sender_bare = sender_jid.to_bare();
    let subscribers = match storage.get_presence_subscribers(&sender_bare).await {
        Ok(subscribers) => subscribers,
        Err(error) => {
            warn!(error = %error, jid = %sender_jid, "Failed to load presence subscribers");
            return;
        }
    };
    if subscribers.is_empty() {
        return;
    }

    let blocking = blocking_storage(state);
    let sender_blocklist = match blocking.load_blocklist(&sender_bare).await {
        Ok(blocklist) => blocklist,
        Err(error) => {
            warn!(
                error = %error,
                jid = %sender_jid,
                "Failed to load sender blocklist for presence broadcast; skipping fail-closed"
            );
            return;
        }
    };
    let candidates: Vec<BareJid> = subscribers
        .into_iter()
        .filter(|subscriber| !sender_blocklist.contains_jid(&Jid::from(subscriber.clone())))
        .collect();
    if candidates.is_empty() {
        return;
    }

    let (remote_candidates, local_candidates): (Vec<BareJid>, Vec<BareJid>) = candidates
        .into_iter()
        .partition(|subscriber| super::remote::is_remote_contact(state, subscriber));

    let mut targets = remote_candidates;
    if !local_candidates.is_empty() {
        let sender_full_jid = Jid::from(sender_jid.clone());
        let blocked_by_subscribers = match blocking
            .filter_blocking_recipients(&local_candidates, &sender_full_jid)
            .await
        {
            Ok(blocked) => blocked,
            Err(error) => {
                warn!(
                    error = %error,
                    jid = %sender_jid,
                    "Failed to batch-check subscriber blocklists for presence broadcast; skipping fail-closed"
                );
                return;
            }
        };
        targets.extend(
            local_candidates
                .into_iter()
                .filter(|subscriber| !blocked_by_subscribers.contains(subscriber)),
        );
    }
    if targets.is_empty() {
        return;
    }

    let sender_blocklist_ref = &sender_blocklist;
    futures::stream::iter(targets)
        .for_each_concurrent(PRESENCE_FANOUT_CONCURRENCY, |subscriber_bare| async move {
            // RFC 6121 §4.4.2: relay the user's own stanza (readdressed), never a
            // rebuild — a rebuild drops extensions and would misattribute the
            // server's XEP-0115 caps to the contact (issue #1101).
            let mut relayed = presence.clone();
            relayed.from = Some(Jid::from(sender_jid.clone()));
            relayed.to = Some(Jid::from(subscriber_bare.clone()));
            // A remote contact's server receives the presence through the
            // remote route (RFC 6121 §4.2.2, §4.4.2; gate G18). A refusal, such
            // as a full queue for the contact's domain, drops it.
            if super::remote::is_remote_contact(state, &subscriber_bare) {
                let _ = super::remote::offer_presence(state, relayed).await;
                return;
            }
            let stanza = Stanza::Presence(relayed);
            if super::subscription::try_route_presence_to_bare_remote(
                state,
                &subscriber_bare,
                &stanza,
                ordered_relay_origin,
            )
            .await
            {
                return;
            }
            let live_resources: Vec<FullJid> =
                crate::server::routes::websocket::handlers::account_reads::available_resources(
                    state,
                    &subscriber_bare,
                )
                .await
                .into_iter()
                .map(|(jid, _)| jid)
                .collect();
            let mut excluded_resources = Vec::with_capacity(live_resources.len());
            for resource in live_resources {
                if sender_blocklist_ref.contains_jid(&Jid::from(resource.clone())) {
                    excluded_resources.push(resource);
                    continue;
                }
                match state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&resource, stanza.clone())
                {
                    waddle_xmpp::registry::BroadcastOutcome::Delivered
                    | waddle_xmpp::registry::BroadcastOutcome::DroppedFull => {
                        excluded_resources.push(resource);
                    }
                    waddle_xmpp::registry::BroadcastOutcome::NotConnected
                    | waddle_xmpp::registry::BroadcastOutcome::DroppedClosed => {}
                }
            }
            // Exclude delivered and backpressured live resources (which are not
            // detached), while allowing NotConnected / DroppedClosed resources
            // that detached mid-fanout to fall through to detached SM recording.
            record_stanza_for_detached_available_resources_excluding(
                state,
                &subscriber_bare,
                &stanza,
                "presence broadcast",
                &excluded_resources,
            )
            .await;
        })
        .await;
}

/// XEP-0115: drive caps resolution for an inbound `<presence>`
/// carrying `<c hash node ver/>`.
///
/// - Cache hit on `(hash, ver)`: record the per-resource mapping so
///   the next PEP fan-out (PR 2) can filter by feature list.
/// - Cache miss with supported `hash`: send the resource a typed
///   disco#info IQ get with `node="<NODE>#<VER>"` per §6.2 and remember
///   the in-flight ver. If the outbound write fails (resource just
///   went offline / channel closed) the pending entry is immediately
///   evicted so the in-memory pending map cannot leak.
/// - Cache miss with unsupported `hash` (e.g. `sha-256`): per §5.4
///   step 2 the server MUST NOT cache. We additionally skip the
///   round-trip entirely — fan-out will treat this resource as
///   "caps unknown" until it next advertises with a supported algo.
async fn resolve_caps_for_presence(
    state: &WebSocketState,
    sender_jid: &FullJid,
    presence: &xmpp_parsers::presence::Presence,
) {
    let Some(caps) = extract_caps_payload(presence) else {
        return;
    };
    let resolver = &state.deps.protocol.caps_resolver;
    let cache_key = waddle_xmpp::xep::xep0115::CapsCacheKey::from_caps(&caps);
    if resolver.cache().contains(&cache_key) {
        resolver.record_resource(sender_jid, cache_key);
        return;
    }
    if !crate::server::caps_resolution::is_supported_hash(&caps.hash) {
        debug!(
            jid = %sender_jid,
            hash = %caps.hash,
            "Skipping caps resolution: advertised hash algorithm not implemented (XEP-0115 §5.4 step 2)"
        );
        return;
    }
    // Dedup in-flight resolutions per (full_jid, hash, ver). Without
    // this guard, a client spamming presence updates with random
    // `ver` values (or re-advertising the same uncached ver before
    // its disco#info reply lands) could grow the pending map
    // unboundedly and amplify outbound disco#info traffic. PR #438
    // review issue (Qodo / Copilot).
    if resolver.has_pending_for(sender_jid, &caps) {
        debug!(
            jid = %sender_jid,
            hash = %caps.hash,
            ver = %caps.ver,
            "Skipping caps disco#info: a resolution is already in flight for this (jid, hash, ver)"
        );
        return;
    }
    let iq_id = format!("waddle-caps-disco-{}", uuid::Uuid::new_v4());
    let iq = build_caps_disco_info_query(
        &state.deps.auth_state.caps_server_domain,
        sender_jid,
        &caps,
        &iq_id,
    );
    resolver.begin_pending(iq_id.clone(), sender_jid.clone(), caps);
    let send_outcome = state
        .deps
        .protocol
        .connection_registry
        .send_to(sender_jid, Stanza::Iq(Box::new(iq)))
        .await;
    if !send_outcome.is_sent() {
        // PR #438 review issue #5: if the recipient already
        // disconnected between the presence-receipt and the
        // disco#info IQ write, evict the orphan pending entry
        // immediately rather than waiting for the next disconnect
        // event.
        debug!(
            jid = %sender_jid,
            iq_id = %iq_id,
            "Caps disco#info send failed; evicting orphaned pending entry"
        );
        let _ = resolver.take_pending(&iq_id);
    }
}

pub(in crate::server::routes::websocket::handlers) fn show_name(
    show: &xmpp_parsers::presence::Show,
) -> &'static str {
    match show {
        xmpp_parsers::presence::Show::Away => "away",
        xmpp_parsers::presence::Show::Chat => "chat",
        xmpp_parsers::presence::Show::Dnd => "dnd",
        xmpp_parsers::presence::Show::Xa => "xa",
    }
}

fn priority_to_i8(priority: &xmpp_parsers::presence::Priority) -> i8 {
    // `Priority` wraps an `i8` whose inner field is crate-private; round-trip
    // through the XML element to read the value back as an integer.
    let element: minidom::Element = priority.into();
    element.text().trim().parse::<i8>().unwrap_or(0)
}

pub(super) fn show_from_name(value: &str) -> Option<xmpp_parsers::presence::Show> {
    match value {
        "away" => Some(xmpp_parsers::presence::Show::Away),
        "chat" => Some(xmpp_parsers::presence::Show::Chat),
        "dnd" => Some(xmpp_parsers::presence::Show::Dnd),
        "xa" => Some(xmpp_parsers::presence::Show::Xa),
        _ => None,
    }
}
