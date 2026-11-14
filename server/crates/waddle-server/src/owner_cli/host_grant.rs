//! `host-grant`: an operator's act on the host-grant authority after a
//! restore (`permissions::host_grant::restore`).

use serde_json::json;
use uuid::Uuid;

use crate::permissions::host_grant::{
    GrantOperationId, HostGrantStore, OperatorRevoke, ServedAccountDomains,
};

use super::args::{HostGrantActionArgs, HostGrantArgs};
use super::connection::open_owner;
use super::error::OwnerCliError;

pub(super) async fn run(args: HostGrantArgs) -> Result<(), OwnerCliError> {
    let application_name = format!("waddle-host-grant-{}", Uuid::new_v4().simple());
    let owner = open_owner(&args.connection, &application_name).await?;
    let store = HostGrantStore::open(owner).map_err(OwnerCliError::HostGrant)?;
    match args.action {
        HostGrantActionArgs::Reopen { scope } => {
            let report = store
                .reopen_scopes(scope)
                .await
                .map_err(OwnerCliError::HostGrant)?;
            println!(
                "{}",
                json!({
                    "reopened": report.reopened,
                    "not_reconstructed": report.not_reconstructed,
                })
            );
            eprintln!(
                "reopened {} host-grant scope(s); {} not reconstructed and left closed",
                report.reopened, report.not_reconstructed
            );
        }
        HostGrantActionArgs::AcknowledgePosition => {
            let (system_identifier, timeline) = store
                .acknowledge_position()
                .await
                .map_err(OwnerCliError::HostGrant)?;
            println!(
                "{}",
                json!({
                    "system_identifier": system_identifier.to_string(),
                    "timeline": timeline,
                })
            );
            eprintln!("acknowledged the PostgreSQL system and timeline the server writes on");
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
        }
    }
    Ok(())
}
