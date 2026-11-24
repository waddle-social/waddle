//! What a user may do to a message: one list decides the message menu, the
//! hover toolbar and the pin rules. Nothing here touches a widget.

use waddle_client_session::{Delivery, ItemKey, TimelineItem};

/// One tap away in the message menu and the hover toolbar.
pub const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "🎉", "😮", "😢"];
/// How many quick reactions the narrower hover toolbar keeps.
pub const TOOLBAR_REACTIONS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageAction {
    AddReaction,
    Reply,
    OpenThread,
    Subthread,
    Pin,
    Unpin,
    Copy,
    Edit,
    Delete,
    /// A moderator removes someone else's message from a room (XEP-0425).
    Remove,
}

impl MessageAction {
    pub fn presentation(self) -> (&'static str, &'static str) {
        match self {
            Self::AddReaction => ("face-smile-symbolic", "Add Reaction…"),
            Self::Reply => ("mail-reply-sender-symbolic", "Reply"),
            Self::OpenThread => ("chat-bubbles-text-symbolic", "Open Thread"),
            Self::Subthread => ("chat-bubbles-text-symbolic", "Start Subthread"),
            Self::Pin => ("view-pin-symbolic", "Pin"),
            Self::Unpin => ("view-pin-symbolic", "Unpin"),
            Self::Copy => ("edit-copy-symbolic", "Copy Text"),
            Self::Edit => ("document-edit-symbolic", "Edit"),
            Self::Delete => ("user-trash-symbolic", "Delete"),
            Self::Remove => ("action-unavailable-symbolic", "Remove Message"),
        }
    }

    /// Whether the hover toolbar shows it; the rest are in the menu.
    pub fn in_toolbar(self) -> bool {
        matches!(
            self,
            Self::AddReaction | Self::Reply | Self::OpenThread | Self::Subthread
        )
    }

    pub fn destructive(self) -> bool {
        matches!(self, Self::Delete | Self::Remove)
    }
}

/// What the open conversation and account allow, apart from the message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Context {
    /// The view is a thread: its action starts a subthread.
    pub in_thread: bool,
    /// The account may pin here (see [`may_pin`]).
    pub can_pin: bool,
    /// The conversation is a room where the account is a moderator.
    pub can_moderate: bool,
}

/// The id a pin names: the archive's stanza id, which a message has once
/// the server has stored it.
pub fn pin_target(item: &TimelineItem) -> Option<&str> {
    match &item.key {
        ItemKey::Stanza(id) => Some(id),
        ItemKey::Origin(_) => None,
    }
}

/// The actions for `item`, in menu order. `pinned` is whether it is in the
/// conversation's pinned list now.
pub fn actions(item: &TimelineItem, context: Context, pinned: bool) -> Vec<MessageAction> {
    if item.retracted {
        return Vec::new();
    }
    let referenced = item.reference_id.is_some();
    let mut list = Vec::new();
    if referenced {
        list.push(MessageAction::AddReaction);
        list.push(MessageAction::Reply);
        list.push(if context.in_thread {
            MessageAction::Subthread
        } else {
            MessageAction::OpenThread
        });
    }
    if context.can_pin && pin_target(item).is_some() {
        list.push(if pinned {
            MessageAction::Unpin
        } else {
            MessageAction::Pin
        });
    }
    list.push(MessageAction::Copy);
    if item.own && item.correction_id.is_some() && item.delivery != Delivery::Failed {
        list.push(MessageAction::Edit);
    }
    if item.own && referenced {
        list.push(MessageAction::Delete);
    } else if context.can_moderate && !item.own && referenced {
        list.push(MessageAction::Remove);
    }
    list
}

/// The room's `urn:waddle:roomconfig:pinpermission` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinPermission {
    Anyone,
    AdminsOnly,
}

/// The account's standing in a room, from its own presence (XEP-0045).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Standing {
    pub owner_or_admin: bool,
    pub moderator: bool,
}

/// Whether the account may pin in a conversation. A room that does not
/// say follows the server default, admins only; a DM lets either person pin.
pub fn may_pin(room: bool, permission: Option<PinPermission>, standing: Standing) -> bool {
    if !room {
        return true;
    }
    match permission.unwrap_or(PinPermission::AdminsOnly) {
        PinPermission::Anyone => true,
        PinPermission::AdminsOnly => standing.owner_or_admin,
    }
}

/// Whether the account may remove other people's messages in a room.
pub fn may_moderate(room: bool, standing: Standing) -> bool {
    room && (standing.moderator || standing.owner_or_admin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_client_session::{IncomingMessage, Timeline};

    fn item(own: bool, stanza: Option<&str>) -> TimelineItem {
        let mut timeline = Timeline::new();
        timeline.apply_live(IncomingMessage {
            stanza_id: stanza.map(str::to_string),
            reference_id: Some(stanza.unwrap_or("origin").to_string()),
            origin_id: Some("origin".into()),
            message_id: Some("wire".into()),
            sender: "ann".into(),
            own,
            body: Some("hello".into()),
            timestamp_ms: 1,
            ..IncomingMessage::default()
        });
        timeline.items()[0].clone()
    }

    #[test]
    fn a_stored_message_offers_the_social_actions() {
        let list = actions(&item(false, Some("s1")), Context::default(), false);
        assert_eq!(
            list,
            [
                MessageAction::AddReaction,
                MessageAction::Reply,
                MessageAction::OpenThread,
                MessageAction::Copy
            ]
        );
    }

    #[test]
    fn own_messages_add_edit_and_delete() {
        let list = actions(&item(true, Some("s1")), Context::default(), false);
        assert!(list.contains(&MessageAction::Edit));
        assert!(list.contains(&MessageAction::Delete));
        assert!(!list.contains(&MessageAction::Remove));
    }

    #[test]
    fn a_failed_send_cannot_be_edited() {
        let mut failed = item(true, Some("s1"));
        failed.delivery = Delivery::Failed;
        assert!(!actions(&failed, Context::default(), false).contains(&MessageAction::Edit));
    }

    #[test]
    fn pinning_needs_permission_and_a_stored_message() {
        let allowed = Context {
            can_pin: true,
            ..Context::default()
        };
        let stored = item(false, Some("s1"));
        assert!(actions(&stored, allowed, false).contains(&MessageAction::Pin));
        assert!(actions(&stored, allowed, true).contains(&MessageAction::Unpin));
        assert!(!actions(&stored, allowed, true).contains(&MessageAction::Pin));
        // Not yet archived: only the sender's origin id is known.
        assert!(!actions(&item(true, None), allowed, false).contains(&MessageAction::Pin));
        assert!(!actions(&stored, Context::default(), false).contains(&MessageAction::Pin));
    }

    #[test]
    fn moderators_remove_only_other_peoples_messages() {
        let moderator = Context {
            can_moderate: true,
            ..Context::default()
        };
        assert!(
            actions(&item(false, Some("s1")), moderator, false).contains(&MessageAction::Remove)
        );
        let own = actions(&item(true, Some("s1")), moderator, false);
        assert!(!own.contains(&MessageAction::Remove));
        assert!(own.contains(&MessageAction::Delete));
    }

    #[test]
    fn threads_start_subthreads() {
        let thread = Context {
            in_thread: true,
            ..Context::default()
        };
        let list = actions(&item(false, Some("s1")), thread, false);
        assert!(list.contains(&MessageAction::Subthread));
        assert!(!list.contains(&MessageAction::OpenThread));
    }

    #[test]
    fn deleted_messages_offer_nothing() {
        let mut gone = item(true, Some("s1"));
        gone.retracted = true;
        assert!(actions(&gone, Context::default(), false).is_empty());
    }

    #[test]
    fn the_toolbar_is_a_subset_of_the_menu() {
        let list = actions(
            &item(true, Some("s1")),
            Context {
                can_pin: true,
                ..Context::default()
            },
            false,
        );
        let toolbar: Vec<_> = list.iter().filter(|a| a.in_toolbar()).collect();
        assert_eq!(toolbar.len(), 3);
        assert!(!toolbar.contains(&&MessageAction::Delete));
    }

    #[test]
    fn room_pin_policy_defaults_to_admins() {
        let admin = Standing {
            owner_or_admin: true,
            moderator: true,
        };
        let member = Standing::default();
        assert!(may_pin(true, None, admin));
        assert!(!may_pin(true, None, member));
        assert!(!may_pin(true, Some(PinPermission::AdminsOnly), member));
        assert!(may_pin(true, Some(PinPermission::Anyone), member));
        assert!(may_pin(false, None, member));
    }

    #[test]
    fn moderation_is_for_room_moderators_and_admins() {
        let moderator = Standing {
            moderator: true,
            owner_or_admin: false,
        };
        let admin = Standing {
            moderator: false,
            owner_or_admin: true,
        };
        assert!(may_moderate(true, moderator));
        assert!(may_moderate(true, admin));
        assert!(!may_moderate(true, Standing::default()));
        assert!(!may_moderate(false, moderator));
    }
}
