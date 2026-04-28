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
//! Only a subset of variants have their interpreter wiring in place — the
//! ones needed by handlers that have been migrated so far (ping, session).
//! The remaining variants (`RouteToConnection`, `BroadcastToRoom`,
//! `DispatchToRoom`, `QueryMam`, `AskSfu`, `RequestEnrichment`,
//! `ProjectInbox`, `SendCarbons`, `LookupArchivedMessage`, …) are defined
//! in the event enum so future handlers can emit them, but land in the
//! interpreter as later migration steps pull their XEP into the sans-I/O
//! world.

use tracing::{debug, error, info, warn};
use waddle_xmpp::carbons::{build_received_carbon, build_sent_carbon};
use waddle_xmpp::parser::stanza_to_string;
use waddle_xmpp::protocol::{CarbonKind, OutboundEvent};
use waddle_xmpp::registry::ConnectionRegistry;
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

/// Execute the side effects described by `events`.
///
/// The function is `async` because future migration steps add variants
/// that genuinely require `.await` (registry lookups, actor calls, MAM
/// storage). The currently-supported variants are all synchronous, so this
/// function will return immediately for the ping/session flow.
pub async fn interpret(
    events: Vec<OutboundEvent>,
    registry: &ConnectionRegistry,
) -> InterpretOutcome {
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
            OutboundEvent::ProjectInbox { owner, peer, .. } => {
                warn!(
                    variant = "ProjectInbox",
                    owner = %owner,
                    peer = %peer,
                    "OutboundEvent variant not yet wired in interpreter"
                );
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
                let targets = registry.get_other_carbon_resources_for_user(&owner, &exclude);
                if targets.is_empty() {
                    debug!(
                        owner = %owner,
                        kind = ?kind,
                        "SendCarbons: no carbon-enabled resources to fan out to"
                    );
                    continue;
                }
                for target in targets {
                    let envelope_result = match kind {
                        CarbonKind::Sent => {
                            build_sent_carbon(&message, &owner_str, &target.to_string())
                        }
                        CarbonKind::Received => {
                            build_received_carbon(&message, &owner_str, &target.to_string())
                        }
                    };
                    let envelope = match envelope_result {
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
                            // send_to — target disconnected. Benign.
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
            OutboundEvent::ArchiveDirect { from, to, .. } => {
                warn!(
                    variant = "ArchiveDirect",
                    from = %from,
                    to = %to,
                    "OutboundEvent variant not yet wired in interpreter"
                );
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
        let outcome = interpret(events, &test_registry()).await;
        assert_eq!(outcome.frames.len(), 1);
        assert!(outcome.frames[0].contains("type=\"result\""));
        assert!(outcome.frames[0].contains("id=\"x\""));
        assert!(!outcome.close);
    }

    #[tokio::test]
    async fn interprets_close_transport() {
        let events = vec![OutboundEvent::CloseTransport];
        let outcome = interpret(events, &test_registry()).await;
        assert!(outcome.close);
        assert!(outcome.frames.is_empty());
    }

    #[tokio::test]
    async fn interprets_log_is_noop_for_caller() {
        let events = vec![OutboundEvent::Log {
            level: tracing::Level::INFO,
            message: "hello".to_string(),
        }];
        let outcome = interpret(events, &test_registry()).await;
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
        let _outcome = interpret(events, &registry).await;

        // alice/phone should receive a <sent> carbon envelope.
        let received = drain_inbound(&mut phone_rx);
        assert_eq!(received.len(), 1, "alice/phone received one carbon");
        // Verify it's wrapped per XEP-0297 — a <sent> child in carbons:2 ns.
        let stanza = &received[0].stanza;
        let msg = match stanza {
            Stanza::Message(m) => m,
            other => panic!("expected Message stanza, got {other:?}"),
        };
        assert!(
            msg.payloads
                .iter()
                .any(|p| p.name() == "sent" && p.ns() == "urn:xmpp:carbons:2"),
            "carbon must carry <sent xmlns='urn:xmpp:carbons:2'/>"
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
        let _outcome = interpret(events, &registry).await;

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
        let _outcome = interpret(events, &registry).await;

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
        let _outcome = interpret(events, &registry).await;

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
    async fn preserves_frame_order_across_multiple_events() {
        let events = vec![
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("a")))),
            OutboundEvent::Log {
                level: tracing::Level::DEBUG,
                message: "between".to_string(),
            },
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("b")))),
        ];
        let outcome = interpret(events, &test_registry()).await;
        assert_eq!(outcome.frames.len(), 2);
        assert!(outcome.frames[0].contains("id=\"a\""));
        assert!(outcome.frames[1].contains("id=\"b\""));
    }
}
