//! `host-grant`: an operator's act on the host-grant authority after a
//! restore (`permissions::host_grant::restore`).
//!
//! Each act is recorded in PostgreSQL. When the environment names the
//! servers' SpiceDB, the act's witnesses are written at once (the scopes'
//! journal witnesses, the position witness); otherwise the next fence pass
//! of a server writes them. A witness that could not be written is reported
//! and does not fail the command: the act is committed either way.

use kameo::actor::{ActorRef, Spawn};
use serde_json::json;
use uuid::Uuid;

use crate::config::SpiceDbConfig;
use crate::permissions::host_grant::{
    position_witness_resource, read_position_witness, sync_journal_witness, sync_journal_witnesses,
    sync_position_witness, GrantOperationId, HostGrantStore, OperatorRevoke, PositionStanding,
    PositionWriter, ReopenSelection, ServedAccountDomains, WitnessSync,
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
            println!(
                "{}",
                json!({
                    "operation_id": record.operation.as_uuid().to_string(),
                    "generation": record.generation.value(),
                })
            );
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
        }
    }
    Ok(())
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
