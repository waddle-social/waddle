//! The session's call verbs: thin calls into the shared protocol facade
//! (XEP-0353 invitations, XEP-0166 sessions, XEP-0215 ICE services), and
//! the translation of its call events into `crate::call`'s types.

use std::sync::Arc;

use jid::{BareJid, Jid};
use waddle_xmpp_client_ffi::{
    WaddleCallEvent, WaddleCallEventKind, WaddleCallMedia, WaddleCallSessionTerminateOutcome,
    WaddleExternalService, WaddleExternalServiceTransport, WaddleExternalServiceType,
    WaddleJingleReason, WaddleLiveKitJoin,
};

use crate::call::{CallEvent, CallEventKind, CallJoin, CallMedia, JingleReason};
use crate::call_engine::Wire;
use crate::call_media::{IceKind, IceService, IceTransport};
use crate::runtime;
use crate::session::Session;

/// How the server answered a session-terminate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminated {
    Ok,
    /// The server no longer knows the session (a crashed peer, a recovered
    /// session); the `<finish/>` bookend still goes out.
    Orphaned,
    Error,
}

fn media_from(media: WaddleCallMedia) -> CallMedia {
    CallMedia {
        audio: media.audio,
        video: media.video,
    }
}

fn join_from(join: WaddleLiveKitJoin) -> CallJoin {
    CallJoin {
        url: join.url,
        room: join.room,
        identity: join.identity,
        token: join.token,
    }
}

fn reason_from(reason: WaddleJingleReason) -> JingleReason {
    match reason {
        WaddleJingleReason::AlternativeSession => JingleReason::AlternativeSession,
        WaddleJingleReason::Busy => JingleReason::Busy,
        WaddleJingleReason::Cancel => JingleReason::Cancel,
        WaddleJingleReason::ConnectivityError => JingleReason::ConnectivityError,
        WaddleJingleReason::Decline => JingleReason::Decline,
        WaddleJingleReason::Expired => JingleReason::Expired,
        WaddleJingleReason::FailedApplication => JingleReason::FailedApplication,
        WaddleJingleReason::FailedTransport => JingleReason::FailedTransport,
        WaddleJingleReason::GeneralError => JingleReason::GeneralError,
        WaddleJingleReason::Gone => JingleReason::Gone,
        WaddleJingleReason::IncompatibleParameters => JingleReason::IncompatibleParameters,
        WaddleJingleReason::MediaError => JingleReason::MediaError,
        WaddleJingleReason::SecurityError => JingleReason::SecurityError,
        WaddleJingleReason::Success => JingleReason::Success,
        WaddleJingleReason::Timeout => JingleReason::Timeout,
        WaddleJingleReason::UnsupportedApplications => JingleReason::UnsupportedApplications,
        WaddleJingleReason::UnsupportedTransports => JingleReason::UnsupportedTransports,
    }
}

fn reason_to(reason: JingleReason) -> WaddleJingleReason {
    match reason {
        JingleReason::AlternativeSession => WaddleJingleReason::AlternativeSession,
        JingleReason::Busy => WaddleJingleReason::Busy,
        JingleReason::Cancel => WaddleJingleReason::Cancel,
        JingleReason::ConnectivityError => WaddleJingleReason::ConnectivityError,
        JingleReason::Decline => WaddleJingleReason::Decline,
        JingleReason::Expired => WaddleJingleReason::Expired,
        JingleReason::FailedApplication => WaddleJingleReason::FailedApplication,
        JingleReason::FailedTransport => WaddleJingleReason::FailedTransport,
        JingleReason::GeneralError => WaddleJingleReason::GeneralError,
        JingleReason::Gone => WaddleJingleReason::Gone,
        JingleReason::IncompatibleParameters => WaddleJingleReason::IncompatibleParameters,
        JingleReason::MediaError => WaddleJingleReason::MediaError,
        JingleReason::SecurityError => WaddleJingleReason::SecurityError,
        JingleReason::Success => WaddleJingleReason::Success,
        JingleReason::Timeout => WaddleJingleReason::Timeout,
        JingleReason::UnsupportedApplications => WaddleJingleReason::UnsupportedApplications,
        JingleReason::UnsupportedTransports => WaddleJingleReason::UnsupportedTransports,
    }
}

/// A call event from the facade. A malformed sender address never crosses
/// into the single-slot engine: call routing needs a valid sender.
pub fn call_event_from(event: WaddleCallEvent) -> Option<CallEvent> {
    let Ok(from) = event.from.parse::<Jid>() else {
        tracing::debug!("dropped call event: invalid sender JID");
        return None;
    };
    let kind = match event.kind {
        WaddleCallEventKind::Propose { media } => CallEventKind::Propose(media_from(media)),
        WaddleCallEventKind::Ringing => CallEventKind::Ringing,
        WaddleCallEventKind::Proceed => CallEventKind::Proceed,
        WaddleCallEventKind::Reject { reason, tie_break } => CallEventKind::Reject {
            reason: reason.map(reason_from),
            tie_break,
        },
        WaddleCallEventKind::Retract { reason, tie_break } => CallEventKind::Retract {
            reason: reason.map(reason_from),
            tie_break,
        },
        WaddleCallEventKind::Finish { reason, .. } => CallEventKind::Finish {
            reason: reason.map(reason_from),
        },
        WaddleCallEventKind::SessionInitiate { join, media } => CallEventKind::SessionInitiate {
            join: join_from(join),
            media: media_from(media),
        },
        WaddleCallEventKind::SessionAccept { join, media } => CallEventKind::SessionAccept {
            join: join_from(join),
            media: media_from(media),
        },
        WaddleCallEventKind::SessionTerminate { reason } => CallEventKind::SessionTerminate {
            reason: reason.map(reason_from),
        },
    };
    Some(CallEvent {
        from,
        sid: event.sid,
        kind,
    })
}

fn ice_service_from(service: WaddleExternalService) -> IceService {
    IceService {
        kind: match service.service_type {
            WaddleExternalServiceType::Stun => IceKind::Stun,
            WaddleExternalServiceType::Turn => IceKind::Turn,
            WaddleExternalServiceType::Turns => IceKind::Turns,
        },
        host: service.host,
        port: service.port,
        transport: service.transport.map(|transport| match transport {
            WaddleExternalServiceTransport::Udp => IceTransport::Udp,
            WaddleExternalServiceTransport::Tcp => IceTransport::Tcp,
        }),
        username: service.username,
        password: service.password,
    }
}

impl Session {
    /// This device's bound address: the initiator of calls it places and
    /// the responder of calls it answers.
    pub fn own_full_jid(&self) -> Option<Jid> {
        format!("{}/{}", self.me.bare, self.resource).parse().ok()
    }

    /// Send one call stanza. True when it went out.
    pub async fn send_call(&self, wire: Wire) -> bool {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move {
            match wire {
                Wire::Propose { to, sid, media } => {
                    client
                        .send_call_propose(to.to_string(), sid, media.audio, media.video)
                        .await
                }
                Wire::Ringing { to, sid } => client.send_call_ringing(to.to_string(), sid).await,
                Wire::Proceed { to, sid } => client.send_call_proceed(to.to_string(), sid).await,
                Wire::Reject {
                    to,
                    sid,
                    tie_break: false,
                } => client.send_call_reject(to.to_string(), sid).await,
                Wire::Reject {
                    to,
                    sid,
                    tie_break: true,
                } => client.send_call_reject_tie_break(to.to_string(), sid).await,
                Wire::Retract { to, sid } => client.send_call_retract(to.to_string(), sid).await,
                Wire::RetractTieBreak { to, sid } => {
                    client
                        .send_call_retract_tie_break(to.to_string(), sid)
                        .await
                }
                Wire::Finish {
                    to,
                    sid,
                    reason: None,
                } => client.send_call_finish(to.to_string(), sid).await,
                Wire::Finish {
                    to,
                    sid,
                    reason: Some(reason),
                } => {
                    client
                        .send_call_finish_with_reason(to.to_string(), sid, reason_to(reason))
                        .await
                }
                Wire::SessionInitiate {
                    to,
                    initiator,
                    sid,
                    media,
                } => {
                    client
                        .send_call_session_initiate(
                            to.to_string(),
                            initiator.to_string(),
                            sid,
                            media.audio,
                            media.video,
                        )
                        .await
                }
                Wire::SessionAccept {
                    to,
                    responder,
                    sid,
                    media,
                } => {
                    client
                        .send_call_session_accept(
                            to.to_string(),
                            responder.to_string(),
                            sid,
                            media.audio,
                            media.video,
                        )
                        .await
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    /// End the Jingle session at `peer`.
    pub async fn terminate_call(&self, peer: Jid, sid: String, reason: JingleReason) -> Terminated {
        let client = Arc::clone(&self.client);
        let outcome = runtime::spawn(async move {
            client
                .send_call_session_terminate_with_outcome(
                    peer.to_string(),
                    sid,
                    Some(reason_to(reason)),
                )
                .await
        })
        .await;
        match outcome {
            Ok(WaddleCallSessionTerminateOutcome::Ok) => Terminated::Ok,
            Ok(WaddleCallSessionTerminateOutcome::Orphaned) => Terminated::Orphaned,
            Ok(WaddleCallSessionTerminateOutcome::Error) | Err(_) => Terminated::Error,
        }
    }

    /// Whether `peer` receives our presence (RFC 6121 subscription `from`
    /// or `both`). XEP-0353 answers a proposal with `<ringing/>` only for
    /// such a contact: replying to a stranger leaks that we are online.
    pub async fn peer_receives_our_presence(&self, peer: BareJid) -> bool {
        let client = Arc::clone(&self.client);
        let contacts = runtime::spawn(async move { client.fetch_roster_contacts().await }).await;
        match contacts {
            Ok(Ok(contacts)) => contacts.iter().any(|contact| {
                contact.receives_our_presence
                    && contact
                        .jid
                        .parse::<Jid>()
                        .is_ok_and(|jid| jid.to_bare() == peer)
            }),
            Ok(Err(error)) => {
                tracing::debug!(?error, "roster lookup for the call gate failed");
                false
            }
            Err(_) => false,
        }
    }

    /// The server's STUN and TURN services for the call's media. A failed
    /// lookup is not a failed call: the SFU supplies its own servers.
    pub async fn ice_services(&self) -> Vec<IceService> {
        let client = Arc::clone(&self.client);
        match runtime::spawn(async move { client.fetch_external_services().await }).await {
            Ok(Ok(services)) => services.into_iter().map(ice_service_from).collect(),
            Ok(Err(error)) => {
                tracing::debug!(?error, "external services lookup failed");
                Vec::new()
            }
            Err(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(from: &str, kind: WaddleCallEventKind) -> WaddleCallEvent {
        WaddleCallEvent {
            from: from.into(),
            to: None,
            sid: "c1".into(),
            kind,
        }
    }

    #[test]
    fn every_jingle_reason_round_trips() {
        use WaddleJingleReason as W;
        for reason in [
            W::AlternativeSession,
            W::Busy,
            W::Cancel,
            W::ConnectivityError,
            W::Decline,
            W::Expired,
            W::FailedApplication,
            W::FailedTransport,
            W::GeneralError,
            W::Gone,
            W::IncompatibleParameters,
            W::MediaError,
            W::SecurityError,
            W::Success,
            W::Timeout,
            W::UnsupportedApplications,
            W::UnsupportedTransports,
        ] {
            assert_eq!(reason_to(reason_from(reason)), reason);
        }
    }

    #[test]
    fn a_propose_arrives_with_its_sender_and_media() {
        let parsed = call_event_from(event(
            "bob@example.org/phone",
            WaddleCallEventKind::Propose {
                media: WaddleCallMedia {
                    audio: true,
                    video: true,
                },
            },
        ))
        .unwrap();
        assert_eq!(parsed.from.as_str(), "bob@example.org/phone");
        assert_eq!(parsed.sid, "c1");
        assert_eq!(
            parsed.kind,
            CallEventKind::Propose(CallMedia {
                audio: true,
                video: true
            })
        );
    }

    #[test]
    fn credentials_and_reasons_cross_unchanged() {
        let parsed = call_event_from(event(
            "bob@example.org/phone",
            WaddleCallEventKind::SessionAccept {
                join: WaddleLiveKitJoin {
                    url: "wss://sfu.example".into(),
                    room: "room".into(),
                    identity: "alice".into(),
                    token: "t".into(),
                },
                media: WaddleCallMedia {
                    audio: true,
                    video: false,
                },
            },
        ))
        .unwrap();
        let CallEventKind::SessionAccept { join, media } = parsed.kind else {
            panic!("session accept");
        };
        assert_eq!((join.room.as_str(), join.token.as_str()), ("room", "t"));
        assert_eq!(media, CallMedia::AUDIO);

        let parsed = call_event_from(event(
            "bob@example.org/phone",
            WaddleCallEventKind::Reject {
                reason: Some(WaddleJingleReason::Expired),
                tie_break: true,
            },
        ))
        .unwrap();
        assert_eq!(
            parsed.kind,
            CallEventKind::Reject {
                reason: Some(JingleReason::Expired),
                tie_break: true
            }
        );
    }

    #[test]
    fn a_malformed_sender_is_dropped() {
        assert!(call_event_from(event("not a jid@", WaddleCallEventKind::Ringing)).is_none());
    }
}
