//! XEP-0045 mediated room invitations and declines.

use std::fmt;

use jid::{BareJid, Jid};
use minidom::Element;
use uuid::Uuid;
use xmpp_parsers::message::{Id, Message};
use xmpp_parsers::muc::user::{Continue, Decline, Invite, MucUser, Reason};

use crate::messaging::namespaces::{NS_CLIENT, NS_MUC_USER};

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod native;
#[cfg(test)]
mod tests;

/// A room-mediated invitation or reply received by this resource.
#[derive(Debug, Clone, PartialEq)]
pub enum MucInvitationEvent {
    /// The room relayed an invitation to this resource.
    Invited(MucInvitation),
    /// The room relayed a decline to the inviter.
    Declined(MucInvitationDecline),
}

/// A mediated invitation received from a room.
#[derive(Debug, Clone, PartialEq)]
pub struct MucInvitation {
    /// Bare JID of the room to join.
    pub room: BareJid,
    /// Bare, full, or occupant JID supplied by the room.
    pub inviter: Jid,
    /// Optional human-readable invitation text.
    pub reason: Option<Reason>,
    /// Password supplied by a password-protected room, when present.
    pub password: Option<MucInvitationPassword>,
    /// An optional request to continue a private conversation in the room.
    pub continuation: Option<Continue>,
}

/// A room password sent with a mediated invitation.
#[derive(Clone, PartialEq, Eq)]
pub struct MucInvitationPassword(String);

impl MucInvitationPassword {
    /// Borrow the password for joining the room.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MucInvitationPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MucInvitationPassword([redacted])")
    }
}

/// A mediated decline received from a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MucInvitationDecline {
    /// Bare JID of the room that sent the invitation.
    pub room: BareJid,
    /// JID of the invitee who declined.
    pub invitee: Jid,
    /// Optional human-readable decline text.
    pub reason: Option<Reason>,
}

/// Build a normal message asking the room to relay an invitation.
#[must_use]
pub fn build_muc_invite_message(room: &BareJid, invitee: &Jid, reason: Option<Reason>) -> Message {
    invitation_message(
        room,
        MucUser {
            invite: Some(Invite {
                from: None,
                to: Some(invitee.clone()),
                reason: reason.map(|reason| reason.0),
            }),
            ..MucUser::default()
        },
    )
}

/// Build a normal message asking the room to relay a decline to the inviter.
#[must_use]
pub fn build_muc_decline_message(room: &BareJid, inviter: &Jid, reason: Option<Reason>) -> Message {
    invitation_message(
        room,
        MucUser {
            decline: Some(Decline {
                from: None,
                to: Some(inviter.clone()),
                reason: reason.map(|reason| reason.0),
            }),
            ..MucUser::default()
        },
    )
}

fn invitation_message(room: &BareJid, payload: MucUser) -> Message {
    let mut message = Message::normal(Jid::from(room.clone()));
    message.id = Some(Id(Uuid::new_v4().to_string()));
    message.payloads.push(payload.into());
    message
}

/// Parse relayed invitations and declines from a normal message, in wire order.
///
/// A room relays both from its own bare JID (XEP-0045 §7.8.2). A message from
/// an occupant JID is a private message and carries no invitation, whatever
/// its payload says.
#[must_use]
pub fn parse_muc_invitation_events(message: &Element) -> Vec<MucInvitationEvent> {
    if !message.is("message", NS_CLIENT) || !matches!(message.attr("type"), None | Some("normal")) {
        return Vec::new();
    }
    let Some(payload) = message.get_child("x", NS_MUC_USER) else {
        return Vec::new();
    };
    let Some(sender) = message
        .attr("from")
        .and_then(|from| from.parse::<Jid>().ok())
    else {
        return Vec::new();
    };
    if !sender.is_bare() {
        return Vec::new();
    }
    let room = sender.into_bare();
    payload
        .children()
        .filter_map(|child| {
            let invite = child.is("invite", NS_MUC_USER);
            if !invite && !child.is("decline", NS_MUC_USER) {
                return None;
            }
            let peer = child.attr("from")?.parse().ok()?;
            let reason = child
                .get_child("reason", NS_MUC_USER)
                .map(|reason| Reason(reason.text()));
            Some(if invite {
                MucInvitationEvent::Invited(MucInvitation {
                    room: room.clone(),
                    inviter: peer,
                    reason,
                    password: payload
                        .get_child("password", NS_MUC_USER)
                        .map(|password| MucInvitationPassword(password.text())),
                    continuation: child
                        .get_child("continue", NS_MUC_USER)
                        .map(|continuation| Continue {
                            thread: continuation.attr("thread").map(str::to_owned),
                        }),
                })
            } else {
                MucInvitationEvent::Declined(MucInvitationDecline {
                    room: room.clone(),
                    invitee: peer,
                    reason,
                })
            })
        })
        .collect()
}
