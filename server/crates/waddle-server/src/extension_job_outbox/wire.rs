//! Turns a successful durable job's typed [`JudgmentResult`] into the
//! actual XEP-0422 `<apply-to>` `urn:waddle:safety-scores:1` broadcast —
//! the "wire delivery" piece #1842/#1853 explicitly deferred. See
//! `waddle_xmpp::xep::xep_waddle_safety_scores` for the wire shape itself
//! (and its own conformance test, which round-trips against the exact
//! client-side parser).
//!
//! **Scope of this first version**: delivery is local-only (a best-effort
//! fan-out to this node's own `connection_registry` sessions for the
//! room's current occupants) and not archived to MAM. A recipient who is
//! offline, connected to a different cluster node, or who reconnects
//! after this broadcast simply does not receive this particular
//! fastening — the same "local delivery is best-effort, cross-node/
//! recovered delivery is a separate concern" split already visible
//! elsewhere in this codebase (e.g. the non-clustered fast path in
//! `server::routes::websocket::handlers::iq::muc_owner_moderation`'s
//! destroy-notification delivery). Extending this to clustered fan-out
//! and/or MAM persistence is a natural fast-follow, not required for this
//! effect to be genuinely new, real, wire-visible functionality.

use async_trait::async_trait;
use waddle_extensions::{JudgmentResult, RoomJid};
use waddle_xmpp_core::xep0359::StanzaId;

#[derive(Debug, thiserror::Error)]
pub enum WireSinkError {
    #[error("safety-scores wire effect requires a room; direct-message judging is not supported")]
    NoRoom,
    #[error("target room jid is invalid: {0}")]
    InvalidRoomJid(String),
    #[error("room actor lookup failed: {0}")]
    RoomLookupFailed(String),
    #[error("room {0} is not currently registered")]
    RoomNotFound(String),
}

/// Emits a successful durable job's judgment result to the wire. The real
/// implementation ([`WebSocketStateSafetyScoresWireSink`], constructed
/// alongside the drain loop at startup) fans the fastening out to the
/// room's current local occupants; the test-only sinks in this module's
/// own `tests` submodule are for tests of the queue/retry logic that don't
/// exercise delivery.
#[async_trait]
pub trait SafetyScoresWireSink: Send + Sync {
    async fn emit_safety_scores(
        &self,
        room: Option<RoomJid>,
        target_stanza_id: StanzaId,
        result: JudgmentResult,
    ) -> Result<(), WireSinkError>;
}

/// A sink that does nothing. Used by tests of [`super::drain`]'s
/// queue/retry logic that are not themselves testing wire delivery.
#[cfg(test)]
pub(crate) struct NullSafetyScoresWireSink;

#[cfg(test)]
#[async_trait]
impl SafetyScoresWireSink for NullSafetyScoresWireSink {
    async fn emit_safety_scores(
        &self,
        _room: Option<RoomJid>,
        _target_stanza_id: StanzaId,
        _result: JudgmentResult,
    ) -> Result<(), WireSinkError> {
        Ok(())
    }
}

pub(super) fn to_wire_scores(
    result: &JudgmentResult,
) -> waddle_xmpp::xep::xep_waddle_safety_scores::SafetyScoresToSend {
    waddle_xmpp::xep::xep_waddle_safety_scores::SafetyScoresToSend {
        model_version: result.model_version.as_str().to_string(),
        scores: result
            .scores
            .iter()
            .map(
                |score| waddle_xmpp::xep::xep_waddle_safety_scores::SafetyScoreToSend {
                    category: score.category.as_str().to_string(),
                    probability: score.probability.value(),
                    taxonomy_version: score.taxonomy_version.as_str().to_string(),
                },
            )
            .collect(),
    }
}

/// Real implementation: fans a successful job's judgment result out to the
/// room's current *local* occupants (see this module's top-of-file scope
/// note). Constructed once at startup alongside the drain loop.
pub struct WebSocketStateSafetyScoresWireSink {
    state: std::sync::Arc<crate::server::routes::websocket::WebSocketState>,
}

impl WebSocketStateSafetyScoresWireSink {
    pub fn new(state: std::sync::Arc<crate::server::routes::websocket::WebSocketState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl SafetyScoresWireSink for WebSocketStateSafetyScoresWireSink {
    async fn emit_safety_scores(
        &self,
        room: Option<RoomJid>,
        target_stanza_id: StanzaId,
        result: JudgmentResult,
    ) -> Result<(), WireSinkError> {
        use waddle_xmpp::muc::room_actor::{GetSnapshot, RoomActor};
        use waddle_xmpp::muc::room_registry_actor::GetRoom;
        use waddle_xmpp::xep::xep_waddle_safety_scores::build_safety_scores_fastening_message;
        use waddle_xmpp::Stanza;

        let room = room.ok_or(WireSinkError::NoRoom)?;
        let room_jid: jid::BareJid = room
            .as_str()
            .parse()
            .map_err(|_| WireSinkError::InvalidRoomJid(room.as_str().to_string()))?;
        let room_actor: kameo::actor::ActorRef<RoomActor> = self
            .state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .map_err(|error| WireSinkError::RoomLookupFailed(format!("{error:?}")))?
            .ok_or_else(|| WireSinkError::RoomNotFound(room_jid.to_string()))?;
        let snapshot = room_actor
            .ask(GetSnapshot)
            .await
            .map_err(|error| WireSinkError::RoomLookupFailed(format!("{error:?}")))?;

        let message = build_safety_scores_fastening_message(
            room_jid,
            target_stanza_id.id.as_str(),
            &to_wire_scores(&result),
        );
        for occupant in snapshot.room.occupants.values() {
            for session in snapshot.room.get_occupant_sessions(&occupant.nick) {
                let mut frame = message.clone();
                frame.to = Some(jid::Jid::from(session.clone()));
                let _ = self
                    .state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&session, Stanza::Message(frame));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    pub(crate) struct CountingSink {
        count: Arc<AtomicUsize>,
    }

    impl CountingSink {
        pub(crate) fn new(count: Arc<AtomicUsize>) -> Self {
            Self { count }
        }
    }

    #[async_trait]
    impl SafetyScoresWireSink for CountingSink {
        async fn emit_safety_scores(
            &self,
            _room: Option<RoomJid>,
            _target_stanza_id: StanzaId,
            _result: JudgmentResult,
        ) -> Result<(), WireSinkError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn to_wire_scores_maps_every_field() {
        let result = JudgmentResult {
            model_version: waddle_extensions::JudgmentModelVersion::new("jev-1").expect("model"),
            scores: vec![waddle_extensions::JudgmentScore {
                category: waddle_extensions::JudgmentCategory::new("is_question")
                    .expect("category"),
                probability: waddle_extensions::JudgmentProbability::new(0.5).expect("prob"),
                taxonomy_version: waddle_extensions::JudgmentTaxonomyVersion::new("v1")
                    .expect("taxonomy"),
            }],
        };
        let wire = to_wire_scores(&result);
        assert_eq!(wire.model_version, "jev-1");
        assert_eq!(wire.scores.len(), 1);
        assert_eq!(wire.scores[0].category, "is_question");
        assert_eq!(wire.scores[0].probability, 0.5);
        assert_eq!(wire.scores[0].taxonomy_version, "v1");
    }
}
