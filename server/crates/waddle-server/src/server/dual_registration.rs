//! ADR-0017 Phase 1 dual-registration helpers.
//!
//! The live connection lifecycle registers and unregisters resources in the
//! DashMap-backed [`waddle_xmpp::registry::ConnectionRegistry`] and mirrors the
//! same lifecycle transitions into the actor-backed
//! [`waddle_xmpp::registry::UserRegistryActor`].
//!
//! # Registration is authoritative and fail-closed
//!
//! The register mirror is a bounded **`ask`** ([`mirror_register`]): the bind
//! path waits for the `UserActor` to actually record the resource and only
//! reports success when it did. This is the crux of ADR-0017 Phase 1
//! completion (see
//! `docs/adrs/0017-phase1-completion-authoritative-registration.md`): a lagging
//! register is a *silent false negative* — a bare-JID selection that misses a
//! live resource looks like a complete set, so it can never fall back. The only
//! sound fix is to make the actor authoritative at bind time. If the mirror
//! cannot be confirmed the caller rolls the DashMap registration back and fails
//! the session, so the two views can never disagree in the miss-a-resource
//! direction.
//!
//! # Unregistration stays best-effort
//!
//! The unregister mirror ([`mirror_unregister`]) is a bounded **`tell`**: a
//! lagging unregister is a *self-healing false positive* — the stale resource
//! is filtered out by presence and evicted on the next closed-channel send, so
//! it never causes a wrong delivery. Teardown paths (SM expiry, disconnect
//! cleanup, drain) must not block for the handler, so the mirror is
//! enqueue-only and capped by [`MIRROR_TIMEOUT`]. The `UserRegistryActor`
//! mailbox is FIFO, so an unregister `tell` is still ordered after the
//! register `ask` that preceded it.
//!
//! The unregister mirror carries an `owner` token
//! (`Option<Arc<AtomicBool>>`, the `carbons_enabled` handle used as the
//! DashMap ownership token): `Some(token)` removes only if the actor's current
//! entry is that same session (mirrors `unregister_if_owner`); `None` removes
//! unconditionally (mirrors a plain `unregister`).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use jid::FullJid;
use kameo::actor::ActorRef;
use kameo::error::SendError;
use tracing::warn;
use waddle_xmpp::ownership::CurrentNodeIdentityPermit;
use waddle_xmpp::registry::{
    ConnectionEntry, RegisterUserResource, RegisterUserResourceUnderAuthority,
    UnregisterUserResource, UserRegistryActor, UserRegistryError,
};

/// Upper bound on how long the fail-closed register `ask` may wait for the
/// `UserRegistryActor` (mailbox enqueue and handler reply). Sized well above
/// normal in-process actor latency but small enough that a wedged actor fails
/// the bind quickly rather than hanging it — the client simply reconnects.
const BIND_REGISTER_TIMEOUT: Duration = Duration::from_secs(2);

/// Upper bound on how long a best-effort unregister mirror may wait to enqueue
/// onto the `UserRegistryActor` mailbox before the caller gives up and logs.
/// Sized well above normal in-process mailbox latency but small enough that a
/// wedged actor cannot meaningfully delay teardown.
const MIRROR_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MirrorRegisterOutcome {
    Registered,
    ForeignOwner,
    Failed,
}

/// Mirror a resource registration into the actor tree (authoritative, bounded).
///
/// `entry` MUST be the live DashMap [`ConnectionEntry`] (obtained via
/// `entry_if_owner`), so the actor shares its `Arc`-backed presence/carbons
/// atomics and later updates stay coherent without per-site mirroring.
///
/// Returns `true` only when the `UserActor` has confirmed the resource is
/// recorded. On any error — mailbox full, timeout, or a dead/poisoned user
/// actor — returns `false`; the caller MUST roll back the DashMap registration
/// and fail the session so the actor tree never silently misses a live
/// resource (ADR-0017 Phase 1 completion).
#[must_use = "a false return means the actor tree did not record the resource; \
              the caller must roll back the DashMap registration and fail the bind"]
#[cfg(test)]
pub(crate) async fn mirror_register(
    user_registry: &ActorRef<UserRegistryActor>,
    jid: FullJid,
    entry: ConnectionEntry,
) -> bool {
    matches!(
        mirror_register_outcome(user_registry, jid, entry).await,
        MirrorRegisterOutcome::Registered
    )
}

pub(crate) async fn mirror_register_outcome(
    user_registry: &ActorRef<UserRegistryActor>,
    jid: FullJid,
    entry: ConnectionEntry,
) -> MirrorRegisterOutcome {
    // `RegisterUserResource` replies `Result<(), UserRegistryError>`, and kameo
    // *flattens* a `Result` reply: `ask().await` yields
    // `Result<(), SendError<RegisterUserResource, UserRegistryError>>`, NOT a
    // nested `Result<Result<..>, SendError>`. So `Ok(())` is the sole success
    // and the single `Err(_)` arm already covers BOTH transport failures
    // (mailbox-full / timeout / actor-not-running) AND a handler-returned
    // `UserRegistryError` (busy / state-lost) as `SendError::HandlerError` —
    // every one is a failed authoritative register that must fail the bind.
    match user_registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry,
        })
        .mailbox_timeout(BIND_REGISTER_TIMEOUT)
        .reply_timeout(BIND_REGISTER_TIMEOUT)
        .await
    {
        Ok(()) => MirrorRegisterOutcome::Registered,
        Err(SendError::HandlerError(UserRegistryError::ClaimHeldByAnotherNode(_))) => {
            MirrorRegisterOutcome::ForeignOwner
        }
        Err(error) => {
            warn!(
                jid = %jid,
                %error,
                "dual-registration: authoritative register into user_registry \
                 actor failed; rolling back DashMap registration and failing \
                 the bind"
            );
            MirrorRegisterOutcome::Failed
        }
    }
}

/// Authoritative local-bind mirror that reuses a weak permit minted from the
/// publication guard acquired before the ConnectionRegistry insertion.
///
/// The caller retains the real guard through rollback enqueue. The actor
/// message cannot keep terminal disable blocked if this bounded ask times out.
pub(crate) async fn mirror_register_outcome_under_authority(
    user_registry: &ActorRef<UserRegistryActor>,
    jid: FullJid,
    entry: ConnectionEntry,
    publication_permit: CurrentNodeIdentityPermit,
) -> MirrorRegisterOutcome {
    match user_registry
        .ask(RegisterUserResourceUnderAuthority {
            jid: jid.clone(),
            entry,
            publication_permit,
        })
        .mailbox_timeout(BIND_REGISTER_TIMEOUT)
        .reply_timeout(BIND_REGISTER_TIMEOUT)
        .await
    {
        Ok(()) => MirrorRegisterOutcome::Registered,
        Err(SendError::HandlerError(UserRegistryError::ClaimHeldByAnotherNode(_))) => {
            MirrorRegisterOutcome::ForeignOwner
        }
        Err(error) => {
            warn!(
                jid = %jid,
                %error,
                "dual-registration: guarded authoritative register into user_registry \
                 actor failed; rolling back DashMap registration and failing the bind"
            );
            MirrorRegisterOutcome::Failed
        }
    }
}

/// Mirror a resource unregistration into the actor tree (best-effort, bounded).
///
/// Bounded `tell`, not `ask` — a lagging unregister is self-healing (see the
/// module docs). Teardown paths (SM expiry, disconnect cleanup, drain) must
/// not block for the handler.
///
/// `owner` is the ownership token (the `carbons_enabled` `Arc<AtomicBool>`):
/// `Some(token)` prunes only if the actor still holds that session (mirrors
/// `unregister_if_owner`), `None` prunes unconditionally (mirrors a plain
/// `unregister`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MirrorUnregisterOutcome {
    Completed,
    Failed,
}

pub(crate) async fn mirror_unregister(
    user_registry: &ActorRef<UserRegistryActor>,
    jid: &FullJid,
    owner: Option<Arc<AtomicBool>>,
) -> MirrorUnregisterOutcome {
    match user_registry
        .tell(UnregisterUserResource {
            jid: jid.clone(),
            owner,
        })
        .mailbox_timeout(MIRROR_TIMEOUT)
        .await
    {
        Ok(()) => MirrorUnregisterOutcome::Completed,
        Err(error) => {
            warn!(
                jid = %jid,
                %error,
                "dual-registration: failed to enqueue unregister mirror into \
                 user_registry actor; the actor tree may retain a stale resource \
                 until the next successful mirror"
            );
            MirrorUnregisterOutcome::Failed
        }
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
        let entry = ConnectionEntry::new(tx);
        let owner = Arc::clone(&entry.carbons_enabled);

        assert!(
            mirror_register(&registry, jid.clone(), entry).await,
            "authoritative register mirror should confirm the resource"
        );

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

        mirror_unregister(&registry, &jid, Some(owner)).await;

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

    /// (a) An owner-gated unregister whose token does not match the actor's
    /// current entry must NOT prune the resource — this is the mirror of
    /// `unregister_if_owner` and prevents a stale teardown from evicting a
    /// resource that a newer session has since re-registered on the same JID.
    #[tokio::test]
    async fn unregister_with_stale_owner_token_is_ignored() {
        let registry = UserRegistryActor::spawn(UserRegistryActor::new());
        let jid = full("alice@example.com/phone");
        let (tx, _rx) = mpsc::channel(16);
        let entry = ConnectionEntry::new(tx);
        // A different Arc than the live entry's ownership token.
        let stale_owner = Arc::new(AtomicBool::new(false));

        assert!(mirror_register(&registry, jid.clone(), entry).await);

        mirror_unregister(&registry, &jid, Some(stale_owner)).await;

        let user = registry
            .ask(GetUser {
                bare_jid: jid.to_bare(),
            })
            .await
            .expect("get user")
            .expect("user actor should survive a stale-owner unregister");
        let resources: Vec<FullJid> = user
            .ask(waddle_xmpp::registry::user_actor::GetResources)
            .await
            .expect("resources");
        assert_eq!(
            resources,
            vec![jid.clone()],
            "stale-owner unregister must not prune the live resource"
        );
    }

    /// (b) A re-register on the same JID replaces the entry; a subsequent
    /// unregister carrying the OLD session's owner token is a no-op, while the
    /// new session (matching token) prunes as expected.
    #[tokio::test]
    async fn reregister_supersedes_old_owner_token() {
        let registry = UserRegistryActor::spawn(UserRegistryActor::new());
        let jid = full("alice@example.com/phone");

        let (tx_old, _rx_old) = mpsc::channel(16);
        let entry_old = ConnectionEntry::new(tx_old);
        let owner_old = Arc::clone(&entry_old.carbons_enabled);
        assert!(mirror_register(&registry, jid.clone(), entry_old).await);

        let (tx_new, _rx_new) = mpsc::channel(16);
        let entry_new = ConnectionEntry::new(tx_new);
        let owner_new = Arc::clone(&entry_new.carbons_enabled);
        assert!(mirror_register(&registry, jid.clone(), entry_new).await);

        // Old session tears down: its token no longer owns the entry.
        mirror_unregister(&registry, &jid, Some(owner_old)).await;
        let user = registry
            .ask(GetUser {
                bare_jid: jid.to_bare(),
            })
            .await
            .expect("get user")
            .expect("user actor should survive stale old-owner unregister");
        let resources: Vec<FullJid> = user
            .ask(waddle_xmpp::registry::user_actor::GetResources)
            .await
            .expect("resources");
        assert_eq!(resources, vec![jid.clone()]);

        // New session tears down: its token owns the entry, so it prunes.
        mirror_unregister(&registry, &jid, Some(owner_new)).await;
        let after = registry
            .ask(GetUser {
                bare_jid: jid.to_bare(),
            })
            .await
            .expect("get user");
        assert!(after.is_none(), "matching-owner unregister must prune");
    }

    /// (c) `None` owner prunes unconditionally, mirroring a plain
    /// `unregister` at teardown sites that do not hold an ownership token.
    #[tokio::test]
    async fn unregister_without_owner_prunes_unconditionally() {
        let registry = UserRegistryActor::spawn(UserRegistryActor::new());
        let jid = full("alice@example.com/phone");
        let (tx, _rx) = mpsc::channel(16);
        let entry = ConnectionEntry::new(tx);

        assert!(mirror_register(&registry, jid.clone(), entry).await);

        mirror_unregister(&registry, &jid, None).await;

        let after = registry
            .ask(GetUser {
                bare_jid: jid.to_bare(),
            })
            .await
            .expect("get user");
        assert!(
            after.is_none(),
            "None-owner unregister must prune unconditionally"
        );
    }

    /// The crux of the fail-closed contract: when the actor tree cannot
    /// confirm the resource, `mirror_register` MUST return `false` so the bind
    /// path rolls the DashMap registration back and fails the session. A dead
    /// `UserRegistryActor` is the simplest way to force the error arm without
    /// needing to wedge a mailbox.
    #[tokio::test]
    async fn register_into_dead_actor_returns_false() {
        let registry = UserRegistryActor::spawn(UserRegistryActor::new());
        registry.kill();
        // Let the actor observe the kill before we send onto its mailbox.
        tokio::task::yield_now().await;

        let jid = full("alice@example.com/phone");
        let (tx, _rx) = mpsc::channel(16);
        assert!(
            !mirror_register(&registry, jid, ConnectionEntry::new(tx)).await,
            "register against a dead actor must report failure so the caller \
             fails the bind rather than silently missing the resource"
        );
    }
}
