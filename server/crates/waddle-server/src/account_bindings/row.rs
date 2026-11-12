//! The stored resource row and its codec.

use jid::FullJid;
use minidom::Element;
use serde::de::DeserializeOwned;
use serde::Serialize;
use uuid::Uuid;
use waddle_xmpp::ownership::account_coordinator::PresentationFlags;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindingImage, AccountResourceBindingState, AccountResourceRetirementImage,
    AccountResourceRevision, AccountResourceSlotImage, AccountResourceSnapshot,
    BindingRetirementDisposition,
};
use waddle_xmpp::ownership::endpoint_transition::LogicalResourceGeneration;
use waddle_xmpp::ownership::joint_identity::{InstallGeneration, ResourceLifetime, SmBinding};
use waddle_xmpp::ownership::{SessionGeneration, SmSessionKey};
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::presence::Show;

use super::error::AccountBindingStoreError;
use crate::db::{Row, Value};

/// One resource row: the slot's durable image and the one-time
/// presentations recorded for its current binding.
///
/// The snapshot's presence extension payloads are stored as one XML
/// document whose root wraps them, so a slot rebuilt from a row answers a
/// presence probe with the payloads the resource announced.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredResource {
    pub image: AccountResourceSlotImage,
    pub presented: PresentationFlags,
}

/// Every column a write sets, in parameter order. `full_jid` is the key and
/// is bound last by both the insert and the update.
const WRITTEN: [&str; 30] = [
    "bare_jid",
    "claim_identity",
    "revision",
    "install_generation",
    "operation_seq",
    "state",
    "binding_ref",
    "binding_claim_identity",
    "logical_generation",
    "occupancy_generation",
    "endpoint_published",
    "actor_registered",
    "presence_available",
    "presence_show",
    "presence_status",
    "presence_priority",
    "carbons_enabled",
    "roster_interested",
    "blocklist_interested",
    "sm_session_id",
    "sm_generation",
    "snapshot_revision",
    "detach_custody",
    "retiring_from",
    "retirement_source_revision",
    "retirement_custody",
    "offline_flushed",
    "subscriptions_flushed",
    "updated_at_ms",
    "presence_payloads",
];

/// The columns [`decode`] reads, in order.
pub(super) const SELECTED: &str = "full_jid, claim_identity, revision, install_generation, \
    operation_seq, state, binding_ref, binding_claim_identity, logical_generation, \
    occupancy_generation, endpoint_published, actor_registered, presence_available, \
    presence_show, presence_status, presence_priority, carbons_enabled, roster_interested, \
    blocklist_interested, sm_session_id, sm_generation, snapshot_revision, detach_custody, \
    retiring_from, retirement_source_revision, retirement_custody, offline_flushed, \
    subscriptions_flushed, presence_payloads";

pub(super) fn insert_sql() -> String {
    let columns = WRITTEN.join(", ");
    let placeholders = vec!["?"; WRITTEN.len() + 1].join(", ");
    format!("INSERT INTO account_resource_bindings ({columns}, full_jid) VALUES ({placeholders})")
}

pub(super) fn update_sql() -> String {
    let assignments = WRITTEN
        .iter()
        .map(|column| format!("{column} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("UPDATE account_resource_bindings SET {assignments} WHERE full_jid = ?")
}

/// Namespace and name of the stored document that wraps a snapshot's
/// presence payloads. It exists only in the `presence_payloads` column.
const PAYLOADS_NS: &str = "urn:waddle:account-resource:presence-payloads:0";
const PAYLOADS_ROOT: &str = "payloads";

/// Serialize the payloads for the column; no document for no payloads.
fn encode_payloads(payloads: &[Element]) -> Option<String> {
    if payloads.is_empty() {
        return None;
    }
    let document = Element::builder(PAYLOADS_ROOT, PAYLOADS_NS)
        .append_all(payloads.iter().cloned())
        .build();
    Some(String::from(&document))
}

fn decode_payloads(stored: Option<String>) -> Result<Vec<Element>, AccountBindingStoreError> {
    let Some(stored) = stored else {
        return Ok(Vec::new());
    };
    let document = stored
        .parse::<Element>()
        .map_err(|_| corrupt("presence_payloads"))?;
    if !document.is(PAYLOADS_ROOT, PAYLOADS_NS) {
        return Err(corrupt("presence_payloads"));
    }
    Ok(document.children().cloned().collect())
}

fn corrupt(field: &'static str) -> AccountBindingStoreError {
    AccountBindingStoreError::CorruptRow { field }
}

pub(super) fn to_json<T: Serialize>(
    value: &T,
    field: &'static str,
) -> Result<String, AccountBindingStoreError> {
    serde_json::to_string(value).map_err(|_| corrupt(field))
}

pub(super) fn from_json<T: DeserializeOwned>(
    value: &str,
    field: &'static str,
) -> Result<T, AccountBindingStoreError> {
    serde_json::from_str(value).map_err(|_| corrupt(field))
}

pub(super) fn signed(value: u64) -> Result<i64, AccountBindingStoreError> {
    i64::try_from(value).map_err(|_| AccountBindingStoreError::CounterOutOfRange)
}

pub(super) fn unsigned(value: i64, field: &'static str) -> Result<u64, AccountBindingStoreError> {
    u64::try_from(value).map_err(|_| corrupt(field))
}

fn revision(
    value: i64,
    field: &'static str,
) -> Result<AccountResourceRevision, AccountBindingStoreError> {
    Ok(AccountResourceRevision::new(unsigned(value, field)?))
}

fn uuid(value: &str, field: &'static str) -> Result<Uuid, AccountBindingStoreError> {
    Uuid::parse_str(value).map_err(|_| corrupt(field))
}

const fn state_to_storage(state: AccountResourceBindingState) -> Option<i64> {
    match state {
        AccountResourceBindingState::Prepared => Some(1),
        AccountResourceBindingState::Published => Some(2),
        AccountResourceBindingState::Active => Some(3),
        AccountResourceBindingState::Detached => Some(4),
        AccountResourceBindingState::Retiring => Some(5),
        // A retired binding is not a current binding; its row is free.
        AccountResourceBindingState::Retired => None,
    }
}

fn state_from_storage(
    value: i64,
    field: &'static str,
) -> Result<AccountResourceBindingState, AccountBindingStoreError> {
    Ok(match value {
        1 => AccountResourceBindingState::Prepared,
        2 => AccountResourceBindingState::Published,
        3 => AccountResourceBindingState::Active,
        4 => AccountResourceBindingState::Detached,
        5 => AccountResourceBindingState::Retiring,
        _ => return Err(corrupt(field)),
    })
}

const fn custody_to_storage(custody: BindingRetirementDisposition) -> i64 {
    match custody {
        BindingRetirementDisposition::NoAcceptedWork => 1,
        BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner => 2,
        BindingRetirementDisposition::Unknown => 3,
    }
}

fn custody_from_storage(
    value: i64,
    field: &'static str,
) -> Result<BindingRetirementDisposition, AccountBindingStoreError> {
    Ok(match value {
        1 => BindingRetirementDisposition::NoAcceptedWork,
        2 => BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        3 => BindingRetirementDisposition::Unknown,
        _ => return Err(corrupt(field)),
    })
}

const fn show_to_storage(show: &Show) -> i64 {
    match show {
        Show::Away => 1,
        Show::Chat => 2,
        Show::Dnd => 3,
        Show::Xa => 4,
    }
}

fn show_from_storage(value: i64) -> Result<Show, AccountBindingStoreError> {
    Ok(match value {
        1 => Show::Away,
        2 => Show::Chat,
        3 => Show::Dnd,
        4 => Show::Xa,
        _ => return Err(corrupt("presence_show")),
    })
}

/// The parameters of an insert or an update of `stored`, ending with the
/// key.
pub(super) fn write_params(
    stored: &StoredResource,
    operation_seq: u64,
    now_ms: i64,
) -> Result<Vec<Value>, AccountBindingStoreError> {
    let image = &stored.image;
    let binding = image.binding.as_ref();
    let state = match binding {
        None => 0,
        Some(binding) => state_to_storage(binding.state).ok_or(corrupt("state"))?,
    };
    let default_snapshot = AccountResourceSnapshot::default();
    let snapshot = binding.map_or(&default_snapshot, |binding| &binding.snapshot);
    let sm = snapshot.stream_management_ref;
    let retirement = binding.and_then(|binding| binding.retirement);
    let snapshot_revision = binding.map_or(AccountResourceRevision::INITIAL, |binding| {
        binding.snapshot_revision
    });
    Ok(crate::db_params![
        image.full_jid.to_bare().to_string(),
        to_json(&image.claim_identity, "claim_identity")?,
        signed(image.revision.value())?,
        signed(image.install_generation.value())?,
        signed(operation_seq)?,
        state,
        binding
            .map(|binding| to_json(&binding.binding_ref, "binding_ref"))
            .transpose()?,
        binding
            .map(|binding| to_json(&binding.binding_claim_identity, "binding_claim_identity"))
            .transpose()?,
        binding.map(|binding| binding.lifetime.logical.as_uuid().to_string()),
        binding.map(|binding| binding.lifetime.occupancy.as_uuid().to_string()),
        i64::from(binding.is_some_and(|binding| binding.published)),
        i64::from(binding.is_some_and(|binding| binding.registered)),
        i64::from(snapshot.presence_available),
        snapshot.presence_show.as_ref().map(show_to_storage),
        snapshot.presence_status.clone(),
        i64::from(snapshot.presence_priority),
        i64::from(snapshot.carbons_enabled),
        i64::from(snapshot.roster_interested),
        i64::from(snapshot.blocklist_interested),
        sm.map(|sm| sm.session.as_uuid().to_string()),
        sm.map(|sm| signed(sm.generation.value())).transpose()?,
        signed(snapshot_revision.value())?,
        binding
            .and_then(|binding| binding.detach_custody)
            .map(custody_to_storage),
        retirement
            .map(|retirement| state_to_storage(retirement.starting_state)
                .ok_or(corrupt("retiring_from")))
            .transpose()?,
        retirement
            .map(|retirement| signed(retirement.source_revision.value()))
            .transpose()?,
        retirement.map(|retirement| custody_to_storage(retirement.custody_disposition)),
        i64::from(stored.presented.offline_messages),
        i64::from(stored.presented.pending_subscriptions),
        now_ms,
        encode_payloads(&snapshot.presence_payloads),
        image.full_jid.to_string(),
    ])
}

/// Decode a row selected with [`SELECTED`] into the stored resource and its
/// operation sequence.
pub(super) fn decode(row: &Row) -> Result<(StoredResource, u64), AccountBindingStoreError> {
    let full_jid = row
        .get::<String>(0)?
        .parse::<FullJid>()
        .map_err(|_| corrupt("full_jid"))?;
    let claim_identity = from_json(&row.get::<String>(1)?, "claim_identity")?;
    let slot_revision = revision(row.get(2)?, "revision")?;
    let install_generation = InstallGeneration::new(unsigned(row.get(3)?, "install_generation")?)
        .ok_or(corrupt("install_generation"))?;
    let operation_seq = unsigned(row.get(4)?, "operation_seq")?;
    let state: i64 = row.get(5)?;
    let presented = PresentationFlags {
        offline_messages: row.get::<i64>(26)? != 0,
        pending_subscriptions: row.get::<i64>(27)? != 0,
    };
    let binding = if state == 0 {
        None
    } else {
        let sm = match (row.get::<Option<String>>(19)?, row.get::<Option<i64>>(20)?) {
            (Some(session), Some(generation)) => Some(SmBinding {
                session: SmSessionKey::from_uuid(uuid(&session, "sm_session_id")?),
                generation: SessionGeneration::new(unsigned(generation, "sm_generation")?)
                    .ok_or(corrupt("sm_generation"))?,
            }),
            (None, None) => None,
            _ => return Err(corrupt("sm_generation")),
        };
        let retirement = match (
            row.get::<Option<i64>>(23)?,
            row.get::<Option<i64>>(24)?,
            row.get::<Option<i64>>(25)?,
        ) {
            (Some(from), Some(source), Some(custody)) => Some(AccountResourceRetirementImage {
                starting_state: state_from_storage(from, "retiring_from")?,
                source_revision: revision(source, "retirement_source_revision")?,
                custody_disposition: custody_from_storage(custody, "retirement_custody")?,
            }),
            (None, None, None) => None,
            _ => return Err(corrupt("retirement")),
        };
        let priority =
            i8::try_from(row.get::<i64>(15)?).map_err(|_| corrupt("presence_priority"))?;
        Some(AccountResourceBindingImage {
            binding_ref: from_json(
                &row.get::<Option<String>>(6)?
                    .ok_or(corrupt("binding_ref"))?,
                "binding_ref",
            )?,
            binding_claim_identity: from_json(
                &row.get::<Option<String>>(7)?
                    .ok_or(corrupt("binding_claim_identity"))?,
                "binding_claim_identity",
            )?,
            lifetime: ResourceLifetime {
                logical: LogicalResourceGeneration::from_uuid(uuid(
                    &row.get::<Option<String>>(8)?
                        .ok_or(corrupt("logical_generation"))?,
                    "logical_generation",
                )?),
                occupancy: OccupancySessionGeneration::from_uuid(uuid(
                    &row.get::<Option<String>>(9)?
                        .ok_or(corrupt("occupancy_generation"))?,
                    "occupancy_generation",
                )?),
            },
            state: state_from_storage(state, "state")?,
            published: row.get::<i64>(10)? != 0,
            registered: row.get::<i64>(11)? != 0,
            snapshot: AccountResourceSnapshot {
                presence_available: row.get::<i64>(12)? != 0,
                presence_show: row
                    .get::<Option<i64>>(13)?
                    .map(show_from_storage)
                    .transpose()?,
                presence_status: row.get(14)?,
                presence_priority: priority,
                presence_payloads: decode_payloads(row.get(28)?)?,
                carbons_enabled: row.get::<i64>(16)? != 0,
                roster_interested: row.get::<i64>(17)? != 0,
                blocklist_interested: row.get::<i64>(18)? != 0,
                stream_management_ref: sm,
            },
            snapshot_revision: revision(row.get(21)?, "snapshot_revision")?,
            detach_custody: row
                .get::<Option<i64>>(22)?
                .map(|custody| custody_from_storage(custody, "detach_custody"))
                .transpose()?,
            retirement,
        })
    };
    Ok((
        StoredResource {
            image: AccountResourceSlotImage {
                full_jid,
                claim_identity,
                revision: slot_revision,
                install_generation,
                binding,
            },
            presented,
        },
        operation_seq,
    ))
}
