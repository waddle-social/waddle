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
//!
//! Stubbed (warn-logged until migration steps land them):
//! - `BroadcastToRoom`, `DispatchToRoom`, `ArchiveGroupchat` — MUC
//!   handler chain (PR10).
//! - `LookupArchivedMessage`, `RequestEnrichment` — need callback
//!   plumbing back into the state machine (PR8).
//! - `AskSfu`, `QueryMam`, `LoadScramCredentials`,
//!   `ValidateOAuthBearer`, `SetTimer`, `CancelTimer`,
//!   `RegisterConnection` — wired in later migration steps.

use std::sync::Arc;
use tracing::{debug, error, info, warn};
use waddle_xmpp::carbons::{build_received_carbon, build_sent_carbon};
use waddle_xmpp::inbox::runtime::direct_message_entry;
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::mam::projection::build_direct_archived_message;
use waddle_xmpp::mam::storage::MamStorage;
use waddle_xmpp::parser::stanza_to_string;
use waddle_xmpp::protocol::{CarbonKind, OutboundEvent};
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::Stanza;

/// Outcome of interpreting a batch of [`OutboundEvent`]s.
///
/// The WebSocket transport uses `frames` to decide what to write back to
/// the client. `close` signals the main loop should drop the connection.
#[derive(Debug, Default)]
pub struct InterpretOutcome {
    /// Serialized XML frames to write to the transport, in order.
    pub frames: Vec<String>,
    /// Set to true when the state machine asked us to close the transport.
    pub close: bool,
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
                // Until the per-connection state-machine routing for
                // `StanzaFromPeer` lands (issue #229 PR5), preserve the
                // legacy "write to peer's outbound channel" behaviour so
                // existing integration tests stay green. The semantic
                // change to "feed StanzaFromPeer into the destination
                // machine" is wired alongside the message.rs cutover
                // in PR5.
                //
                // `jid` is now a typed `Jid` (full or bare); the
                // current legacy registry only supports full-JID
                // delivery, so bare-JID targets are logged and
                // deferred to PR5's resource-selection logic. This
                // is a *temporary* gap during the staged migration;
                // it does not regress existing behaviour because no
                // production handler emits `RouteToConnection` with a
                // bare JID until PR5 cuts over.
                match jid.clone().try_into_full() {
                    Ok(full) => match registry.send_to(&full, *stanza).await {
                        waddle_xmpp::registry::SendResult::Sent => {
                            debug!(jid = %full, "RouteToConnection delivered");
                        }
                        waddle_xmpp::registry::SendResult::NotConnected => {
                            debug!(jid = %full, "RouteToConnection: target offline, dropping");
                        }
                        waddle_xmpp::registry::SendResult::ChannelClosed => {
                            warn!(jid = %full, "RouteToConnection: target channel closed, dropping");
                        }
                    },
                    Err(bare) => {
                        warn!(
                            bare_jid = %bare,
                            "RouteToConnection: bare-JID resource selection lands in PR5; \
                             dropping this event for now"
                        );
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
            OutboundEvent::LookupArchivedMessage { id, archive, .. } => {
                warn!(
                    variant = "LookupArchivedMessage",
                    callback_id = id.0,
                    archive = %archive,
                    "OutboundEvent variant not yet wired in interpreter"
                );
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
            OutboundEvent::RequestEnrichment { id, .. } => {
                warn!(
                    variant = "RequestEnrichment",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
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
