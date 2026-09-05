use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use jid::BareJid;
use waddle_xmpp::ingress::{
    EffectMessageIdentity, IngressEffectIntent, IngressEffectKey, RecipientSmAppendIdentity,
    RelayTargetIdentity,
};
use waddle_xmpp::muc::RoomClaimFenceContext;
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
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
    AmbiguousDispatchToRoomRemote {
        room: BareJid,
        relay_target: RelayTargetIdentity,
    },
    OperationalFenceLoss,
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
    pub room_scope: Option<IngressShadowRoomScope>,
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

/// How the room named by a MUC-scoped stanza relates to this node's
/// authority. Both variants make the room a server authority for digest
/// input (a client-authored `<stanza-id by='room'/>` is stripped either
/// way); only a local fence is asserted inside the shadow transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressShadowRoomScope {
    /// The room effect executes on this node: the transaction asserts the
    /// exact room claim under `FOR SHARE` before any room-fenced write.
    LocalFence(IngressShadowRoomFence),
    /// The room effect was relayed to the owning node. The owner and epoch
    /// the relay used are recorded as observation-time context; this node
    /// never holds that claim, so the transaction must not assert it. The
    /// owner fences its own writes through remote write acceptance.
    RemoteAuthority(IngressShadowRoomFence),
}

impl IngressShadowRoomScope {
    pub fn room(&self) -> &BareJid {
        match self {
            Self::LocalFence(fence) | Self::RemoteAuthority(fence) => &fence.room,
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
    room_scope: Option<IngressShadowRoomScope>,
    intents: Vec<IngressEffectIntent>,
    intent_keys: BTreeSet<IngressEffectKey>,
    markers: Vec<ShadowDecisionMarker>,
    next_append_identity: u64,
    next_route_identity: u64,
    overflowed: bool,
}

impl IngressEffectCapture {
    pub fn new(stanza_lang: Option<Lang>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaptureState {
                stanza_lang,
                sanitized_message: None,
                room_scope: None,
                intents: Vec::new(),
                intent_keys: BTreeSet::new(),
                markers: Vec::new(),
                next_append_identity: 0,
                next_route_identity: 0,
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

    pub fn record_recipient_sm_append(&self, stream: SmSessionId) {
        self.with_state(|state| {
            let append_identity = RecipientSmAppendIdentity::new(state.next_append_identity);
            state.next_append_identity = state
                .next_append_identity
                .checked_add(1)
                .expect("capture append identity should not overflow in tests or production");
            let intent = IngressEffectIntent::RecipientSmAppend {
                stream,
                append_identity,
            };
            let key = intent.semantic_key();
            if state.intent_keys.insert(key) {
                state.intents.push(intent);
            }
        });
    }

    pub fn next_route_identity(&self) -> EffectMessageIdentity {
        let mut state = self
            .inner
            .lock()
            .expect("capture mutex should not be poisoned");
        let identity = EffectMessageIdentity::capture_ordinal(state.next_route_identity);
        state.next_route_identity = state
            .next_route_identity
            .checked_add(1)
            .expect("capture route identity should not overflow in tests or production");
        identity
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
                room_scope: state.room_scope.clone(),
                intents: state.intents.clone(),
                markers: state.markers.clone(),
            },
            Err(_) => IngressEffectCaptureSnapshot {
                stanza_lang: None,
                sanitized_message: None,
                room_scope: None,
                intents: Vec::new(),
                markers: vec![ShadowDecisionMarker::Overflow],
            },
        }
    }

    /// Bind the exact room claim this node holds for a locally executed
    /// room effect. The shadow transaction asserts it before any room-fenced
    /// write.
    pub fn record_room_fence(&self, room_fence: IngressShadowRoomFence) {
        self.with_state(|state| {
            state.room_scope = Some(IngressShadowRoomScope::LocalFence(room_fence));
        });
    }

    /// Replace parse-time room ownership with the claim a relayed delivery
    /// actually used. That claim belongs to the owning node, so it is kept
    /// as authority context only; this node's transaction never asserts it.
    pub fn record_remote_room_authority(&self, room_fence: IngressShadowRoomFence) {
        self.with_state(|state| {
            state.room_scope = Some(IngressShadowRoomScope::RemoteAuthority(room_fence));
        });
    }

    /// Discard a provisional room scope when the live MUC path never reached
    /// its actor snapshot boundary. The shadow must not assert an unrelated
    /// later room claim for a locally generated pre-dispatch error reply.
    pub fn clear_room_scope(&self) {
        self.with_state(|state| state.room_scope = None);
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
            route_identity: EffectMessageIdentity::capture_ordinal(0),
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
        assert_eq!(snapshot.room_scope, None);
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

        capture.clear_room_scope();

        assert_eq!(capture.snapshot().room_scope, None);
    }

    #[test]
    fn remote_room_authority_replaces_a_provisional_local_fence() {
        let capture = IngressEffectCapture::new(None);
        let room: BareJid = "room@muc.example.com".parse().expect("room jid");
        capture.record_room_fence(IngressShadowRoomFence {
            room: room.clone(),
            owner: NodeIdentity::new("this-node", "this-epoch"),
            claim_epoch: ClaimEpoch(3),
        });
        let relayed = IngressShadowRoomFence {
            room: room.clone(),
            owner: NodeIdentity::new("owner-node", "owner-epoch"),
            claim_epoch: ClaimEpoch(9),
        };

        capture.record_remote_room_authority(relayed.clone());

        let snapshot = capture.snapshot();
        assert_eq!(
            snapshot.room_scope,
            Some(IngressShadowRoomScope::RemoteAuthority(relayed))
        );
        assert_eq!(
            snapshot
                .room_scope
                .as_ref()
                .map(IngressShadowRoomScope::room),
            Some(&room)
        );
    }

    #[test]
    fn recipient_sm_append_sequence_is_capture_global_not_per_stream() {
        let capture = IngressEffectCapture::new(None);
        capture.record_recipient_sm_append(SmSessionId::new("stream-a"));
        capture.record_recipient_sm_append(SmSessionId::new("stream-a"));
        capture.record_recipient_sm_append(SmSessionId::new("stream-b"));

        let append_identities: Vec<_> = capture
            .snapshot()
            .intents
            .into_iter()
            .filter_map(|intent| match intent {
                IngressEffectIntent::RecipientSmAppend {
                    stream,
                    append_identity,
                } => Some((stream.to_string(), append_identity.as_u64())),
                _ => None,
            })
            .collect();

        assert_eq!(
            append_identities,
            vec![
                ("stream-a".to_string(), 0),
                ("stream-a".to_string(), 1),
                ("stream-b".to_string(), 2),
            ]
        );
    }
}
