use super::*;
use serde::{Deserialize, Serialize};
use waddle_xmpp::muc::{build_destroy_notification, DestroyRequest, RoomRegistry, NS_MUC_OWNER};

use crate::server::routes::websocket::cleanup::{
    cancel_destroy_completion_attempt, drain_destroy_completions, register_destroy_completion,
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
    room_jid: String,
    reason: Option<String>,
    alternate_venue: Option<String>,
    password: Option<String>,
    members: Vec<String>,
    recipients: Vec<PersistedDestroyRecipient>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedDestroyRecipient {
    nick: String,
    sessions: Vec<String>,
}

#[derive(Clone)]
struct DestroyCompletionSnapshot {
    attempt: waddle_xmpp::muc::DestroyAttemptId,
    room_jid: BareJid,
    request: DestroyRequest,
    members: Vec<BareJid>,
    recipients: Vec<(String, Vec<FullJid>)>,
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
            room_jid: completion.room_jid.to_string(),
            reason: completion.request.reason.clone(),
            alternate_venue: completion
                .request
                .alternate_venue
                .as_ref()
                .map(ToString::to_string),
            password: completion.request.password.clone(),
            members: completion
                .room
                .get_all_affiliations()
                .into_iter()
                .filter(|entry| entry.affiliation >= waddle_xmpp::Affiliation::Member)
                .map(|entry| entry.jid.to_string())
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
                        .map(|session| session.to_string())
                        .collect(),
                })
                .collect(),
        })
    }
}

impl TryFrom<PersistedDestroyCompletion> for DestroyCompletionSnapshot {
    type Error = ();

    fn try_from(value: PersistedDestroyCompletion) -> Result<Self, Self::Error> {
        let room_jid = value.room_jid.parse().map_err(|_| ())?;
        let alternate_venue = value
            .alternate_venue
            .map(|jid| jid.parse())
            .transpose()
            .map_err(|_| ())?;
        let members = value
            .members
            .into_iter()
            .map(|jid| jid.parse())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ())?;
        let recipients = value
            .recipients
            .into_iter()
            .map(|recipient| {
                recipient
                    .sessions
                    .into_iter()
                    .map(|jid| jid.parse())
                    .collect::<Result<Vec<_>, _>>()
                    .map(|sessions| (recipient.nick, sessions))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ())?;
        Ok(Self {
            attempt: waddle_xmpp::muc::DestroyAttemptId::from_uuid(value.attempt),
            room_jid,
            request: DestroyRequest {
                reason: value.reason,
                alternate_venue,
                password: value.password,
            },
            members,
            recipients,
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
    members: &[BareJid],
) -> Result<(), ()> {
    let db_actor = state.deps.app_state.db_pool.global_actor().clone();
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
    Ok(())
}

/// Execute the server-owned half of a committed owner-IQ destroy. The
/// registry retains this typed completion across lost replies, then hands it
/// back to this layer once the durable destroy has committed.
pub(crate) async fn complete_destroy_post_commit(
    state: &WebSocketState,
    completion: waddle_xmpp::muc::room_registry_actor::DestroyCompletion,
    inline_session: Option<&FullJid>,
) -> Result<Vec<String>, ()> {
    let persisted = PersistedDestroyCompletion::try_from(&completion)?;
    let snapshot = DestroyCompletionSnapshot::try_from(persisted)?;
    complete_destroy_snapshot(state, snapshot, inline_session).await
}

async fn complete_destroy_snapshot(
    state: &WebSocketState,
    snapshot: DestroyCompletionSnapshot,
    inline_session: Option<&FullJid>,
) -> Result<Vec<String>, ()> {
    let room_jid = snapshot.room_jid;
    if wipe_destroyed_room_durable_state(state, &room_jid, &snapshot.members)
        .await
        .is_err()
    {
        warn!(
            room = %room_jid,
            "Destroy committed but the app-level durable wipe (catalog row / \
             group-DM tuples / boundaries / invite ledger) did not fully \
             stick; leftover rows are idempotently re-cleanable and cannot \
             resurrect the room's ban/affiliation state"
        );
        return Err(());
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
                is_inline_session,
                &identity,
            );
            if is_inline_session {
                frames.push(stanza_to_xml(&Stanza::Presence(presence)));
            } else {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&session_jid, Stanza::Presence(presence));
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

const DESTROY_COMPLETION_LEASE_TIMEOUT_MS: i64 = 30_000;

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
                  (attempt_id, payload_json, available_at_ms, lease_token, leased_at_ms) \
                  VALUES (?, ?, ?, NULL, NULL) ON CONFLICT (attempt_id) DO NOTHING"
                .to_string(),
            params: crate::db_params![
                completion.attempt.as_uuid().to_string(),
                payload,
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
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbExecute {
            sql: "UPDATE clustering_muc_destroy_outbox SET available_at_ms = ? \
                  WHERE attempt_id = ?"
                .to_string(),
            params: crate::db_params![crate::time::now_ms(), attempt.as_uuid().to_string()],
        })
        .await
    {
        Ok(_) => true,
        Err(error) => {
            warn!(attempt = %attempt.as_uuid(), %error, "Failed to arm persisted MUC destroy cleanup");
            false
        }
    }
}

pub(crate) async fn acknowledge_persisted_destroy_completion(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
) -> bool {
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbExecute {
            sql: "DELETE FROM clustering_muc_destroy_outbox WHERE attempt_id = ?".to_string(),
            params: crate::db_params![attempt.as_uuid().to_string()],
        })
        .await
    {
        Ok(_) => true,
        Err(error) => {
            warn!(attempt = %attempt.as_uuid(), %error, "Failed to acknowledge persisted MUC destroy completion");
            false
        }
    }
}

pub(crate) async fn discard_persisted_destroy_completion(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
) {
    let _ = acknowledge_persisted_destroy_completion(state, attempt).await;
}

/// Redrive persisted completion work after a process exit or an app-storage
/// failure. The atomic lease prevents two cluster nodes from emitting the
/// same XEP-0045 presence while they race the same idempotent cleanup.
pub(crate) async fn drain_persisted_destroy_completions(state: &WebSocketState) {
    use crate::db::{row_value, ValueExt};

    let now = crate::time::now_ms();
    let rows = match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbQuery {
            sql: "SELECT attempt_id, payload_json FROM clustering_muc_destroy_outbox \
                  WHERE available_at_ms <= ? AND \
                  (lease_token IS NULL OR leased_at_ms < ?) LIMIT 32"
                .to_string(),
            params: crate::db_params![now, now - DESTROY_COMPLETION_LEASE_TIMEOUT_MS],
        })
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(%error, "Failed to list persisted MUC destroy completions");
            return;
        }
    };
    for row in rows {
        let Ok(attempt_id) = row_value(&row, 0).and_then(ValueExt::as_string) else {
            warn!("Invalid persisted MUC destroy attempt id");
            continue;
        };
        let Ok(payload) = row_value(&row, 1).and_then(ValueExt::as_string) else {
            warn!(attempt = %attempt_id, "Invalid persisted MUC destroy payload");
            continue;
        };
        let lease = uuid::Uuid::new_v4().to_string();
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
                    lease.clone(),
                    now,
                    attempt_id.clone(),
                    now,
                    now - DESTROY_COMPLETION_LEASE_TIMEOUT_MS,
                ],
            })
            .await
        {
            Ok(count) => count == 1,
            Err(error) => {
                warn!(attempt = %attempt_id, %error, "Failed to claim persisted MUC destroy completion");
                false
            }
        };
        if !claimed {
            continue;
        }
        let result = serde_json::from_str::<PersistedDestroyCompletion>(&payload)
            .map_err(|_| ())
            .and_then(DestroyCompletionSnapshot::try_from);
        let completed = match result {
            Ok(snapshot) if persisted_destroy_snapshot_matches_attempt(&snapshot, &attempt_id) => {
                complete_destroy_snapshot(state, snapshot, None)
                    .await
                    .is_ok()
            }
            Ok(_) => {
                warn!(attempt = %attempt_id, "Persisted MUC destroy completion payload does not match its attempt id");
                false
            }
            Err(()) => false,
        };
        let (sql, params) = if completed {
            (
                "DELETE FROM clustering_muc_destroy_outbox WHERE attempt_id = ? AND lease_token = ?",
                crate::db_params![attempt_id.clone(), lease],
            )
        } else {
            (
                "UPDATE clustering_muc_destroy_outbox SET lease_token = NULL, leased_at_ms = NULL \
                 WHERE attempt_id = ? AND lease_token = ?",
                crate::db_params![attempt_id.clone(), lease],
            )
        };
        if let Err(error) = state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(crate::db::actor::DbExecute {
                sql: sql.to_string(),
                params,
            })
            .await
        {
            warn!(attempt = %attempt_id, %error, "Failed to finalize persisted MUC destroy completion");
        }
    }
}

pub(super) async fn handle_muc_owner_and_moderation_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
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
            )];
        };

        let room_target = target.split('/').next().unwrap_or(target);
        let Ok(room_jid) = room_target.parse::<BareJid>() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                jid_malformed_iq_error("Malformed JID in IQ addressing."),
            )];
        };

        if !is_muc_room_jid(state, &room_jid).await {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                item_not_found_iq_error("Requested item not found."),
            )];
        }

        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )];
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
                )];
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
                )];
            }
        }

        if has_destroy {
            // XEP-0045 §10.9: parse the optional alternate venue and
            // reason out of the `<destroy/>` payload, snapshot the
            // current occupants from the room actor, then broadcast a
            // typed destroy presence to each session before tearing
            // the actor down. The sender's frame is returned inline
            // alongside the IQ result so it lands on the same socket;
            // others are routed via the connection registry.
            let destroy_request = parse_destroy_request(iq).unwrap_or_default();
            let room_actor = match get_room_actor_result(state, &room_jid).await {
                Ok(Some(room_actor)) => room_actor,
                Ok(None) => {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        item_not_found_iq_error("Requested item not found."),
                    )];
                }
                Err(error) => {
                    warn!(room = %room_jid, %error, "MUC destroy room lookup failed");
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                }
            };
            let Ok(snapshot) = room_actor.ask(GetSnapshot).await else {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            };
            let attempt = waddle_xmpp::muc::DestroyAttemptId::generate();
            let completion = waddle_xmpp::muc::room_registry_actor::DestroyCompletion {
                attempt,
                room_jid: room_jid.clone(),
                room: snapshot.room,
                request: destroy_request,
            };
            if persist_destroy_completion(state, &completion)
                .await
                .is_err()
            {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
            if let Err(error) = register_destroy_completion(state, completion.clone()).await {
                warn!(room = %room_jid, %error, "Failed to retain MUC destroy completion");
                cancel_destroy_completion_attempt(state, attempt).await;
                discard_persisted_destroy_completion(state, attempt).await;
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
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
                    arm_destroy_completion_recovery(state, attempt).await;
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
                    )];
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
                    )];
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
                    )];
                }
                // A transport-level ask failure: the atomic handler either
                // fully applied or not at all, so there is no split state.
                // Answer retryable, never `item-not-found` (#1276 P1-B).
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        error = %error,
                        "Destroy ask failed; returning a retryable error"
                    );
                    // The actor may have committed before the reply was
                    // lost, so retain the inert row. Reconciliation can arm
                    // it only once it proves the matching destroy committed.
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                }
            }
            let mut frames = drain_destroy_completions(state, Some(sender_jid)).await;
            let room_jid_string = room_jid.to_string();
            frames.push(build_iq_result_xml(
                id,
                Some(room_jid_string.as_str()),
                response_to,
                None,
            ));
            return frames;
        }

        if matches!(iq, xmpp_parsers::iq::Iq::Get { .. }) {
            match build_muc_owner_config_response(state, &room_jid, id, response_to).await {
                Ok(response) => return vec![response],
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
                    )];
                }
            }
        }

        if let Err(error) =
            apply_muc_owner_config(state, &room_jid, iq, authenticated_session.as_ref()).await
        {
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
            )];
        }

        // Treat all other owner IQ sets as successful config submit for instant rooms.
        let room_jid_string = room_jid.to_string();
        return vec![build_iq_result_xml(
            id,
            Some(room_jid_string.as_str()),
            response_to,
            None,
        )];
    }

    if let Some(request) = parse_moderation_iq(iq) {
        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )];
        };
        let Some(room_jid) = iq.to().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };
        let room_actor = match get_room_actor_result(state, &room_jid).await {
            Ok(Some(room_actor)) => room_actor,
            Ok(None) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            Err(error) => {
                warn!(room = %room_jid, %error, "MUC moderation room lookup failed");
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
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
                )];
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
            )];
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
                )];
            }
            Ok(None) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            Err(error) => {
                warn!(room = %room_jid, target = %request.target_id, error = %error, "Failed to look up moderation target");
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
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
        return frames;
    }
    Vec::new()
}

#[cfg(test)]
mod destroy_completion_tests {
    use super::*;

    fn snapshot(attempt: waddle_xmpp::muc::DestroyAttemptId) -> DestroyCompletionSnapshot {
        DestroyCompletionSnapshot {
            attempt,
            room_jid: "destroy@muc.example.com".parse().expect("valid bare JID"),
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
}
