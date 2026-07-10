//! Outstanding mediated-invite ledger (XEP-0045 §7.8.2, #1264).
//!
//! Every server-relayed mediated invitation records a `(room, invitee)
//! → inviter` row. A later `<decline/>` is only honoured when a
//! matching row exists — without the ledger, any authenticated user
//! could make the room deliver a "declined your invitation" message to
//! an arbitrary occupant. The row also names the inviter the decline
//! must reach, so declines route durably (offline inviters get a
//! pending-delivery row) instead of only to currently-connected
//! occupants.
//!
//! Rows are consumed by a decline and wiped wholesale when the room is
//! destroyed (#1261). A row for an invitee who simply joins stays
//! behind harmlessly: the invitee was genuinely invited, so a later
//! decline from them is authentic (if unusual), and the room-destroy
//! wipe bounds the table's lifetime.

use jid::BareJid;

use crate::db::actor::{DbActor, DbExecute, DbQueryOne};
use crate::db::{row_value, ValueExt};
use kameo::actor::ActorRef;

/// One outstanding mediated invitation: `inviter` invited `invitee`
/// to `room` and the room relayed the invite (§7.8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutstandingInvite {
    pub room: BareJid,
    pub invitee: BareJid,
    pub inviter: BareJid,
}

/// Record (or refresh) the outstanding invite for `(room, invitee)`.
/// A re-invite by a different inviter replaces the previous row — the
/// most recent mediated invite is the one a decline answers.
pub(crate) async fn record_invite(
    actor: ActorRef<DbActor>,
    invite: &OutstandingInvite,
) -> Result<(), String> {
    actor
        .ask(DbExecute {
            sql: "INSERT INTO muc_pending_invites (room_jid, invitee_jid, inviter_jid, \
                  created_at) VALUES (?, ?, ?, ?) ON CONFLICT(room_jid, invitee_jid) DO UPDATE \
                  SET inviter_jid = excluded.inviter_jid, created_at = excluded.created_at"
                .to_string(),
            params: vec![
                invite.room.to_string().into(),
                invite.invitee.to_string().into(),
                invite.inviter.to_string().into(),
                chrono::Utc::now().to_rfc3339().into(),
            ],
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Look up the outstanding invite for `(room, invitee)` without
/// consuming it. Returns `None` when no invite is outstanding — the
/// caller MUST NOT forward a decline in that case (#1264 spoofing
/// hardening). Consumption is a separate step ([`consume_invite`]) so
/// a decline whose delivery could not even be queued leaves the ledger
/// row intact for a retry.
pub(crate) async fn find_invite(
    actor: ActorRef<DbActor>,
    room: &BareJid,
    invitee: &BareJid,
) -> Result<Option<OutstandingInvite>, String> {
    let row = actor
        .ask(DbQueryOne {
            sql: "SELECT inviter_jid FROM muc_pending_invites WHERE room_jid = ? AND \
                  invitee_jid = ?"
                .to_string(),
            params: vec![room.to_string().into(), invitee.to_string().into()],
        })
        .await
        .map_err(|error| error.to_string())?;
    let Some(row) = row else {
        return Ok(None);
    };
    let inviter = row_value(&row, 0)
        .map_err(|error| error.to_string())?
        .as_string()
        .map_err(|error| error.to_string())?
        .parse::<BareJid>()
        .map_err(|error| format!("stored inviter JID is unparseable: {error}"))?;
    Ok(Some(OutstandingInvite {
        room: room.clone(),
        invitee: invitee.clone(),
        inviter,
    }))
}

/// Consume the outstanding invite for `(room, invitee)` after its
/// decline has been delivered or durably queued.
pub(crate) async fn consume_invite(
    actor: ActorRef<DbActor>,
    room: &BareJid,
    invitee: &BareJid,
) -> Result<(), String> {
    actor
        .ask(DbExecute {
            sql: "DELETE FROM muc_pending_invites WHERE room_jid = ? AND invitee_jid = ?"
                .to_string(),
            params: vec![room.to_string().into(), invitee.to_string().into()],
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Wipe every outstanding invite for `room` — the room-destroy path
/// (#1261): a destroyed room has nothing left to decline.
pub(crate) async fn delete_room_invites(
    actor: ActorRef<DbActor>,
    room: &BareJid,
) -> Result<(), String> {
    actor
        .ask(DbExecute {
            sql: "DELETE FROM muc_pending_invites WHERE room_jid = ?".to_string(),
            params: vec![room.to_string().into()],
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}
