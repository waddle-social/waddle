//! `mailbox-dead-letters`: inspect and repair dead-lettered durable-mailbox
//! entries online while servers are running.

use serde::Serialize;
use uuid::Uuid;
use waddle_xmpp::ingress::mailbox::{DirectRequestKey, MailboxOperationKey, RoomRequestKey};
use waddle_xmpp::ingress::MessageKey;

use crate::config::LineageConfig;
use crate::ingress::mailbox_worker::{
    list_mailbox_dead_letters, operator_repair_mailbox_dead_letter, OperatorMailboxRepairOutcome,
};
use crate::ingress_uow::{DeadLetteredMailboxEntry, IngressUnitOfWork, MailboxLaneId};

use super::args::{MailboxDeadLettersAction, MailboxDeadLettersArgs};
use super::connection::open_owner;
use super::error::OwnerCliError;

#[derive(Debug, Serialize)]
struct DeadLetterEntryJson {
    lane_id: String,
    sequence: u64,
    operation: String,
    operation_kind: &'static str,
    parent_key: String,
    target_node_id: String,
    target_node_epoch: String,
    attempts: u32,
    reason: &'static str,
    dead_lettered_at: String,
}

#[derive(Debug, Serialize)]
struct RepairItemJson {
    lane_id: String,
    operation: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct RepairReportJson {
    repaired: usize,
    skipped_invalid_evidence: usize,
    not_dead_lettered: usize,
    lane_missing: usize,
    entries: Vec<RepairItemJson>,
}

/// Format an operation key as `<kind>:<uuid>`, matching [`parse_operation_key`].
pub(super) fn format_operation_key(operation: MailboxOperationKey) -> (&'static str, String) {
    match operation {
        MailboxOperationKey::RoomAcceptance(key) => {
            ("room_acceptance", key.to_storage().to_string())
        }
        MailboxOperationKey::RoomJoin(key) => ("room_join", key.to_storage().to_string()),
        MailboxOperationKey::RoomPresence(key) => ("room_presence", key.to_storage().to_string()),
        MailboxOperationKey::RoomLeave(key) => ("room_leave", key.to_storage().to_string()),
        MailboxOperationKey::RoomIq(key) => ("room_iq", key.to_storage().to_string()),
        MailboxOperationKey::Direct(key) => ("direct", key.to_storage().to_string()),
    }
}

/// Parse `<kind>:<uuid>` into a typed [`MailboxOperationKey`].
pub(super) fn parse_operation_key(value: &str) -> Result<MailboxOperationKey, String> {
    let Some((kind, raw_uuid)) = value.split_once(':') else {
        return Err(
            "expected `<kind>:<uuid>`, where `<kind>` is one of room_acceptance, room_join, \
             room_presence, room_leave, room_iq, or direct"
                .to_owned(),
        );
    };
    let uuid: Uuid = raw_uuid
        .parse()
        .map_err(|_| format!("invalid UUID `{raw_uuid}` in mailbox operation key"))?;
    match kind {
        "room_acceptance" => Ok(MailboxOperationKey::RoomAcceptance(
            MessageKey::from_storage(uuid),
        )),
        "room_join" => Ok(MailboxOperationKey::RoomJoin(RoomRequestKey::from_storage(
            uuid,
        ))),
        "room_presence" => Ok(MailboxOperationKey::RoomPresence(
            RoomRequestKey::from_storage(uuid),
        )),
        "room_leave" => Ok(MailboxOperationKey::RoomLeave(
            RoomRequestKey::from_storage(uuid),
        )),
        "room_iq" => Ok(MailboxOperationKey::RoomIq(RoomRequestKey::from_storage(
            uuid,
        ))),
        "direct" => Ok(MailboxOperationKey::Direct(DirectRequestKey::from_storage(
            uuid,
        ))),
        other => Err(format!(
            "unknown mailbox operation kind `{other}`; expected one of room_acceptance, \
             room_join, room_presence, room_leave, room_iq, or direct"
        )),
    }
}

fn entry_to_json(entry: &DeadLetteredMailboxEntry) -> DeadLetterEntryJson {
    let (operation_kind, parent_key) = format_operation_key(entry.operation);
    DeadLetterEntryJson {
        lane_id: entry.lane.as_uuid().to_string(),
        sequence: entry.sequence.get(),
        operation: format!("{operation_kind}:{parent_key}"),
        operation_kind,
        parent_key,
        target_node_id: entry.target_owner.node_id.clone(),
        target_node_epoch: entry.target_owner.node_epoch.clone(),
        attempts: entry.attempts,
        reason: entry.reason.as_str(),
        dead_lettered_at: entry.dead_lettered_at.clone(),
    }
}

pub(super) async fn run(args: MailboxDeadLettersArgs) -> Result<(), OwnerCliError> {
    let lineage = LineageConfig::from_env()
        .map_err(|error| OwnerCliError::Configuration(error.to_string()))?;
    if lineage.deployment_uuid.is_none() {
        return Err(OwnerCliError::Configuration(
            "WADDLE_DEPLOYMENT_UUID is required".to_owned(),
        ));
    }
    let application_name = format!("waddle-mailbox-repair-{}", Uuid::new_v4().simple());
    let owner = open_owner(&args.connection, &application_name).await?;
    let uow = IngressUnitOfWork::open(owner, lineage).map_err(OwnerCliError::MailboxRepair)?;

    match args.action {
        MailboxDeadLettersAction::List { limit } => {
            let entries = list_mailbox_dead_letters(&uow, limit)
                .await
                .map_err(OwnerCliError::MailboxRepair)?;
            let rendered: Vec<DeadLetterEntryJson> = entries.iter().map(entry_to_json).collect();
            let json = serde_json::to_string_pretty(&rendered)
                .map_err(|error| OwnerCliError::Configuration(error.to_string()))?;
            println!("{json}");
            eprintln!("found {} dead-lettered mailbox entries", rendered.len());
            Ok(())
        }
        MailboxDeadLettersAction::Repair {
            lane,
            operation,
            all,
            limit,
            force,
        } => {
            let targets: Vec<(MailboxLaneId, MailboxOperationKey)> = if all {
                list_mailbox_dead_letters(&uow, limit)
                    .await
                    .map_err(OwnerCliError::MailboxRepair)?
                    .into_iter()
                    .map(|entry| (entry.lane, entry.operation))
                    .collect()
            } else {
                let (Some(lane_uuid), Some(operation)) = (lane, operation) else {
                    return Err(OwnerCliError::Configuration(
                        "specify either `--lane <UUID> --operation <KIND:UUID>` or `--all`"
                            .to_owned(),
                    ));
                };
                vec![(MailboxLaneId::from_uuid(lane_uuid), operation)]
            };

            let mut report = RepairReportJson {
                repaired: 0,
                skipped_invalid_evidence: 0,
                not_dead_lettered: 0,
                lane_missing: 0,
                entries: Vec::with_capacity(targets.len()),
            };
            for (lane_id, op) in targets {
                let (kind, parent_key) = format_operation_key(op);
                let op_str = format!("{kind}:{parent_key}");
                match operator_repair_mailbox_dead_letter(&uow, lane_id, op, force)
                    .await
                    .map_err(OwnerCliError::MailboxRepair)?
                {
                    OperatorMailboxRepairOutcome::Repaired { previous_reason } => {
                        report.repaired += 1;
                        report.entries.push(RepairItemJson {
                            lane_id: lane_id.as_uuid().to_string(),
                            operation: op_str,
                            status: "repaired",
                            previous_reason: Some(previous_reason.as_str()),
                        });
                    }
                    OperatorMailboxRepairOutcome::RefusedCanonicalEvidenceInvalid => {
                        report.skipped_invalid_evidence += 1;
                        report.entries.push(RepairItemJson {
                            lane_id: lane_id.as_uuid().to_string(),
                            operation: op_str,
                            status: "refused_canonical_evidence_invalid",
                            previous_reason: Some("canonical_evidence_invalid"),
                        });
                    }
                    OperatorMailboxRepairOutcome::NotDeadLettered => {
                        report.not_dead_lettered += 1;
                        report.entries.push(RepairItemJson {
                            lane_id: lane_id.as_uuid().to_string(),
                            operation: op_str,
                            status: "not_dead_lettered",
                            previous_reason: None,
                        });
                    }
                    OperatorMailboxRepairOutcome::LaneMissing => {
                        report.lane_missing += 1;
                        report.entries.push(RepairItemJson {
                            lane_id: lane_id.as_uuid().to_string(),
                            operation: op_str,
                            status: "lane_missing",
                            previous_reason: None,
                        });
                    }
                }
            }
            let json = serde_json::to_string_pretty(&report)
                .map_err(|error| OwnerCliError::Configuration(error.to_string()))?;
            println!("{json}");
            eprintln!(
                "mailbox dead-letter repair complete: repaired={}, skipped_invalid_evidence={}, \
                 not_dead_lettered={}, lane_missing={}",
                report.repaired,
                report.skipped_invalid_evidence,
                report.not_dead_lettered,
                report.lane_missing
            );
            if !all && report.repaired == 0 {
                if report.skipped_invalid_evidence > 0 {
                    return Err(OwnerCliError::MailboxRepairRefused(
                        "entry was dead-lettered for canonical_evidence_invalid; pass --force only \
                         after verifying canonical rows"
                            .to_owned(),
                    ));
                }
                if report.lane_missing > 0 {
                    return Err(OwnerCliError::MailboxRepairRefused(
                        "requested mailbox lane was not found".to_owned(),
                    ));
                }
                return Err(OwnerCliError::MailboxRepairRefused(
                    "requested mailbox entry is not currently dead-lettered".to_owned(),
                ));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{format_operation_key, parse_operation_key};
    use uuid::Uuid;
    use waddle_xmpp::ingress::mailbox::{DirectRequestKey, MailboxOperationKey, RoomRequestKey};
    use waddle_xmpp::ingress::MessageKey;

    #[test]
    fn operation_key_round_trips_for_every_kind() {
        let id = Uuid::nil();
        for op in [
            MailboxOperationKey::RoomAcceptance(MessageKey::from_storage(id)),
            MailboxOperationKey::RoomJoin(RoomRequestKey::from_storage(id)),
            MailboxOperationKey::RoomPresence(RoomRequestKey::from_storage(id)),
            MailboxOperationKey::RoomLeave(RoomRequestKey::from_storage(id)),
            MailboxOperationKey::RoomIq(RoomRequestKey::from_storage(id)),
            MailboxOperationKey::Direct(DirectRequestKey::from_storage(id)),
        ] {
            let (kind, key) = format_operation_key(op);
            let encoded = format!("{kind}:{key}");
            assert_eq!(parse_operation_key(&encoded), Ok(op));
        }
        assert!(parse_operation_key("unknown:00000000-0000-0000-0000-000000000000").is_err());
        assert!(parse_operation_key("room_join:not-a-uuid").is_err());
        assert!(parse_operation_key("room_join").is_err());
    }
}
