//! What a replica does to the rooms it hosts when a managed room's grants
//! change: the sweep of the host-grant revocation fence.
//!
//! The canonical grants decide every new use of a managed room, and the
//! revocation fence (`permissions::host_grant::fence`) keeps a replica from
//! acting on a decision read before a revoke. What neither removes is what a
//! room actor already holds: an occupant that was seated under a grant that
//! has since been revoked, the affiliation the actor mirrors for it, the role
//! derived from that affiliation, and its media session. This module puts
//! those back in line with the grants.
//!
//! The fence asks for a sweep of a scope whenever this replica installs a
//! new generation of it, and of every scope when the replica registers. For
//! each room of the scope that this process holds an actor for, the sweep
//! reads the room's canonical grants and:
//!
//! - removes every occupant the grants do not admit to read the room, with
//!   the presence XEP-0045 prescribes: status 301 for a ban (§9.1) and
//!   status 321 for the loss of membership in a members-only room (§9.4),
//!   to the occupant and to everyone who stays, and ends its media session;
//! - lowers an affiliation the actor mirrors above the recorded one, which
//!   re-derives the occupant's role, with the affiliation presence of the
//!   change;
//! - keeps the recorded owners in the actor's list, so that the actor's own
//!   last-owner rule does not refuse a removal the store already decided.
//!
//! The sweep never seats anyone and never raises an entry above the recorded
//! affiliation. It acts on a read that shows what the grants hold; when the
//! grants cannot be read it changes nothing and tries again,
//! so a database that does not answer never empties a room. A closed scope
//! (retired, restored, fenced) admits nobody, which a read shows: its rooms
//! are emptied with status 321.
//!
//! The acknowledgement of a revoke does not wait for the sweep (see `fence`).
//! Until the sweep has removed it, a revoked occupant that is still seated
//! receives the presence the room actor broadcasts to its occupants and
//! nothing else: every other use is decided from the grants.
//!
//! Only the process that holds a room's actor sweeps it. Log lines name no
//! JID and no room.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use tokio::sync::mpsc;
use waddle_xmpp::muc::room_actor::{
    AdminItemsApplied, ApplyAffiliationChange, ChangeAffiliation, GetSnapshot,
    RemoveUnadmittedOccupant,
};
use waddle_xmpp::muc::room_registry_actor::{GetRoom, RoomRegistryActor};
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp::{Affiliation, Stanza};

use crate::permissions::host_grant::{
    HostGrantAction, HostGrantGate, ManagedRoomGrants, RoomAffiliation, RoomBinding, RoomGate,
    ScopeLifecycleId,
};
use crate::room_effect_outbox::RoomEffectArmSupervisor;
use crate::server::routes::websocket::WebSocketState;

/// How long the sweep waits before it tries a scope again whose rooms it
/// could not all bring in line.
const SWEEP_RETRY: Duration = Duration::from_secs(1);
/// Longest one ask of a room actor or the room registry waits.
const SWEEP_ASK_TIMEOUT: Duration = Duration::from_secs(5);

/// Start this replica's sweep and have the revocation fence drive it. A
/// state whose gate has no fence (SQLite, a schema without the host-grant
/// relations' store) has nothing to sweep. Only the first sweep installed on
/// a gate runs.
pub(crate) fn install(state: &WebSocketState) {
    let gate = &state.deps.app_state.host_grants;
    let Some(fence) = gate.fence() else {
        return;
    };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let (requests, pending) = mpsc::unbounded_channel();
    if !fence.install_sweep(Arc::new(move |lifecycle| {
        // The receiver is gone only when the replica is stopping.
        let _ = requests.send(lifecycle);
    })) {
        return;
    }
    let sweep = Sweep {
        // Without the fence: the task must not keep the fence alive, and it
        // admits nothing.
        grants: gate.detached(),
        rooms: state.deps.protocol.room_registry.clone(),
        connections: Arc::clone(&state.deps.protocol.connection_registry),
        sfu: state.deps.protocol.sfu.clone(),
        effects: state.deps.protocol.room_effect_arm_supervisor.clone(),
    };
    runtime.spawn(run(sweep, pending));
}

/// Sweep one room now, as the fence's sweep would. `true` when the room is
/// in line with its grants or this process holds no actor for it.
#[cfg(feature = "test-support")]
pub(crate) async fn sweep_room_for_test(state: &WebSocketState, room: &BareJid) -> bool {
    Sweep {
        grants: state.deps.app_state.host_grants.detached(),
        rooms: state.deps.protocol.room_registry.clone(),
        connections: Arc::clone(&state.deps.protocol.connection_registry),
        sfu: state.deps.protocol.sfu.clone(),
        effects: state.deps.protocol.room_effect_arm_supervisor.clone(),
    }
    .room(room)
    .await
}

struct Sweep {
    grants: HostGrantGate,
    rooms: ActorRef<RoomRegistryActor>,
    connections: Arc<ConnectionRegistry>,
    sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
    effects: RoomEffectArmSupervisor,
}

/// Sweep each requested scope until every room of it is in line. The task
/// ends when the fence that requests the sweeps is dropped.
async fn run(sweep: Sweep, mut requests: mpsc::UnboundedReceiver<ScopeLifecycleId>) {
    let mut pending: BTreeSet<ScopeLifecycleId> = BTreeSet::new();
    loop {
        if pending.is_empty() {
            match requests.recv().await {
                Some(lifecycle) => {
                    pending.insert(lifecycle);
                }
                None => return,
            }
        } else {
            tokio::select! {
                request = requests.recv() => match request {
                    Some(lifecycle) => {
                        pending.insert(lifecycle);
                    }
                    None => return,
                },
                () = tokio::time::sleep(SWEEP_RETRY) => {}
            }
        }
        while let Ok(lifecycle) = requests.try_recv() {
            pending.insert(lifecycle);
        }
        let mut unfinished = BTreeSet::new();
        for lifecycle in std::mem::take(&mut pending) {
            if !sweep.scope(lifecycle).await {
                unfinished.insert(lifecycle);
            }
        }
        if !unfinished.is_empty() {
            tracing::debug!(
                scopes = unfinished.len(),
                "A host-grant revocation sweep did not finish and is tried again"
            );
        }
        pending = unfinished;
    }
}

impl Sweep {
    /// Sweep every room of a scope. `true` when all of them are in line.
    async fn scope(&self, lifecycle: ScopeLifecycleId) -> bool {
        let Some(enrolled) = self.grants.enrolled().settled_snapshot().await else {
            return false;
        };
        let mut finished = true;
        for room in enrolled.scope_rooms(lifecycle) {
            let Ok(room) = room.parse::<BareJid>() else {
                continue;
            };
            finished &= self.room(&room).await;
        }
        finished
    }

    /// Sweep one room. `true` when it is in line with its grants, or this
    /// process holds no actor for it.
    async fn room(&self, room: &BareJid) -> bool {
        let actor = match tokio::time::timeout(
            SWEEP_ASK_TIMEOUT,
            self.rooms.ask(GetRoom {
                room_jid: room.clone(),
            }),
        )
        .await
        {
            Ok(Ok(Some(actor))) => actor,
            // No actor in this process: nothing is seated here.
            Ok(Ok(None)) => return true,
            Ok(Err(_)) | Err(_) => return false,
        };
        // Only a read that shows the grants is acted on.
        let grants = match self.grants.room_as_stored(RoomBinding::Room(room)).await {
            RoomGate::Managed(grants) => grants,
            RoomGate::Unmanaged => return true,
            RoomGate::Unavailable => return false,
        };
        let Ok(Ok(snapshot)) =
            tokio::time::timeout(SWEEP_ASK_TIMEOUT, actor.ask(GetSnapshot)).await
        else {
            return false;
        };
        let held = snapshot.room;
        let mut finished = true;

        // The actor's own last-owner rule counts the owners its list holds.
        for owner in grants.grants.recorded(RoomAffiliation::Owner) {
            if held.affiliation(&owner) != Affiliation::Owner {
                finished &= ask(actor.ask(ChangeAffiliation {
                    jid: owner,
                    affiliation: Affiliation::Owner,
                }))
                .await
                .is_some();
            }
        }

        let seated: BTreeSet<BareJid> = held
            .occupants
            .values()
            .map(|occupant| occupant.real_jid.to_bare())
            .collect();
        let listed = held.all_affiliations().into_iter().map(|entry| entry.jid);
        let accounts: BTreeSet<BareJid> = seated.iter().cloned().chain(listed).collect();
        for account in accounts {
            let mirrored = held.affiliation(&account);
            let Some(change) = correction(&grants, &account, mirrored, seated.contains(&account))
            else {
                continue;
            };
            let applied = match change {
                Correction::Remove(affiliation) => {
                    ask(actor.ask(RemoveUnadmittedOccupant {
                        jid: account,
                        affiliation,
                    }))
                    .await
                }
                Correction::Lower(affiliation) => {
                    ask(actor.ask(ApplyAffiliationChange {
                        actor: None,
                        jid: account,
                        affiliation,
                    }))
                    .await
                }
            };
            match applied {
                Some(applied) => self.deliver(room, applied).await,
                None => finished = false,
            }
        }
        finished
    }

    /// Send what a correction produced: the presence of each removal and
    /// affiliation change, and the end of each removed session's media.
    async fn deliver(&self, room: &BareJid, applied: AdminItemsApplied) {
        let AdminItemsApplied {
            presence_updates,
            removed_by_moderation,
            voice_changes,
            outbox_reservation,
        } = applied;
        if !removed_by_moderation.is_empty() {
            waddle_xmpp::counter_add!(
                "waddle.host_grant.fence.occupants_removed",
                "{session}",
                "Occupant sessions the host-grant revocation sweep removed from managed rooms \
                 because the room's grants no longer admit their account",
                u64::try_from(removed_by_moderation.len()).unwrap_or(u64::MAX),
            );
        }
        match outbox_reservation {
            // The actor committed the effects with the change; the outbox
            // delivers them, on whichever replica holds each session.
            Some(reservation) => {
                end_media(self.sfu.as_ref(), room, &removed_by_moderation);
                self.effects.spawn_reservation_drain(reservation);
            }
            None => {
                crate::server::routes::websocket::muc_call_sfu::converge_moderation_deltas_via_sfu(
                    self.sfu.as_ref(),
                    room,
                    &removed_by_moderation,
                    &voice_changes,
                );
                for (recipient, presence) in presence_updates {
                    let _ = self
                        .connections
                        .send_to(&recipient, Stanza::Presence(presence))
                        .await;
                }
            }
        }
    }
}

/// What the sweep applies to the room actor for one account.
enum Correction {
    /// Remove every seat of the account, after setting the actor's entry
    /// for it to the affiliation when one is given (`Outcast` or `None`).
    Remove(Option<Affiliation>),
    /// Lower the mirrored affiliation to the recorded one.
    Lower(Affiliation),
}

/// The correction of one account, from the grants as stored, the affiliation
/// the room actor mirrors for it and whether it holds a seat. `None` when
/// the actor is in line with the grants for it.
///
/// The actor's list mirrors the recorded affiliation, and an occupant keeps
/// its seat only while the grants admit it to read the room. So:
///
/// - a seated account the grants do not admit is removed: as banned when its
///   standing is a ban, as no longer a member when no membership is
///   recorded for it, and otherwise without touching its entry (a scope
///   that admits nobody, a privileged grant the replica does not honour);
/// - an account that is not seated and whose recorded affiliation is below
///   membership has its entry set to it, which also refuses a join of that
///   account the actor has not yet applied;
/// - any other entry above the recorded affiliation is lowered to it.
fn correction(
    grants: &ManagedRoomGrants,
    account: &BareJid,
    mirrored: Affiliation,
    seated: bool,
) -> Option<Correction> {
    let recorded = muc_affiliation(grants.grants.recorded_affiliation(account));
    let banned = grants.standing_as_read(account) == RoomAffiliation::Outcast;
    let admitted = grants
        .grants
        .allows(account, HostGrantAction::Read, &grants.served);
    if seated && !admitted {
        return Some(Correction::Remove(if banned {
            Some(Affiliation::Outcast)
        } else if recorded < Affiliation::Member {
            Some(Affiliation::None)
        } else {
            // The account keeps its recorded standing and may not stay for
            // now; the actor's entry is not lowered below what is recorded.
            None
        }));
    }
    if recorded < Affiliation::Member {
        return (mirrored != recorded).then_some(Correction::Remove(Some(recorded)));
    }
    (mirrored > recorded).then_some(Correction::Lower(recorded))
}

fn muc_affiliation(affiliation: RoomAffiliation) -> Affiliation {
    match affiliation {
        RoomAffiliation::Owner => Affiliation::Owner,
        RoomAffiliation::Admin => Affiliation::Admin,
        RoomAffiliation::Member => Affiliation::Member,
        RoomAffiliation::None => Affiliation::None,
        RoomAffiliation::Outcast => Affiliation::Outcast,
    }
}

/// The reply of one room-actor ask, or `None` when the actor refused, failed
/// or did not answer in time: the room is then swept again.
async fn ask<T, E>(reply: impl std::future::IntoFuture<Output = Result<T, E>>) -> Option<T> {
    tokio::time::timeout(SWEEP_ASK_TIMEOUT, reply.into_future())
        .await
        .ok()
        .and_then(Result::ok)
}

fn end_media(sfu: Option<&Arc<dyn waddle_sfu::SfuService>>, room: &BareJid, removed: &[FullJid]) {
    let Some(sfu) = sfu else {
        return;
    };
    for session in removed {
        crate::server::routes::websocket::muc_call_sfu::unregister_participant_via_sfu_ungated(
            sfu, room, session,
        );
    }
}
