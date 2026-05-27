//! Concrete [`crate::SfuService`] impl backed by a LiveKit deployment.
//!
//! Keeps an in-memory registry of active calls keyed by [`CallId`]
//! containing the set of joined [`Identity`] values, used by the MUC
//! focus path to decide when a call has ended.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::runtime::Handle;
use tokio::sync::Semaphore;

use crate::admin::{admin_base_url_from_ws, LiveKitAdmin, ReqwestLiveKitAdmin};
use crate::call::{CallId, CallState, Identity, MediaCapabilities};
use crate::config::{SfuConfig, WebsocketUrl};
use crate::error::SfuError;
use crate::token::{mint_join_token, IssuedJti, JoinToken, Jti, MintInputs};
use crate::turn::{mint_turn_credential, TurnCredential, TurnHost};
use crate::SfuService;

/// Upper bound on outstanding (un-revoked) JTIs tracked per
/// `(call, identity)`. A participant should never be sitting on more
/// than a handful of concurrent tokens — every reconnect mints a
/// fresh one and the previous one is supposed to drop. The cap turns
/// a buggy client (or a malicious one trying to wedge the tracker)
/// from an unbounded memory leak into a strict FIFO: the oldest
/// outstanding JTI is dropped and forgotten when the cap is hit.
pub(crate) const MAX_ISSUED_PER_PARTICIPANT: usize = 16;

/// Upper bound on concurrent LiveKit admin REST calls in flight.
/// `unregister_call_participant` fires-and-forgets these from the
/// teardown hot path; with one HTTP round-trip + 5s timeout per call
/// a burst of session-terminates would otherwise spawn arbitrarily
/// many reqwest tasks. The semaphore is a fixed-size FIFO valve.
const ADMIN_CONCURRENCY: usize = 32;

/// Shared registry of in-call participants. Held in an `Arc` so the
/// spawned admin teardown future can re-check membership before
/// firing `DeleteRoom`, closing the race where a fresh joiner
/// re-creates the call between local-clear and the remote evict.
type CallRegistry = Arc<DashMap<CallId, HashSet<Identity>>>;

pub struct LiveKitSfu {
    config: SfuConfig,
    calls: CallRegistry,
    /// Live JWT identifiers per `(call, identity)`, each carrying
    /// its `exp` so revocation entries can be swept once the token
    /// would have lapsed anyway. Capped at
    /// [`MAX_ISSUED_PER_PARTICIPANT`] entries per key — the oldest
    /// is evicted FIFO when a fresh token is minted past the cap so
    /// a misbehaving client cannot push the tracker into unbounded
    /// memory growth.
    issued: DashMap<(CallId, Identity), Vec<IssuedJti>>,
    /// Map of revoked JWT identifiers to the `exp` of the token they
    /// belonged to. Entries are swept lazily once `Utc::now() > exp`:
    /// a revoked token past its expiry cannot be replayed regardless
    /// of whether the SFU still remembers its jti, so keeping it in
    /// the map after that point is pure overhead. Bookkeeping today —
    /// LiveKit itself doesn't call back to verify jti, so a stolen
    /// token stays usable until its `exp`. Documented limitation; the
    /// path-to-real-revocation needs LiveKit cooperation (webhook
    /// validation hook) or a shared revocation store (Redis) once
    /// Waddle scales past a single SFU instance.
    revoked: DashMap<Jti, DateTime<Utc>>,
    /// LiveKit admin REST client. Used to evict participants and
    /// delete empty rooms when a teardown signal arrives over XMPP —
    /// without this, LiveKit only notices a hangup when the underlying
    /// PeerConnection's DTLS read deadline expires, leaving "dtls
    /// timeout" warnings in `livekit-sfu` logs and stale rooms
    /// lingering up to `empty_timeout` (default 5 min).
    admin: Arc<dyn LiveKitAdmin>,
    /// Captured at construction time. Remote evict happens as a
    /// `tokio::spawn` here so the synchronous `SfuService` surface
    /// can stay sync and the IQ-result on session-terminate never
    /// blocks on the SFU's admin endpoint. `None` when the SFU was
    /// constructed outside a Tokio runtime (only the local-bookkeeping
    /// unit tests do this) — in that case the remote leg is a no-op,
    /// matching pre-LK-admin behaviour for those tests.
    runtime: Option<Handle>,
    /// Bounds the number of concurrent admin REST calls in flight so
    /// a teardown burst can't fan out into thousands of reqwest tasks
    /// — see [`ADMIN_CONCURRENCY`] for the cap.
    admin_permits: Arc<Semaphore>,
}

impl std::fmt::Debug for LiveKitSfu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveKitSfu")
            .field("config", &self.config)
            .field("calls_len", &self.calls.len())
            .field("issued_len", &self.issued.len())
            .field("revoked_len", &self.revoked.len())
            .field("runtime_attached", &self.runtime.is_some())
            .finish()
    }
}

impl LiveKitSfu {
    /// Build a [`LiveKitSfu`] backed by the production reqwest admin
    /// client. Captures the current Tokio runtime handle if one is
    /// active so the teardown hot path can fire `RemoveParticipant` +
    /// `DeleteRoom` admin calls without making the [`SfuService`]
    /// surface async.
    pub fn new(config: SfuConfig) -> Result<Self, SfuError> {
        let admin_base = admin_base_url_from_ws(&config.ws_url)?;
        let admin = ReqwestLiveKitAdmin::new(
            admin_base,
            config.api_key.clone(),
            config.api_secret.clone(),
        )?;
        Ok(Self::with_admin(config, Arc::new(admin)))
    }

    /// Build with a caller-supplied admin client. Used by tests to
    /// inject a recording mock without standing up an HTTP server;
    /// production code goes through [`Self::new`] which constructs a
    /// real [`crate::admin::ReqwestLiveKitAdmin`] backed by reqwest.
    pub fn with_admin(config: SfuConfig, admin: Arc<dyn LiveKitAdmin>) -> Self {
        Self {
            config,
            calls: Arc::new(DashMap::new()),
            issued: DashMap::new(),
            revoked: DashMap::new(),
            admin,
            runtime: Handle::try_current().ok(),
            admin_permits: Arc::new(Semaphore::new(ADMIN_CONCURRENCY)),
        }
    }

    pub fn config(&self) -> &SfuConfig {
        &self.config
    }

    /// Number of distinct identities currently registered against
    /// `call_id`. Exposed for test inspection; production code calls
    /// [`Self::unregister_call_participant`] which already returns a
    /// [`CallState`] derived from this.
    pub fn participant_count(&self, call_id: &CallId) -> usize {
        self.calls.get(call_id).map(|e| e.len()).unwrap_or(0)
    }

    /// Number of currently-tracked revoked JTIs. Exposed for tests
    /// to pin the bound on the revocation map.
    #[cfg(test)]
    pub(crate) fn revoked_count(&self) -> usize {
        self.revoked.len()
    }

    /// Number of currently-tracked issued JTIs for `(call, identity)`.
    /// Exposed for tests to pin the per-participant FIFO bound.
    #[cfg(test)]
    pub(crate) fn issued_count(&self, call_id: &CallId, identity: &Identity) -> usize {
        self.issued
            .get(&(call_id.clone(), identity.clone()))
            .map(|e| e.len())
            .unwrap_or(0)
    }

    /// Drop every revocation entry whose original token would have
    /// expired at or before `now`. Called from
    /// [`SfuService::unregister_call_participant`] so the map stays
    /// bounded under steady call churn.
    fn sweep_expired_revoked(&self, now: DateTime<Utc>) {
        self.revoked.retain(|_, exp| *exp > now);
    }

    /// Drop `identity` from the in-memory registry and revoke every
    /// JWT it ever held. Returns `(was_present, remaining_after)` so
    /// the caller can distinguish "this participant just left the
    /// last seat" from "we never knew about this participant at all"
    /// — both look like `remaining == 0` but only the first warrants
    /// a `DeleteRoom` admin call.
    fn clear_local_state(&self, call_id: &CallId, identity: &Identity) -> (bool, usize) {
        let (was_present, remaining) = match self.calls.get_mut(call_id) {
            Some(mut entry) => {
                let was_present = entry.remove(identity);
                (was_present, entry.len())
            }
            None => (false, 0),
        };

        if let Some((_, issued)) = self.issued.remove(&(call_id.clone(), identity.clone())) {
            for issued in issued {
                self.revoked.insert(issued.jti, issued.exp);
            }
        }
        self.sweep_expired_revoked(Utc::now());

        if remaining == 0 {
            self.calls.remove(call_id);
        }

        (was_present, remaining)
    }

    /// Fire-and-forget the LiveKit admin REST calls that mirror a
    /// local unregister. `RemoveParticipant` always runs because
    /// LiveKit may know about the participant even when our local
    /// registry has lost track of them (stale state, alternate
    /// federation entry). `DeleteRoom` only fires when we *know* we
    /// just emptied a call — gated on `was_present && remaining == 0`
    /// at the call site — and even then re-checks `calls` inside the
    /// spawn to close the rejoin race: another participant may have
    /// registered in the same tick, in which case kicking the room
    /// would evict them too. Spawn target is the runtime handle
    /// captured at construction; when none is attached (e.g. plain
    /// `#[test]` fixtures) the remote leg silently drops, matching
    /// pre-admin behaviour for those tests. The admin concurrency
    /// semaphore bounds in-flight HTTP tasks so a teardown burst
    /// can't fan out unboundedly.
    fn schedule_remote_teardown(&self, call_id: CallId, identity: Identity, we_just_emptied: bool) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let admin = Arc::clone(&self.admin);
        let permits = Arc::clone(&self.admin_permits);
        let calls = Arc::clone(&self.calls);
        runtime.spawn(async move {
            // `acquire_owned` returns `Err` only if the semaphore was
            // closed — which only happens if the SFU itself is being
            // dropped, in which case admitting more tasks is moot.
            let Ok(_permit) = permits.acquire_owned().await else {
                return;
            };

            if let Err(err) = admin.remove_participant(&call_id, &identity).await {
                tracing::warn!(
                    call_id = %call_id,
                    identity = %identity.as_livekit_identity(),
                    error = %err,
                    "LiveKit RemoveParticipant failed; SFU may rely on DTLS timeout"
                );
            }
            if we_just_emptied {
                // Rejoin race: between local-clear and this point a
                // fresh participant may have re-registered (same
                // `call_id` is shared across all Muji occupants of a
                // MUC). `DeleteRoom` would evict that just-joined
                // session, so only proceed when the call is *still*
                // empty in our local view.
                if calls.get(&call_id).is_none() {
                    if let Err(err) = admin.delete_room(&call_id).await {
                        tracing::warn!(
                            call_id = %call_id,
                            error = %err,
                            "LiveKit DeleteRoom failed; empty room will linger until empty_timeout"
                        );
                    }
                }
            }
        });
    }
}

impl SfuService for LiveKitSfu {
    fn issue_join_token(
        &self,
        call_id: &CallId,
        identity: &Identity,
        capabilities: MediaCapabilities,
    ) -> Result<JoinToken, SfuError> {
        let token = mint_join_token(MintInputs {
            api_key: &self.config.api_key,
            api_secret: &self.config.api_secret,
            ws_url: &self.config.ws_url,
            call_id,
            identity,
            capabilities,
            ttl: self.config.token_ttl,
        })?;
        // Track the (jti, exp) pair against `(call, identity)` so a
        // subsequent unregister revokes every JWT this participant
        // ever held for the call. Cap the per-participant vec to
        // bound memory under reconnect storms or a misbehaving
        // client; oldest entries are evicted FIFO and silently
        // forgotten (their tokens will simply lapse on their own
        // `exp`, which the rest of this struct already relies on).
        let mut entry = self
            .issued
            .entry((call_id.clone(), identity.clone()))
            .or_default();
        while entry.len() >= MAX_ISSUED_PER_PARTICIPANT {
            entry.remove(0);
        }
        entry.push(IssuedJti {
            jti: token.jti.clone(),
            exp: token.expires_at,
        });
        Ok(token)
    }

    fn issue_turn_credentials(&self, identity: &Identity) -> Result<TurnCredential, SfuError> {
        mint_turn_credential(
            &self.config.turn_shared_secret,
            identity,
            self.config.turn_ttl,
        )
    }

    fn register_call_participant(&self, call_id: &CallId, identity: &Identity) {
        self.calls
            .entry(call_id.clone())
            .or_default()
            .insert(identity.clone());
    }

    fn has_call_participant(&self, call_id: &CallId, identity: &Identity) -> bool {
        self.calls
            .get(call_id)
            .is_some_and(|entry| entry.contains(identity))
    }

    fn unregister_call_participant(&self, call_id: &CallId, identity: &Identity) -> CallState {
        let (was_present, remaining) = self.clear_local_state(call_id, identity);

        let state = if was_present && remaining == 0 {
            CallState::Ended
        } else {
            // `Active { remaining }` covers two cases that look the
            // same to the caller (don't broadcast "call ended"): the
            // normal "other participants are still here" path, and
            // the defensive "this participant was never registered"
            // path where `remaining == 0` does not imply we just
            // emptied a known-active call. Treating an unknown
            // identity as `Ended` would broadcast a phantom
            // call-ended signal to the MUC.
            CallState::Active { remaining }
        };

        // Schedule the LiveKit-side evict. `RemoveParticipant` always
        // runs (LiveKit may know about the participant even when our
        // local registry has lost track — federation, stale state).
        // `DeleteRoom` only fires when we know we just emptied a
        // call we previously tracked, and the spawn re-checks the
        // registry inside the future to close the rejoin race.
        let we_just_emptied = was_present && remaining == 0;
        self.schedule_remote_teardown(call_id.clone(), identity.clone(), we_just_emptied);

        state
    }

    fn note_participant_left(&self, call_id: &CallId, identity: &Identity) {
        // LiveKit's `participant_left` webhook is the SFU
        // acknowledging it already removed the participant — usually
        // because we asked it to. Doing only the local cleanup avoids
        // a feedback loop where the webhook fires another
        // `RemoveParticipant` against an already-removed participant
        // (LiveKit would return `not_found`, which is mapped to
        // success, but the round-trip is wasted and amplifies the
        // race with quick rejoins).
        let _ = self.clear_local_state(call_id, identity);
    }

    fn is_revoked(&self, jti: &Jti) -> bool {
        // Lazy sweep on the read path: an entry past its `exp` is
        // by definition unusable and so reads as not-revoked. Drop
        // it from the map so memory doesn't grow on every check.
        if let Some(entry) = self.revoked.get(jti) {
            let exp = *entry.value();
            drop(entry);
            if Utc::now() >= exp {
                self.revoked.remove(jti);
                return false;
            }
            return true;
        }
        false
    }

    fn ws_url(&self) -> &WebsocketUrl {
        &self.config.ws_url
    }

    fn turn_host(&self) -> &TurnHost {
        &self.config.turn_host
    }

    fn webhook_secret(&self) -> &crate::config::ApiSecret {
        &self.config.webhook_secret
    }

    fn participants_for_call(&self, call_id: &CallId) -> Vec<Identity> {
        self.calls
            .get(call_id)
            .map(|entry| entry.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiKey, ApiSecret, TurnSharedSecret};
    use chrono::Duration;
    use jid::FullJid;
    use url::Url;

    fn fixture_config() -> SfuConfig {
        SfuConfig {
            api_key: ApiKey::new("APIxxxxxxxx"),
            api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("test secret meets min length"),
            webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("test secret meets min length"),
            ws_url: WebsocketUrl::new(Url::parse("wss://livekit.waddle.social").unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.waddle.social"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-shared-secret-value"),
            token_ttl: Duration::seconds(3600),
            turn_ttl: Duration::seconds(3600),
        }
    }

    fn fixture_identity(name: &str) -> Identity {
        let jid: FullJid = format!("{name}@waddle.social/desktop")
            .parse()
            .expect("jid");
        Identity::from_jid(jid)
    }

    #[test]
    fn registry_tracks_participants_per_call() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("r1").unwrap();
        let a = fixture_identity("alice");
        let b = fixture_identity("bob");

        sfu.register_call_participant(&call, &a);
        sfu.register_call_participant(&call, &b);
        assert_eq!(sfu.participant_count(&call), 2);

        match sfu.unregister_call_participant(&call, &a) {
            CallState::Active { remaining } => assert_eq!(remaining, 1),
            CallState::Ended => panic!("call should still be active"),
        }

        match sfu.unregister_call_participant(&call, &b) {
            CallState::Ended => {}
            CallState::Active { .. } => panic!("call should end with no participants"),
        }
        assert_eq!(sfu.participant_count(&call), 0);
    }

    #[test]
    fn issue_join_token_returns_room_scoped_jwt() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c1").unwrap();
        let identity = fixture_identity("alice");

        let token = sfu
            .issue_join_token(&call, &identity, MediaCapabilities::full_participant())
            .expect("token issued");
        assert_eq!(token.room, call);
        assert!(!token.jwt.as_str().is_empty());
    }

    #[test]
    fn issue_turn_credentials_yields_time_limited_pair() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let identity = fixture_identity("alice");
        let cred = sfu.issue_turn_credentials(&identity).expect("cred issued");
        assert!(cred.expires_at > chrono::Utc::now());
        assert!(cred
            .username
            .as_str()
            .contains("alice@waddle.social/desktop"));
    }

    #[test]
    fn unregister_revokes_every_jti_issued_to_the_participant() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c-revoke").unwrap();
        let alice = fixture_identity("alice");

        let t1 = sfu
            .issue_join_token(&call, &alice, MediaCapabilities::full_participant())
            .unwrap();
        let t2 = sfu
            .issue_join_token(&call, &alice, MediaCapabilities::full_participant())
            .unwrap();
        assert!(!sfu.is_revoked(&t1.jti));
        assert!(!sfu.is_revoked(&t2.jti));

        // Register + unregister: every previously-issued jti must
        // be revoked once the participant has left the call.
        sfu.register_call_participant(&call, &alice);
        sfu.unregister_call_participant(&call, &alice);

        assert!(sfu.is_revoked(&t1.jti));
        assert!(sfu.is_revoked(&t2.jti));
    }

    #[test]
    fn revocation_is_scoped_per_participant() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c-scope").unwrap();
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");

        let alice_token = sfu
            .issue_join_token(&call, &alice, MediaCapabilities::full_participant())
            .unwrap();
        let bob_token = sfu
            .issue_join_token(&call, &bob, MediaCapabilities::full_participant())
            .unwrap();

        sfu.register_call_participant(&call, &alice);
        sfu.register_call_participant(&call, &bob);
        sfu.unregister_call_participant(&call, &alice);

        // Alice's hangup must not revoke bob's still-active token.
        assert!(sfu.is_revoked(&alice_token.jti));
        assert!(!sfu.is_revoked(&bob_token.jti));
    }

    #[test]
    fn issued_jti_vec_is_capped_per_participant() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c-cap").unwrap();
        let alice = fixture_identity("alice");

        // Mint well past the cap; every fresh token should slot in,
        // but the per-participant vec must never exceed it.
        for _ in 0..(MAX_ISSUED_PER_PARTICIPANT * 3) {
            sfu.issue_join_token(&call, &alice, MediaCapabilities::full_participant())
                .expect("token issued");
            assert!(
                sfu.issued_count(&call, &alice) <= MAX_ISSUED_PER_PARTICIPANT,
                "issued vec must stay <= MAX_ISSUED_PER_PARTICIPANT"
            );
        }
        assert_eq!(
            sfu.issued_count(&call, &alice),
            MAX_ISSUED_PER_PARTICIPANT,
            "issued vec must saturate exactly at the cap"
        );
    }

    #[test]
    fn revoked_entries_are_swept_once_past_expiry() {
        use chrono::Duration as ChronoDuration;
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");

        // Seed the revoked map directly with a past-exp entry so
        // the test does not depend on real-time tickdown of the
        // token TTL.
        let stale_jti = Jti::new();
        let fresh_jti = Jti::new();
        sfu.revoked
            .insert(stale_jti.clone(), Utc::now() - ChronoDuration::seconds(60));
        sfu.revoked
            .insert(fresh_jti.clone(), Utc::now() + ChronoDuration::seconds(60));

        // Reading the stale jti must return false (the token can
        // no longer be replayed regardless) AND drop the entry.
        assert!(!sfu.is_revoked(&stale_jti));
        assert!(sfu.is_revoked(&fresh_jti));
        assert_eq!(sfu.revoked_count(), 1);

        // Running the unregister-path sweep clears any other stale
        // entries that piled up since the last sweep.
        sfu.revoked
            .insert(Jti::new(), Utc::now() - ChronoDuration::seconds(1));
        let alice = fixture_identity("alice");
        let call = CallId::new("c-sweep").unwrap();
        sfu.register_call_participant(&call, &alice);
        sfu.unregister_call_participant(&call, &alice);
        assert_eq!(
            sfu.revoked_count(),
            1,
            "unregister sweep must clear past-exp entries; one fresh entry should remain"
        );
    }

    #[test]
    fn register_is_idempotent() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c1").unwrap();
        let identity = fixture_identity("alice");

        sfu.register_call_participant(&call, &identity);
        sfu.register_call_participant(&call, &identity);
        assert_eq!(sfu.participant_count(&call), 1);
    }

    // -------- Admin-evict path (tokio runtime present) --------

    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    use crate::admin::LiveKitAdmin;

    #[derive(Default)]
    struct RecordingAdmin {
        remove_calls: Mutex<Vec<(CallId, Identity)>>,
        delete_calls: Mutex<Vec<CallId>>,
    }

    impl RecordingAdmin {
        fn remove_snapshot(&self) -> Vec<(CallId, Identity)> {
            self.remove_calls.lock().expect("recording lock").clone()
        }

        fn delete_snapshot(&self) -> Vec<CallId> {
            self.delete_calls.lock().expect("recording lock").clone()
        }
    }

    impl LiveKitAdmin for RecordingAdmin {
        fn remove_participant<'a>(
            &'a self,
            room: &'a CallId,
            identity: &'a Identity,
        ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
            let room = room.clone();
            let identity = identity.clone();
            Box::pin(async move {
                self.remove_calls
                    .lock()
                    .expect("recording lock")
                    .push((room, identity));
                Ok(())
            })
        }

        fn delete_room<'a>(
            &'a self,
            room: &'a CallId,
        ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
            let room = room.clone();
            Box::pin(async move {
                self.delete_calls.lock().expect("recording lock").push(room);
                Ok(())
            })
        }
    }

    /// Yield enough times for any spawned admin task on the current
    /// runtime to make progress. The spawned future does a couple of
    /// `Mutex` operations and returns, so two yields are more than
    /// sufficient; tighten or loosen if `RecordingAdmin` grows steps.
    async fn drain_admin_tasks() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn unregister_schedules_remove_participant_on_the_admin_client() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-evict").unwrap();
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");

        sfu.register_call_participant(&call, &alice);
        sfu.register_call_participant(&call, &bob);

        // Alice leaves: RemoveParticipant must fire; DeleteRoom must
        // NOT fire because bob is still in the call.
        let state = sfu.unregister_call_participant(&call, &alice);
        assert!(matches!(state, CallState::Active { remaining: 1 }));
        drain_admin_tasks().await;

        let removes = admin.remove_snapshot();
        assert_eq!(
            removes.len(),
            1,
            "RemoveParticipant should fire exactly once"
        );
        assert_eq!(&removes[0].0, &call);
        assert_eq!(
            removes[0].1.as_livekit_identity(),
            alice.as_livekit_identity()
        );
        assert!(
            admin.delete_snapshot().is_empty(),
            "DeleteRoom must not fire while the call still has participants"
        );
    }

    #[tokio::test]
    async fn unregister_last_participant_also_schedules_delete_room() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-empty").unwrap();
        let alice = fixture_identity("alice");

        sfu.register_call_participant(&call, &alice);
        let state = sfu.unregister_call_participant(&call, &alice);
        assert_eq!(state, CallState::Ended);
        drain_admin_tasks().await;

        let deletes = admin.delete_snapshot();
        assert_eq!(deletes.len(), 1, "DeleteRoom must fire on last participant");
        assert_eq!(&deletes[0], &call);

        let removes = admin.remove_snapshot();
        assert_eq!(
            removes.len(),
            1,
            "RemoveParticipant still fires for the last leaver"
        );
        assert_eq!(&removes[0].0, &call);
    }

    #[tokio::test]
    async fn unregister_of_unknown_identity_fires_remove_participant_but_not_delete_room() {
        // Edge case: a session-terminate arrives without a matching
        // register (e.g. server-side state was lost, a client races
        // a re-init, a replayed terminate from a long-dead session).
        // `RemoveParticipant` must still fire because LiveKit may
        // hold the participant via a separate path. `DeleteRoom`
        // MUST NOT fire — we don't know the call's true state, and
        // tearing it down could evict participants we never tracked.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-ghost").unwrap();
        let ghost = fixture_identity("mallory");

        let state = sfu.unregister_call_participant(&call, &ghost);
        assert!(
            matches!(state, CallState::Active { remaining: 0 }),
            "ghost unregister must NOT report CallState::Ended; got {state:?}",
        );
        drain_admin_tasks().await;

        let removes = admin.remove_snapshot();
        assert_eq!(removes.len(), 1);
        assert_eq!(
            removes[0].1.as_livekit_identity(),
            ghost.as_livekit_identity()
        );
        assert!(
            admin.delete_snapshot().is_empty(),
            "DeleteRoom must not fire when we never tracked the participant",
        );
    }

    #[tokio::test]
    async fn note_participant_left_clears_local_state_without_admin_call() {
        // The LiveKit webhook bridge calls this path when LiveKit's
        // `participant_left` fires. Doing a back-channel admin
        // RemoveParticipant here would amplify the wire traffic (LK
        // would 404 our redundant call) and racily kick fresh
        // rejoiners. The trait contract forbids it; assert the
        // production impl honours it.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-webhook").unwrap();
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);

        sfu.note_participant_left(&call, &alice);
        drain_admin_tasks().await;

        assert_eq!(sfu.participant_count(&call), 0, "registry must be cleared");
        assert!(
            admin.remove_snapshot().is_empty(),
            "note_participant_left must NOT spawn RemoveParticipant",
        );
        assert!(
            admin.delete_snapshot().is_empty(),
            "note_participant_left must NOT spawn DeleteRoom",
        );
    }

    #[tokio::test]
    async fn last_participant_delete_room_skipped_when_someone_rejoins() {
        // Race: Alice hangs up (clearing local state + scheduling
        // teardown), Bob joins the same MUC call before the spawn
        // gets to its DeleteRoom step. The re-check inside the
        // spawn must observe Bob's registration and suppress
        // DeleteRoom so Bob's session is not evicted. We simulate
        // the rejoin by registering Bob immediately after Alice's
        // unregister returns, before yielding to the spawn.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-rejoin").unwrap();
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");

        sfu.register_call_participant(&call, &alice);
        let state = sfu.unregister_call_participant(&call, &alice);
        assert_eq!(state, CallState::Ended);

        // Bob rejoins before the spawned future polls. With a single-
        // threaded current-thread runtime this synchronous register
        // is guaranteed to land before any `yield_now`-scheduled
        // continuation observes the registry.
        sfu.register_call_participant(&call, &bob);

        drain_admin_tasks().await;

        let removes = admin.remove_snapshot();
        assert_eq!(
            removes.len(),
            1,
            "RemoveParticipant for Alice must still fire"
        );
        assert!(
            admin.delete_snapshot().is_empty(),
            "DeleteRoom must be suppressed by the rejoin re-check; got {:?}",
            admin.delete_snapshot(),
        );
    }
}
