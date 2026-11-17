//! `account`: an operator's act on an account of this deployment.
//!
//! `retire` records the account's JID in the ledger of retired JIDs and
//! removes the account, so that no provisioning path issues the JID again
//! (`auth::retirement`, ADR-0020 D8). On PostgreSQL it runs as the migrator
//! login acting as the schema owner, like the other operator subcommands:
//! the restricted runtime role may neither write the ledger nor delete an
//! account. A deployment on SQLite names its database file instead.
//!
//! Once the retirement committed it asks every serving replica to end the
//! account's open sessions and waits for their acknowledgements
//! (`auth::session_endings`). The retirement stands whatever the replicas
//! answer: a run that is not confirmed by every serving replica exits as
//! pending, and running it again asks again.

use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::retirement::retire_account;
use crate::auth::session_endings::{
    await_session_endings, prune_session_endings, request_session_ending, ReplicaEnding,
    SessionEndingError, SessionEndingReport,
};
use crate::db::{Database, DatabaseConfig, DatabaseDriver};

use super::args::{AccountActionArgs, AccountArgs, OwnerConnectionArgs};
use super::connection::open_owner;
use super::error::OwnerCliError;

pub(super) async fn run(args: AccountArgs) -> Result<(), OwnerCliError> {
    match args.action {
        AccountActionArgs::Retire {
            jid,
            sqlite_database,
            schema,
            migrator_login,
            schema_owner,
            session_wait,
        } => {
            let database = match sqlite_database {
                Some(path) => {
                    if !path.is_file() {
                        return Err(OwnerCliError::Configuration(format!(
                            "{} is not a database file",
                            path.display()
                        )));
                    }
                    Database::from_config(
                        "account retirement",
                        &DatabaseConfig::new(
                            DatabaseDriver::Sqlite,
                            format!("sqlite://{}", path.to_string_lossy()),
                        ),
                    )
                    .await
                    .map_err(OwnerCliError::Connect)?
                }
                None => {
                    let (Some(schema), Some(migrator_login), Some(schema_owner)) =
                        (schema, migrator_login, schema_owner)
                    else {
                        return Err(OwnerCliError::Configuration(
                            "name the PostgreSQL owner connection (--schema, --migrator-login \
                             and --schema-owner, or their environment variables), or the \
                             SQLite database file with --sqlite-database"
                                .to_owned(),
                        ));
                    };
                    let application_name =
                        format!("waddle-account-retire-{}", Uuid::new_v4().simple());
                    open_owner(
                        &OwnerConnectionArgs {
                            schema,
                            migrator_login,
                            schema_owner,
                        },
                        &application_name,
                    )
                    .await?
                }
            };
            let retirement = retire_account(&database, &jid)
                .await
                .map_err(OwnerCliError::Retirement)?;
            // From here on the retirement stands: what follows is printed
            // with it, whatever the outcome.
            let sessions = end_sessions(&database, &jid, Duration::from_secs(session_wait)).await;
            println!(
                "{}",
                json!({
                    "jid": retirement.jid,
                    "newly_retired": retirement.newly_retired,
                    "native_accounts_removed": retirement.native_accounts,
                    "accounts_removed": retirement.accounts,
                    "sessions": sessions_json(sessions.as_ref().ok()),
                })
            );
            if retirement.newly_retired {
                eprintln!("retired the JID; it is never issued again");
            } else {
                eprintln!("the JID was already retired");
            }
            let report = sessions.map_err(OwnerCliError::SessionEnding)?;
            let pending = report
                .replicas
                .iter()
                .filter(|replica| replica.ending == ReplicaEnding::Pending)
                .count();
            if pending > 0 {
                return Err(OwnerCliError::SessionsUnconfirmed { pending });
            }
            if report.replicas.is_empty() {
                eprintln!(
                    "no server holds a lease: a server that serves within a day ends what is \
                     left of the account's sessions"
                );
            } else if report
                .replicas
                .iter()
                .any(|replica| replica.ending == ReplicaEnding::Lapsed)
            {
                eprintln!(
                    "every serving server ended the account's sessions; a server that stopped \
                     renewing its lease ends what it holds if it serves again within a day"
                );
            } else {
                eprintln!("every serving server ended the account's sessions");
            }
        }
    }
    Ok(())
}

/// Ask every serving replica to end the open sessions of the retired
/// account `jid` and wait for them for at most `wait`. Old leases, requests
/// and acknowledgements are removed first; a failure to remove them is
/// reported and changes nothing else.
async fn end_sessions(
    database: &Database,
    jid: &jid::BareJid,
    wait: Duration,
) -> Result<SessionEndingReport, SessionEndingError> {
    if let Err(error) = prune_session_endings(database).await {
        eprintln!("warning: old session-ending rows were not removed: {error}");
    }
    let request = request_session_ending(database, jid).await?;
    await_session_endings(database, &request, wait).await
}

/// The `sessions` member of the printed object: one entry per replica whose
/// lease counted, and whether no serving replica is left to confirm.
fn sessions_json(report: Option<&SessionEndingReport>) -> Value {
    let Some(report) = report else {
        return json!({
            "request": Value::Null,
            "confirmed": false,
            "replicas": [],
        });
    };
    let replicas: Vec<Value> = report
        .replicas
        .iter()
        .map(|replica| {
            let ended = match replica.ending {
                ReplicaEnding::Completed(ended) => Some(ended),
                ReplicaEnding::Lapsed | ReplicaEnding::Pending => None,
            };
            json!({
                "replica": replica.replica.simple().to_string(),
                "node": replica.node,
                "state": replica.ending.label(),
                "streams_closed": ended.map(|ended| ended.streams_closed),
                "detached_sessions_ended": ended.map(|ended| ended.detached_sessions_ended),
            })
        })
        .collect();
    json!({
        "request": report.request.simple().to_string(),
        "confirmed": report.confirmed(),
        "replicas": replicas,
    })
}
