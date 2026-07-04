//! ADR-0017 Phase 1 dual-registration helpers.
//!
//! The live connection lifecycle registers and unregisters resources in the
//! DashMap-backed [`waddle_xmpp::registry::ConnectionRegistry`]. During Phase 1
//! we mirror those same lifecycle transitions into the actor-backed
//! [`waddle_xmpp::registry::UserRegistryActor`] so the actor tree tracks live
//! sessions ahead of the read-cutover.
//!
//! Both mirrors are **best-effort**: the DashMap registry is authoritative for
//! delivery. Bare-JID selection and fan-out delivery read the actor tree as a
//! fast path but fall back to the DashMap when the actor has no entry (see
//! `route_to_connection` / `deliver_peer_to_full`), so a failed or lost mirror
//! degrades to the DashMap rather than dropping a stanza. A failure is logged,
//! never propagated — it must not fail a connection or a teardown path, and it
//! must never stall one either: every mirror `ask` is bounded by
//! [`MIRROR_TIMEOUT`] so a slow/wedged `UserRegistryActor` cannot hang the
//! live registration or teardown path (Copilot review on PR #1177).

use std::time::Duration;

use jid::FullJid;
use kameo::actor::ActorRef;
use tracing::warn;
use waddle_xmpp::registry::{
    ConnectionEntry, RegisterUserResource, UnregisterUserResource, UserRegistryActor,
};

/// Upper bound on how long a best-effort mirror may wait to enqueue onto the
/// `UserRegistryActor` mailbox before the caller gives up and logs. Sized well
/// above normal in-process mailbox latency but small enough that a wedged
/// actor cannot meaningfully delay connection setup or teardown.
const MIRROR_TIMEOUT: Duration = Duration::from_secs(2);

/// Mirror a resource registration into the actor tree (best-effort, bounded).
///
/// `entry` MUST be the live DashMap [`ConnectionEntry`] (obtained via
/// `entry_if_owner`), so the actor shares its `Arc`-backed presence/carbons
/// atomics and later updates stay coherent without per-site mirroring.
///
/// Uses a bounded `tell` (enqueue-only), not `ask` (Copilot review on PR
/// #1177): the mirror is best-effort and its reply is unused, so the live bind
/// path must not block for the handler to run — only for a mailbox slot, and
/// only up to [`MIRROR_TIMEOUT`]. The `UserRegistryActor` mailbox is FIFO, so a
/// later `unregister` tell (or a same-session read) is still ordered after this
/// register.
pub(crate) async fn mirror_register(
    user_registry: &ActorRef<UserRegistryActor>,
    jid: FullJid,
    entry: ConnectionEntry,
) {
    if let Err(error) = user_registry
        .tell(RegisterUserResource {
            jid: jid.clone(),
            entry,
        })
        .mailbox_timeout(MIRROR_TIMEOUT)
        .await
    {
        warn!(
            jid = %jid,
            %error,
            "dual-registration: failed to enqueue register mirror into \
             user_registry actor; DashMap registration remains authoritative"
        );
    }
}

/// Mirror a resource unregistration into the actor tree (best-effort, bounded).
///
/// Bounded `tell`, not `ask` — see [`mirror_register`]. Teardown paths (SM
/// expiry, disconnect cleanup, drain) must not block for the handler.
pub(crate) async fn mirror_unregister(user_registry: &ActorRef<UserRegistryActor>, jid: &FullJid) {
    if let Err(error) = user_registry
        .tell(UnregisterUserResource { jid: jid.clone() })
        .mailbox_timeout(MIRROR_TIMEOUT)
        .await
    {
        warn!(
            jid = %jid,
            %error,
            "dual-registration: failed to enqueue unregister mirror into \
             user_registry actor; the actor tree may retain a stale resource \
             until the next successful mirror"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::Spawn;
    use tokio::sync::mpsc;
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

        mirror_register(&registry, jid.clone(), ConnectionEntry::new(tx)).await;

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
