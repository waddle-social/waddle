//! Core read/modify/write flows for XEP-0492 notification settings.
//!
//! Binding crates deserialize their own boundary options and call these
//! helpers; the XMPP fetch/merge/publish/retract semantics live here.

use crate::error::ClientError;
use crate::pep::NS_BOOKMARKS;
use crate::pep::NS_PUBSUB;
use jid::BareJid;
use minidom::Element;
use uuid::Uuid;

use crate::error::{
    ClientResult, STANZA_CONDITION_ITEM_NOT_FOUND, STANZA_CONDITION_PRECONDITION_NOT_MET,
};
use crate::push::CommandDriver;
use crate::xep::xep0402::{
    build_fetch_bookmarks_iq, build_publish_bookmark_iq, parse_bookmarks_response, BookmarkItem,
};
use crate::xep::xep0492::{
    build_fetch_dm_bookmarks_iq, build_publish_dm_bookmark_iq, build_retract_dm_bookmark_iq,
    dm_notify_is_default, merge_notify, merge_notify_into_extensions, read_dm_bookmark_notify,
    surface_bookmark_notify, NotifyMode, RichPushSummary, NS_WADDLE_DM_BOOKMARKS,
};

fn rich_push_summary(opt_in: bool) -> RichPushSummary {
    if opt_in {
        RichPushSummary::OptIn
    } else {
        RichPushSummary::OptOut
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Inputs for updating a room bookmark's notification policy.
pub struct SetRoomNotificationMode {
    /// Bare JID of the room in scope.
    pub room_jid: BareJid,
    /// Fallback bookmark name used when the room has no existing bookmark name.
    pub name: Option<String>,
    /// Selected notification or availability policy.
    pub mode: NotifyMode,
    /// Whether the room bookmark opts into Waddle rich push summaries.
    pub rich_payload_opt_in: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Outcome of updating a room bookmark notification policy.
#[non_exhaustive]
pub enum SetRoomNotificationModeOutcome {
    /// The requested update succeeded and returns the resulting item.
    Ok {
        #[doc = "Item to publish."]
        item: BookmarkItem,
    },
    /// Existing node configuration does not satisfy the requested publish options.
    NodeConfigMismatch,
    /// The operation failed; inspect the associated error details.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Inputs for updating a direct-message bookmark's notification policy.
pub struct SetDmNotificationMode {
    /// Bare JID identifying the direct-message peer.
    pub dm_jid: BareJid,
    /// Selected notification or availability policy.
    pub mode: NotifyMode,
    /// Whether the direct-message bookmark opts into Waddle rich push summaries.
    pub rich_payload_opt_in: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Outcome of updating a direct-message bookmark notification policy.
#[non_exhaustive]
pub enum SetDmNotificationModeOutcome {
    /// The requested update succeeded and returns the resulting item.
    Ok {
        /// XMPP address identifying the referenced entity.
        jid: BareJid,
        /// Selected notification or availability policy.
        mode: NotifyMode,
        /// Rich push-summary opt-in stored in the resulting direct-message bookmark.
        rich_payload_opt_in: bool,
    },
    /// The target item or membership has been removed.
    Removed {
        /// XMPP address identifying the referenced entity.
        jid: BareJid,
    },
    /// Existing node configuration does not satisfy the requested publish options.
    NodeConfigMismatch,
    /// The operation failed; inspect the associated error details.
    Error,
}

/// Set the room notification mode and return the updated value.
///
/// A room with no bookmark yet gets one with `autojoin` on: the user is
/// using the room, and an own bookmark with `autojoin` off means they
/// left it (see [`set_room_membership`]).
///
/// # Errors
///
/// Propagates transport, IQ timeout, and unhandled stanza errors from the supplied IQ sender. Missing nodes and documented publish conflicts use the fallback paths described above.
pub async fn set_room_notification_mode<D: CommandDriver>(
    driver: &D,
    options: SetRoomNotificationMode,
) -> ClientResult<SetRoomNotificationModeOutcome> {
    let rich_payload = rich_push_summary(options.rich_payload_opt_in);
    let mode = options.mode;
    let published = publish_merged_room_bookmark(
        driver,
        options.room_jid,
        options.name,
        "set_room_notification_mode.publish",
        |existing| {
            (
                existing.is_none_or(|item| item.autojoin),
                mode,
                rich_payload,
            )
        },
    )
    .await?;
    Ok(match published {
        PublishedRoomBookmark::Ok(item) => SetRoomNotificationModeOutcome::Ok { item },
        PublishedRoomBookmark::NodeConfigMismatch => {
            SetRoomNotificationModeOutcome::NodeConfigMismatch
        }
        PublishedRoomBookmark::Error => SetRoomNotificationModeOutcome::Error,
    })
}

/// Whether the user is in a room, as their own XEP-0402 bookmark records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomMembership {
    /// The user left: `autojoin` off, so no client of the account joins
    /// the room (XEP-0402 §4), and notifications off, since the server
    /// notifies a room's members whether or not they are in it.
    Left,
    /// The user is in the room: `autojoin` on, with `notify_mode` as the
    /// room's notification mode.
    Joined {
        /// Notification mode the room gets back.
        notify_mode: NotifyMode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Inputs for leaving or rejoining a room for every client of the account.
pub struct SetRoomMembership {
    /// Bare JID of the room in scope.
    pub room_jid: BareJid,
    /// Fallback bookmark name used when the room has no existing bookmark name.
    pub name: Option<String>,
    /// Whether the user is in the room from now on.
    pub membership: RoomMembership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Outcome of leaving or rejoining a room.
#[non_exhaustive]
pub enum SetRoomMembershipOutcome {
    /// The bookmark was published; carries the resulting item.
    Ok {
        /// Item as published.
        item: BookmarkItem,
    },
    /// Existing node configuration does not satisfy the requested publish options.
    NodeConfigMismatch,
    /// The operation failed; inspect the associated error details.
    Error,
}

/// Leave or rejoin a room by publishing the user's bookmark for it.
///
/// The bookmark's name, nick, password, foreign extensions and rich
/// push opt-in are kept as they are.
///
/// # Errors
///
/// Propagates transport, IQ timeout, and unhandled stanza errors from the supplied IQ sender.
pub async fn set_room_membership<D: CommandDriver>(
    driver: &D,
    options: SetRoomMembership,
) -> ClientResult<SetRoomMembershipOutcome> {
    let (autojoin, mode) = match options.membership {
        RoomMembership::Left => (false, NotifyMode::Never),
        RoomMembership::Joined { notify_mode } => (true, notify_mode),
    };
    let published = publish_merged_room_bookmark(
        driver,
        options.room_jid,
        options.name,
        "set_room_membership.publish",
        |existing| {
            let opted_in = existing
                .is_some_and(|item| surface_bookmark_notify(&item.extensions).rich_payload_opt_in);
            (autojoin, mode, rich_push_summary(opted_in))
        },
    )
    .await?;
    Ok(match published {
        PublishedRoomBookmark::Ok(item) => SetRoomMembershipOutcome::Ok { item },
        PublishedRoomBookmark::NodeConfigMismatch => SetRoomMembershipOutcome::NodeConfigMismatch,
        PublishedRoomBookmark::Error => SetRoomMembershipOutcome::Error,
    })
}

enum PublishedRoomBookmark {
    Ok(BookmarkItem),
    NodeConfigMismatch,
    Error,
}

/// Fetch the user's bookmarks, merge `decide`'s autojoin and notify
/// settings into the one for `room_jid`, and publish it. Everything
/// else on an existing bookmark is kept; `name` is used only when the
/// bookmark has none.
async fn publish_merged_room_bookmark<D: CommandDriver>(
    driver: &D,
    room_jid: BareJid,
    name: Option<String>,
    operation: &'static str,
    decide: impl FnOnce(Option<&BookmarkItem>) -> (bool, NotifyMode, RichPushSummary),
) -> ClientResult<PublishedRoomBookmark> {
    let fetch_iq = build_fetch_bookmarks_iq(&Uuid::new_v4().to_string());
    let items = match driver.send_iq(fetch_iq).await {
        Ok(elem) => parse_bookmarks_response(&elem),
        Err(err) if is_stanza_condition(&err, STANZA_CONDITION_ITEM_NOT_FOUND) => Vec::new(),
        Err(err) => return Err(err),
    };

    let existing = items.iter().find(|item| item.jid == room_jid);
    let (autojoin, mode, rich_payload) = decide(existing);
    let existing_extensions = existing.map(|item| build_extensions_wrapper(&item.extensions));
    let merged_extensions =
        merge_notify_into_extensions(existing_extensions.as_ref(), mode, rich_payload);
    let extensions = merged_extensions.children().cloned().collect();

    let merged_item = BookmarkItem {
        jid: room_jid,
        name: existing.and_then(|item| item.name.clone()).or(name),
        autojoin,
        nick: existing.and_then(|item| item.nick.clone()),
        password: existing.and_then(|item| item.password.clone()),
        extensions,
    };

    let publish_iq = build_publish_bookmark_iq(&merged_item, &Uuid::new_v4().to_string());
    match driver.send_iq(publish_iq).await {
        Ok(_) => Ok(PublishedRoomBookmark::Ok(merged_item)),
        Err(err) if is_stanza_condition(&err, STANZA_CONDITION_PRECONDITION_NOT_MET) => {
            Ok(PublishedRoomBookmark::NodeConfigMismatch)
        }
        Err(err) if warn_stanza_error(&err, operation) => Ok(PublishedRoomBookmark::Error),
        Err(err) => Err(err),
    }
}

/// Set the dm notification mode and return the updated value.
///
/// # Errors
///
/// Propagates transport, IQ timeout, and unhandled stanza errors from the supplied IQ sender. Missing nodes and documented publish conflicts use the fallback paths described above.
pub async fn set_dm_notification_mode<D: CommandDriver>(
    driver: &D,
    options: SetDmNotificationMode,
) -> ClientResult<SetDmNotificationModeOutcome> {
    let fetch_iq = build_fetch_dm_bookmarks_iq(&Uuid::new_v4().to_string());
    let items = match driver.send_iq(fetch_iq).await {
        Ok(elem) => parse_dm_bookmark_payloads(&elem),
        Err(err) if is_stanza_condition(&err, STANZA_CONDITION_ITEM_NOT_FOUND) => Vec::new(),
        Err(err) => return Err(err),
    };

    let existing_notify = items
        .iter()
        .find(|(jid, _)| *jid == options.dm_jid)
        .and_then(|(_, payload)| read_dm_bookmark_notify(payload))
        .cloned();
    let merged = merge_notify(
        existing_notify.as_ref(),
        options.mode,
        rich_push_summary(options.rich_payload_opt_in),
    );

    if dm_notify_is_default(&merged, NotifyMode::Always) {
        let retract_iq = build_retract_dm_bookmark_iq(&options.dm_jid, &Uuid::new_v4().to_string());
        return match driver.send_iq(retract_iq).await {
            Ok(_) => Ok(SetDmNotificationModeOutcome::Removed {
                jid: options.dm_jid,
            }),
            Err(err) if is_stanza_condition(&err, STANZA_CONDITION_ITEM_NOT_FOUND) => {
                Ok(SetDmNotificationModeOutcome::Removed {
                    jid: options.dm_jid,
                })
            }
            Err(err) if warn_stanza_error(&err, "set_dm_notification_mode.retract") => {
                Ok(SetDmNotificationModeOutcome::Error)
            }
            Err(err) => Err(err),
        };
    }

    let publish_iq =
        build_publish_dm_bookmark_iq(&options.dm_jid, &merged, &Uuid::new_v4().to_string());
    match driver.send_iq(publish_iq).await {
        Ok(_) => Ok(SetDmNotificationModeOutcome::Ok {
            jid: options.dm_jid,
            mode: options.mode,
            rich_payload_opt_in: options.rich_payload_opt_in,
        }),
        Err(err) if is_stanza_condition(&err, STANZA_CONDITION_PRECONDITION_NOT_MET) => {
            Ok(SetDmNotificationModeOutcome::NodeConfigMismatch)
        }
        Err(err) if warn_stanza_error(&err, "set_dm_notification_mode.publish") => {
            Ok(SetDmNotificationModeOutcome::Error)
        }
        Err(err) => Err(err),
    }
}

/// Read direct-message bookmark items from a PubSub result, ignoring malformed or unrelated payloads.
#[must_use]
pub fn parse_dm_bookmark_payloads(iq: &Element) -> Vec<(BareJid, Element)> {
    let Some(items) = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .filter(|items| items.attr("node") == Some(NS_WADDLE_DM_BOOKMARKS))
    else {
        return Vec::new();
    };
    items
        .children()
        .filter(|child| child.name() == "item" && child.ns() == NS_PUBSUB)
        .filter_map(|item| {
            let jid: BareJid = item.attr("id")?.parse().ok()?;
            jid.node()?;
            let payload = item.get_child("dm-bookmark", NS_WADDLE_DM_BOOKMARKS)?;
            Some((jid, payload.clone()))
        })
        .collect()
}

fn build_extensions_wrapper(children: &[Element]) -> Element {
    let mut builder = Element::builder("extensions", NS_BOOKMARKS);
    for child in children {
        builder = builder.append(child.clone());
    }
    builder.build()
}

fn is_stanza_condition(err: &ClientError, condition: &str) -> bool {
    matches!(err, crate::error::ClientError::StanzaError(stanza_err) if stanza_err.condition == condition)
}

fn warn_stanza_error(err: &ClientError, operation: &'static str) -> bool {
    let crate::error::ClientError::StanzaError(stanza_err) = err else {
        return false;
    };
    tracing::warn!(
        operation,
        condition = %stanza_err.condition,
        error_type = ?stanza_err.error_type,
        text = stanza_err.text.as_deref(),
        "unexpected stanza error while updating notification settings"
    );
    true
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use futures::executor::block_on;

    use super::*;
    use crate::error::{ClientError, StanzaError, StanzaErrorType};
    use crate::pep::{NS_BOOKMARKS, NS_PUBSUB};
    use crate::xep::xep0492::{read_fallback_mode, NS_NOTIFICATION_SETTINGS};

    struct MockDriver {
        responses: RefCell<VecDeque<ClientResult<Element>>>,
        sent: RefCell<Vec<Element>>,
    }

    impl MockDriver {
        fn new(responses: Vec<ClientResult<Element>>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                sent: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandDriver for MockDriver {
        async fn send_iq(&self, iq: Element) -> ClientResult<Element> {
            self.sent.borrow_mut().push(iq);
            self.responses
                .borrow_mut()
                .pop_front()
                .expect("mock response queued")
        }
    }

    fn ok_iq() -> Element {
        Element::builder("iq", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .build()
    }

    fn stanza_error(condition: &str) -> ClientError {
        ClientError::StanzaError(StanzaError {
            error_type: StanzaErrorType::Cancel,
            condition: condition.to_string(),
            text: None,
            application_condition: None,
        })
    }

    fn bookmarks_response(items: Vec<BookmarkItem>) -> Element {
        let mut items_builder = Element::builder("items", NS_PUBSUB)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_BOOKMARKS);
        for item in items {
            items_builder = items_builder.append(
                Element::builder("item", NS_PUBSUB)
                    .attr(
                        minidom::rxml::xml_ncname!("id").to_owned(),
                        item.jid.to_string(),
                    )
                    .append(item.build_conference_element())
                    .build(),
            );
        }
        Element::builder("iq", "jabber:client")
            .append(
                Element::builder("pubsub", NS_PUBSUB)
                    .append(items_builder.build())
                    .build(),
            )
            .build()
    }

    fn dm_bookmarks_response(items: Vec<(BareJid, Element)>) -> Element {
        let mut items_builder = Element::builder("items", NS_PUBSUB).attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NS_WADDLE_DM_BOOKMARKS,
        );
        for (jid, payload) in items {
            items_builder = items_builder.append(
                Element::builder("item", NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), jid.to_string())
                    .append(payload)
                    .build(),
            );
        }
        Element::builder("iq", "jabber:client")
            .append(
                Element::builder("pubsub", NS_PUBSUB)
                    .append(items_builder.build())
                    .build(),
            )
            .build()
    }

    #[test]
    fn room_notification_update_preserves_existing_bookmark_fields() {
        let room_jid: BareJid = "room@muc.waddle.test".parse().unwrap();
        let existing = BookmarkItem {
            jid: room_jid.clone(),
            name: Some("Existing".to_string()),
            autojoin: true,
            nick: Some("alice".to_string()),
            password: Some("secret".to_string()),
            extensions: Vec::new(),
        };
        let driver = MockDriver::new(vec![Ok(bookmarks_response(vec![existing])), Ok(ok_iq())]);

        let outcome = block_on(set_room_notification_mode(
            &driver,
            SetRoomNotificationMode {
                room_jid,
                name: Some("Replacement".to_string()),
                mode: NotifyMode::Never,
                rich_payload_opt_in: true,
            },
        ))
        .unwrap();

        let SetRoomNotificationModeOutcome::Ok { item } = outcome else {
            panic!("expected ok outcome");
        };
        assert_eq!(item.name.as_deref(), Some("Existing"));
        assert!(item.autojoin);
        assert_eq!(item.nick.as_deref(), Some("alice"));
        assert_eq!(item.password.as_deref(), Some("secret"));
        let notify = item
            .extensions
            .iter()
            .find(|child| child.is("notify", NS_NOTIFICATION_SETTINGS))
            .expect("notify extension");
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::Never));
        assert_eq!(driver.sent.borrow().len(), 2);
    }

    fn published_conference(sent: &Element) -> &Element {
        sent.get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
            .and_then(|publish| publish.get_child("item", NS_PUBSUB))
            .and_then(|item| item.get_child("conference", NS_BOOKMARKS))
            .expect("published conference")
    }

    #[test]
    fn leaving_a_room_publishes_autojoin_off_and_never_notify() {
        let room_jid: BareJid = "room@muc.waddle.test".parse().unwrap();
        let foreign: Element = "<pinned xmlns='urn:example:pinned'/>".parse().unwrap();
        let existing = BookmarkItem {
            jid: room_jid.clone(),
            name: Some("Existing".to_string()),
            autojoin: true,
            nick: Some("alice".to_string()),
            password: Some("secret".to_string()),
            extensions: vec![foreign.clone()],
        };
        let driver = MockDriver::new(vec![Ok(bookmarks_response(vec![existing])), Ok(ok_iq())]);

        let outcome = block_on(set_room_membership(
            &driver,
            SetRoomMembership {
                room_jid,
                name: Some("Replacement".to_string()),
                membership: RoomMembership::Left,
            },
        ))
        .unwrap();

        let SetRoomMembershipOutcome::Ok { item } = outcome else {
            panic!("expected ok outcome");
        };
        assert!(!item.autojoin);
        let sent = driver.sent.borrow();
        let conference = published_conference(&sent[1]);
        // XEP-0402 §2.2: an absent attribute is the false value.
        assert_eq!(conference.attr("autojoin"), None);
        assert_eq!(conference.attr("name"), Some("Existing"));
        assert_eq!(
            conference
                .get_child("nick", NS_BOOKMARKS)
                .map(Element::text),
            Some("alice".to_string())
        );
        assert_eq!(
            conference
                .get_child("password", NS_BOOKMARKS)
                .map(Element::text),
            Some("secret".to_string())
        );
        let extensions = conference
            .get_child("extensions", NS_BOOKMARKS)
            .expect("extensions");
        assert!(extensions.children().any(|child| *child == foreign));
        let notify = extensions
            .get_child("notify", NS_NOTIFICATION_SETTINGS)
            .expect("notify extension");
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::Never));
    }

    #[test]
    fn leaving_keeps_the_rich_push_opt_in() {
        let room_jid: BareJid = "room@muc.waddle.test".parse().unwrap();
        let opted_in =
            merge_notify_into_extensions(None, NotifyMode::Always, RichPushSummary::OptIn);
        let existing = BookmarkItem {
            jid: room_jid.clone(),
            name: None,
            autojoin: true,
            nick: None,
            password: None,
            extensions: opted_in.children().cloned().collect(),
        };
        let driver = MockDriver::new(vec![Ok(bookmarks_response(vec![existing])), Ok(ok_iq())]);

        let outcome = block_on(set_room_membership(
            &driver,
            SetRoomMembership {
                room_jid,
                name: None,
                membership: RoomMembership::Left,
            },
        ))
        .unwrap();

        let SetRoomMembershipOutcome::Ok { item } = outcome else {
            panic!("expected ok outcome");
        };
        let surfaced = surface_bookmark_notify(&item.extensions);
        assert_eq!(surfaced.fallback_mode, Some(NotifyMode::Never));
        assert!(surfaced.rich_payload_opt_in);
    }

    #[test]
    fn rejoining_a_room_publishes_autojoin_on_and_the_given_notify_mode() {
        let room_jid: BareJid = "room@muc.waddle.test".parse().unwrap();
        let left = merge_notify_into_extensions(None, NotifyMode::Never, RichPushSummary::OptOut);
        let existing = BookmarkItem {
            jid: room_jid.clone(),
            name: Some("Existing".to_string()),
            autojoin: false,
            nick: Some("alice".to_string()),
            password: None,
            extensions: left.children().cloned().collect(),
        };
        let driver = MockDriver::new(vec![Ok(bookmarks_response(vec![existing])), Ok(ok_iq())]);

        block_on(set_room_membership(
            &driver,
            SetRoomMembership {
                room_jid,
                name: None,
                membership: RoomMembership::Joined {
                    notify_mode: NotifyMode::OnMention,
                },
            },
        ))
        .unwrap();

        let sent = driver.sent.borrow();
        let conference = published_conference(&sent[1]);
        assert_eq!(conference.attr("autojoin"), Some("true"));
        assert_eq!(conference.attr("name"), Some("Existing"));
        assert_eq!(
            conference
                .get_child("nick", NS_BOOKMARKS)
                .map(Element::text),
            Some("alice".to_string())
        );
        let notify = conference
            .get_child("extensions", NS_BOOKMARKS)
            .and_then(|extensions| extensions.get_child("notify", NS_NOTIFICATION_SETTINGS))
            .expect("notify extension");
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::OnMention));
    }

    #[test]
    fn leaving_a_room_with_no_bookmark_creates_one_from_the_given_name() {
        let room_jid: BareJid = "room@muc.waddle.test".parse().unwrap();
        let driver = MockDriver::new(vec![
            Err(stanza_error(STANZA_CONDITION_ITEM_NOT_FOUND)),
            Ok(ok_iq()),
        ]);

        block_on(set_room_membership(
            &driver,
            SetRoomMembership {
                room_jid: room_jid.clone(),
                name: Some("Lobby".to_string()),
                membership: RoomMembership::Left,
            },
        ))
        .unwrap();

        let sent = driver.sent.borrow();
        let conference = published_conference(&sent[1]);
        assert_eq!(conference.attr("autojoin"), None);
        assert_eq!(conference.attr("name"), Some("Lobby"));
    }

    #[test]
    fn room_membership_precondition_maps_to_node_config_mismatch() {
        let room_jid: BareJid = "room@muc.waddle.test".parse().unwrap();
        let driver = MockDriver::new(vec![
            Ok(bookmarks_response(Vec::new())),
            Err(stanza_error(STANZA_CONDITION_PRECONDITION_NOT_MET)),
        ]);

        let outcome = block_on(set_room_membership(
            &driver,
            SetRoomMembership {
                room_jid,
                name: None,
                membership: RoomMembership::Left,
            },
        ))
        .unwrap();

        assert_eq!(outcome, SetRoomMembershipOutcome::NodeConfigMismatch);
    }

    #[test]
    fn a_notification_mode_on_a_room_with_no_bookmark_keeps_it_joined() {
        // An own bookmark with autojoin off means the user left the room
        // (XEP-0402 §4), so choosing a mode must not publish that.
        let room_jid: BareJid = "room@muc.waddle.test".parse().unwrap();
        let driver = MockDriver::new(vec![Ok(bookmarks_response(Vec::new())), Ok(ok_iq())]);

        block_on(set_room_notification_mode(
            &driver,
            SetRoomNotificationMode {
                room_jid,
                name: None,
                mode: NotifyMode::Never,
                rich_payload_opt_in: false,
            },
        ))
        .unwrap();

        let sent = driver.sent.borrow();
        assert_eq!(
            published_conference(&sent[1]).attr("autojoin"),
            Some("true")
        );
    }

    #[test]
    fn a_notification_mode_on_a_left_room_keeps_it_left() {
        let room_jid: BareJid = "room@muc.waddle.test".parse().unwrap();
        let existing = BookmarkItem {
            jid: room_jid.clone(),
            name: None,
            autojoin: false,
            nick: None,
            password: None,
            extensions: Vec::new(),
        };
        let driver = MockDriver::new(vec![Ok(bookmarks_response(vec![existing])), Ok(ok_iq())]);

        block_on(set_room_notification_mode(
            &driver,
            SetRoomNotificationMode {
                room_jid,
                name: None,
                mode: NotifyMode::Always,
                rich_payload_opt_in: false,
            },
        ))
        .unwrap();

        let sent = driver.sent.borrow();
        assert_eq!(published_conference(&sent[1]).attr("autojoin"), None);
    }

    #[test]
    fn room_notification_precondition_maps_to_node_config_mismatch() {
        let room_jid: BareJid = "room@muc.waddle.test".parse().unwrap();
        let driver = MockDriver::new(vec![
            Ok(bookmarks_response(Vec::new())),
            Err(stanza_error(STANZA_CONDITION_PRECONDITION_NOT_MET)),
        ]);

        let outcome = block_on(set_room_notification_mode(
            &driver,
            SetRoomNotificationMode {
                room_jid,
                name: None,
                mode: NotifyMode::OnMention,
                rich_payload_opt_in: false,
            },
        ))
        .unwrap();

        assert_eq!(outcome, SetRoomNotificationModeOutcome::NodeConfigMismatch);
    }

    #[test]
    fn dm_default_setting_retracts_bookmark_item() {
        let dm_jid: BareJid = "bob@waddle.test".parse().unwrap();
        let driver = MockDriver::new(vec![Ok(dm_bookmarks_response(Vec::new())), Ok(ok_iq())]);

        let outcome = block_on(set_dm_notification_mode(
            &driver,
            SetDmNotificationMode {
                dm_jid: dm_jid.clone(),
                mode: NotifyMode::Always,
                rich_payload_opt_in: false,
            },
        ))
        .unwrap();

        assert_eq!(
            outcome,
            SetDmNotificationModeOutcome::Removed { jid: dm_jid }
        );
        let sent = driver.sent.borrow();
        assert_eq!(sent.len(), 2);
        let retract = sent[1]
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("retract", NS_PUBSUB))
            .expect("retract sent");
        assert_eq!(retract.attr("node"), Some(NS_WADDLE_DM_BOOKMARKS));
    }

    #[test]
    fn dm_publish_precondition_maps_to_node_config_mismatch() {
        let dm_jid: BareJid = "bob@waddle.test".parse().unwrap();
        let driver = MockDriver::new(vec![
            Ok(dm_bookmarks_response(Vec::new())),
            Err(stanza_error(STANZA_CONDITION_PRECONDITION_NOT_MET)),
        ]);

        let outcome = block_on(set_dm_notification_mode(
            &driver,
            SetDmNotificationMode {
                dm_jid,
                mode: NotifyMode::Never,
                rich_payload_opt_in: false,
            },
        ))
        .unwrap();

        assert_eq!(outcome, SetDmNotificationModeOutcome::NodeConfigMismatch);
    }

    #[test]
    fn dm_fetch_item_not_found_is_treated_as_empty_bookmark_set() {
        let dm_jid: BareJid = "bob@waddle.test".parse().unwrap();
        let driver = MockDriver::new(vec![
            Err(stanza_error(STANZA_CONDITION_ITEM_NOT_FOUND)),
            Ok(ok_iq()),
        ]);

        let outcome = block_on(set_dm_notification_mode(
            &driver,
            SetDmNotificationMode {
                dm_jid: dm_jid.clone(),
                mode: NotifyMode::Always,
                rich_payload_opt_in: false,
            },
        ))
        .unwrap();

        assert_eq!(
            outcome,
            SetDmNotificationModeOutcome::Removed { jid: dm_jid }
        );
    }

    #[test]
    fn dm_publish_outcome_carries_typed_notify_values() {
        let dm_jid: BareJid = "bob@waddle.test".parse().unwrap();
        let driver = MockDriver::new(vec![Ok(dm_bookmarks_response(Vec::new())), Ok(ok_iq())]);

        let outcome = block_on(set_dm_notification_mode(
            &driver,
            SetDmNotificationMode {
                dm_jid: dm_jid.clone(),
                mode: NotifyMode::Never,
                rich_payload_opt_in: true,
            },
        ))
        .unwrap();

        assert_eq!(
            outcome,
            SetDmNotificationModeOutcome::Ok {
                jid: dm_jid,
                mode: NotifyMode::Never,
                rich_payload_opt_in: true,
            }
        );
    }
}
