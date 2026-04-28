//! Effect interpreter for [`waddle_xmpp::protocol::OutboundEvent`].
//!
//! The state machine in `waddle-xmpp::protocol` is pure and synchronous —
//! it emits typed outbound events that *describe* side effects but does
//! not perform them. This module is the async counterpart: it
//! pattern-matches each event and runs the real operation against the
//! transport, the connection registry, MUC rooms, MAM storage, the SFU
//! actor, etc.
//!
//! # Typed payloads at the I/O boundary
//!
//! Per the project's typed-payloads hard rule, stanzas travel through
//! the state machine as typed values (`Stanza`, `Iq`, `Message`).
//! Serialization to the XML wire format happens here, exactly once,
//! when we hand bytes off to the transport.
//!
//! # Current coverage
//!
//! Wired:
//! - [`OutboundEvent::SendStanza`] — serialize and emit on the local wire.
//! - [`OutboundEvent::CloseTransport`] — signal the main loop to close.
//! - [`OutboundEvent::Log`] — route through `tracing`.
//! - [`OutboundEvent::RouteToConnection`] — legacy
//!   `ConnectionRegistry::send_to` semantics; the recipient-pass
//!   pipeline activation arrives in a later cutover PR.
//! - [`OutboundEvent::SendCarbons`] — XEP-0280 carbon fan-out via the
//!   XEP-0297 `<sent>`/`<received>` envelope, including detached
//!   XEP-0198 resumable sessions.
//! - [`OutboundEvent::ArchiveDirect`] — XEP-0313 §5.1 personal MAM
//!   write keyed under `archive_jid`. Eligibility was vetted by
//!   [`waddle_xmpp::protocol::handlers::archive::ArchiveHandler`].
//! - [`OutboundEvent::ProjectInbox`] — Waddle inbox upsert keyed by
//!   `(owner, peer)` with `archive_ref` linking back to the MAM entry.
//! - [`OutboundEvent::UnregisterConnection`] — drop from the registry.
//! - [`OutboundEvent::LookupArchivedMessage`] — XEP-0359 stanza-id /
//!   origin-id lookup against personal MAM; result feeds back as
//!   [`InboundEvent::ArchivedMessageLoaded`] in
//!   [`InterpretOutcome::feedback`].
//! - [`OutboundEvent::RequestEnrichment`] — XEP-0372 link enrichment
//!   via `ExtensionManager`; result feeds back as
//!   [`InboundEvent::EnrichmentComplete`] in
//!   [`InterpretOutcome::feedback`]. Fail-open semantics match legacy.
//!
//! Stubbed (warn-logged until migration steps land them):
//! - `BroadcastToRoom`, `DispatchToRoom`, `ArchiveGroupchat` — MUC
//!   handler chain (PR10).
//! - `AskSfu`, `QueryMam`, `LoadScramCredentials`,
//!   `ValidateOAuthBearer`, `SetTimer`, `CancelTimer`,
//!   `RegisterConnection` — wired in later migration steps.

use std::str::FromStr;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use waddle_extensions::ExtensionManager;
use waddle_xmpp::carbons::{build_received_carbon, build_sent_carbon};
use waddle_xmpp::inbox::runtime::direct_message_entry;
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::mam::projection::build_direct_archived_message;
use waddle_xmpp::mam::storage::MamStorage;
use waddle_xmpp::mam::ArchivedMessage as MamArchivedMessage;
use waddle_xmpp::parser::stanza_to_string;
use waddle_xmpp::protocol::event::{
    ArchivedMessage as ProtocolArchivedMessage, InboundEvent, MessageRef, StanzaIdRef,
    StanzaIdValue,
};
use waddle_xmpp::protocol::{CarbonKind, OutboundEvent};
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;

/// Outcome of interpreting a batch of [`OutboundEvent`]s.
///
/// The WebSocket transport uses `frames` to decide what to write back to
/// the client. `close` signals the main loop should drop the connection.
/// `feedback` carries [`InboundEvent`] callback completions the state
/// machine must consume to resume parked dispatches (XEP-0359
/// rich-target lookup, link enrichment). The caller is responsible for
/// pumping `feedback` back through `state_machine.handle(...)` until
/// the resulting outcome's `feedback` drains — see #229 PR9 cutover.
#[derive(Debug, Default)]
pub struct InterpretOutcome {
    /// Serialized XML frames to write to the transport, in order.
    pub frames: Vec<String>,
    /// Set to true when the state machine asked us to close the transport.
    pub close: bool,
    /// Async-callback completions to feed back to the state machine.
    pub feedback: Vec<InboundEvent>,
}

/// Typed dependency context for the interpreter.
///
/// Grows as later migration steps add storage/actor handles
/// (`extension_manager`, etc.). Threading dependencies through one
/// struct rather than as loose function parameters keeps the call-site
/// churn small.
#[derive(Clone)]
pub struct Deps<'a> {
    pub connection_registry: &'a ConnectionRegistry,
    /// XEP-0198 stream-management session registry. Used to fan
    /// XEP-0280 carbons out to *detached but resumable* resources so
    /// briefly-disconnected secondary devices don't lose carbon
    /// history while resumable. `None` in unit tests that don't
    /// exercise SM behaviour.
    pub sm_session_registry: Option<&'a Arc<InMemorySmSessionRegistry>>,
    /// XEP-0313 MAM persistence backend. `None` in unit tests that
    /// don't exercise archive writes; production wiring (`iq.rs`)
    /// always supplies it.
    pub mam_storage: Option<&'a Arc<dyn MamStorage>>,
    /// Waddle inbox-projection backend. `None` in unit tests; always
    /// supplied in production.
    pub inbox_storage: Option<&'a Arc<dyn InboxStorage>>,
    /// Wasm-extension manager for XEP-0372/etc. link enrichment.
    /// `None` in unit tests that don't exercise the
    /// [`OutboundEvent::RequestEnrichment`] arm.
    pub extension_manager: Option<&'a Arc<ExtensionManager>>,
}

impl<'a> Deps<'a> {
    /// Build a minimal `Deps` with only the connection registry — a
    /// test-only convenience for unit tests that don't exercise SM
    /// fan-out, archive, or inbox storage.
    #[cfg(test)]
    pub fn registry_only(connection_registry: &'a ConnectionRegistry) -> Self {
        Self {
            connection_registry,
            sm_session_registry: None,
            mam_storage: None,
            inbox_storage: None,
            extension_manager: None,
        }
    }

    /// Build a `Deps` for unit tests that exercise the storage arms
    /// (`ArchiveDirect`, `ProjectInbox`). SM fan-out is left disabled
    /// so the carbon-detached path stays an independent test concern.
    #[cfg(test)]
    pub fn test_with_storage(
        connection_registry: &'a ConnectionRegistry,
        mam_storage: &'a Arc<dyn MamStorage>,
        inbox_storage: &'a Arc<dyn InboxStorage>,
    ) -> Self {
        Self {
            connection_registry,
            sm_session_registry: None,
            mam_storage: Some(mam_storage),
            inbox_storage: Some(inbox_storage),
            extension_manager: None,
        }
    }

    /// Build a `Deps` for unit tests that exercise the
    /// [`OutboundEvent::RequestEnrichment`] arm.
    #[cfg(test)]
    pub fn test_with_extension_manager(
        connection_registry: &'a ConnectionRegistry,
        extension_manager: &'a Arc<ExtensionManager>,
    ) -> Self {
        Self {
            connection_registry,
            sm_session_registry: None,
            mam_storage: None,
            inbox_storage: None,
            extension_manager: Some(extension_manager),
        }
    }
}

/// Execute the side effects described by `events`.
///
/// The function is `async` because future migration steps add variants
/// that genuinely require `.await` (registry lookups, actor calls, MAM
/// storage). The currently-supported variants are all synchronous, so this
/// function will return immediately for the ping/session flow.
pub async fn interpret(events: Vec<OutboundEvent>, deps: &Deps<'_>) -> InterpretOutcome {
    let registry = deps.connection_registry;
    let mut outcome = InterpretOutcome::default();

    for event in events {
        match event {
            OutboundEvent::SendStanza(stanza) => match stanza.to_element_string() {
                Ok(xml) => outcome.frames.push(xml),
                Err(err) => {
                    error!(error = %err, "failed to serialize outbound stanza; dropping frame");
                }
            },
            OutboundEvent::CloseTransport => {
                outcome.close = true;
            }
            OutboundEvent::Log { level, message } => {
                // Route the log back through tracing so it ends up in the
                // application's log pipeline. We format the state-machine
                // message into the event text (via `%message`) rather than
                // as a structured field so it renders the same as the rest
                // of the codebase's logs.
                match level {
                    tracing::Level::ERROR => error!(%message, "protocol"),
                    tracing::Level::WARN => warn!(%message, "protocol"),
                    tracing::Level::INFO => info!(%message, "protocol"),
                    tracing::Level::DEBUG | tracing::Level::TRACE => {
                        debug!(%message, "protocol")
                    }
                }
            }

            // -------------------------------------------------------
            // Variants defined for future migration steps. We log only the
            // variant discriminant (and, where cheap, typed identifiers
            // like JIDs or stanza ids) — never the typed payload. Some of
            // these variants carry `Message` / `Iq` structs containing
            // user content, and their `Debug` impls would leak that
            // content into logs.
            // -------------------------------------------------------
            OutboundEvent::RouteToConnection { jid, stanza } => {
                // #229 PR12 cutover: the destination's main loop is
                // now wired (PR11) to dispatch on `DeliveryKind` and
                // run the recipient-pass pipeline for `PeerStanza`
                // values, so we deliver as `PeerStanza`. The
                // recipient's `XmppStateMachine::on_peer_stanza`
                // takes it from there: XEP-0191 incoming block,
                // XEP-0359 recipient stamp, XEP-0313 recipient-side
                // archive, XEP-0280 received-carbons, inbox
                // projection, then `SendStanza` to the wire.
                //
                // `jid` is a typed `Jid` — full or bare. Full-JID
                // targets deliver to that single resource. Bare-JID
                // targets go through RFC 6121 §8.5.2.1 resource
                // selection (highest-priority available resources;
                // tie-broken by delivering to all of them).
                //
                // Known design gap (`#229` follow-up): when the
                // target has *no* connected resources (offline /
                // detached / `ChannelClosed` for every selected
                // resource), recipient-side persistence
                // (recipient MAM archive, inbox projection,
                // received-carbons fan-out) is currently lost
                // because those effects only run during a recipient
                // pass that requires a live destination machine.
                // The eventual `message.rs` cutover needs an offline
                // fallback (sender-side recipient-archive write OR a
                // headless recipient-pass runner). Tracked in the
                // local plan and the issue's follow-ups.
                match jid.clone().try_into_full() {
                    Ok(full) => deliver_peer_to_full(registry, &full, *stanza).await,
                    Err(bare) => {
                        let targets = registry.select_routable_resources_for_user(&bare);
                        if targets.is_empty() {
                            debug!(
                                bare_jid = %bare,
                                "RouteToConnection: bare-JID has no available resources \
                                 (RFC 6121 §8.5.2.1.1); dropping. Offline-recipient \
                                 persistence is a known #229 follow-up."
                            );
                        } else {
                            for full in targets {
                                deliver_peer_to_full(registry, &full, (*stanza).clone()).await;
                            }
                        }
                    }
                }
            }
            OutboundEvent::DispatchToRoom { room, .. } => {
                warn!(
                    variant = "DispatchToRoom",
                    room = %room,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::BroadcastToRoom { room, .. } => {
                warn!(
                    variant = "BroadcastToRoom",
                    room = %room,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::ProjectInbox {
                owner,
                peer,
                message,
                archive_ref,
                increment_unread,
            } => {
                let Some(inbox_storage) = deps.inbox_storage else {
                    debug!(
                        owner = %owner,
                        peer = %peer,
                        "ProjectInbox: no inbox_storage in Deps; skipping (test fixture?)"
                    );
                    continue;
                };
                // Build the inbox entry from the typed message, then
                // overwrite its stanza-id with the typed `archive_ref`
                // so the inbox row links to the canonicalized MAM
                // entry the handler stamped (rather than re-deriving
                // from the wire `<message id=...>`).
                let timestamp = chrono::Utc::now().timestamp();
                let mut entry = direct_message_entry(peer.clone(), &message, timestamp);
                entry.last_stanza_id = archive_ref.id.as_str().to_string();
                if let Err(error) = inbox_storage.upsert(&owner, entry, increment_unread).await {
                    warn!(
                        owner = %owner,
                        peer = %peer,
                        %error,
                        "ProjectInbox: inbox upsert failed; dropping projection"
                    );
                } else {
                    debug!(
                        owner = %owner,
                        peer = %peer,
                        archive_ref = archive_ref.id.as_str(),
                        increment_unread,
                        "ProjectInbox: persisted"
                    );
                }
            }
            OutboundEvent::SendCarbons {
                owner,
                message,
                kind,
                exclude,
            } => {
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
                    continue;
                }
                for target in live_targets {
                    let envelope = match build_carbon_envelope(kind, &message, &owner_str, &target)
                    {
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
                        let envelope =
                            match build_carbon_envelope(kind, &message, &owner_str, &target) {
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
                            .record_stanza_for_detached_bound_resource(&target, &stanza)
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
            OutboundEvent::LookupArchivedMessage {
                id,
                archive,
                reference,
            } => {
                let result = lookup_archived_message(deps, &archive, &reference).await;
                outcome
                    .feedback
                    .push(InboundEvent::ArchivedMessageLoaded { id, result });
            }
            OutboundEvent::RegisterConnection(jid) => {
                warn!(
                    variant = "RegisterConnection",
                    jid = %jid,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::UnregisterConnection(jid) => {
                let _entry = registry.unregister(&jid);
                debug!(jid = %jid, "UnregisterConnection: removed from registry");
            }
            OutboundEvent::ArchiveGroupchat { room, sender, .. } => {
                warn!(
                    variant = "ArchiveGroupchat",
                    room = %room,
                    sender = %sender,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::ArchiveDirect {
                archive_jid,
                from,
                to,
                message,
            } => {
                let Some(mam_storage) = deps.mam_storage else {
                    debug!(
                        archive_jid = %archive_jid,
                        from = %from,
                        to = %to,
                        "ArchiveDirect: no mam_storage in Deps; skipping (test fixture?)"
                    );
                    continue;
                };
                // Per XEP-0313 §5.1.3, the eligibility check is
                // upstream (ArchiveHandler) — the interpreter just
                // persists. The handler also already canonicalized the
                // XEP-0359 `<stanza-id by=archive_jid/>` stamp on the
                // typed message, so the projection serializer captures
                // it for replay.
                let archived = build_direct_archived_message(
                    &archive_jid.to_string(),
                    &from.to_string(),
                    &to.to_string(),
                    &message,
                );
                let archive_jid_str = archive_jid.to_string();
                match mam_storage
                    .store_message(archive_jid_str.as_str(), &archived)
                    .await
                {
                    Ok(archive_id) => {
                        debug!(
                            archive_jid = %archive_jid,
                            archive_id,
                            "ArchiveDirect: persisted"
                        );
                    }
                    Err(error) => {
                        // Archive errors must not block dispatch — the
                        // message is already on the wire to other
                        // resources via routing/carbons. Log and drop.
                        warn!(
                            archive_jid = %archive_jid,
                            from = %from,
                            to = %to,
                            %error,
                            "ArchiveDirect: store_message failed; dropping archive write"
                        );
                    }
                }
            }
            OutboundEvent::RequestEnrichment { id, message } => {
                let enriched = enrich_message_event(deps, *message).await;
                outcome.feedback.push(InboundEvent::EnrichmentComplete {
                    id,
                    message: Box::new(enriched),
                });
            }
            OutboundEvent::AskSfu { id, .. } => {
                warn!(
                    variant = "AskSfu",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::QueryMam { id, .. } => {
                warn!(
                    variant = "QueryMam",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::LoadScramCredentials { id, .. } => {
                warn!(
                    variant = "LoadScramCredentials",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::ValidateOAuthBearer { id, .. } => {
                warn!(
                    variant = "ValidateOAuthBearer",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::SetTimer { id, duration_ms } => {
                warn!(
                    variant = "SetTimer",
                    timer_id = id.0,
                    duration_ms,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::CancelTimer(id) => {
                warn!(
                    variant = "CancelTimer",
                    timer_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
        }
    }

    outcome
}

/// Run XEP-0372 link enrichment against a typed message and return
/// the rewritten value. Fails open: when no extension manager is
/// supplied (test fixture, or a connection that opted out) or any
/// extension actor returns an error, the original message is
/// returned unchanged. This matches legacy `message.rs` behavior so
/// the dispatcher cutover (#229 PR9) does not regress UX when a wasm
/// extension is misbehaving.
/// Deliver a single `Stanza` to a specific full-JID destination as
/// a [`waddle_xmpp::registry::DeliveryKind::PeerStanza`] so the
/// destination's main loop runs the recipient pass before any wire
/// write. Centralizes the per-target send + result-logging shape so
/// both the full-JID and bare-JID-resource-selection arms of
/// [`OutboundEvent::RouteToConnection`] go through the same path.
async fn deliver_peer_to_full(
    registry: &waddle_xmpp::registry::ConnectionRegistry,
    target: &jid::FullJid,
    stanza: Stanza,
) {
    match registry.send_peer_to(target, stanza).await {
        waddle_xmpp::registry::SendResult::Sent => {
            debug!(jid = %target, "RouteToConnection: peer-stanza queued for recipient pass");
        }
        waddle_xmpp::registry::SendResult::NotConnected => {
            debug!(jid = %target, "RouteToConnection: target offline, dropping");
        }
        waddle_xmpp::registry::SendResult::ChannelClosed => {
            warn!(jid = %target, "RouteToConnection: target channel closed, dropping");
        }
    }
}

async fn enrich_message_event(deps: &Deps<'_>, mut message: Message) -> Message {
    let Some(extension_manager) = deps.extension_manager else {
        debug!(
            "RequestEnrichment: no extension_manager in Deps; \
             feeding original message back unchanged"
        );
        return message;
    };
    // `enrich_message` mutates in place and is itself fail-open
    // (returns 0 when no body / no links / no extension actors). Any
    // per-actor RPC failure inside is logged by the extension layer.
    let _added = extension_manager.enrich_message(&mut message).await;
    message
}

/// Resolve a typed [`MessageRef`] against `archive`'s personal MAM
/// archive and project the storage row into the typed protocol
/// [`ProtocolArchivedMessage`] shape the
/// [`waddle_xmpp::protocol::handlers::rich_target_validation::RichTargetValidationHandler`]
/// expects on the [`InboundEvent::ArchivedMessageLoaded`] callback.
///
/// Storage failures are demoted to `Ok(None)` with a WARN log so the
/// handler treats them as `<item-not-found>` per XEP-0308 / 0424 /
/// 0425 / 0461 — the same surface clients see when the target
/// genuinely doesn't exist. We do not propagate the storage error
/// shape into the callback because the protocol-side type does not
/// model it and the resulting reply would be the same.
async fn lookup_archived_message(
    deps: &Deps<'_>,
    archive: &jid::BareJid,
    reference: &MessageRef,
) -> Option<Box<ProtocolArchivedMessage>> {
    let Some(mam_storage) = deps.mam_storage else {
        debug!(
            archive = %archive,
            "LookupArchivedMessage: no mam_storage in Deps; treating as not-found"
        );
        return None;
    };
    let archive_str = archive.to_string();
    let lookup = match reference {
        MessageRef::StanzaId { id, .. } => {
            // Strict stanza-id match: `get_message_by_message_id`
            // matches only the `stanza_id` column (not `origin_id`)
            // so the OR-collision identified in #229 PR8 review
            // (origin-id colliding with someone else's stanza-id)
            // can't return the wrong row.
            mam_storage
                .get_message_by_message_id(&archive_str, id.as_str())
                .await
        }
        MessageRef::OriginId { sender, origin_id } => {
            // No origin-id-only accessor on `MamStorage` today, so we
            // narrow with `MamQuery.with = sender` (storage-level
            // sender filter) and pick the first row whose
            // `origin_id` matches the requested value. This enforces
            // the typed `MessageRef::OriginId` contract — scoped to
            // the *original sender*, not just any row sharing that
            // opaque value — without leaking through the
            // OR-collision in `get_message_by_stanza_id`.
            let query = waddle_xmpp::mam::MamQuery {
                with: Some(sender.to_string()),
                ..Default::default()
            };
            match mam_storage.query_messages(&archive_str, &query).await {
                Ok(result) => Ok(result
                    .messages
                    .into_iter()
                    .find(|row| row_matches_origin_id(row, sender, origin_id.as_str()))),
                Err(e) => Err(e),
            }
        }
    };
    match lookup {
        Ok(Some(row)) => project_archived_row(archive, row),
        Ok(None) => None,
        Err(error) => {
            warn!(
                archive = %archive,
                %error,
                "LookupArchivedMessage: storage error; treating as not-found"
            );
            None
        }
    }
}

/// Verify a storage row genuinely matches the typed
/// [`MessageRef::OriginId`] key — `origin_id` field equality plus
/// `sender` (`from`) equality via parsed [`jid::BareJid`] comparison.
/// String equality on `from` would mishandle case-folded localparts
/// or full-JID-vs-bare-JID strings; the typed compare normalizes both.
fn row_matches_origin_id(
    row: &MamArchivedMessage,
    expected_sender: &jid::BareJid,
    expected_origin_id: &str,
) -> bool {
    if row.origin_id.as_deref() != Some(expected_origin_id) {
        return false;
    }
    match row.from.parse::<jid::BareJid>() {
        Ok(bare) => bare == *expected_sender,
        Err(_) => false,
    }
}

/// Project a storage [`MamArchivedMessage`] row into the protocol-side
/// [`ProtocolArchivedMessage`] consumed by handler completions.
///
/// Falls back to an empty body-only stanza if `stanza_xml` is missing
/// or unparseable — the rich-target handler primarily inspects the
/// tombstone state and the sender's bare JID, both of which we can
/// reconstruct without the original wire form. Logs a WARN at each
/// projection failure mode so regressions stay observable.
fn project_archived_row(
    archive: &jid::BareJid,
    row: MamArchivedMessage,
) -> Option<Box<ProtocolArchivedMessage>> {
    let tombstoned = matches!(
        row.rich.as_ref().and_then(|r| r.payload.as_ref()),
        Some(waddle_xmpp::mam::ArchivedRichPayload::Tombstone(_))
    );

    let message = match parse_archived_message_xml(row.stanza_xml.as_deref()) {
        Some(m) => m,
        None => fallback_archived_message(&row),
    };

    let stamp_id = row
        .stanza_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| row.id.clone());
    let stanza_id = StanzaIdRef {
        by: archive.clone(),
        id: StanzaIdValue::new(&stamp_id),
    };

    Some(Box::new(ProtocolArchivedMessage {
        stanza_id,
        message: Box::new(message),
        tombstoned,
    }))
}

fn parse_archived_message_xml(xml: Option<&str>) -> Option<Message> {
    let xml = xml?;
    let element = match Element::from_str(xml) {
        Ok(e) => e,
        Err(error) => {
            warn!(
                %error,
                "LookupArchivedMessage: failed to parse stored stanza_xml; \
                 falling back to body-only reconstruction"
            );
            return None;
        }
    };
    let element_name = element.name().to_string();
    let element_ns = element.ns().to_string();
    match Message::try_from(element) {
        Ok(message) => Some(message),
        Err(error) => {
            warn!(
                %error,
                element_name = %element_name,
                element_ns = %element_ns,
                "LookupArchivedMessage: stored stanza_xml parsed but failed \
                 to convert into xmpp_parsers::message::Message; falling \
                 back to body-only reconstruction"
            );
            None
        }
    }
}

fn fallback_archived_message(row: &MamArchivedMessage) -> Message {
    let to: Option<jid::Jid> = row.to.parse().ok();
    let from: Option<jid::Jid> = row.from.parse().ok();
    let mut msg = Message::new(to);
    msg.from = from;
    msg.id = row.stanza_id.clone();
    if !row.body.is_empty() {
        msg.bodies
            .insert(String::new(), xmpp_parsers::message::Body(row.body.clone()));
    }
    msg
}

/// Build the XEP-0297-wrapped carbon envelope for `kind`. Pulled out
/// so the live-resources fan-out and the detached-XEP-0198 fan-out
/// share one builder.
fn build_carbon_envelope(
    kind: CarbonKind,
    original: &xmpp_parsers::message::Message,
    owner_bare: &str,
    target_full: &jid::FullJid,
) -> Result<xmpp_parsers::message::Message, jid::Error> {
    let target = target_full.to_string();
    match kind {
        CarbonKind::Sent => build_sent_carbon(original, owner_bare, &target),
        CarbonKind::Received => build_received_carbon(original, owner_bare, &target),
    }
}

/// Helper trait so the interpreter has a single, typed serialization
/// entry point for any `Stanza` leaving the state machine. Keeping it
/// private to this module prevents callers from serializing stanzas in
/// other spots — the I/O boundary stays narrow.
trait ToElementString {
    fn to_element_string(&self) -> Result<String, waddle_xmpp::XmppError>;
}

impl ToElementString for waddle_xmpp::Stanza {
    fn to_element_string(&self) -> Result<String, waddle_xmpp::XmppError> {
        use waddle_xmpp::Stanza;
        match self {
            Stanza::Iq(iq) => stanza_to_string(iq.clone()),
            Stanza::Message(msg) => stanza_to_string(msg.clone()),
            Stanza::Presence(p) => stanza_to_string(p.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::Stanza;
    use xmpp_parsers::iq::{Iq, IqType};
    use xmpp_parsers::minidom::Element;

    fn test_registry() -> ConnectionRegistry {
        ConnectionRegistry::new()
    }

    fn result_iq(id: &str) -> Iq {
        Iq {
            from: None,
            to: None,
            id: id.to_string(),
            payload: IqType::Result(Some(Element::builder("query", "jabber:iq:roster").build())),
        }
    }

    #[tokio::test]
    async fn interprets_send_stanza() {
        let events = vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq(
            "x",
        ))))];
        let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
        assert_eq!(outcome.frames.len(), 1);
        assert!(outcome.frames[0].contains("type=\"result\""));
        assert!(outcome.frames[0].contains("id=\"x\""));
        assert!(!outcome.close);
    }

    #[tokio::test]
    async fn interprets_close_transport() {
        let events = vec![OutboundEvent::CloseTransport];
        let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
        assert!(outcome.close);
        assert!(outcome.frames.is_empty());
    }

    #[tokio::test]
    async fn interprets_log_is_noop_for_caller() {
        let events = vec![OutboundEvent::Log {
            level: tracing::Level::INFO,
            message: "hello".to_string(),
        }];
        let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
        assert!(outcome.frames.is_empty());
        assert!(!outcome.close);
    }

    // -----------------------------------------------------------------
    // XEP-0280 — SendCarbons fan-out
    // -----------------------------------------------------------------

    fn chat_msg(from: &str, to: &str, body: &str) -> xmpp_parsers::message::Message {
        let mut m = xmpp_parsers::message::Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = xmpp_parsers::message::MessageType::Chat;
        m.bodies
            .insert(String::new(), xmpp_parsers::message::Body(body.to_string()));
        m
    }

    fn drain_inbound(
        rx: &mut tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza>,
    ) -> Vec<waddle_xmpp::registry::OutboundStanza> {
        let mut out = Vec::new();
        while let Ok(stanza) = rx.try_recv() {
            out.push(stanza);
        }
        out
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_fans_out_to_other_carbon_enabled_resources() {
        let registry = ConnectionRegistry::new();
        // Owner: alice. Two resources — web (originating, excluded)
        // and phone (carbon-enabled, expected target).
        let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("jid");
        let (_web_tx, _web_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_web.clone(), _web_tx, true);
        let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_phone.clone(), phone_tx, true);

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::SendCarbons {
            owner,
            message: Box::new(original),
            kind: CarbonKind::Sent,
            exclude: alice_web,
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        // Verify the XEP-0280 <sent xmlns='urn:xmpp:carbons:2'> wrapper and
        // its nested XEP-0297 <forwarded xmlns='urn:xmpp:forward:0'> payload.
        let received = drain_inbound(&mut phone_rx);
        assert_eq!(received.len(), 1, "alice/phone received one carbon");
        let stanza = &received[0].stanza;
        let msg = match stanza {
            Stanza::Message(m) => m,
            other => panic!("expected Message stanza, got {other:?}"),
        };
        let sent = msg
            .payloads
            .iter()
            .find(|p| p.name() == "sent" && p.ns() == "urn:xmpp:carbons:2")
            .expect("carbon must carry <sent xmlns='urn:xmpp:carbons:2'/>");
        assert!(
            sent.children()
                .any(|p| p.name() == "forwarded" && p.ns() == "urn:xmpp:forward:0"),
            "carbon <sent/> must carry <forwarded xmlns='urn:xmpp:forward:0'/>"
        );
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_skips_originating_resource() {
        let registry = ConnectionRegistry::new();
        let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let (web_tx, mut web_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_web.clone(), web_tx, true);

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::SendCarbons {
            owner,
            message: Box::new(original),
            kind: CarbonKind::Sent,
            exclude: alice_web,
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        // No carbon to alice/web — it's the originating resource.
        let received = drain_inbound(&mut web_rx);
        assert!(received.is_empty(), "originating resource excluded");
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_skips_resources_without_carbons_enabled() {
        let registry = ConnectionRegistry::new();
        let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("jid");
        let (_web_tx, _web_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_web.clone(), _web_tx, true);
        // alice/phone has carbons DISABLED.
        let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_phone.clone(), phone_tx, false);

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::SendCarbons {
            owner,
            message: Box::new(original),
            kind: CarbonKind::Sent,
            exclude: alice_web,
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        let received = drain_inbound(&mut phone_rx);
        assert!(received.is_empty(), "carbons-disabled resource skipped");
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_received_kind_emits_received_envelope() {
        let registry = ConnectionRegistry::new();
        let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
        let (_desk_tx, _desk_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob_desk.clone(), _desk_tx, true);
        let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob_phone.clone(), phone_tx, true);

        let owner: jid::BareJid = "bob@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::SendCarbons {
            owner,
            message: Box::new(original),
            kind: CarbonKind::Received,
            exclude: bob_desk,
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        let received = drain_inbound(&mut phone_rx);
        assert_eq!(received.len(), 1);
        let msg = match &received[0].stanza {
            Stanza::Message(m) => m,
            other => panic!("expected Message, got {other:?}"),
        };
        assert!(
            msg.payloads
                .iter()
                .any(|p| p.name() == "received" && p.ns() == "urn:xmpp:carbons:2"),
            "kind=Received emits <received xmlns='urn:xmpp:carbons:2'/>"
        );
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_queues_for_detached_xep_0198_resources() {
        // Regression test for the carbon-fan-out-skipping-detached-SM
        // bug: a XEP-0198-resumable session that briefly disconnected
        // must still receive its carbon copies via
        // record_stanza_for_detached_bound_resource so the queued
        // stanzas replay on resume. Without the detached pass, brief
        // disconnects silently lose carbon history.
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

        let registry = ConnectionRegistry::new();
        let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("jid");

        // alice/web: live, originating resource (excluded).
        let (_web_tx, _web_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_web.clone(), _web_tx, true);

        // alice/phone: detached, carbons-enabled, resumable via SM.
        let sm = Arc::new(InMemorySmSessionRegistry::new());
        let detached = DetachedSession {
            stream_id: "phone-stream-id".to_string(),
            user_id: "alice".to_string(),
            jid: alice_phone.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        };
        sm.store_session(detached).await.expect("store session");

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let deps = Deps {
            connection_registry: &registry,
            sm_session_registry: Some(&sm),
            mam_storage: None,
            inbox_storage: None,
            extension_manager: None,
        };
        let _outcome = interpret(
            vec![OutboundEvent::SendCarbons {
                owner: owner.clone(),
                message: Box::new(original),
                kind: CarbonKind::Sent,
                exclude: alice_web,
            }],
            &deps,
        )
        .await;

        // The detached resource should have a queued carbon ready
        // for resume — peek the session and assert a non-empty
        // outbound replay queue.
        let session = sm
            .peek_session("phone-stream-id")
            .await
            .expect("peek")
            .expect("session present");
        assert!(
            !session.unacked_stanzas.is_empty(),
            "detached SM session must have at least one queued carbon for resume"
        );
    }

    // -----------------------------------------------------------------
    // XEP-0313 — ArchiveDirect persistence
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn xep_0313_archive_direct_persists_to_mam_storage() {
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let from: jid::BareJid = "alice@example.com".parse().expect("bare");
        let to: jid::BareJid = "bob@example.com".parse().expect("bare");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "hello");
        msg.id = Some("orig-1".to_string());

        let events = vec![OutboundEvent::ArchiveDirect {
            archive_jid: archive_jid.clone(),
            from,
            to,
            message: Box::new(msg),
        }];
        let _outcome = interpret(events, &deps).await;

        let stored = mam
            .query_messages(&archive_jid.to_string(), &Default::default())
            .await
            .expect("query");
        assert_eq!(
            stored.messages.len(),
            1,
            "ArchiveDirect persists exactly one row"
        );
        let row = &stored.messages[0];
        assert_eq!(row.from, "alice@example.com");
        assert_eq!(row.to, "bob@example.com");
        assert_eq!(row.body, "hello");
        assert_eq!(row.stanza_id.as_deref(), Some("orig-1"));
    }

    #[tokio::test]
    async fn xep_0359_archive_ref_pivots_inbox_row_to_mam_row_by_stanza_id() {
        // End-to-end of the bug Qodo + Codex flagged: inbox writes
        // `archive_ref` from the canonical XEP-0359 `<stanza-id>`
        // stamp, and `MamStorage::get_message_by_stanza_id` must
        // resolve that same id against `archive_jid`. If the
        // projection ever stops using the canonical stamp as
        // `ArchivedMessage.stanza_id`/`id`, the inbox row points at a
        // dangling stanza-id and clients can't pivot to the archive.
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::protocol::event::{StanzaIdRef, StanzaIdValue};
        use waddle_xmpp::xep::xep0359::build_stanza_id_element;
        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "pivot test");
        msg.id = Some("wire-id".to_string());
        // Simulate CanonicalizeHandler stamping the canonical id
        // under alice's archive — the same id InboxHandler will
        // emit as `archive_ref`.
        let canonical_id = "alice-canonical-1";
        msg.payloads
            .push(build_stanza_id_element(canonical_id, "alice@example.com"));

        let events = vec![
            OutboundEvent::ArchiveDirect {
                archive_jid: alice.clone(),
                from: alice.clone(),
                to: bob.clone(),
                message: Box::new(msg.clone()),
            },
            OutboundEvent::ProjectInbox {
                owner: alice.clone(),
                peer: bob.clone(),
                message: Box::new(msg),
                archive_ref: StanzaIdRef {
                    by: alice.clone(),
                    id: StanzaIdValue::new(canonical_id),
                },
                increment_unread: false,
            },
        ];
        let _outcome = interpret(events, &deps).await;

        // Inbox row carries the canonical stamp.
        let entries = inbox_concrete.list(&alice).await.expect("inbox list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].last_stanza_id, canonical_id);

        // The same id resolves a MAM row in alice's archive — pivot
        // works.
        let row = mam
            .get_message_by_stanza_id(&alice.to_string(), canonical_id)
            .await
            .expect("mam lookup")
            .expect("MAM row keyed by canonical stanza-id");
        assert_eq!(row.id, canonical_id);
        assert_eq!(row.body, "pivot test");
    }

    #[tokio::test]
    async fn xep_0313_archive_direct_writes_one_entry_per_event() {
        // Sender pass + recipient pass on the same dispatch (true
        // local-to-local) emit two events with distinct archive_jids
        // — the interpreter writes one entry per archive.
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let msg = chat_msg("alice@example.com/web", "bob@example.com", "yo");

        let events = vec![
            OutboundEvent::ArchiveDirect {
                archive_jid: alice.clone(),
                from: alice.clone(),
                to: bob.clone(),
                message: Box::new(msg.clone()),
            },
            OutboundEvent::ArchiveDirect {
                archive_jid: bob.clone(),
                from: alice.clone(),
                to: bob.clone(),
                message: Box::new(msg),
            },
        ];
        let _outcome = interpret(events, &deps).await;

        let alice_archive = mam
            .query_messages(&alice.to_string(), &Default::default())
            .await
            .expect("query alice");
        let bob_archive = mam
            .query_messages(&bob.to_string(), &Default::default())
            .await
            .expect("query bob");
        assert_eq!(
            alice_archive.messages.len(),
            1,
            "alice archive has the sender-pass entry"
        );
        assert_eq!(
            bob_archive.messages.len(),
            1,
            "bob archive has the recipient-pass entry"
        );
    }

    #[tokio::test]
    async fn xep_0313_archive_direct_drops_when_storage_errors() {
        // Storage errors must NOT fail dispatch. We use a fake that
        // always errors and assert interpret returns normally; the
        // archive write is logged-and-dropped.
        use async_trait::async_trait;
        use waddle_xmpp::mam::storage::{MamStorage, MamStorageError};
        use waddle_xmpp::mam::{ArchivedMessage, MamQuery, MamResult};

        struct FailingMam;
        #[async_trait]
        impl MamStorage for FailingMam {
            async fn store_message(
                &self,
                _: &str,
                _: &ArchivedMessage,
            ) -> Result<String, MamStorageError> {
                Err(MamStorageError::Database("simulated".into()))
            }
            async fn query_messages(
                &self,
                _: &str,
                _: &MamQuery,
            ) -> Result<MamResult, MamStorageError> {
                Ok(MamResult {
                    messages: Vec::new(),
                    complete: true,
                    first_id: None,
                    last_id: None,
                    count: Some(0),
                })
            }
            async fn get_message(
                &self,
                _: &str,
            ) -> Result<Option<ArchivedMessage>, MamStorageError> {
                Ok(None)
            }
            async fn replace_with_tombstone(
                &self,
                _: &str,
                _: waddle_xmpp::mam::ArchivedTombstone,
            ) -> Result<bool, MamStorageError> {
                Ok(false)
            }
            async fn get_message_by_stanza_id(
                &self,
                _: &str,
                _: &str,
            ) -> Result<Option<ArchivedMessage>, MamStorageError> {
                Ok(None)
            }
            async fn get_message_by_message_id(
                &self,
                _: &str,
                _: &str,
            ) -> Result<Option<ArchivedMessage>, MamStorageError> {
                Ok(None)
            }
            async fn get_message_by_archive_or_stanza_id(
                &self,
                _: &str,
                _: &str,
            ) -> Result<Option<ArchivedMessage>, MamStorageError> {
                Ok(None)
            }
            async fn count_messages(&self, _: &str) -> Result<u32, MamStorageError> {
                Ok(0)
            }
            async fn delete_before(
                &self,
                _: &str,
                _: chrono::DateTime<chrono::Utc>,
            ) -> Result<u64, MamStorageError> {
                Ok(0)
            }
        }

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(FailingMam);
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let msg = chat_msg("alice@example.com/web", "bob@example.com", "yo");
        let events = vec![OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice,
            to: bob,
            message: Box::new(msg),
        }];
        let outcome = interpret(events, &deps).await;
        // No frames, no close — error swallowed.
        assert!(outcome.frames.is_empty());
        assert!(!outcome.close);
    }

    // -----------------------------------------------------------------
    // Inbox projection
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn inbox_project_writes_owner_peer_keyed_row_with_typed_archive_ref() {
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::protocol::event::{StanzaIdRef, StanzaIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let peer: jid::BareJid = "bob@example.com".parse().expect("bare");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "hi there");
        msg.id = Some("origin-X".to_string());

        let events = vec![OutboundEvent::ProjectInbox {
            owner: owner.clone(),
            peer: peer.clone(),
            message: Box::new(msg),
            archive_ref: StanzaIdRef {
                by: owner.clone(),
                id: StanzaIdValue::new("alice-archive-1"),
            },
            increment_unread: false,
        }];
        let _outcome = interpret(events, &deps).await;

        let entries = inbox_concrete.list(&owner).await.expect("list");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.partner, peer);
        assert_eq!(
            entry.last_stanza_id, "alice-archive-1",
            "last_stanza_id is sourced from the typed archive_ref, not the wire id"
        );
        assert_eq!(entry.unread, 0, "increment_unread=false leaves unread at 0");
    }

    #[tokio::test]
    async fn inbox_project_increment_unread_bumps_recipient_count() {
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::protocol::event::{StanzaIdRef, StanzaIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let owner: jid::BareJid = "bob@example.com".parse().expect("bare");
        let peer: jid::BareJid = "alice@example.com".parse().expect("bare");
        let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bob");

        let events = vec![OutboundEvent::ProjectInbox {
            owner: owner.clone(),
            peer: peer.clone(),
            message: Box::new(msg),
            archive_ref: StanzaIdRef {
                by: owner.clone(),
                id: StanzaIdValue::new("bob-archive-1"),
            },
            increment_unread: true,
        }];
        let _outcome = interpret(events, &deps).await;

        let total = inbox_concrete.total_unread(&owner).await.expect("unread");
        assert_eq!(
            total, 1,
            "increment_unread=true bumps the owner's unread count"
        );
    }

    // -----------------------------------------------------------------
    // XEP-0308/0424/0461 — LookupArchivedMessage callback round-trip
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn xep_0424_lookup_archived_message_by_stanza_id_feeds_archived_loaded_back() {
        use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
        use waddle_xmpp::protocol::event::CallbackId;
        use waddle_xmpp::protocol::event::StanzaIdValue;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        // Seed the archive with a row keyed under alice's bare,
        // canonical stamp = "canon-A1".
        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let row = ArchivedMessage {
            id: "canon-A1".to_string(),
            timestamp: chrono::Utc::now(),
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            body: "hello".to_string(),
            stanza_id: Some("canon-A1".to_string()),
            thread_id: None,
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: None,
            message_type: "chat".to_string(),
            stanza_xml: Some(
                r#"<message xmlns='jabber:client' type='chat' from='alice@example.com/web' to='bob@example.com'><body>hello</body></message>"#.to_string(),
            ),
            rich: None,
            nickname_generation: None,
        };
        mam.store_message(&archive_jid.to_string(), &row)
            .await
            .expect("seed");

        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(7),
            archive: archive_jid.clone(),
            reference: MessageRef::StanzaId {
                by: archive_jid.clone(),
                id: StanzaIdValue::new("canon-A1"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        assert_eq!(outcome.feedback.len(), 1);
        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded { id, result } => {
                assert_eq!(id, CallbackId(7));
                let archived = result.expect("row resolved");
                assert_eq!(archived.stanza_id.id.as_str(), "canon-A1");
                assert_eq!(archived.stanza_id.by, archive_jid);
                assert!(!archived.tombstoned);
                assert_eq!(
                    archived.message.bodies.get("").map(|b| b.0.clone()),
                    Some("hello".to_string()),
                    "stanza_xml is parsed back into a typed Message"
                );
            }
            other => panic!("expected ArchivedMessageLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn xep_0424_lookup_archived_message_not_found_feeds_none_back() {
        use waddle_xmpp::mam::InMemoryMamStorage;
        use waddle_xmpp::protocol::event::CallbackId;
        use waddle_xmpp::protocol::event::StanzaIdValue;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(11),
            archive: archive_jid.clone(),
            reference: MessageRef::StanzaId {
                by: archive_jid,
                id: StanzaIdValue::new("never-stamped"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                id: CallbackId(11),
                result: None,
            } => {}
            other => panic!("expected ArchivedMessageLoaded(None), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn xep_0359_lookup_archived_message_by_origin_id_feeds_archived_loaded_back() {
        // OriginId lookup MUST be sender-scoped per the typed
        // `MessageRef::OriginId { sender, origin_id }` contract.
        // Seed two rows in alice's archive that share the same
        // `origin_id` value but come from different senders:
        // post-filter on `sender` must pick the alice-authored row,
        // not the bob-authored one.
        use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
        use waddle_xmpp::protocol::event::{CallbackId, OriginIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let alice_bare: jid::BareJid = "alice@example.com".parse().expect("bare");

        // Bob-authored row in alice's archive (cross-resource self
        // chat / received DM) sharing the colliding origin-id.
        let bob_row = ArchivedMessage {
            id: "row-from-bob".to_string(),
            timestamp: chrono::Utc::now(),
            from: "bob@example.com".to_string(),
            to: "alice@example.com".to_string(),
            body: "from bob".to_string(),
            stanza_id: Some("alice-stamp-bob".to_string()),
            thread_id: None,
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: Some("collision".to_string()),
            message_type: "chat".to_string(),
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        };
        // Alice-authored row in alice's archive (sender-side) with
        // the same origin-id.
        let alice_row = ArchivedMessage {
            id: "row-from-alice".to_string(),
            timestamp: chrono::Utc::now(),
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            body: "from alice".to_string(),
            stanza_id: Some("alice-stamp-alice".to_string()),
            thread_id: None,
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: Some("collision".to_string()),
            message_type: "chat".to_string(),
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        };
        // Insert bob's row FIRST so a naive OR-matcher would return it.
        mam.store_message(&archive_jid.to_string(), &bob_row)
            .await
            .expect("seed bob");
        mam.store_message(&archive_jid.to_string(), &alice_row)
            .await
            .expect("seed alice");

        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(21),
            archive: archive_jid.clone(),
            reference: MessageRef::OriginId {
                sender: alice_bare.clone(),
                origin_id: OriginIdValue::new("collision"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                id: CallbackId(21),
                result: Some(archived),
            } => {
                let body = archived
                    .message
                    .bodies
                    .get("")
                    .map(|b| b.0.clone())
                    .unwrap_or_default();
                assert_eq!(
                    body, "from alice",
                    "OriginId lookup must scope to sender; bob's row was a collision decoy"
                );
            }
            other => panic!("expected alice-authored row, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn xep_0359_lookup_archived_message_by_origin_id_rejects_cross_sender_collision() {
        // Same archive, same origin_id, different sender than
        // requested -> result MUST be None (handler will treat as
        // <item-not-found>).
        use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
        use waddle_xmpp::protocol::event::{CallbackId, OriginIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let row = ArchivedMessage {
            id: "row-1".to_string(),
            timestamp: chrono::Utc::now(),
            from: "bob@example.com".to_string(),
            to: "alice@example.com".to_string(),
            body: "bob's".to_string(),
            stanza_id: Some("alice-stamp".to_string()),
            thread_id: None,
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: Some("oid-1".to_string()),
            message_type: "chat".to_string(),
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        };
        mam.store_message(&archive_jid.to_string(), &row)
            .await
            .expect("seed");

        // Look up for a DIFFERENT sender (charlie) with the colliding
        // origin-id. Must surface as not-found.
        let charlie_bare: jid::BareJid = "charlie@example.com".parse().expect("bare");
        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(31),
            archive: archive_jid,
            reference: MessageRef::OriginId {
                sender: charlie_bare,
                origin_id: OriginIdValue::new("oid-1"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                id: CallbackId(31),
                result: None,
            } => {}
            other => {
                panic!("OriginId lookup must reject cross-sender collisions, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn xep_0359_lookup_archived_message_strict_stanza_id_ignores_origin_id_collision() {
        // StanzaId path uses `get_message_by_message_id` (stanza_id
        // ONLY), so a row whose `origin_id` happens to equal the
        // requested stanza-id MUST NOT be returned.
        use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
        use waddle_xmpp::protocol::event::{CallbackId, StanzaIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        // Row whose origin_id equals the value the caller is looking
        // up via StanzaId. Stanza_id is something else.
        let collision_row = ArchivedMessage {
            id: "row-collide".to_string(),
            timestamp: chrono::Utc::now(),
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            body: "collide".to_string(),
            stanza_id: Some("real-stamp".to_string()),
            thread_id: None,
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: Some("queried-id".to_string()),
            message_type: "chat".to_string(),
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        };
        mam.store_message(&archive_jid.to_string(), &collision_row)
            .await
            .expect("seed");

        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(41),
            archive: archive_jid.clone(),
            reference: MessageRef::StanzaId {
                by: archive_jid,
                id: StanzaIdValue::new("queried-id"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                id: CallbackId(41),
                result: None,
            } => {}
            other => {
                panic!("strict stanza-id lookup must ignore origin-id collisions, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn xep_0424_lookup_archived_message_propagates_tombstone_state() {
        use waddle_xmpp::mam::{
            ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone,
            InMemoryMamStorage, RichMessageId,
        };
        use waddle_xmpp::protocol::event::CallbackId;
        use waddle_xmpp::protocol::event::StanzaIdValue;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let row = ArchivedMessage {
            id: "tomb-1".to_string(),
            timestamp: chrono::Utc::now(),
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            body: String::new(),
            stanza_id: Some("tomb-1".to_string()),
            thread_id: None,
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: None,
            message_type: "chat".to_string(),
            stanza_xml: None,
            rich: Some(ArchivedRichMessage {
                payload: Some(ArchivedRichPayload::Tombstone(ArchivedTombstone {
                    retraction_id: Some(RichMessageId::new("retract-1").expect("rich id")),
                    stamp: chrono::Utc::now(),
                    moderation: None,
                })),
                reply: None,
                references: Vec::new(),
                mentions: Vec::new(),
            }),
            nickname_generation: None,
        };
        mam.store_message(&archive_jid.to_string(), &row)
            .await
            .expect("seed");

        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(13),
            archive: archive_jid.clone(),
            reference: MessageRef::StanzaId {
                by: archive_jid,
                id: StanzaIdValue::new("tomb-1"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                result: Some(archived),
                ..
            } => {
                assert!(
                    archived.tombstoned,
                    "ArchivedRichPayload::Tombstone surfaces as `tombstoned: true`"
                );
            }
            other => panic!("expected Some archived row, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // XEP-0372 — RequestEnrichment callback round-trip
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn enrichment_request_without_extension_manager_fails_open_with_original_message() {
        // No extension manager in Deps -> the original typed message
        // is returned unchanged via EnrichmentComplete. This is the
        // legacy fail-open contract (see `enrich_message` in the
        // legacy `message.rs` path).
        use waddle_xmpp::protocol::event::CallbackId;
        let registry = ConnectionRegistry::new();
        let deps = Deps::registry_only(&registry);

        let mut original = chat_msg("alice@example.com/web", "bob@example.com", "look https://x");
        original.id = Some("orig-id".to_string());

        let events = vec![OutboundEvent::RequestEnrichment {
            id: CallbackId(42),
            message: Box::new(original.clone()),
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::EnrichmentComplete {
                id: CallbackId(42),
                message,
            } => {
                assert_eq!(message.id, original.id);
                assert_eq!(
                    message.bodies.get("").map(|b| b.0.clone()),
                    Some("look https://x".to_string()),
                );
            }
            other => panic!("expected EnrichmentComplete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enrichment_failure_fail_open_feeds_original_message_back() {
        // Fail-open contract: when the extension manager has no
        // working actors (e.g. all extension RPCs failed at startup,
        // or the deployment intentionally disabled extensions),
        // `enrich_message` is a no-op and the dispatch must still
        // resume with the *original* message via EnrichmentComplete
        // — never block on enrichment, never drop the message.
        // We model this with a disabled config (no actors loaded),
        // which is the exact failure mode legacy `message.rs` falls
        // back to when the wasm runtime can't start any extension.
        use waddle_extensions::{ExtensionConfig, ExtensionManager};
        use waddle_xmpp::protocol::event::CallbackId;

        let registry = ConnectionRegistry::new();
        let em = Arc::new(
            ExtensionManager::from_config(ExtensionConfig {
                enabled: false,
                ..Default::default()
            })
            .await
            .expect("disabled extension manager"),
        );
        let deps = Deps::test_with_extension_manager(&registry, &em);

        let mut original = chat_msg(
            "alice@example.com/web",
            "bob@example.com",
            "check https://example.com",
        );
        original.id = Some("fail-open-id".to_string());
        let original_payload_count = original.payloads.len();

        let events = vec![OutboundEvent::RequestEnrichment {
            id: CallbackId(123),
            message: Box::new(original.clone()),
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::EnrichmentComplete {
                id: CallbackId(123),
                message,
            } => {
                assert_eq!(
                    message.id.as_deref(),
                    Some("fail-open-id"),
                    "fail-open path returns the original message id"
                );
                assert_eq!(
                    message.bodies.get("").map(|b| b.0.clone()),
                    original.bodies.get("").map(|b| b.0.clone()),
                    "fail-open path returns the original body unchanged"
                );
                assert_eq!(
                    message.payloads.len(),
                    original_payload_count,
                    "fail-open path adds no payloads when no actor produces enrichment"
                );
            }
            other => panic!("expected EnrichmentComplete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enrichment_request_calls_extension_manager_and_feeds_complete_back() {
        // Wire a real (empty) ExtensionManager — no extension actors
        // configured, so `enrich_message` returns 0 enrichments and
        // we still feed back the original message via
        // EnrichmentComplete with the original CallbackId. This proves
        // the callback round-trip without depending on a live wasm
        // runtime.
        use waddle_extensions::{ExtensionConfig, ExtensionManager};
        use waddle_xmpp::protocol::event::CallbackId;

        let registry = ConnectionRegistry::new();
        let em = Arc::new(
            ExtensionManager::from_config(ExtensionConfig {
                enabled: false,
                ..Default::default()
            })
            .await
            .expect("disabled extension manager"),
        );
        let deps = Deps::test_with_extension_manager(&registry, &em);

        let mut original = chat_msg("alice@example.com/web", "bob@example.com", "ping");
        original.id = Some("e-id".to_string());

        let events = vec![OutboundEvent::RequestEnrichment {
            id: CallbackId(99),
            message: Box::new(original),
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::EnrichmentComplete {
                id: CallbackId(99),
                message,
            } => {
                assert_eq!(message.id.as_deref(), Some("e-id"));
            }
            other => panic!("expected EnrichmentComplete, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // #229 PR12 — RouteToConnection delivers as PeerStanza
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn route_to_connection_full_jid_queues_peer_stanza_kind() {
        // Locks in the staged-cutover contract: full-JID
        // RouteToConnection events queue an OutboundStanza tagged
        // PeerStanza so the destination's main loop runs the
        // recipient pass before any wire write.
        use waddle_xmpp::registry::DeliveryKind;
        let registry = ConnectionRegistry::new();
        let bob: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob.clone(), bob_tx, false);

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(bob.clone()),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        let queued = drain_inbound(&mut bob_rx);
        assert_eq!(queued.len(), 1, "delivered to bob's queue exactly once");
        assert_eq!(
            queued[0].kind,
            DeliveryKind::PeerStanza,
            "RouteToConnection MUST tag PeerStanza so the destination main \
             loop runs the recipient pass; got {:?}",
            queued[0].kind
        );
    }

    #[tokio::test]
    async fn route_to_connection_bare_jid_selects_highest_priority_available_resources() {
        // RFC 6121 §8.5.2.1 resource selection: deliver to every
        // resource tied at the highest available priority. A
        // bare-JID `to` from the sender pass (handlers/route.rs
        // emits `message.to` verbatim) lands here; without selection
        // the cutover would silently drop bare-targeted 1:1 traffic.
        use waddle_xmpp::registry::DeliveryKind;
        let registry = ConnectionRegistry::new();
        let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
        let bob_tablet: jid::FullJid = "bob@example.com/tablet".parse().expect("jid");
        let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
        let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
        let (tablet_tx, mut tablet_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob_desk.clone(), desk_tx, false);
        registry.register_with_carbons(bob_phone.clone(), phone_tx, false);
        registry.register_with_carbons(bob_tablet.clone(), tablet_tx, false);
        // desk + phone available at priority 5 (tied); tablet at
        // lower priority 1. Tablet must NOT receive.
        registry.update_presence(&bob_desk, true, 5);
        registry.update_presence(&bob_phone, true, 5);
        registry.update_presence(&bob_tablet, true, 1);

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bare");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        let desk_q = drain_inbound(&mut desk_rx);
        let phone_q = drain_inbound(&mut phone_rx);
        let tablet_q = drain_inbound(&mut tablet_rx);
        assert_eq!(
            desk_q.len(),
            1,
            "desk (tied at max priority) gets the message"
        );
        assert_eq!(
            phone_q.len(),
            1,
            "phone (tied at max priority) gets the message"
        );
        assert!(
            tablet_q.is_empty(),
            "tablet (lower priority) is excluded by RFC 6121 §8.5.2.1.2"
        );
        for q in [&desk_q, &phone_q] {
            assert_eq!(q[0].kind, DeliveryKind::PeerStanza);
        }
    }

    #[tokio::test]
    async fn route_to_connection_bare_jid_with_no_available_resources_drops() {
        // Known design gap: when the bare-JID target has no
        // available resources, the recipient-pass side effects
        // (recipient archive, inbox) do NOT run. Documented as a
        // known #229 follow-up. This test pins the current behavior
        // (drop) so the gap doesn't quietly mutate before the
        // follow-up lands an offline-pass strategy.
        let registry = ConnectionRegistry::new();
        let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob_desk.clone(), desk_tx, false);
        // Registered but presence NOT made available — `bob_desk`
        // is not eligible per §8.5.2.1.1.

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        assert!(
            drain_inbound(&mut desk_rx).is_empty(),
            "no available resources -> no delivery (offline-pass is a #229 follow-up)"
        );
    }

    #[tokio::test]
    async fn preserves_frame_order_across_multiple_events() {
        let events = vec![
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("a")))),
            OutboundEvent::Log {
                level: tracing::Level::DEBUG,
                message: "between".to_string(),
            },
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("b")))),
        ];
        let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
        assert_eq!(outcome.frames.len(), 2);
        assert!(outcome.frames[0].contains("id=\"a\""));
        assert!(outcome.frames[1].contains("id=\"b\""));
    }
}
