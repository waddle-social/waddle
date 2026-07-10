//! Core read/modify/write flows for XEP-0492 notification settings.
//!
//! Binding crates deserialize their own boundary options and call these
//! helpers; the XMPP fetch/merge/publish/retract semantics live here.

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
    NotifyMode, NS_WADDLE_DM_BOOKMARKS,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetRoomNotificationMode {
    pub room_jid: BareJid,
    pub name: Option<String>,
    pub mode: NotifyMode,
    pub rich_payload_opt_in: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetRoomNotificationModeOutcome {
    Ok { item: BookmarkItem },
    NodeConfigMismatch,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetDmNotificationMode {
    pub dm_jid: BareJid,
    pub mode: NotifyMode,
    pub rich_payload_opt_in: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetDmNotificationModeOutcome {
    Ok {
        jid: BareJid,
        mode: NotifyMode,
        rich_payload_opt_in: bool,
    },
    Removed {
        jid: BareJid,
    },
    NodeConfigMismatch,
    Error,
}

pub async fn set_room_notification_mode<D: CommandDriver>(
    driver: &D,
    options: SetRoomNotificationMode,
) -> ClientResult<SetRoomNotificationModeOutcome> {
    let fetch_iq = build_fetch_bookmarks_iq(&Uuid::new_v4().to_string());
    let items = match driver.send_iq(fetch_iq).await {
        Ok(elem) => parse_bookmarks_response(&elem),
        Err(err) if is_stanza_condition(&err, STANZA_CONDITION_ITEM_NOT_FOUND) => Vec::new(),
        Err(err) => return Err(err),
    };

    let existing = items.iter().find(|item| item.jid == options.room_jid);
    let existing_extensions = existing.map(|item| build_extensions_wrapper(&item.extensions));
    let merged_extensions = merge_notify_into_extensions(
        existing_extensions.as_ref(),
        options.mode,
        options.rich_payload_opt_in,
    );
    let extensions = merged_extensions.children().cloned().collect();

    let merged_item = BookmarkItem {
        jid: options.room_jid,
        name: existing.and_then(|item| item.name.clone()).or(options.name),
        autojoin: existing.map(|item| item.autojoin).unwrap_or(false),
        nick: existing.and_then(|item| item.nick.clone()),
        password: existing.and_then(|item| item.password.clone()),
        extensions,
    };

    let publish_iq = build_publish_bookmark_iq(&merged_item, &Uuid::new_v4().to_string());
    match driver.send_iq(publish_iq).await {
        Ok(_) => Ok(SetRoomNotificationModeOutcome::Ok { item: merged_item }),
        Err(err) if is_stanza_condition(&err, STANZA_CONDITION_PRECONDITION_NOT_MET) => {
            Ok(SetRoomNotificationModeOutcome::NodeConfigMismatch)
        }
        Err(err) if warn_stanza_error(&err, "set_room_notification_mode.publish") => {
            Ok(SetRoomNotificationModeOutcome::Error)
        }
        Err(err) => Err(err),
    }
}

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
        options.rich_payload_opt_in,
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

pub fn parse_dm_bookmark_payloads(iq: &Element) -> Vec<(BareJid, Element)> {
    let Some(items) = iq
        .get_child("pubsub", crate::pep::NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", crate::pep::NS_PUBSUB))
        .filter(|items| items.attr("node") == Some(NS_WADDLE_DM_BOOKMARKS))
    else {
        return Vec::new();
    };
    items
        .children()
        .filter(|child| child.name() == "item" && child.ns() == crate::pep::NS_PUBSUB)
        .filter_map(|item| {
            let jid: BareJid = item.attr("id")?.parse().ok()?;
            jid.node()?;
            let payload = item.get_child("dm-bookmark", NS_WADDLE_DM_BOOKMARKS)?;
            Some((jid, payload.clone()))
        })
        .collect()
}

fn build_extensions_wrapper(children: &[Element]) -> Element {
    let mut builder = Element::builder("extensions", crate::pep::NS_BOOKMARKS);
    for child in children {
        builder = builder.append(child.clone());
    }
    builder.build()
}

fn is_stanza_condition(err: &crate::error::ClientError, condition: &str) -> bool {
    matches!(err, crate::error::ClientError::StanzaError(stanza_err) if stanza_err.condition == condition)
}

fn warn_stanza_error(err: &crate::error::ClientError, operation: &'static str) -> bool {
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
