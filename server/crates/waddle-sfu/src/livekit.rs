//! Concrete [`crate::SfuService`] impl backed by a LiveKit deployment.
//!
//! Keeps an in-memory registry of active calls keyed by [`CallId`]
//! containing the set of joined [`Identity`] values, used by the MUC
//! focus path to decide when a call has ended.

use std::collections::HashSet;

use dashmap::DashMap;

use crate::call::{CallId, CallState, Identity, MediaCapabilities};
use crate::config::{SfuConfig, WebsocketUrl};
use crate::error::SfuError;
use crate::token::{mint_join_token, JoinToken, MintInputs};
use crate::turn::{mint_turn_credential, TurnCredential, TurnHost};
use crate::SfuService;

#[derive(Debug)]
pub struct LiveKitSfu {
    config: SfuConfig,
    calls: DashMap<CallId, HashSet<Identity>>,
}

impl LiveKitSfu {
    pub fn new(config: SfuConfig) -> Self {
        Self {
            config,
            calls: DashMap::new(),
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
}

impl SfuService for LiveKitSfu {
    fn issue_join_token(
        &self,
        call_id: &CallId,
        identity: &Identity,
        capabilities: MediaCapabilities,
    ) -> Result<JoinToken, SfuError> {
        mint_join_token(MintInputs {
            api_key: &self.config.api_key,
            api_secret: &self.config.api_secret,
            ws_url: &self.config.ws_url,
            call_id,
            identity,
            capabilities,
            ttl: self.config.token_ttl,
        })
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

    fn unregister_call_participant(&self, call_id: &CallId, identity: &Identity) -> CallState {
        let remaining = match self.calls.get_mut(call_id) {
            Some(mut entry) => {
                entry.remove(identity);
                entry.len()
            }
            None => 0,
        };

        if remaining == 0 {
            self.calls.remove(call_id);
            CallState::Ended
        } else {
            CallState::Active { remaining }
        }
    }

    fn ws_url(&self) -> &WebsocketUrl {
        &self.config.ws_url
    }

    fn turn_host(&self) -> &TurnHost {
        &self.config.turn_host
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
    fn register_is_idempotent() {
        let sfu = LiveKitSfu::new(fixture_config());
        let call = CallId::new("c1").unwrap();
        let identity = fixture_identity("alice");

        sfu.register_call_participant(&call, &identity);
        sfu.register_call_participant(&call, &identity);
        assert_eq!(sfu.participant_count(&call), 1);
    }
}
