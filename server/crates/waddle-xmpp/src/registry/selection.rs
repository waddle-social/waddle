//! Actor-backed bare-JID resource selection (ADR-0017 Phase 3 Slice 9).
//!
//! Free-function replacements for the retired DashMap `ConnectionRegistry`
//! selection surface (`select_routable_resources_for_user` /
//! `get_resources_for_user`, removed from `connection_registry/resources.rs`
//! in this slice). Every caller that used to scan the DashMap now asks the
//! actor tree instead — `UserRegistryActor` to resolve the bare JID's
//! `UserActor`, then the `UserActor` itself — matching ADR-0017 Phase 1's
//! "the actor is the sole authoritative source" completion and closing out
//! the transitional survivors the Phase 1 completion note named.
//!
//! Both helpers degrade to `Vec::new()` on any actor error (no actor for the
//! bare JID, a busy/wedged registry or user actor, or an ask timeout) rather
//! than propagating a typed error — callers already treat an empty selection
//! as "no local resource" (the correct fail-closed behavior for a routing
//! decision), exactly as the retired DashMap methods' callers did for an
//! absent bare JID.

use std::time::Duration;

use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use tracing::warn;

use super::user_actor::delivery::SelectRoutableResources;
use super::user_actor::{GetAvailableResources, GetResources, UserActor};
use super::user_registry::{GetUser, UserRegistryActor};

/// Upper bound on each actor ask issued by the selection helpers below —
/// mirrors the bound the interpreter's own routing hot path already applies
/// to the identical `GetUser` + per-actor ask pattern.
const SELECTION_ASK_TIMEOUT: Duration = Duration::from_secs(2);

async fn resolve_user_actor(
    user_registry: &ActorRef<UserRegistryActor>,
    bare_jid: &BareJid,
) -> Option<ActorRef<UserActor>> {
    match user_registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .mailbox_timeout(SELECTION_ASK_TIMEOUT)
        .reply_timeout(SELECTION_ASK_TIMEOUT)
        .await
    {
        Ok(Some(actor)) => Some(actor),
        Ok(None) => None,
        Err(error) => {
            warn!(
                bare_jid = %bare_jid,
                %error,
                "actor selection: GetUser failed; degrading to no local resources"
            );
            None
        }
    }
}

/// Every currently-connected resource of `bare_jid`, sourced from the
/// authoritative actor tree. Mirrors the retired DashMap
/// `get_resources_for_user` exactly: no presence filter, every registered
/// resource.
pub async fn get_resources_for_user(
    user_registry: &ActorRef<UserRegistryActor>,
    bare_jid: &BareJid,
) -> Vec<FullJid> {
    let Some(user_actor) = resolve_user_actor(user_registry, bare_jid).await else {
        return Vec::new();
    };
    match user_actor
        .ask(GetResources)
        .mailbox_timeout(SELECTION_ASK_TIMEOUT)
        .reply_timeout(SELECTION_ASK_TIMEOUT)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(
                bare_jid = %bare_jid,
                %error,
                "actor selection: GetResources failed; degrading to no local resources"
            );
            Vec::new()
        }
    }
}

/// Every currently presence-available resource of `bare_jid`, sourced from
/// the authoritative actor tree. Presence side-effect fanout uses this shape:
/// unlike message routing, RFC 6121 presence broadcasts go to all available
/// resources, not just the highest-priority bare-JID message targets.
pub async fn available_resources_for_user(
    user_registry: &ActorRef<UserRegistryActor>,
    bare_jid: &BareJid,
) -> Vec<(FullJid, i8)> {
    let Some(user_actor) = resolve_user_actor(user_registry, bare_jid).await else {
        return Vec::new();
    };
    match user_actor
        .ask(GetAvailableResources)
        .mailbox_timeout(SELECTION_ASK_TIMEOUT)
        .reply_timeout(SELECTION_ASK_TIMEOUT)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(
                bare_jid = %bare_jid,
                %error,
                "actor selection: GetAvailableResources failed; degrading to no available resources"
            );
            Vec::new()
        }
    }
}

/// RFC 6121 §8.5.2.1 destination-resource selection for bare-JID 1:1 message
/// routing, sourced from the authoritative actor tree. Mirrors the retired
/// DashMap `select_routable_resources_for_user` exactly: every currently
/// -connected resource whose advertised priority equals the maximum among
/// the user's available, non-negative-priority resources.
pub async fn select_routable_resources_for_user(
    user_registry: &ActorRef<UserRegistryActor>,
    bare_jid: &BareJid,
) -> Vec<FullJid> {
    let Some(user_actor) = resolve_user_actor(user_registry, bare_jid).await else {
        return Vec::new();
    };
    match user_actor
        .ask(SelectRoutableResources)
        .mailbox_timeout(SELECTION_ASK_TIMEOUT)
        .reply_timeout(SELECTION_ASK_TIMEOUT)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(
                bare_jid = %bare_jid,
                %error,
                "actor selection: SelectRoutableResources failed; degrading to no local resources"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests;
