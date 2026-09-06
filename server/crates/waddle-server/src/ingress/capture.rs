//! Bounded, deduplicated typed obligations collected during Phase A.
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use waddle_xmpp::ingress::{
    EffectMessageIdentity, IngressEffectIntent, IngressEffectKey, RecipientSmAppendIdentity,
};
use waddle_xmpp::pending_delivery::SmSessionId;
const MAX_CAPTURE_ENTRIES: usize = 128;
#[derive(Debug, Clone, PartialEq)]
pub struct IngressEffectCaptureSnapshot {
    pub intents: Vec<IngressEffectIntent>,
    pub overflowed: bool,
}
impl Default for IngressEffectCapture {
    fn default() -> Self {
        Self::new()
    }
}
#[derive(Debug, Clone)]
pub struct IngressEffectCapture {
    inner: Arc<Mutex<CaptureState>>,
}

#[derive(Debug)]
struct CaptureState {
    intents: Vec<IngressEffectIntent>,
    intent_keys: BTreeSet<IngressEffectKey>,
    next_append_identity: u64,
    next_route_identity: u64,
    overflowed: bool,
}

impl IngressEffectCapture {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaptureState {
                intents: Vec::new(),
                intent_keys: BTreeSet::new(),
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

    pub fn snapshot(&self) -> IngressEffectCaptureSnapshot {
        match self.inner.lock() {
            Ok(state) => IngressEffectCaptureSnapshot {
                intents: state.intents.clone(),
                overflowed: state.overflowed,
            },
            Err(_) => IngressEffectCaptureSnapshot {
                intents: Vec::new(),
                overflowed: true,
            },
        }
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
        let entry_count = state.intents.len();
        if entry_count > MAX_CAPTURE_ENTRIES {
            state.intents.clear();
            state.intent_keys.clear();
            state.overflowed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicates_typed_intents() {
        let capture = IngressEffectCapture::new();
        let intent = IngressEffectIntent::RouteDirect {
            recipient: "bob@example.com".parse().expect("recipient"),
            fanout: Vec::new(),
            route_identity: EffectMessageIdentity::capture_ordinal(0),
        };
        capture.record_intent(intent.clone());
        capture.record_intent(intent);
        assert_eq!(capture.snapshot().intents.len(), 1);
        assert!(!capture.snapshot().overflowed);
    }

    #[test]
    fn overflow_discards_incomplete_obligations_and_stays_overflowed() {
        let capture = IngressEffectCapture::new();
        for _ in 0..=MAX_CAPTURE_ENTRIES {
            capture.record_recipient_sm_append(SmSessionId::new("recipient"));
        }
        capture.record_recipient_sm_append(SmSessionId::new("later"));
        assert!(capture.snapshot().overflowed);
        assert!(capture.snapshot().intents.is_empty());
    }

    #[test]
    fn append_identity_is_global_across_recipient_streams() {
        let capture = IngressEffectCapture::new();
        for stream in ["first", "first", "second"] {
            capture.record_recipient_sm_append(SmSessionId::new(stream));
        }
        let ordinals: Vec<_> = capture
            .snapshot()
            .intents
            .into_iter()
            .map(|intent| match intent {
                IngressEffectIntent::RecipientSmAppend {
                    append_identity, ..
                } => append_identity.as_u64(),
                _ => panic!("expected append intent"),
            })
            .collect();
        assert_eq!(ordinals, [0, 1, 2]);
    }
}
