use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use jid::BareJid;
use waddle_xmpp::ingress::{IngressEffectIntent, IngressEffectKey};
use waddle_xmpp::muc::RoomClaimFenceContext;
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use xmpp_parsers::message::{Lang, Message};

const MAX_CAPTURE_ENTRIES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShadowAuthorizationDeniedReason {
    BlockedSender,
    Forbidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowSemanticRejectedReason {
    ClientAuthoredFrameworkEnvelope,
    ClientAuthoredInboxPayload,
    MalformedPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowDecisionMarker {
    AuthorizationDenied {
        reason: ShadowAuthorizationDeniedReason,
    },
    SemanticRejected {
        reason: ShadowSemanticRejectedReason,
    },
    Overflow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IngressEffectCaptureSnapshot {
    pub stanza_lang: Option<Lang>,
    /// Sanitized, pre-rewrite stanza captured by the message handler.
    pub sanitized_message: Option<Message>,
    pub room_fence: Option<IngressShadowRoomFence>,
    pub intents: Vec<IngressEffectIntent>,
    pub markers: Vec<ShadowDecisionMarker>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressShadowRoomFence {
    pub room: BareJid,
    pub owner: NodeIdentity,
    pub claim_epoch: ClaimEpoch,
}

impl IngressShadowRoomFence {
    pub fn from_context(room: &BareJid, context: &RoomClaimFenceContext) -> Self {
        Self {
            room: room.clone(),
            owner: context.owner.clone(),
            claim_epoch: context.epoch,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IngressEffectCapture {
    inner: Arc<Mutex<CaptureState>>,
}

#[derive(Debug)]
struct CaptureState {
    stanza_lang: Option<Lang>,
    sanitized_message: Option<Message>,
    room_fence: Option<IngressShadowRoomFence>,
    intents: Vec<IngressEffectIntent>,
    intent_keys: BTreeSet<IngressEffectKey>,
    markers: Vec<ShadowDecisionMarker>,
    overflowed: bool,
}

impl IngressEffectCapture {
    pub fn new(stanza_lang: Option<Lang>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaptureState {
                stanza_lang,
                sanitized_message: None,
                room_fence: None,
                intents: Vec::new(),
                intent_keys: BTreeSet::new(),
                markers: Vec::new(),
                overflowed: false,
            })),
        }
    }

    pub fn record_intent(&self, intent: IngressEffectIntent) {
        self.with_state(|state| {
            let key = intent.semantic_key();
            if state.intent_keys.insert(key) {
                state.intents.push(intent.clone());
            }
        });
    }

    pub fn record_marker(&self, marker: ShadowDecisionMarker) {
        self.with_state(|state| {
            if !state.markers.contains(&marker) {
                state.markers.push(marker.clone());
            }
        });
    }

    /// Replaces the raw ingress clone with the handler's sanitized message.
    pub fn record_sanitized_message(&self, message: &Message) {
        self.with_state(|state| state.sanitized_message = Some(message.clone()));
    }

    pub fn snapshot(&self) -> IngressEffectCaptureSnapshot {
        match self.inner.lock() {
            Ok(state) => IngressEffectCaptureSnapshot {
                stanza_lang: state.stanza_lang.clone(),
                sanitized_message: state.sanitized_message.clone(),
                room_fence: state.room_fence.clone(),
                intents: state.intents.clone(),
                markers: state.markers.clone(),
            },
            Err(_) => IngressEffectCaptureSnapshot {
                stanza_lang: None,
                sanitized_message: None,
                room_fence: None,
                intents: Vec::new(),
                markers: vec![ShadowDecisionMarker::Overflow],
            },
        }
    }

    pub fn record_room_fence(&self, room_fence: IngressShadowRoomFence) {
        self.with_state(|state| state.room_fence = Some(room_fence));
    }

    /// Discard a provisional room scope when the live MUC path never reached
    /// its actor snapshot boundary. The shadow must not assert an unrelated
    /// later room claim for a locally generated pre-dispatch error reply.
    pub fn clear_room_fence(&self) {
        self.with_state(|state| state.room_fence = None);
    }

    fn with_state(&self, action: impl FnOnce(&mut CaptureState)) {
        let mut state = match self.inner.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        if state.overflowed {
            return;
        }
        action(&mut state);
        let entry_count = state.intents.len() + state.markers.len();
        if entry_count > MAX_CAPTURE_ENTRIES {
            state.intents.clear();
            state.intent_keys.clear();
            state.markers.clear();
            state.markers.push(ShadowDecisionMarker::Overflow);
            state.overflowed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::{BareJid, FullJid};
    use waddle_xmpp_core::xep0359::StanzaId;

    #[test]
    fn dedupes_intents_and_markers() {
        let capture = IngressEffectCapture::new(Some(Lang::from("en")));
        let intent = IngressEffectIntent::RouteDirect {
            recipient: "bob@example.com".parse::<BareJid>().expect("bare jid"),
            fanout: vec!["bob@example.com/phone"
                .parse::<FullJid>()
                .expect("full jid")],
        };
        capture.record_intent(intent.clone());
        capture.record_intent(intent);
        capture.record_marker(ShadowDecisionMarker::SemanticRejected {
            reason: ShadowSemanticRejectedReason::MalformedPayload,
        });
        capture.record_marker(ShadowDecisionMarker::SemanticRejected {
            reason: ShadowSemanticRejectedReason::MalformedPayload,
        });

        let snapshot = capture.snapshot();
        assert_eq!(snapshot.stanza_lang, Some(Lang::from("en")));
        assert_eq!(snapshot.room_fence, None);
        assert_eq!(snapshot.intents.len(), 1);
        assert_eq!(snapshot.markers.len(), 1);
    }

    #[test]
    fn overflow_stops_accumulation_and_records_marker() {
        let capture = IngressEffectCapture::new(None);
        for index in 0..=MAX_CAPTURE_ENTRIES {
            let bare: BareJid = format!("user-{index}@example.com")
                .parse()
                .expect("bare jid");
            capture.record_intent(IngressEffectIntent::ArchiveAuthoritative {
                archive: bare.clone(),
                stanza_id: StanzaId::new(format!("sid-{index}"), jid::Jid::from(bare.clone())),
                by: bare,
            });
        }
        capture.record_marker(ShadowDecisionMarker::SemanticRejected {
            reason: ShadowSemanticRejectedReason::ClientAuthoredInboxPayload,
        });

        let snapshot = capture.snapshot();
        assert!(snapshot.intents.is_empty());
        assert_eq!(snapshot.markers, vec![ShadowDecisionMarker::Overflow]);
    }

    #[test]
    fn clearing_a_provisional_room_fence_removes_room_authority() {
        let capture = IngressEffectCapture::new(None);
        capture.record_room_fence(IngressShadowRoomFence {
            room: "room@muc.example.com".parse().expect("room jid"),
            owner: NodeIdentity::new("room-node", "room-epoch"),
            claim_epoch: ClaimEpoch(3),
        });

        capture.clear_room_fence();

        assert_eq!(capture.snapshot().room_fence, None);
    }
}
