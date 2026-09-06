//! Outstanding mediated-invite ledger (XEP-0045 §7.8.2, #1264).
//!
//! Every server-relayed mediated invitation records a
//! `(room, invitee, inviter)` row. A later `<decline/>` is only
//! honoured when a matching row exists — without the ledger, any
//! authenticated user could make the room deliver a "declined your
//! invitation" message to an arbitrary user. The row also names the
//! inviter the decline must reach, so declines route durably (offline
//! inviters get a pending-delivery row) instead of only to
//! currently-connected occupants.
//!
//! Rows are keyed per inviter: two occupants may each have a live
//! invitation out to the same person, and a decline answers exactly
//! one of them (selected via the decline's `to` attribute). This also
//! prevents a second inviter from silently rerouting an earlier
//! invitation's decline to themselves.
//!
//! Rows are claimed atomically by a decline (a keyed `DELETE` whose
//! affected-row count is the claim, so concurrent declines from two
//! devices forward exactly one), expire after [`INVITE_TTL`], and are
//! wiped wholesale when the room is destroyed (#1261). A row for an
//! invitee who simply joins stays behind until it expires: the invitee
//! was genuinely invited, so a late decline from them is authentic
//! (if unusual).

use jid::BareJid;

use crate::db::actor::{DbActor, DbExecute, DbQuery};
use crate::db::{row_value, DatabaseError, ValueExt};
use kameo::actor::ActorRef;

/// Typed failures at the invitation ledger storage boundary.
#[derive(Debug, thiserror::Error)]
pub enum InviteStorageError {
    #[error("invitation ledger actor unavailable")]
    ActorUnavailable,
    #[error("invitation ledger database operation failed: {0}")]
    Database(#[from] DatabaseError),
    #[error("stored invitation has an invalid inviter JID: {0}")]
    InvalidInviter(#[from] jid::Error),
}

fn actor_error<M>(error: kameo::error::SendError<M, DatabaseError>) -> InviteStorageError {
    match error {
        kameo::error::SendError::HandlerError(error) => InviteStorageError::Database(error),
        _ => InviteStorageError::ActorUnavailable,
    }
}

/// How long a mediated invitation stays declinable. Bounds both ledger
/// growth for never-answered invites and the window in which a stale
/// "declined your invitation" can reach an inviter.
const INVITE_TTL: chrono::Duration = chrono::Duration::days(30);

fn expiry_cutoff() -> String {
    (chrono::Utc::now() - INVITE_TTL).to_rfc3339()
}

/// One outstanding mediated invitation: `inviter` invited `invitee`
/// to `room` and the room relayed the invite (§7.8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutstandingInvite {
    pub room: BareJid,
    pub invitee: BareJid,
    pub inviter: BareJid,
}

/// Outcome of [`record_invite`]: whether this `(room, invitee,
/// inviter)` invitation is new or was already outstanding. Callers use
/// `AlreadyOutstanding` as the anti-spam dedup signal — an identical
/// re-invite is answered with silent success instead of another
/// delivery (and, for offline invitees, another pending-delivery row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    New {
        created_at: chrono::DateTime<chrono::Utc>,
    },
    AlreadyOutstanding,
}

/// Record the outstanding invite for `(room, invitee, inviter)`.
/// An identical unexpired invitation reports `AlreadyOutstanding`
/// (and keeps its original timestamp); an expired one is refreshed
/// and reported as `New`.
///
/// The dedup decision is a SINGLE conditional-upsert statement whose
/// affected-row count is the answer, so two concurrent identical
/// invites racing through the serialized [`DbActor`] resolve to
/// exactly one `New` — there is no check-then-insert window.
pub(crate) async fn record_invite(
    actor: ActorRef<DbActor>,
    invite: &OutstandingInvite,
) -> Result<RecordOutcome, InviteStorageError> {
    record_invite_at(actor, invite, chrono::Utc::now()).await
}

/// Record using the timestamp frozen by ingress planning.
pub(crate) async fn record_invite_at(
    actor: ActorRef<DbActor>,
    invite: &OutstandingInvite,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<RecordOutcome, InviteStorageError> {
    // Opportunistic hygiene: expired rows for this (room, invitee) are
    // dead weight the reads already ignore — drop them here so the
    // ledger stays bounded without a dedicated janitor.
    actor
        .ask(DbExecute {
            sql: "DELETE FROM muc_pending_invites WHERE room_jid = ? AND invitee_jid = ? AND \
                  created_at <= ?"
                .to_string(),
            params: vec![
                invite.room.to_string().into(),
                invite.invitee.to_string().into(),
                expiry_cutoff().into(),
            ],
        })
        .await
        .map_err(actor_error)?;
    let affected = actor
        .ask(DbExecute {
            sql: "INSERT INTO muc_pending_invites (room_jid, invitee_jid, inviter_jid, \
                  created_at) VALUES (?, ?, ?, ?) ON CONFLICT(room_jid, invitee_jid, \
                  inviter_jid) DO UPDATE SET created_at = excluded.created_at WHERE \
                  muc_pending_invites.created_at <= ?"
                .to_string(),
            params: vec![
                invite.room.to_string().into(),
                invite.invitee.to_string().into(),
                invite.inviter.to_string().into(),
                created_at.to_rfc3339().into(),
                expiry_cutoff().into(),
            ],
        })
        .await
        .map_err(actor_error)?;
    if affected > 0 {
        Ok(RecordOutcome::New { created_at })
    } else {
        Ok(RecordOutcome::AlreadyOutstanding)
    }
}

/// List every unexpired outstanding invite for `(room, invitee)`.
/// Empty means the caller MUST NOT forward a decline (#1264 spoofing
/// hardening).
pub(crate) async fn list_invites(
    actor: ActorRef<DbActor>,
    room: &BareJid,
    invitee: &BareJid,
) -> Result<Vec<OutstandingInvite>, InviteStorageError> {
    let rows = actor
        .ask(DbQuery {
            sql: "SELECT inviter_jid FROM muc_pending_invites WHERE room_jid = ? AND \
                  invitee_jid = ? AND created_at > ? ORDER BY inviter_jid"
                .to_string(),
            params: vec![
                room.to_string().into(),
                invitee.to_string().into(),
                expiry_cutoff().into(),
            ],
        })
        .await
        .map_err(actor_error)?;
    let mut invites = Vec::new();
    for row in rows {
        let inviter = row_value(&row, 0)?.as_string()?.parse::<BareJid>()?;
        invites.push(OutstandingInvite {
            room: room.clone(),
            invitee: invitee.clone(),
            inviter,
        });
    }
    Ok(invites)
}

/// Atomically claim (delete) one outstanding invite. Returns `false`
/// when the row was already gone — a concurrent decline from another
/// device claimed it first, so the caller must not forward a second
/// decline for it.
pub(crate) async fn claim_invite(
    actor: ActorRef<DbActor>,
    invite: &OutstandingInvite,
) -> Result<bool, InviteStorageError> {
    let affected = actor
        .ask(DbExecute {
            sql: "DELETE FROM muc_pending_invites WHERE room_jid = ? AND invitee_jid = ? AND \
                  inviter_jid = ?"
                .to_string(),
            params: vec![
                invite.room.to_string().into(),
                invite.invitee.to_string().into(),
                invite.inviter.to_string().into(),
            ],
        })
        .await
        .map_err(actor_error)?;
    Ok(affected > 0)
}

/// Wipe every outstanding invite for `room` — the room-destroy path
/// (#1261): a destroyed room has nothing left to decline.
pub(crate) async fn delete_room_invites(
    actor: ActorRef<DbActor>,
    room: &BareJid,
) -> Result<(), InviteStorageError> {
    actor
        .ask(DbExecute {
            sql: "DELETE FROM muc_pending_invites WHERE room_jid = ?".to_string(),
            params: vec![room.to_string().into()],
        })
        .await
        .map_err(actor_error)?;
    Ok(())
}

#[cfg(test)]
mod tests;
