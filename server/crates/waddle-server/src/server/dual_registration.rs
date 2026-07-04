//! ADR-0017 Phase 1 dual-registration helpers.
//!
//! The live connection lifecycle registers and unregisters resources in the
//! DashMap-backed [`waddle_xmpp::registry::ConnectionRegistry`]. During Phase 1
//! we mirror those same lifecycle transitions into the actor-backed
//! [`waddle_xmpp::registry::UserRegistryActor`] so the actor tree tracks live
//! sessions ahead of the read-cutover.
//!
//! Both mirrors are **best-effort**: the DashMap registry is authoritative for
//! delivery, and nothing reads the actor tree for routing yet, so a failed or
//! lost mirror can never drop a live stanza. A failure is logged, never
//! propagated — it must not fail a connection or a teardown path.

use jid::FullJid;
use kameo::actor::ActorRef;
use tokio::sync::mpsc;
use tracing::warn;
use waddle_xmpp::registry::{
    OutboundStanza, RegisterUserResource, UnregisterUserResource, UserRegistryActor,
};

/// Mirror a resource registration into the actor tree (best-effort).
pub(crate) async fn mirror_register(
    user_registry: &ActorRef<UserRegistryActor>,
    jid: FullJid,
    sender: mpsc::Sender<OutboundStanza>,
    carbons_enabled: bool,
) {
    if let Err(error) = user_registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            sender,
            carbons_enabled,
        })
        .await
    {
        warn!(
            jid = %jid,
            %error,
            "dual-registration: failed to mirror register into user_registry \
             actor; DashMap registration remains authoritative"
        );
    }
}

/// Mirror a resource unregistration into the actor tree (best-effort).
pub(crate) async fn mirror_unregister(user_registry: &ActorRef<UserRegistryActor>, jid: &FullJid) {
    if let Err(error) = user_registry
        .ask(UnregisterUserResource { jid: jid.clone() })
        .await
    {
        warn!(
            jid = %jid,
            %error,
            "dual-registration: failed to mirror unregister into user_registry \
             actor; the actor tree may retain a stale resource until the next \
             successful mirror"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::Spawn;
    use waddle_xmpp::registry::{GetUser, UserRegistryActor};

    fn full(s: &str) -> FullJid {
        s.parse().expect("full jid")
    }

    /// The register mirror makes the resource visible in the actor tree, and
    /// the unregister mirror prunes it — the round-trip the live lifecycle
    /// relies on so the mirror does not leak dead resources.
    #[tokio::test]
    async fn register_then_unregister_round_trips_through_actor_tree() {
        let registry = UserRegistryActor::spawn(UserRegistryActor::new());
        let jid = full("alice@example.com/phone");
        let (tx, _rx) = mpsc::channel(16);

        mirror_register(&registry, jid.clone(), tx, false).await;

        let user = registry
            .ask(GetUser {
                bare_jid: jid.to_bare(),
            })
            .await
            .expect("get user")
            .expect("user actor should exist after register mirror");
        let resources: Vec<FullJid> = user
            .ask(waddle_xmpp::registry::user_actor::GetResources)
            .await
            .expect("resources");
        assert_eq!(resources, vec![jid.clone()]);

        mirror_unregister(&registry, &jid).await;

        // The user actor is pruned once its last resource unregisters.
        let after = registry
            .ask(GetUser {
                bare_jid: jid.to_bare(),
            })
            .await
            .expect("get user");
        assert!(
            after.is_none(),
            "actor tree should prune the user after the unregister mirror"
        );
    }
}
