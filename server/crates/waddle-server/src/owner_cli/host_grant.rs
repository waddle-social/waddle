//! `host-grant`: an operator's act on the host-grant authority: after a
//! restore (`permissions::host_grant::restore`), and when an account's
//! grants are to be revoked through the revocation fence
//! (`permissions::host_grant::fence`), as after a reported reassignment of
//! its JID.
//!
//! Each act is recorded in PostgreSQL. A revoke then waits for the fence's
//! acknowledgement, which the command reports. When the environment names the
//! servers' SpiceDB, the act's witnesses are written at once (the scopes'
//! journal witnesses, the position witness); otherwise the next fence pass
//! of a server writes them. A witness that could not be written is reported
//! and does not fail the command: the act is committed either way.

use std::time::Duration;

use kameo::actor::{ActorRef, Spawn};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::SpiceDbConfig;
use crate::permissions::host_grant::{
    position_witness_resource, read_position_witness, sync_journal_witness, sync_journal_witnesses,
    sync_position_witness, FenceAcknowledgement, GrantOperationId, HostGrantError, HostGrantStore,
    OperatorRevoke, PositionStanding, PositionWriter, ReopenSelection, RoomAffiliation,
    ScopeLifecycleId, ServedAccountDomains, WitnessSync,
};
use crate::permissions::{EnsureAuthzCodecIncarnation, PermissionActor};

use super::args::{HostGrantActionArgs, HostGrantArgs};
use super::connection::open_owner;
use super::error::OwnerCliError;

pub(super) async fn run(args: HostGrantArgs) -> Result<(), OwnerCliError> {
    let application_name = format!("waddle-host-grant-{}", Uuid::new_v4().simple());
    let spicedb = SpiceDbConfig::from_env().map_err(OwnerCliError::Configuration)?;
    let owner = open_owner(&args.connection, &application_name).await?;
    let store = HostGrantStore::open(owner).map_err(OwnerCliError::HostGrant)?;
    let permissions = match &spicedb {
        Some(config) => Some(connect_spicedb(config).await),
        None => None,
    }
    .flatten();
    match args.action {
        HostGrantActionArgs::Reopen {
            scope,
            lost_operations_reapplied,
        } => {
            let selection = match scope {
                Some(lifecycle) => ReopenSelection::Scope {
                    lifecycle,
                    lost_operations_reapplied,
                },
                None => ReopenSelection::All,
            };
            let report = store
                .reopen_scopes(selection)
                .await
                .map_err(OwnerCliError::HostGrant)?;
            println!(
                "{}",
                json!({
                    "reopened": report.reopened,
                    "not_reconstructed": report.not_reconstructed,
                    "lost_operations": report.lost_operations,
                })
            );
            eprintln!(
                "reopened {} host-grant scope(s); {} not reconstructed and {} with lost \
                 operations left closed",
                report.reopened, report.not_reconstructed, report.lost_operations
            );
            if let Some(permissions) = &permissions {
                let failed = match scope {
                    Some(lifecycle) => sync_journal_witness(&store, permissions, lifecycle)
                        .await
                        .is_err(),
                    None => sync_journal_witnesses(&store, permissions).await.failed > 0,
                };
                report_witnesses(failed);
            }
        }
        HostGrantActionArgs::AcknowledgePosition { witnessed } => {
            let witnessed = match (witnessed, &permissions) {
                (Some(witnessed), Some(permissions)) => {
                    // The acknowledgement applies only to the witness it
                    // names; say so when SpiceDB holds another one.
                    if let Ok(Some((Some(held), _))) =
                        read_position_witness(&store, permissions).await
                    {
                        if held != witnessed {
                            eprintln!(
                                "warning: SpiceDB holds the position witness {held}, not the one \
                                 given; the acknowledgement applies only while the witness it \
                                 names is current"
                            );
                        }
                    }
                    witnessed
                }
                (Some(witnessed), None) => witnessed,
                (None, Some(permissions)) => match read_position_witness(&store, permissions)
                    .await
                    .map_err(OwnerCliError::HostGrant)?
                {
                    Some((Some(witness), _)) => witness,
                    _ => {
                        return Err(OwnerCliError::Configuration(
                            "SpiceDB holds no position witness of this authority; there is \
                             nothing to acknowledge"
                                .to_owned(),
                        ))
                    }
                },
                (None, None) => {
                    let resource = store
                        .authority_id()
                        .await
                        .map_err(OwnerCliError::HostGrant)?
                        .map(|authority_id| position_witness_resource(&authority_id))
                        .unwrap_or_default();
                    return Err(OwnerCliError::Configuration(format!(
                        "--witnessed is required without WADDLE_SPICEDB_ENDPOINT: read the \
                         subject of waddle_grant_projection:{resource} from SpiceDB"
                    )));
                }
            };
            let acknowledgement = store
                .acknowledge_position(witnessed)
                .await
                .map_err(OwnerCliError::HostGrant)?;
            println!(
                "{}",
                json!({
                    "supersedes": acknowledgement.supersedes.stamp(),
                    "system_identifier": acknowledgement.system_identifier.to_string(),
                    "timeline": acknowledgement.timeline,
                })
            );
            eprintln!(
                "acknowledged the position witness for the PostgreSQL system and timeline the \
                 server writes on"
            );
            if let Some(permissions) = &permissions {
                let applied =
                    sync_position_witness(&store, permissions, PositionWriter::Commit, None).await;
                match applied {
                    // Applied at once: the acknowledgement is spent, and is
                    // removed so that nothing can apply it again.
                    Ok((WitnessSync::Written, Some(PositionStanding::Acknowledged))) => {
                        store
                            .consume_position_acknowledgement(&acknowledgement)
                            .await
                            .map_err(OwnerCliError::HostGrant)?;
                        eprintln!("applied: the position witness names the current position");
                    }
                    Ok((WitnessSync::Written, Some(_))) => {
                        eprintln!("the position witness names the current position")
                    }
                    Ok((sync, standing)) => {
                        report_position(sync, standing);
                        if standing.is_some() {
                            eprintln!(
                                "the acknowledgement is not applied yet; a fence pass applies it \
                                 once it matches the witness and the timeline"
                            );
                        }
                    }
                    Err(_) => report_witnesses(true),
                }
            }
        }
        HostGrantActionArgs::Revoke {
            scope,
            room,
            subject,
            ban,
            operation_id,
            account_domain,
            fence_wait,
        } => {
            let command = OperatorRevoke {
                operation: GrantOperationId::from_uuid(operation_id),
                lifecycle: scope,
                room,
                subject,
                ban,
            };
            let receipt = store
                .operator_revoke(&command, &ServedAccountDomains::new([account_domain]))
                .await
                .map_err(OwnerCliError::HostGrant)?;
            let record = receipt.record();
            eprintln!("recorded the revoke; it is effective from its commit");
            if let Some(permissions) = &permissions {
                let journal = sync_journal_witness(&store, permissions, scope).await;
                let position =
                    sync_position_witness(&store, permissions, PositionWriter::Commit, None).await;
                report_witnesses(journal.is_err() || position.is_err());
                if let Ok((sync, standing)) = position {
                    report_position(sync, standing);
                }
            }
            let fence = store
                .await_revocation_fence(scope, record.generation, Duration::from_secs(fence_wait))
                .await
                .map_err(OwnerCliError::HostGrant)?;
            println!(
                "{}",
                json!({
                    "operation_id": record.operation.as_uuid().to_string(),
                    "generation": record.generation.value(),
                    "fence": fence_json(fence),
                })
            );
            report_fence(fence)?;
        }
        HostGrantActionArgs::Grants { subject } => {
            let grants = store
                .subject_grants(&subject)
                .await
                .map_err(OwnerCliError::HostGrant)?;
            for (lifecycle, grant) in &grants {
                println!(
                    "{}",
                    json!({
                        "scope": lifecycle.to_storage(),
                        "room": grant.room.to_string(),
                        "affiliation": grant.affiliation.as_storage(),
                        "effective_affiliation": grant.effective_affiliation.as_storage(),
                        "generation": grant.changed_generation.value(),
                    })
                );
            }
            eprintln!("{} grant row(s)", grants.len());
        }
        HostGrantActionArgs::RevokeAccount {
            subject,
            ban,
            operation_id,
            account_domain,
            fence_wait,
        } => {
            let target = if ban {
                RoomAffiliation::Outcast
            } else {
                RoomAffiliation::None
            };
            let served = ServedAccountDomains::new([account_domain]);
            let grants = store
                .subject_grants(&subject)
                .await
                .map_err(OwnerCliError::HostGrant)?;
            let wait = Duration::from_secs(fence_wait);
            let mut tally = RevokeTally::default();
            let mut witnessed = Vec::new();
            for (lifecycle, grant) in grants {
                // A revoke leaves a ban in place, and nothing is recorded
                // where the account already holds what is asked for. Such a
                // room is still reported with the fence's answer at its
                // scope's current generation, which covers the revoke or ban
                // an earlier run (or anyone) recorded there: a repeated act
                // confirms every room, not only those it changed.
                let unchanged = grant.affiliation == target
                    || (!ban && grant.affiliation == RoomAffiliation::Outcast);
                let outcome = if unchanged {
                    match store.scope(lifecycle).await {
                        Ok(Some(scope)) => {
                            let fence = store
                                .await_revocation_fence(lifecycle, scope.generation, wait)
                                .await;
                            tally.fence_line(
                                json!({ "recorded": false, "unchanged": true }),
                                scope.generation,
                                fence,
                            )
                        }
                        Ok(None) => tally.failure(HostGrantError::UnknownScope),
                        Err(error) => tally.failure(error),
                    }
                } else {
                    let command = OperatorRevoke {
                        operation: room_operation(operation_id, lifecycle, &grant.room),
                        lifecycle,
                        room: grant.room.clone(),
                        subject: subject.clone(),
                        ban,
                    };
                    match store.operator_revoke(&command, &served).await {
                        Ok(receipt) => {
                            let generation = receipt.record().generation;
                            witnessed.push(lifecycle);
                            let fence = store
                                .await_revocation_fence(lifecycle, generation, wait)
                                .await;
                            tally.fence_line(
                                json!({
                                    "recorded": true,
                                    "operation_id": command.operation.as_uuid().to_string(),
                                }),
                                generation,
                                fence,
                            )
                        }
                        Err(error) => tally.failure(error),
                    }
                };
                let mut line = json!({
                    "scope": lifecycle.to_storage(),
                    "room": grant.room.to_string(),
                    "previous_affiliation": grant.affiliation.as_storage(),
                });
                if let (Some(line), Some(outcome)) = (line.as_object_mut(), outcome.as_object()) {
                    line.extend(outcome.clone());
                }
                println!("{line}");
            }
            if let Some(permissions) = &permissions {
                let mut failed = false;
                witnessed.dedup();
                for lifecycle in witnessed {
                    failed |= sync_journal_witness(&store, permissions, lifecycle)
                        .await
                        .is_err();
                }
                failed |= sync_position_witness(&store, permissions, PositionWriter::Commit, None)
                    .await
                    .is_err();
                report_witnesses(failed);
            }
            tally.finish()?;
            eprintln!("every room is revoked and acknowledged by the revocation fence");
        }
        HostGrantActionArgs::Fence { scope } => {
            let state = store
                .scope(scope)
                .await
                .map_err(OwnerCliError::HostGrant)?
                .ok_or(OwnerCliError::HostGrant(HostGrantError::UnknownScope))?;
            let fence = store
                .revocation_fence(scope, state.generation)
                .await
                .map_err(OwnerCliError::HostGrant)?;
            println!(
                "{}",
                json!({
                    "generation": state.generation.value(),
                    "fence": fence_json(fence),
                })
            );
        }
    }
    Ok(())
}

/// The operation identity of one room's revoke within a `revoke-account`
/// act: derived from the act's own identity, the scope and the room, so that
/// repeating the act replays each room's revoke instead of recording it
/// again.
fn room_operation(act: Uuid, lifecycle: ScopeLifecycleId, room: &jid::BareJid) -> GrantOperationId {
    let mut hash = Sha256::new();
    hash.update(b"waddle:host-grant:revoke-account:v1\0");
    hash.update(act.as_bytes());
    hash.update(lifecycle.to_storage().as_bytes());
    hash.update(room.as_str().as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    GrantOperationId::from_uuid(Uuid::from_bytes(bytes))
}

/// What a `revoke-account` act found across its rooms. Every room is
/// reported on its own line; one room's failure does not stop the others.
#[derive(Default)]
struct RevokeTally {
    /// Rooms the store refused to revoke (a room's last owner, a closed
    /// scope's lasting refusal).
    refused: usize,
    /// Rooms whose revoke or acknowledgement could not be read: the act is
    /// run again.
    failed: usize,
    /// Rooms whose revoke is recorded and not yet acknowledged.
    unacknowledged: usize,
}

impl RevokeTally {
    /// The line of a room whose revoke is recorded (now or before), with the
    /// fence's answer at `generation`.
    fn fence_line(
        &mut self,
        mut line: serde_json::Value,
        generation: crate::permissions::host_grant::GrantGeneration,
        fence: Result<FenceAcknowledgement, HostGrantError>,
    ) -> serde_json::Value {
        let fence = match fence {
            Ok(fence) => {
                self.unacknowledged += usize::from(!fence.acknowledged());
                fence_json(fence)
            }
            Err(error) => {
                self.failed += 1;
                json!({ "acknowledged": false, "error": error.kind() })
            }
        };
        if let Some(fields) = line.as_object_mut() {
            fields.insert("generation".to_owned(), json!(generation.value()));
            fields.insert("fence".to_owned(), fence);
        }
        line
    }

    /// The line of a room that was not revoked. The scope's lasting
    /// refusals are what an operator resolves there (or, for a closed
    /// scope, nothing is admitted anyway); any other error is one the act
    /// is repeated for.
    fn failure(&mut self, error: HostGrantError) -> serde_json::Value {
        match error {
            HostGrantError::LastOwner => self.refused += 1,
            HostGrantError::ScopeRetired | HostGrantError::ScopeExhausted => {}
            ref error if super::error::host_grant_error_is_transient(error) => {
                self.failed += 1;
                return json!({ "recorded": false, "error": error.kind() });
            }
            _ => self.refused += 1,
        }
        json!({ "recorded": false, "refusal": error.kind() })
    }

    /// The command's outcome: a room that has to be run again first, then
    /// a refusal, then a revoke that is recorded and not yet acknowledged.
    fn finish(&self) -> Result<(), OwnerCliError> {
        if self.failed > 0 {
            return Err(OwnerCliError::RevokeAccountIncomplete {
                failed: self.failed,
            });
        }
        if self.refused > 0 {
            return Err(OwnerCliError::RevokesRefused {
                refused: self.refused,
            });
        }
        if self.unacknowledged > 0 {
            return Err(OwnerCliError::FenceUnacknowledged);
        }
        Ok(())
    }
}

fn fence_json(fence: FenceAcknowledgement) -> serde_json::Value {
    match fence {
        FenceAcknowledgement::Acknowledged { replicas } => {
            json!({ "acknowledged": true, "replicas": replicas, "lagging": 0 })
        }
        FenceAcknowledgement::Pending { replicas, lagging } => {
            json!({ "acknowledged": false, "replicas": replicas, "lagging": lagging })
        }
    }
}

/// Say what the fence answered, and fail the command while a server that
/// may still decide has not installed the revoke.
fn report_fence(fence: FenceAcknowledgement) -> Result<(), OwnerCliError> {
    match fence {
        FenceAcknowledgement::Acknowledged { replicas } => {
            eprintln!(
                "acknowledged by the revocation fence: {replicas} server(s) hold a lease and \
                 every one has installed the revoke"
            );
            Ok(())
        }
        FenceAcknowledgement::Pending { lagging, .. } => {
            eprintln!(
                "{lagging} server(s) hold a lease and have not installed the revoke yet; a \
                 server that stopped is waited for until its lease has run out"
            );
            Err(OwnerCliError::FenceUnacknowledged)
        }
    }
}

/// A permission actor on the servers' SpiceDB, or `None` (reported) when it
/// cannot be reached or its store is not one the servers' codec guard
/// accepts: the act is recorded either way, and the next fence pass of a
/// server writes its witnesses. The guard runs first, as at a server's
/// start, so no witness is written into a store the guard did not mark.
async fn connect_spicedb(config: &SpiceDbConfig) -> Option<ActorRef<PermissionActor>> {
    let actor = match PermissionActor::connect(config).await {
        Ok(actor) => PermissionActor::spawn(actor),
        Err(_) => {
            eprintln!(
                "warning: SpiceDB could not be reached; the next fence pass of a server writes \
                 this act's witnesses"
            );
            return None;
        }
    };
    if actor.ask(EnsureAuthzCodecIncarnation).await.is_err() {
        eprintln!(
            "warning: the SpiceDB store is not one the servers accept; the next fence pass of a \
             server writes this act's witnesses"
        );
        return None;
    }
    Some(actor)
}

/// Say why the position witness was not moved, when it was not: the
/// writer reads the latch and never moves the witness past a latched or
/// regressed position.
fn report_position(sync: WitnessSync, standing: Option<PositionStanding>) {
    match (sync, standing) {
        (WitnessSync::Latched, _) => eprintln!(
            "warning: the PostgreSQL position is behind its witness; the regression is now \
             latched and the authority stays refused until a restore is declared"
        ),
        (_, Some(PositionStanding::Regressed)) => eprintln!(
            "warning: the position witness was not moved: a regression is latched or the \
             position is behind it"
        ),
        (_, Some(PositionStanding::Unverified)) => eprintln!(
            "warning: the position witness was not moved: the position is unverified \
             (another system or timeline, or a latch without its witness)"
        ),
        (WitnessSync::Malformed, _) => eprintln!(
            "warning: the position witness, its latch or its epoch cannot be read; the first \
             fence pass after a declared restore removes them"
        ),
        _ => {}
    }
}

fn report_witnesses(failed: bool) {
    if failed {
        eprintln!("warning: a witness was not written; the next fence pass of a server writes it");
    } else {
        eprintln!("witnessed in SpiceDB");
    }
}
