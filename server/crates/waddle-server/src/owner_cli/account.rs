//! `account`: an operator's act on an account of this deployment.
//!
//! `retire` records the account's JID in the ledger of retired JIDs and
//! removes the account, so that no provisioning path issues the JID again
//! (`auth::retirement`, ADR-0020 D8). On PostgreSQL it runs as the migrator
//! login acting as the schema owner, like the other operator subcommands:
//! the restricted runtime role may neither write the ledger nor delete an
//! account. A deployment on SQLite names its database file instead.

use serde_json::json;
use uuid::Uuid;

use crate::auth::retirement::retire_account;
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
            println!(
                "{}",
                json!({
                    "jid": retirement.jid,
                    "newly_retired": retirement.newly_retired,
                    "native_accounts_removed": retirement.native_accounts,
                    "accounts_removed": retirement.accounts,
                })
            );
            if retirement.newly_retired {
                eprintln!("retired the JID; it is never issued again");
            } else {
                eprintln!("the JID was already retired");
            }
        }
    }
    Ok(())
}
