use super::*;
use serde::{Deserialize, Serialize};
use waddle_xmpp::muc::{build_destroy_notification, DestroyRequest, RoomRegistry, NS_MUC_OWNER};

use crate::server::routes::websocket::cleanup::{
    cancel_destroy_completion_attempt, drain_destroy_completion_attempt,
    register_destroy_completion,
};

/// Extract the optional alternate venue, reason, and password from a
/// `<destroy xmlns='muc#owner'/>` child of the owner-config IQ. All
/// fields are optional per XEP-0045 §10.9, so a malformed or empty
/// element yields `DestroyRequest::default()` and the destroy still
/// proceeds.
fn parse_destroy_request(iq: &xmpp_parsers::iq::Iq) -> Option<DestroyRequest> {
    let xmpp_parsers::iq::Iq::Set { payload: query, .. } = iq else {
        return None;
    };
    let destroy = query.get_child("destroy", NS_MUC_OWNER)?;
    let mut request = DestroyRequest::default();
    if let Some(jid_str) = destroy.attr("jid") {
        request.alternate_venue = jid_str.parse().ok();
    }
    for child in destroy.children() {
        match child.name() {
            "reason" => {
                let text = child.text();
                if !text.is_empty() {
                    request.reason = Some(text);
                }
            }
            "password" => {
                let text = child.text();
                if !text.is_empty() {
                    request.password = Some(text);
                }
            }
            _ => {}
        }
    }
    Some(request)
}

/// Durable boundary representation for owner-IQ destroy completion.  The
/// actor-owned `MucRoom` deliberately is not serializable; capture only the
/// typed data the server needs after a crash: durable members and the exact
/// XEP-0045 recipient/request snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedDestroyCompletion {
    attempt: uuid::Uuid,
    room_jid: BareJid,
    reason: Option<String>,
    alternate_venue: Option<BareJid>,
    password: Option<String>,
    members: Vec<BareJid>,
    recipients: Vec<PersistedDestroyRecipient>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedDestroyRecipient {
    nick: String,
    sessions: Vec<FullJid>,
}

#[derive(Clone)]
struct DestroyCompletionSnapshot {
    attempt: waddle_xmpp::muc::DestroyAttemptId,
    room_jid: BareJid,
    lifecycle: Option<waddle_xmpp::muc::RoomLifecycleId>,
    request: DestroyRequest,
    members: Vec<BareJid>,
    recipients: Vec<(String, Vec<FullJid>)>,
}

pub(crate) struct LeasedDestroyCompletion {
    pub(crate) attempt: waddle_xmpp::muc::DestroyAttemptId,
    lease_token: String,
    snapshot: DestroyCompletionSnapshot,
}

fn persisted_destroy_snapshot_matches_attempt(
    snapshot: &DestroyCompletionSnapshot,
    stored_attempt: &str,
) -> bool {
    uuid::Uuid::parse_str(stored_attempt)
        .map(waddle_xmpp::muc::DestroyAttemptId::from_uuid)
        .is_ok_and(|attempt| snapshot.attempt == attempt)
}

impl TryFrom<&waddle_xmpp::muc::room_registry_actor::DestroyCompletion>
    for PersistedDestroyCompletion
{
    type Error = ();

    fn try_from(
        completion: &waddle_xmpp::muc::room_registry_actor::DestroyCompletion,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            attempt: completion.attempt.as_uuid(),
            room_jid: completion.room_jid.clone(),
            reason: completion.request.reason.clone(),
            alternate_venue: completion.request.alternate_venue.clone(),
            password: completion.request.password.clone(),
            members: completion
                .room
                .get_all_affiliations()
                .into_iter()
                .filter(|entry| entry.affiliation >= waddle_xmpp::Affiliation::Member)
                .map(|entry| entry.jid)
                .collect(),
            recipients: completion
                .room
                .occupants
                .values()
                .map(|occupant| PersistedDestroyRecipient {
                    nick: occupant.nick.clone(),
                    sessions: completion
                        .room
                        .get_occupant_sessions(&occupant.nick)
                        .into_iter()
                        .collect(),
                })
                .collect(),
        })
    }
}

impl TryFrom<PersistedDestroyCompletion> for DestroyCompletionSnapshot {
    type Error = ();

    fn try_from(value: PersistedDestroyCompletion) -> Result<Self, Self::Error> {
        Ok(Self {
            attempt: waddle_xmpp::muc::DestroyAttemptId::from_uuid(value.attempt),
            room_jid: value.room_jid,
            lifecycle: None,
            request: DestroyRequest {
                reason: value.reason,
                alternate_venue: value.alternate_venue,
                password: value.password,
            },
            members: value.members,
            recipients: value
                .recipients
                .into_iter()
                .map(|recipient| (recipient.nick, recipient.sessions))
                .collect(),
        })
    }
}

/// XEP-0045 §10.9 (#1261): "The room ... destroys the room, even if it
/// was defined as persistent." Delete every waddle-side durable row the
/// join path would otherwise rematerialize the destroyed room from,
/// ordered so that a mid-sequence failure is always FAIL-CLOSED —
/// every prefix of the sequence only ever narrows someone's access,
/// never widens it — and every step is an idempotent delete, so a
/// failed destroy leaves the room live and a retry re-runs the
/// sequence safely:
///
/// 1. the outstanding mediated-invite ledger (#1264) — an
///    undeclinable invite is the mildest possible consequence;
/// 2. for group DMs, each member's permission tuple (fail-hard — a
///    surviving tuple is durable authorization for a dead room) and
///    XEP-0402 bookmark (best-effort — a stale bookmark is cosmetic);
/// 3. group-DM per-member archive-visibility boundaries — deleted
///    only AFTER the member tuples: an absent boundary row means
///    full-history visibility, so dropping it while a member tuple
///    still authorizes MAM reads would silently WIDEN that member's
///    history access on an aborted destroy;
/// 4. the managed-channel catalog row (`channels`) LAST — it is the
///    resurrection vector `handle_muc_join` rebuilds managed rooms
///    from, so it only falls once everything else committed.
///
/// Runs BEFORE occupants are notified and before the actor is torn
/// down: a destroy that cannot durably destroy fails as a whole (the
/// caller returns `internal-server-error` and the room stays live)
/// instead of acknowledging a destruction that will resurrect.
async fn wipe_destroyed_room_durable_state(
    state: &WebSocketState,
    room_jid: &BareJid,
    lifecycle: Option<waddle_xmpp::muc::RoomLifecycleId>,
    members: &[BareJid],
) -> Result<bool, ()> {
    let db_actor = state.deps.app_state.db_pool.global_actor().clone();
    if !destroyed_lifecycle_is_current(state, room_jid, lifecycle).await? {
        return Ok(false);
    }
    if let Err(error) = crate::server::routes::websocket::muc_invites::delete_room_invites(
        db_actor.clone(),
        room_jid,
    )
    .await
    {
        warn!(
            room = %room_jid,
            error = %error,
            "Failed to delete outstanding invites for destroyed room"
        );
        return Err(());
    }
    if !destroyed_lifecycle_is_current(state, room_jid, lifecycle).await? {
        return Ok(false);
    }
    if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) {
        let channel = match crate::server::xmpp_state::get_xmpp_channel(
            db_actor.clone(),
            &channel_id,
        )
        .await
        {
            Ok(channel) => channel,
            Err(error) => {
                warn!(
                    room = %room_jid,
                    channel = %channel_id,
                    error = %error,
                    "Failed to look up channel row while destroying room"
                );
                return Err(());
            }
        };
        if channel.is_some_and(|channel| {
            channel.channel_type == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM
        }) {
            // Group-DM membership is durably authorized via permission
            // tuples and surfaced via XEP-0402 bookmarks; both must go
            // with the room or the members stay durably entitled to a
            // room that no longer exists.
            for member in members {
                if !destroyed_lifecycle_is_current(state, room_jid, lifecycle).await? {
                    return Ok(false);
                }
                if let Err(error) = crate::admin::channels::remove_group_dm_member_tuple(
                    &state.deps.app_state,
                    &channel_id,
                    member,
                )
                .await
                {
                    warn!(
                        room = %room_jid,
                        member = %member,
                        error = %error,
                        "Failed to revoke group-DM member tuple for destroyed room"
                    );
                    return Err(());
                }
                if crate::admin::channels::retract_group_dm_bookmark(
                    &state.deps.app_state,
                    member,
                    room_jid,
                )
                .await
                .is_err()
                {
                    warn!(
                        room = %room_jid,
                        member = %member,
                        "Failed to retract group-DM bookmark for destroyed room"
                    );
                }
            }
        }
        // Boundaries fall only after the member tuples above: an
        // absent boundary means full-history visibility, so this
        // delete is only safe once no tuple authorizes MAM reads.
        if !destroyed_lifecycle_is_current(state, room_jid, lifecycle).await? {
            return Ok(false);
        }
        if let Err(error) = db_actor
            .ask(crate::db::actor::DbExecute {
                sql: "DELETE FROM group_dm_archive_boundaries WHERE room_jid = ?".to_string(),
                params: vec![room_jid.to_string().into()],
            })
            .await
        {
            warn!(
                room = %room_jid,
                error = %error,
                "Failed to delete archive boundaries for destroyed room"
            );
            return Err(());
        }
        if !destroyed_lifecycle_is_current(state, room_jid, lifecycle).await? {
            return Ok(false);
        }
        if let Err(error) =
            crate::server::xmpp_channels::delete_xmpp_channel(db_actor.clone(), &channel_id).await
        {
            warn!(
                room = %room_jid,
                channel = %channel_id,
                error = %error,
                "Failed to delete channel catalog row for destroyed room"
            );
            return Err(());
        }
    }
    Ok(true)
}

/// A retry from an old room incarnation must never mutate a replacement that
/// reused the same room JID. The clustered room catalog is the lifecycle
/// back-link: the recorded lifecycle must still be the tombstone and no newer
/// live catalog row may have appeared before any app-owned destructive step.
async fn destroyed_lifecycle_is_current(
    state: &WebSocketState,
    room_jid: &BareJid,
    lifecycle: Option<waddle_xmpp::muc::RoomLifecycleId>,
) -> Result<bool, ()> {
    use crate::db::{row_value, Value};

    let Some(lifecycle) = lifecycle else {
        // A clustered legacy row without a lifecycle cannot be safely replayed
        // after a room-JID reuse. Return an error rather than a stale-result
        // success, so the lease is released and the completion remains visible
        // for explicit recovery instead of being acknowledged without cleanup.
        #[cfg(feature = "clustering")]
        if state
            .deps
            .app_state
            .clustering_claims
            .muc_durable_store
            .is_some()
        {
            return Err(());
        }
        #[cfg(not(feature = "clustering"))]
        return Ok(true);
        #[cfg(feature = "clustering")]
        return Ok(true);
    };
    let rows = state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbQuery {
            sql: "SELECT EXISTS ( \
                  SELECT 1 FROM clustering_muc_room_lifecycles destroyed \
                  WHERE destroyed.lifecycle_id = ? AND destroyed.room_jid = ? \
                    AND destroyed.state = 'tombstoned' \
                    AND NOT EXISTS ( \
                        SELECT 1 FROM clustering_muc_rooms current \
                        WHERE current.room_jid = ? \
                    ) \
                )"
                .to_string(),
            params: crate::db_params![
                lifecycle.to_string(),
                room_jid.to_string(),
                room_jid.to_string(),
            ],
        })
        .await
        .map_err(|error| {
            warn!(room = %room_jid, lifecycle = %lifecycle, %error, "Failed to verify destroy cleanup lifecycle fence");
        })?;
    rows.first()
        .and_then(|row| row_value(row, 0).ok())
        .and_then(|value| match value {
            Value::Integer(value) => Some(*value != 0),
            _ => None,
        })
        .ok_or_else(|| {
            warn!(room = %room_jid, lifecycle = %lifecycle, "Invalid destroy cleanup lifecycle fence result");
        })
}

/// Execute the server-owned half of a committed owner-IQ destroy. The
/// registry retains this typed completion across lost replies, then hands it
/// back to this layer once the durable destroy has committed.
async fn complete_destroy_snapshot(
    state: &WebSocketState,
    snapshot: DestroyCompletionSnapshot,
    inline_session: Option<&FullJid>,
) -> Result<Vec<String>, ()> {
    let room_jid = snapshot.room_jid;
    let wipe_completed = match wipe_destroyed_room_durable_state(
        state,
        &room_jid,
        snapshot.lifecycle,
        &snapshot.members,
    )
    .await
    {
        Ok(completed) => completed,
        Err(()) => {
            warn!(
                room = %room_jid,
                "Destroy committed but the app-level durable wipe (catalog row / \
                 group-DM tuples / boundaries / invite ledger) did not fully \
                 stick; leftover rows are idempotently re-cleanable and cannot \
                 resurrect the room's ban/affiliation state"
            );
            return Err(());
        }
    };
    if !wipe_completed {
        debug!(room = %room_jid, "Skipped stale destroy completion after room lifecycle replacement");
        return Ok(Vec::new());
    }
    let mut frames = Vec::new();
    for (nick, sessions) in snapshot.recipients {
        for session_jid in sessions {
            let is_inline_session = inline_session.is_some_and(|session| session_jid == *session);
            let occupant_bare = session_jid.to_bare();
            let identity = waddle_xmpp::xep::xep0421::OccupantIdentity {
                bare_jid: &occupant_bare,
                real_jid: Some(&session_jid),
                secret: &state.deps.occupant_id_secret,
            };
            let presence = build_destroy_notification(
                &room_jid,
                &nick,
                &session_jid,
                &snapshot.request,
                // This loop sends only a destroyed occupant's own final
                // unavailable presence to that occupant's sessions. Delivery
                // being recovered rather than inline cannot change XEP-0045
                // self-presence semantics (status 110).
                true,
                &identity,
            );
            if is_inline_session {
                frames.push(stanza_to_xml(&Stanza::Presence(presence)));
            } else {
                let frame = Stanza::Presence(presence);
                let outcome = state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&session_jid, frame.clone());
                if outcome == waddle_xmpp::registry::BroadcastOutcome::Delivered {
                    // No-op: local delivery succeeded.
                } else if delivery_outcome_warrants_retry(&outcome) {
                    #[cfg(feature = "clustering")]
                    {
                        if !try_deliver_registered_remote_destroy_notification(
                            state,
                            &session_jid,
                            &frame,
                        )
                        .await
                        {
                            return Err(());
                        }
                    }
                    #[cfg(not(feature = "clustering"))]
                    {
                        // Non-delivered local delivery is best-effort on
                        // non-clustered nodes and must not block destroy
                        // completion finalization.
                    }
                }
            }
            super::super::super::muc_call_sfu::unregister_participant_from_room(
                state,
                &room_jid,
                &session_jid,
            );
        }
    }
    debug!(room = %room_jid, "Completed committed MUC room destroy");
    Ok(frames)
}

fn delivery_outcome_warrants_retry(outcome: &waddle_xmpp::registry::BroadcastOutcome) -> bool {
    !matches!(outcome, waddle_xmpp::registry::BroadcastOutcome::Delivered,)
}

#[cfg(feature = "clustering")]
async fn try_deliver_registered_remote_destroy_notification(
    state: &WebSocketState,
    target: &FullJid,
    stanza: &Stanza,
) -> bool {
    let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    else {
        return true;
    };
    bridge
        .try_deliver_registered_remote_resource(
            target,
            stanza,
            waddle_xmpp::registry::DeliveryKind::DirectFrame,
        )
        .await
        .is_some()
}

const DESTROY_COMPLETION_LEASE_TIMEOUT_MS: i64 = 30_000;
const DESTROY_COMPLETION_LEASE_RENEWAL_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(10);
const DESTROY_COMPLETION_DRAIN_BATCH_LIMIT: i64 = 32;
const DESTROY_COMPLETION_RETRY_DELAY_MS: i64 = 1_000;

fn destroy_completion_origin_instance_id() -> &'static str {
    static INSTANCE_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTANCE_ID
        .get_or_init(|| uuid::Uuid::new_v4().to_string())
        .as_str()
}

/// Persist first, before the registry is allowed to commit the terminal room
/// mutation. The row is inert until the registry reports a committed destroy;
/// no janitor may delete app state or emit XEP-0045 presences from an intent
/// whose terminal mutation failed or was cancelled.
pub(crate) async fn persist_destroy_completion(
    state: &WebSocketState,
    completion: &waddle_xmpp::muc::room_registry_actor::DestroyCompletion,
) -> Result<(), ()> {
    let payload = serde_json::to_string(&PersistedDestroyCompletion::try_from(completion)?)
        .map_err(|_| ())?;
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbExecute {
            sql: "INSERT INTO clustering_muc_destroy_outbox \
                  (attempt_id, payload_json, origin_instance_id, available_at_ms, lease_token, leased_at_ms) \
                  VALUES (?, ?, ?, ?, NULL, NULL) ON CONFLICT (attempt_id) DO NOTHING"
                .to_string(),
            params: crate::db_params![
                completion.attempt.as_uuid().to_string(),
                payload,
                destroy_completion_origin_instance_id(),
                i64::MAX,
            ],
        })
        .await
        .map_err(|error| {
            warn!(attempt = %completion.attempt.as_uuid(), %error, "Failed to persist MUC destroy completion");
        })?;
    Ok(())
}

/// Preserve the legacy non-clustered fast path after a committed registry
/// destroy. Clustered destroys arm this exact row inside the durable room
/// tombstone transaction; this best-effort update is therefore never the
/// crash-recovery proof for a durable destroy.
pub(crate) async fn arm_destroy_completion_recovery(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
) -> bool {
    for retry in 0..3 {
        match state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(crate::db::actor::DbExecute {
                sql: "UPDATE clustering_muc_destroy_outbox SET available_at_ms = ? \
                      WHERE attempt_id = ? AND lifecycle_id IS NULL AND \
                      (available_at_ms = ? OR available_at_ms <= ?)"
                    .to_string(),
                params: crate::db_params![
                    crate::time::now_ms(),
                    attempt.as_uuid().to_string(),
                    i64::MAX,
                    crate::time::now_ms(),
                ],
            })
            .await
        {
            Ok(1) => return true,
            Ok(count) => {
                warn!(attempt = %attempt.as_uuid(), %count, retry, "MUC destroy recovery arm did not update its outbox row")
            }
            Err(error) => {
                warn!(attempt = %attempt.as_uuid(), %error, retry, "Failed to arm persisted MUC destroy cleanup")
            }
        }
    }
    false
}

async fn arm_recovered_inert_destroy_completion(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
) -> bool {
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbExecute {
            sql: "UPDATE clustering_muc_destroy_outbox SET available_at_ms = ? \
                  WHERE attempt_id = ? AND available_at_ms = ?"
                .to_string(),
            params: crate::db_params![
                crate::time::now_ms(),
                attempt.as_uuid().to_string(),
                i64::MAX,
            ],
        })
        .await
    {
        Ok(1) => true,
        Ok(count) => {
            warn!(attempt = %attempt.as_uuid(), %count, "Recovered inert MUC destroy completion changed before it could be armed");
            false
        }
        Err(error) => {
            warn!(attempt = %attempt.as_uuid(), %error, "Failed to arm proven terminal MUC destroy completion");
            false
        }
    }
}

async fn reclaim_foreign_inert_destroy_completion(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
) -> bool {
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbExecute {
            sql: "DELETE FROM clustering_muc_destroy_outbox \
                  WHERE attempt_id = ? AND available_at_ms = ? AND lease_token IS NULL AND lifecycle_id IS NULL"
                .to_string(),
            params: crate::db_params![attempt.as_uuid().to_string(), i64::MAX],
        })
        .await
    {
        Ok(1) => true,
        Ok(0) => false,
        Ok(count) => {
            warn!(attempt = %attempt.as_uuid(), %count, "Foreign inert MUC destroy completion changed before reclamation");
            false
        }
        Err(error) => {
            warn!(attempt = %attempt.as_uuid(), %error, "Failed to reclaim foreign inert MUC destroy completion");
            false
        }
    }
}

pub(crate) async fn discard_persisted_destroy_completion(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
) {
    // This is only called before a destroy commits, while the row is inert
    // and cannot be leased by the janitor.
    if let Err(error) = state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbExecute {
            sql: "DELETE FROM clustering_muc_destroy_outbox WHERE attempt_id = ? AND available_at_ms = ?"
                .to_string(),
            params: crate::db_params![attempt.as_uuid().to_string(), i64::MAX],
        })
        .await
    {
        warn!(attempt = %attempt.as_uuid(), %error, "Failed to discard inert persisted MUC destroy completion");
    }
}

/// Claim an armed completion using the same conditional database lease for
/// inline and janitor execution. The lease token is required for both
/// acknowledgement and retry release, so one path cannot delete another
/// executor's live lease by attempt ID alone.
pub(crate) async fn claim_persisted_destroy_completion(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
) -> Option<LeasedDestroyCompletion> {
    use crate::db::{row_value, ValueExt};

    let now = crate::time::now_ms();
    let lease_token = uuid::Uuid::new_v4().to_string();
    let claimed = match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbExecute {
            sql: "UPDATE clustering_muc_destroy_outbox \
                  SET lease_token = ?, leased_at_ms = ? \
                  WHERE attempt_id = ? AND available_at_ms <= ? AND \
                  (lease_token IS NULL OR leased_at_ms < ?)"
                .to_string(),
            params: crate::db_params![
                lease_token.clone(),
                now,
                attempt.as_uuid().to_string(),
                now,
                now - DESTROY_COMPLETION_LEASE_TIMEOUT_MS,
            ],
        })
        .await
    {
        Ok(count) => count == 1,
        Err(error) => {
            warn!(attempt = %attempt.as_uuid(), %error, "Failed to claim persisted MUC destroy completion");
            false
        }
    };
    if !claimed {
        return None;
    }
    let rows = match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbQuery {
            sql: "SELECT payload_json, lifecycle_id FROM clustering_muc_destroy_outbox \
                  WHERE attempt_id = ? AND lease_token = ?"
                .to_string(),
            params: crate::db_params![attempt.as_uuid().to_string(), lease_token.clone()],
        })
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(attempt = %attempt.as_uuid(), %error, "Failed to load claimed MUC destroy completion");
            return None;
        }
    };
    let Some(row) = rows.first() else {
        warn!(attempt = %attempt.as_uuid(), "Claimed MUC destroy completion disappeared before load");
        return None;
    };
    let Ok(payload) = row_value(row, 0).and_then(ValueExt::as_string) else {
        warn!(attempt = %attempt.as_uuid(), "Invalid persisted MUC destroy payload");
        return None;
    };
    let Ok(lifecycle_id) = row_value(row, 1).and_then(ValueExt::as_optional_string) else {
        warn!(attempt = %attempt.as_uuid(), "Invalid persisted MUC destroy lifecycle id");
        return None;
    };
    let lifecycle = match lifecycle_id {
        Some(value) => match uuid::Uuid::parse_str(&value) {
            Ok(value) => Some(waddle_xmpp::muc::RoomLifecycleId::from_uuid(value)),
            Err(_) => {
                warn!(attempt = %attempt.as_uuid(), lifecycle = %value, "Invalid persisted MUC destroy lifecycle id");
                return None;
            }
        },
        None => None,
    };
    let Ok(mut snapshot) = serde_json::from_str::<PersistedDestroyCompletion>(&payload)
        .map_err(|_| ())
        .and_then(DestroyCompletionSnapshot::try_from)
    else {
        warn!(attempt = %attempt.as_uuid(), "Invalid persisted MUC destroy completion payload");
        return None;
    };
    if !persisted_destroy_snapshot_matches_attempt(&snapshot, &attempt.as_uuid().to_string()) {
        warn!(attempt = %attempt.as_uuid(), "Persisted MUC destroy completion payload does not match its attempt id");
        return None;
    }
    snapshot.lifecycle = lifecycle;
    Some(LeasedDestroyCompletion {
        attempt,
        lease_token,
        snapshot,
    })
}

/// Return whether the exact outbox row still exists after a local completion
/// could not acquire a lease. A missing row means another node already
/// finalized the shared work, whereas a present row may merely be actively
/// leased and must remain queued locally.
pub(crate) async fn persisted_destroy_completion_exists(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
) -> Option<bool> {
    use crate::db::{row_value, Value};

    let rows = match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbQuery {
            sql: "SELECT EXISTS (SELECT 1 FROM clustering_muc_destroy_outbox WHERE attempt_id = ?)"
                .to_string(),
            params: crate::db_params![attempt.as_uuid().to_string()],
        })
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(attempt = %attempt.as_uuid(), %error, "Failed to check persisted MUC destroy completion");
            return None;
        }
    };
    rows.first()
        .and_then(|row| row_value(row, 0).ok())
        .and_then(|value| match value {
            Value::Integer(value) => Some(*value != 0),
            _ => None,
        })
}

/// Extend an exact executor's conditional lease. Cleanup can revoke an
/// arbitrary number of group-DM tuples and must not let another node take
/// over while those side effects are in progress.
async fn renew_persisted_destroy_completion_lease(
    state: &WebSocketState,
    completion: &LeasedDestroyCompletion,
) -> Result<(), ()> {
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbExecute {
            sql: "UPDATE clustering_muc_destroy_outbox SET leased_at_ms = ? \
                  WHERE attempt_id = ? AND lease_token = ?"
                .to_string(),
            params: crate::db_params![
                crate::time::now_ms(),
                completion.attempt.as_uuid().to_string(),
                completion.lease_token.clone(),
            ],
        })
        .await
    {
        Ok(1) => Ok(()),
        Ok(count) => {
            warn!(attempt = %completion.attempt.as_uuid(), %count, "MUC destroy completion lease was lost during cleanup");
            Err(())
        }
        Err(error) => {
            warn!(attempt = %completion.attempt.as_uuid(), %error, "Failed to renew MUC destroy completion lease");
            Err(())
        }
    }
}

pub(crate) async fn complete_leased_destroy_completion(
    state: &WebSocketState,
    completion: &LeasedDestroyCompletion,
    inline_session: Option<&FullJid>,
) -> Result<Vec<String>, ()> {
    renew_persisted_destroy_completion_lease(state, completion).await?;
    let cleanup = complete_destroy_snapshot(state, completion.snapshot.clone(), inline_session);
    tokio::pin!(cleanup);
    loop {
        tokio::select! {
            result = &mut cleanup => return result,
            () = tokio::time::sleep(DESTROY_COMPLETION_LEASE_RENEWAL_INTERVAL) => {
                renew_persisted_destroy_completion_lease(state, completion).await?;
            }
        }
    }
}

async fn finalize_persisted_destroy_completion_with_future<F>(
    state: &WebSocketState,
    completion: &LeasedDestroyCompletion,
    renewal_interval: std::time::Duration,
    finalization: F,
) -> bool
where
    F: std::future::Future<Output = Result<u64, ()>>,
{
    let mut finalization = std::pin::pin!(finalization);
    loop {
        tokio::select! {
            biased;
            result = &mut finalization => {
                return match result {
                    Ok(count) => count == 1,
                    Err(()) => false,
                };
            }
            () = tokio::time::sleep(renewal_interval) => {
                if renew_persisted_destroy_completion_lease(state, completion)
                    .await
                    .is_err()
                {
                    return false;
                }
            }
        }
    }
}

pub(crate) async fn finalize_persisted_destroy_completion(
    state: &WebSocketState,
    completion: &LeasedDestroyCompletion,
    completed: bool,
) -> bool {
    let (sql, params) = if completed {
        (
            "DELETE FROM clustering_muc_destroy_outbox WHERE attempt_id = ? AND lease_token = ?",
            crate::db_params![
                completion.attempt.as_uuid().to_string(),
                completion.lease_token.clone()
            ],
        )
    } else {
        (
            "UPDATE clustering_muc_destroy_outbox \
             SET available_at_ms = ?, lease_token = NULL, leased_at_ms = NULL \
             WHERE attempt_id = ? AND lease_token = ?",
            crate::db_params![
                crate::time::now_ms().saturating_add(DESTROY_COMPLETION_RETRY_DELAY_MS),
                completion.attempt.as_uuid().to_string(),
                completion.lease_token.clone()
            ],
        )
    };
    finalize_persisted_destroy_completion_with_future(
        state,
        completion,
        DESTROY_COMPLETION_LEASE_RENEWAL_INTERVAL,
        async move {
            state
                .deps
                .app_state
                .db_pool
                .global_actor()
                .ask(crate::db::actor::DbExecute {
                    sql: sql.to_string(),
                    params,
                })
                .await
                .map_err(|error| {
                    warn!(attempt = %completion.attempt.as_uuid(), %error, "Failed to finalize persisted MUC destroy completion");
                })
        },
    )
    .await
}

async fn list_due_destroy_completion_attempts(
    state: &WebSocketState,
    now: i64,
    limit: i64,
) -> Vec<waddle_xmpp::muc::DestroyAttemptId> {
    use crate::db::{row_value, ValueExt};

    let rows = match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbQuery {
            sql: "SELECT attempt_id FROM clustering_muc_destroy_outbox \
                  WHERE available_at_ms <= ? AND \
                  (lease_token IS NULL OR leased_at_ms < ?) \
                  ORDER BY available_at_ms ASC, attempt_id ASC \
                  LIMIT ?"
                .to_string(),
            params: crate::db_params![now, now - DESTROY_COMPLETION_LEASE_TIMEOUT_MS, limit],
        })
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(%error, "Failed to list persisted MUC destroy completions");
            return Vec::new();
        }
    };

    rows.into_iter()
        .filter_map(|row| {
            let attempt_id = row_value(&row, 0).and_then(ValueExt::as_string).ok()?;
            let attempt_id = uuid::Uuid::parse_str(&attempt_id).ok()?;
            Some(waddle_xmpp::muc::DestroyAttemptId::from_uuid(attempt_id))
        })
        .collect()
}

/// Redrive persisted completion work after a process exit or an app-storage
/// failure. Every execution, including inline owner-IQ cleanup, uses the
/// same conditional lease helper above.
pub(crate) async fn drain_persisted_destroy_completions(state: &WebSocketState) {
    let now = crate::time::now_ms();
    for attempt in
        list_due_destroy_completion_attempts(state, now, DESTROY_COMPLETION_DRAIN_BATCH_LIMIT).await
    {
        let Some(completion) = claim_persisted_destroy_completion(state, attempt).await else {
            continue;
        };
        let completed = complete_leased_destroy_completion(state, &completion, None)
            .await
            .is_ok();
        let _ = finalize_persisted_destroy_completion(state, &completion, completed).await;
    }
    reconcile_foreign_inert_destroy_completions(state).await;
}

/// Recover only inert rows created by an earlier process. Current-process
/// rows may still be between completion persistence and the registry's
/// terminal commit, so a janitor must never infer their outcome. After a
/// restart, a durable tombstone proves a commit and may be armed; every other
/// inert intent is retained: it may belong to a live peer that has not yet
/// committed. Inert rows do not block recreation, so retaining uncertain work
/// is fail-closed without making stale intents durable demand-side locks.
async fn reconcile_foreign_inert_destroy_completions(state: &WebSocketState) {
    use crate::db::{row_value, ValueExt};

    let rows = match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbQuery {
            sql: "SELECT attempt_id, origin_instance_id, lifecycle_id FROM clustering_muc_destroy_outbox \
                  WHERE available_at_ms = ? AND lease_token IS NULL"
                .to_string(),
            params: crate::db_params![i64::MAX],
        })
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(%error, "Failed to list inert MUC destroy completions for restart recovery");
            return;
        }
    };
    for row in rows {
        let Ok(attempt_id) = row_value(&row, 0).and_then(ValueExt::as_string) else {
            warn!("Invalid inert MUC destroy attempt id");
            continue;
        };
        let Ok(attempt_uuid) = uuid::Uuid::parse_str(&attempt_id) else {
            warn!(attempt = %attempt_id, "Invalid inert MUC destroy attempt id");
            continue;
        };
        let Ok(origin) = row_value(&row, 1).and_then(ValueExt::as_optional_string) else {
            warn!(attempt = %attempt_id, "Invalid inert MUC destroy origin instance id");
            continue;
        };
        if origin.as_deref() == Some(destroy_completion_origin_instance_id()) {
            continue;
        }
        let attempt = waddle_xmpp::muc::DestroyAttemptId::from_uuid(attempt_uuid);
        let Ok(lifecycle_id) = row_value(&row, 2).and_then(ValueExt::as_optional_string) else {
            warn!(attempt = %attempt_id, "Invalid inert MUC destroy lifecycle id");
            continue;
        };
        if lifecycle_id.is_none() {
            #[cfg(feature = "clustering")]
            if state
                .deps
                .app_state
                .clustering_claims
                .muc_durable_store
                .is_some()
            {
                debug!(attempt = %attempt.as_uuid(), "Skipping foreign inert MUC destroy completion without terminal lifecycle proof");
                continue;
            }
            if reclaim_foreign_inert_destroy_completion(state, attempt).await {
                debug!(attempt = %attempt.as_uuid(), "Reclaimed foreign inert MUC destroy completion without terminal lifecycle proof");
            }
            continue;
        }
        #[cfg(feature = "clustering")]
        let committed = match &state.deps.app_state.clustering_claims.muc_durable_store {
            Some(store) => match store.recover_inert_destroy_completion(&attempt).await {
                Ok(committed) => committed,
                Err(error) => {
                    warn!(attempt = %attempt.as_uuid(), %error, "Failed to prove inert MUC destroy completion's terminal lifecycle");
                    continue;
                }
            },
            // Lifecycle-less foreign rows were already reclaimed above; any
            // remaining foreign inert row in single-node mode cannot be
            // proven committed and must remain inert.
            None => false,
        };
        // Builds without the `clustering` feature reclaim lifecycle-less
        // foreign rows above and have no durable proof path for anything else.
        #[cfg(not(feature = "clustering"))]
        let committed = false;
        if committed {
            let _ = arm_recovered_inert_destroy_completion(state, attempt).await;
        } else {
            debug!(attempt = %attempt.as_uuid(), "Retaining unproven inert MUC destroy completion");
        }
    }
}

pub(super) async fn handle_muc_owner_and_moderation_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: &Option<Session>,
) -> ResponseBatch {
    let iq = ctx.iq;
    let id = ctx.id;
    let payload_ns = ctx.payload_ns;
    let target_to = ctx.target_to;
    let has_destroy = ctx.has_destroy;
    let response_from = ctx.response_from;
    let response_to = ctx.response_to;

    // MUC owner IQ (XEP-0045): instant room config submit and room destroy.
    // This is needed for clients that create a room by:
    // 1) joining via presence
    // 2) submitting an empty owner form (`jabber:x:data` type='submit')
    if payload_ns == "http://jabber.org/protocol/muc#owner" {
        let Some(target) = target_to else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )]
            .into();
        };

        let room_target = target.split('/').next().unwrap_or(target);
        let Ok(room_jid) = room_target.parse::<BareJid>() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                jid_malformed_iq_error("Malformed JID in IQ addressing."),
            )]
            .into();
        };

        if !is_muc_room_jid(state, &room_jid).await {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                item_not_found_iq_error("Requested item not found."),
            )]
            .into();
        }

        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )]
            .into();
        };
        match muc_owner_authorized(state, &room_jid, sender_jid, authenticated_session.as_ref())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    forbidden_iq_error("Operation not permitted."),
                )]
                .into();
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = %error,
                    "Failed to authorize MUC owner IQ"
                );
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )]
                .into();
            }
        }

        if has_destroy {
            // XEP-0045 §10.9: parse the optional alternate venue and
            // reason out of the `<destroy/>` payload. Admission is then
            // sealed before capturing the recipient snapshot, so every
            // occupant the terminal destroy removes is notified.
            let destroy_request = parse_destroy_request(iq).unwrap_or_default();
            let attempt = waddle_xmpp::muc::DestroyAttemptId::generate();
            let room = match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                .seal_room_for_destroy_snapshot(room_jid.clone(), attempt)
                .await
            {
                Ok(room) => room,
                Err(error) => {
                    // A snapshot reply can be lost after the registry has
                    // sealed and retained this exact pre-seal. Ask it to
                    // reopen now; if that acknowledgement is lost too, the
                    // retained SnapshotPreseal phase reconciles itself on a
                    // later registry touch and never commits a destroy
                    // without a persisted completion.
                    match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                        .abort_destroy_room_attempt(room_jid.clone(), attempt)
                        .await
                    {
                        Ok(true) | Ok(false) => {}
                        Err(abort_error) => {
                            warn!(room = %room_jid, %abort_error, "Failed to request reconciliation of an ambiguous MUC destroy snapshot pre-seal");
                        }
                    }
                    warn!(room = %room_jid, %error, "MUC destroy seal-and-snapshot failed");
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )]
                    .into();
                }
            };
            let completion = waddle_xmpp::muc::room_registry_actor::DestroyCompletion {
                attempt,
                room_jid: room_jid.clone(),
                room,
                request: destroy_request,
            };
            if persist_destroy_completion(state, &completion)
                .await
                .is_err()
            {
                match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                    .abort_destroy_room_attempt(room_jid.clone(), attempt)
                    .await
                {
                    Ok(true) => discard_persisted_destroy_completion(state, attempt).await,
                    Ok(false) => {}
                    Err(error) => {
                        warn!(room = %room_jid, %error, "Failed to reopen MUC room after destroy completion persistence failure");
                    }
                }
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )]
                .into();
            }
            if let Err(error) = register_destroy_completion(state, completion.clone()).await {
                warn!(room = %room_jid, %error, "Failed to retain MUC destroy completion");
                cancel_destroy_completion_attempt(state, attempt).await;
                discard_persisted_destroy_completion(state, attempt).await;
                if let Err(error) = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                    .abort_destroy_room_attempt(room_jid.clone(), attempt)
                    .await
                {
                    warn!(room = %room_jid, %error, "Failed to reopen MUC room after destroy completion registration failure");
                }
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )]
                .into();
            }
            // Single atomic commit point (#1261, #1276 Greptile P1s).
            // The attempt-bound registry destroy handler
            // removes the in-memory entry AND wipes the clustering durable
            // state (config/subject/affiliations incl. bans — the
            // security-sensitive resurrection vector) under one claim
            // fence: on a durable-delete failure it restores the entry and
            // reports `DurableWipeFailed`, so the room ends up either fully
            // destroyed or fully intact. There is no separate pre-wipe to
            // roll back, so no fail-open window can leave a live room
            // stripped of its bans/affiliations. Only the irreversible
            // app-level rows (catalog row, group-DM tuples/bookmarks,
            // archive boundaries, invite ledger) are wiped afterwards,
            // strictly AFTER this commit.
            match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                .destroy_room_with_attempt(room_jid.clone(), attempt)
                .await
            {
                Ok(waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome::Destroyed) => {
                    // In clustered mode the durable destroy transaction has
                    // already armed this attempt's outbox row. Retain the
                    // non-clustered fast path, whose registry has no durable
                    // room-store transaction to extend.
                    #[cfg(feature = "clustering")]
                    let clustered = state
                        .deps
                        .app_state
                        .clustering_claims
                        .muc_durable_store
                        .is_some();
                    #[cfg(not(feature = "clustering"))]
                    let clustered = false;
                    if !clustered && !arm_destroy_completion_recovery(state, attempt).await {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            internal_server_error_iq_error("Internal server error."),
                        )]
                        .into();
                    }
                }
                // The clustering durable wipe failed and the registry
                // atomically restored the room — nothing was torn down, so
                // answer retryable, never a false success.
                Ok(
                    waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome::DurableWipeFailed,
                ) => {
                    discard_persisted_destroy_completion(state, attempt).await;
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )]
                    .into();
                }
                Ok(
                    waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome::ReleaseBacklogFull,
                ) => {
                    cancel_destroy_completion_attempt(state, attempt).await;
                    discard_persisted_destroy_completion(state, attempt).await;
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )]
                    .into();
                }
                // A concurrent destroy already removed the room between the
                // `get_room_actor` lookup above and here — genuinely gone.
                Ok(waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome::NotRegistered) => {
                    cancel_destroy_completion_attempt(state, attempt).await;
                    discard_persisted_destroy_completion(state, attempt).await;
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        item_not_found_iq_error("Requested item not found."),
                    )]
                    .into();
                }
                // `Unavailable` means the terminal message never entered the
                // registry. Its registered completion must be cancelled and
                // the exact pre-seal reopened; leaving it would otherwise
                // let a later reconciliation destroy a room after this
                // request definitively failed to reach the handler.
                Err(waddle_xmpp::muc::room_registry_actor::RoomRegistryError::Unavailable) => {
                    cancel_destroy_completion_attempt(state, attempt).await;
                    discard_persisted_destroy_completion(state, attempt).await;
                    match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                        .abort_destroy_room_attempt(room_jid.clone(), attempt)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            warn!(room = %room_jid, "Confirmed-undelivered MUC destroy could not reopen its registered pre-seal");
                        }
                        Err(error) => {
                            warn!(room = %room_jid, %error, "Failed to reopen confirmed-undelivered MUC destroy");
                        }
                    }
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )]
                    .into();
                }
                // A reply timeout may have lost the acknowledgement after
                // the terminal handler entered. Retain its exact completion
                // for registry reconciliation, never cancel or unseal it.
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        error = %error,
                        "Destroy ask failed; returning a retryable error"
                    );
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )]
                    .into();
                }
            }
            let mut frames =
                drain_destroy_completion_attempt(state, attempt, Some(sender_jid)).await;
            let room_jid_string = room_jid.to_string();
            frames.push(build_iq_result_xml(
                id,
                Some(room_jid_string.as_str()),
                response_to,
                None,
            ));
            return frames.into();
        }

        if matches!(iq, xmpp_parsers::iq::Iq::Get { .. }) {
            match build_muc_owner_config_response(state, &room_jid, id, response_to).await {
                Ok(response) => return vec![response].into(),
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        error = %error,
                        "Failed to build MUC owner config response"
                    );
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )]
                    .into();
                }
            }
        }

        let mut batch = match apply_muc_owner_config(
            state,
            &room_jid,
            iq,
            authenticated_session.as_ref(),
            phase.bound_jid(),
        )
        .await
        {
            Ok(batch) => batch,
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = %error,
                    "Failed to apply MUC owner config"
                );
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )]
                .into();
            }
        };

        // Treat all other owner IQ sets as successful config submit for instant rooms.
        let room_jid_string = room_jid.to_string();
        let mut frames = vec![build_iq_result_xml(
            id,
            Some(room_jid_string.as_str()),
            response_to,
            None,
        )];
        frames.append(&mut batch.frames);
        return ResponseBatch {
            frames,
            completions: batch.completions,
        };
    }

    if let Some(request) = parse_moderation_iq(iq) {
        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )]
            .into();
        };
        let Some(room_jid) = iq.to().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )]
            .into();
        };
        let room_actor = match get_room_actor_result(state, &room_jid).await {
            Ok(Some(room_actor)) => room_actor,
            Ok(None) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )]
                .into();
            }
            Err(error) => {
                warn!(room = %room_jid, %error, "MUC moderation room lookup failed");
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )]
                .into();
            }
        };
        let context = match room_actor
            .ask(GetAdminContext {
                sender_jid: sender_jid.clone(),
            })
            .await
        {
            Ok(context) => context,
            Err(_) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )]
                .into();
            }
        };
        // XEP-0425 §"only moderators are allowed to moderate" combined with
        // XEP-0045 §5.1.2: runtime moderation privilege is role-bound, not
        // affiliation-bound. Owner/Admin affiliations only matter to the
        // extent that they cause the room to grant the Moderator *role* on
        // entry; if an owner has explicitly taken a non-moderator role
        // (e.g. visitor), that signal is intentional and must be honoured.
        if !matches!(context.role, waddle_xmpp::Role::Moderator) {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                forbidden_iq_error("Operation not permitted."),
            )]
            .into();
        }
        match state
            .deps
            .protocol
            .mam_storage
            .get_message(&request.target_id)
            .await
        {
            Ok(Some(message)) if message.to.to_string() == room_jid.to_string() => {}
            Ok(Some(_)) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )]
                .into();
            }
            Ok(None) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )]
                .into();
            }
            Err(error) => {
                warn!(room = %room_jid, target = %request.target_id, error = %error, "Failed to look up moderation target");
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )]
                .into();
            }
        }

        let moderator_nick = context
            .nick
            .unwrap_or_else(|| sender_jid.resource().to_string());
        let moderated_by = format!("{room_jid}/{moderator_nick}");
        let stamp_time = chrono::Utc::now();
        // XEP-0425 v1 §3: the broadcast `<moderated>` element
        // carries the moderator's XEP-0421 occupant-id so
        // clients have a stable attribution identifier alongside
        // the bare JID in `by=`.
        let moderator_occupant_id = waddle_xmpp::xep::xep0421::generate_occupant_id(
            &sender_jid.to_bare(),
            &room_jid,
            &state.deps.occupant_id_secret,
        );
        let mut moderation = build_moderation_result_message(
            Some(jid::Jid::from(room_jid.clone())),
            &request.target_id,
            &moderated_by,
            Some(moderator_occupant_id.as_str()),
            request.reason.as_deref(),
        );
        let archive_id = uuid::Uuid::now_v7().to_string();
        let room_jid_full = jid::Jid::from(room_jid.clone());
        add_stanza_id_xep0359(
            &mut moderation,
            &Xep0359StanzaId::new(archive_id.as_str(), room_jid_full.clone()),
        );

        if let (Some(target_id), Ok(moderator_jid)) = (
            RichMessageId::new(request.target_id.clone()),
            moderated_by.parse::<Jid>(),
        ) {
            let archived = ArchivedMessage {
                id: archive_id.clone(),
                timestamp: chrono::Utc::now(),
                from: jid::Jid::from(room_jid.clone()),
                to: jid::Jid::from(room_jid.clone()),
                // XEP-0425 moderation tombstone has no `<body>` — `None`
                // is the wire-faithful "no body element" form.
                body: None,
                stanza_id: moderation
                    .id
                    .as_ref()
                    .map(|id| Xep0359StanzaId::new(id.0.clone(), room_jid_full.clone())),
                // XEP-0425 moderation tombstone: leak-prone fields are
                // already cleared by construction (this row is a fresh
                // tombstone, not a scrub of an existing message).
                thread: None,
                reply: None,
                origin_id: None,
                message_type: xmpp_parsers::message::MessageType::Groupchat,
                stanza_xml: None,
                rich: Some(ArchivedRichMessage {
                    payload: Some(ArchivedRichPayload::Moderation(ArchivedModeration {
                        target_id,
                        moderated_by: moderator_jid,
                        stamp: Some(stamp_time),
                        reason: request.reason.as_deref().and_then(RichText::new),
                    })),
                    reply: None,
                    references: Vec::new(),
                    mentions: Vec::new(),
                    subjects: Default::default(),
                    // Room-authored moderation event row: no occupant
                    // sender to identify.
                    occupant_id: None,
                    muc_sender: None,
                }),
                nickname_generation: None,
            };
            if let Err(error) = state
                .deps
                .protocol
                .mam_storage
                .store_message(&room_jid, &archived)
                .await
            {
                warn!(room = %room_jid, target = %request.target_id, error = %error, "Failed to archive moderation event");
            }

            // XEP-0425 §"the archiving service MAY replace the
            // retracted message with a tombstone": replace the
            // original room archive row with a moderation tombstone
            // whose `<retracted/>` carries `<moderated by/>` and the
            // optional reason.
            let original_lookup = state
                .deps
                .protocol
                .mam_storage
                .get_message(&request.target_id)
                .await;
            match original_lookup {
                Ok(Some(original)) if original.to.to_string() == room_jid.to_string() => {
                    // Use the moderation message's server-assigned
                    // archive id (XEP-0359 stanza-id stamped via
                    // `add_stanza_id_xep0359` above). That's the id
                    // clients see on the live moderation broadcast
                    // and need to correlate against the tombstone —
                    // `moderation.id` is the client message-id
                    // attribute, which would not match the archive
                    // entry clients can resolve.
                    let tombstone = waddle_xmpp::mam::ArchivedTombstone {
                        retraction_id: waddle_xmpp::mam::RichMessageId::new(archive_id.clone()),
                        stamp: stamp_time,
                        moderation: Some(ArchivedModeration {
                            target_id: waddle_xmpp::mam::RichMessageId::new(
                                request.target_id.clone(),
                            )
                            .expect("target id is non-empty here"),
                            moderated_by: moderated_by
                                .parse::<Jid>()
                                .expect("moderated_by parsed earlier"),
                            stamp: Some(stamp_time),
                            reason: request.reason.as_deref().and_then(RichText::new),
                        }),
                        // Internal tombstone-retry identity only; the
                        // MAM response builders never read this field.
                        sender_scope: original
                            .rich
                            .as_ref()
                            .and_then(|rich| rich.muc_sender.as_ref())
                            .map(|sender| sender.jid.to_bare()),
                    };
                    match state
                        .deps
                        .protocol
                        .mam_storage
                        .replace_with_tombstone(&original.id, tombstone)
                        .await
                    {
                        Ok(true) => {
                            if let Some(stanza_id) = original.stanza_id.as_ref() {
                                crate::server::routes::websocket::link_preview_refs::clear_current_message_preview_refs(
                                    state.deps.app_state.db_pool.global_actor(),
                                    &room_jid,
                                    &stanza_id.id,
                                )
                                .await;
                            }
                        }
                        Ok(false) => warn!(
                            room = %room_jid,
                            target = %request.target_id,
                            "Moderation tombstone target disappeared before replacement"
                        ),
                        Err(error) => {
                            warn!(
                                room = %room_jid,
                                target = %request.target_id,
                                error = %error,
                                "Failed to replace original with moderation tombstone"
                            );
                        }
                    }
                    // XEP-0425 §Tombstones / XEP-0198: scrub the
                    // pre-tombstone groupchat reflection from any
                    // detached resume queues so a recipient mid-resume
                    // does not replay the moderated content. Best
                    // effort — tombstone is already applied to the
                    // archive. Scope by the room JID so the matcher's
                    // stanza-id branch finds groupchat reflections
                    // that key by the room's XEP-0359 stamp, and so a
                    // colliding wire id in another conversation is not
                    // accidentally scrubbed (Codex P1, Copilot review
                    // on PR #305).
                    use waddle_xmpp::stream_management::SmSessionRegistry as _;
                    let target_id = request.target_id.as_str();
                    // XEP-0425 moderation targets the room-assigned
                    // XEP-0359 stanza-id: cached reflections are
                    // matched ONLY on `<stanza-id by=room/>`, never on
                    // the client-chosen wire id (which any occupant
                    // could mint to collide with another reflection).
                    let scrub_target = waddle_xmpp::tombstone::TombstoneTarget::Groupchat {
                        stanza_id: request.target_id.clone(),
                        room: room_jid.clone(),
                    };
                    match state
                        .deps
                        .protocol
                        .sm_session_registry
                        .scrub_unacked_for_tombstone(&scrub_target)
                        .await
                    {
                        Ok(removed) if removed > 0 => debug!(
                            room = %room_jid,
                            target = target_id,
                            removed,
                            "XEP-0425 moderation: scrubbed unacked SM queue entries"
                        ),
                        Ok(_) => {}
                        Err(error) => warn!(
                            room = %room_jid,
                            target = target_id,
                            %error,
                            "XEP-0425 moderation: scrub_unacked_for_tombstone failed; pre-scrub stanza may still replay on resume"
                        ),
                    }
                    // F2: promotion (#1097/#1098) parks unacked copies
                    // in pending_delivery — scrub that layer with the
                    // same keys or the moderated content delivers
                    // verbatim at the recipient's next login.
                    match state
                        .deps
                        .protocol
                        .pending_delivery_storage
                        .scrub_for_tombstone(&scrub_target)
                        .await
                    {
                        Ok(removed) if removed > 0 => debug!(
                            room = %room_jid,
                            target = target_id,
                            removed,
                            "XEP-0425 moderation: scrubbed pending_delivery rows"
                        ),
                        Ok(_) => {}
                        Err(error) => warn!(
                            room = %room_jid,
                            target = target_id,
                            %error,
                            "XEP-0425 moderation: pending_delivery scrub_for_tombstone failed; moderated content may still deliver at next login"
                        ),
                    }
                }
                Ok(_) => {}
                Err(error) => warn!(
                    room = %room_jid,
                    target = %request.target_id,
                    error = %error,
                    "Failed to look up moderation target for tombstone"
                ),
            }
        }

        let mut frames = Vec::new();
        if let Ok(snapshot) = room_actor.ask(GetSnapshot).await {
            for occupant in snapshot.room.occupants.values() {
                for occupant_jid in snapshot.room.get_occupant_sessions(&occupant.nick) {
                    let mut outbound = moderation.clone();
                    outbound.to = Some(jid::Jid::from(occupant_jid.clone()));
                    if occupant_jid == *sender_jid {
                        frames.push(stanza_to_xml(&Stanza::Message(outbound)));
                        continue;
                    }
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .try_send_to(&occupant_jid, Stanza::Message(outbound));
                }
            }
        }

        frames.push(build_iq_result_xml(id, response_from, response_to, None));
        return frames.into();
    }
    ResponseBatch::default()
}

#[cfg(test)]
mod destroy_completion_tests {
    use super::*;
    use crate::db::actor::{DbExecute, DbQueryOne};
    use crate::db::{row_value, Value};
    use crate::server::routes::websocket::tests::create_test_websocket_state;

    fn snapshot(attempt: waddle_xmpp::muc::DestroyAttemptId) -> DestroyCompletionSnapshot {
        DestroyCompletionSnapshot {
            attempt,
            room_jid: "destroy@muc.example.com".parse().expect("valid bare JID"),
            lifecycle: None,
            request: DestroyRequest::default(),
            members: Vec::new(),
            recipients: Vec::new(),
        }
    }

    #[test]
    fn persisted_muc_destroy_completion_requires_its_exact_attempt_identity() {
        let matching = waddle_xmpp::muc::DestroyAttemptId::generate();
        let different = waddle_xmpp::muc::DestroyAttemptId::generate();
        let snapshot = snapshot(matching);

        assert!(persisted_destroy_snapshot_matches_attempt(
            &snapshot,
            &matching.as_uuid().to_string()
        ));
        assert!(!persisted_destroy_snapshot_matches_attempt(
            &snapshot,
            &different.as_uuid().to_string()
        ));
        assert!(!persisted_destroy_snapshot_matches_attempt(
            &snapshot,
            "not-a-uuid"
        ));
    }

    fn persisted_snapshot_payload(snapshot: &DestroyCompletionSnapshot) -> String {
        serde_json::to_string(&PersistedDestroyCompletion {
            attempt: snapshot.attempt.as_uuid(),
            room_jid: snapshot.room_jid.clone(),
            reason: snapshot.request.reason.clone(),
            alternate_venue: snapshot.request.alternate_venue.clone(),
            password: snapshot.request.password.clone(),
            members: snapshot.members.clone(),
            recipients: snapshot
                .recipients
                .iter()
                .map(|(nick, sessions)| PersistedDestroyRecipient {
                    nick: nick.clone(),
                    sessions: sessions.clone(),
                })
                .collect(),
        })
        .expect("serialize persisted destroy snapshot")
    }

    #[test]
    fn persisted_destroy_completion_round_trips_typed_jids() {
        let attempt = waddle_xmpp::muc::DestroyAttemptId::generate();
        let snapshot = DestroyCompletionSnapshot {
            attempt,
            room_jid: "destroy@muc.example.com".parse().expect("valid bare jid"),
            lifecycle: None,
            request: DestroyRequest {
                reason: Some("cleanup".to_string()),
                alternate_venue: Some("next@muc.example.com".parse().expect("valid bare jid")),
                password: Some("secret".to_string()),
            },
            members: vec![
                "member-a@example.com".parse().expect("valid member jid"),
                "member-b@example.com".parse().expect("valid member jid"),
            ],
            recipients: vec![(
                "nick".to_string(),
                vec![
                    "member-a@example.com/phone"
                        .parse()
                        .expect("valid full jid"),
                    "member-b@example.com/laptop"
                        .parse()
                        .expect("valid full jid"),
                ],
            )],
        };
        let persisted: PersistedDestroyCompletion =
            serde_json::from_str(&persisted_snapshot_payload(&snapshot))
                .expect("deserialize persisted snapshot");
        let recovered =
            DestroyCompletionSnapshot::try_from(persisted).expect("recover typed snapshot");

        assert_eq!(recovered.room_jid, snapshot.room_jid);
        assert_eq!(
            recovered.request.alternate_venue,
            snapshot.request.alternate_venue
        );
        assert_eq!(recovered.members, snapshot.members);
        assert_eq!(recovered.recipients, snapshot.recipients);
    }

    async fn insert_persisted_completion(
        state: &WebSocketState,
        snapshot: &DestroyCompletionSnapshot,
        available_at_ms: i64,
    ) {
        insert_persisted_completion_with_metadata(state, snapshot, available_at_ms, None, None)
            .await;
    }

    async fn insert_persisted_completion_with_metadata(
        state: &WebSocketState,
        snapshot: &DestroyCompletionSnapshot,
        available_at_ms: i64,
        origin_instance_id: Option<&str>,
        lifecycle_id: Option<&str>,
    ) {
        state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbExecute {
                sql: "INSERT INTO clustering_muc_destroy_outbox \
                      (attempt_id, payload_json, origin_instance_id, lifecycle_id, available_at_ms, lease_token, leased_at_ms) \
                      VALUES (?, ?, ?, ?, ?, NULL, NULL)"
                    .to_string(),
                params: crate::db_params![
                    snapshot.attempt.as_uuid().to_string(),
                    persisted_snapshot_payload(snapshot),
                    origin_instance_id.map(str::to_string),
                    lifecycle_id.map(str::to_string),
                    available_at_ms,
                ],
            })
            .await
            .expect("insert persisted destroy completion");
    }

    async fn leased_at_ms(
        state: &WebSocketState,
        attempt: waddle_xmpp::muc::DestroyAttemptId,
    ) -> i64 {
        let row = state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbQueryOne {
                sql: "SELECT leased_at_ms FROM clustering_muc_destroy_outbox WHERE attempt_id = ?"
                    .to_string(),
                params: crate::db_params![attempt.as_uuid().to_string()],
            })
            .await
            .expect("load leased_at_ms")
            .expect("persisted destroy completion row");
        match row_value(&row, 0).expect("leased_at_ms value") {
            Value::Integer(value) => *value,
            _ => panic!("leased_at_ms must be an integer"),
        }
    }

    #[tokio::test]
    async fn failed_destroy_completion_reschedules_behind_older_due_work() {
        let state = create_test_websocket_state().await;
        let failed_attempt = waddle_xmpp::muc::DestroyAttemptId::generate();
        let failed_snapshot = snapshot(failed_attempt);
        let original_due_at = crate::time::now_ms() - 10_000;
        insert_persisted_completion(&state, &failed_snapshot, original_due_at).await;

        let claimed = claim_persisted_destroy_completion(&state, failed_attempt)
            .await
            .expect("claim failed retry row");
        let retry_floor = crate::time::now_ms();
        assert!(
            finalize_persisted_destroy_completion(&state, &claimed, false).await,
            "failed completion release should keep the outbox row"
        );

        let retried_row = state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbQueryOne {
                sql:
                    "SELECT available_at_ms FROM clustering_muc_destroy_outbox WHERE attempt_id = ?"
                        .to_string(),
                params: crate::db_params![failed_attempt.as_uuid().to_string()],
            })
            .await
            .expect("load retried row")
            .expect("retried row remains");
        let retried_due_at: i64 = match row_value(&retried_row, 0).expect("load retried due time") {
            Value::Integer(value) => *value,
            _ => panic!("retried due time must be an integer"),
        };
        assert!(
            retried_due_at >= retry_floor.saturating_add(DESTROY_COMPLETION_RETRY_DELAY_MS),
            "failed completion should requeue with nonzero backoff"
        );

        for _ in 0..DESTROY_COMPLETION_DRAIN_BATCH_LIMIT {
            let other_attempt = waddle_xmpp::muc::DestroyAttemptId::generate();
            insert_persisted_completion(&state, &snapshot(other_attempt), retry_floor).await;
        }

        let selected = list_due_destroy_completion_attempts(
            &state,
            retried_due_at,
            DESTROY_COMPLETION_DRAIN_BATCH_LIMIT,
        )
        .await;

        assert_eq!(
            selected.len(),
            DESTROY_COMPLETION_DRAIN_BATCH_LIMIT as usize,
            "the drain should still cap each batch"
        );
        assert!(
            !selected.contains(&failed_attempt),
            "a failed completion must yield one batch window to older due work after release"
        );
    }

    #[tokio::test]
    async fn non_clustered_restart_reclaims_foreign_inert_destroy_completion_without_lifecycle() {
        let state = create_test_websocket_state().await;
        let attempt = waddle_xmpp::muc::DestroyAttemptId::generate();
        let destroy_snapshot = snapshot(attempt);
        insert_persisted_completion_with_metadata(
            state.as_ref(),
            &destroy_snapshot,
            i64::MAX,
            Some("foreign-process"),
            None,
        )
        .await;

        reconcile_foreign_inert_destroy_completions(state.as_ref()).await;

        let exists = persisted_destroy_completion_exists(state.as_ref(), attempt)
            .await
            .expect("restart reclaim query succeeds");
        assert!(
            !exists,
            "a foreign inert destroy completion without lifecycle proof must be reclaimed in non-clustered mode"
        );
        assert!(
            claim_persisted_destroy_completion(state.as_ref(), attempt)
                .await
                .is_none(),
            "reclaimed foreign inert destroy completion must not remain claimable"
        );
    }

    #[tokio::test]
    async fn finalization_renews_the_completion_lease_until_the_db_ack_finishes() {
        let state = create_test_websocket_state().await;
        let attempt = waddle_xmpp::muc::DestroyAttemptId::generate();
        let destroy_snapshot = snapshot(attempt);
        insert_persisted_completion(&state, &destroy_snapshot, crate::time::now_ms() - 1).await;
        let claimed = claim_persisted_destroy_completion(state.as_ref(), attempt)
            .await
            .expect("claim persisted destroy completion");
        let leased_before = leased_at_ms(state.as_ref(), attempt).await;

        let finalized = finalize_persisted_destroy_completion_with_future(
            state.as_ref(),
            &claimed,
            std::time::Duration::from_millis(1),
            async {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                Ok(0)
            },
        )
        .await;

        assert!(
            !finalized,
            "the helper test future intentionally does not acknowledge the outbox row"
        );
        let leased_after = leased_at_ms(state.as_ref(), attempt).await;
        assert!(
            leased_after > leased_before,
            "lease renewal must continue while durable finalization is still outstanding"
        );
    }

    #[test]
    fn non_delivered_destroy_presence_send_is_not_inlined_as_a_local_retry_target() {
        assert!(!delivery_outcome_warrants_retry(
            &waddle_xmpp::registry::BroadcastOutcome::Delivered
        ));
        assert!(delivery_outcome_warrants_retry(
            &waddle_xmpp::registry::BroadcastOutcome::NotConnected
        ));
        assert!(delivery_outcome_warrants_retry(
            &waddle_xmpp::registry::BroadcastOutcome::DroppedClosed
        ));
        assert!(delivery_outcome_warrants_retry(
            &waddle_xmpp::registry::BroadcastOutcome::DroppedFull
        ));
    }
}
