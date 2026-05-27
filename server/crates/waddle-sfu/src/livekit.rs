//! Concrete [`crate::SfuService`] impl backed by a LiveKit deployment.
//!
//! Keeps an in-memory registry of active calls keyed by [`CallId`]
//! containing the set of joined [`Identity`] values, used by the MUC
//! focus path to decide when a call has ended.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use dashmap::DashMap;

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

#[derive(Debug)]
pub struct LiveKitSfu {
    config: SfuConfig,
    calls: DashMap<CallId, HashSet<Identity>>,
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
}

impl LiveKitSfu {
    pub fn new(config: SfuConfig) -> Self {
        Self {
            config,
            calls: DashMap::new(),
            issued: DashMap::new(),
            revoked: DashMap::new(),
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
        let remaining = match self.calls.get_mut(call_id) {
            Some(mut entry) => {
                entry.remove(identity);
                entry.len()
            }
            None => 0,
        };

        // Revoke every JWT issued to this (call, identity). Token
        // theft after the legitimate hangup is the threat model;
        // see `revoked` field comment for the LiveKit-cooperation
        // gap that makes this advisory today. Each revocation
        // carries the original token's `exp` so the entry can be
        // swept once it would have lapsed anyway.
        if let Some((_, issued)) = self.issued.remove(&(call_id.clone(), identity.clone())) {
            for issued in issued {
                self.revoked.insert(issued.jti, issued.exp);
            }
        }

        // Opportunistically sweep revoked entries past their
        // original expiry. This keeps the map bounded under steady
        // call churn — every unregister cleans up at least as much
        // as it adds, plus any older entries that have aged out.
        self.sweep_expired_revoked(Utc::now());

        if remaining == 0 {
            self.calls.remove(call_id);
            CallState::Ended
        } else {
            CallState::Active { remaining }
        }
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
        let sfu = LiveKitSfu::new(fixture_config());
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
        let sfu = LiveKitSfu::new(fixture_config());
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
        let sfu = LiveKitSfu::new(fixture_config());
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
        let sfu = LiveKitSfu::new(fixture_config());
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
        let sfu = LiveKitSfu::new(fixture_config());
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
        let sfu = LiveKitSfu::new(fixture_config());
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
        let sfu = LiveKitSfu::new(fixture_config());

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
        let sfu = LiveKitSfu::new(fixture_config());
        let call = CallId::new("c1").unwrap();
        let identity = fixture_identity("alice");

        sfu.register_call_participant(&call, &identity);
        sfu.register_call_participant(&call, &identity);
        assert_eq!(sfu.participant_count(&call), 1);
    }
}
