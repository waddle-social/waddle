//! Typed native boundary for room-authorized planning objects.
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError, WaddleNotifyMode, WaddleRoomConfigPatch};
use chrono::DateTime;
use jid::BareJid;
use uuid::Uuid;
use waddle_xmpp_client::discovery::DiscoveryExt;
use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, build_pubsub_retract_iq,
    parse_pubsub_items_result,
};
use waddle_xmpp_core::event_planning::{
    AttendanceStatus, PlanningItem, PlanningKind, PlanningOption, PlanningStatus,
    NODE_EVENT_PLANNING,
};
macro_rules! boundary_enum {
    ($ffi:ident, $core:ident, $($variant:ident),+) => {
        #[derive(Clone, Copy, Debug, uniffi::Enum)]
        pub enum $ffi { $($variant),+ }
        impl From<$ffi> for $core { fn from(value: $ffi) -> Self { match value { $($ffi::$variant => Self::$variant),+ } } }
        impl From<$core> for $ffi { fn from(value: $core) -> Self { match value { $($core::$variant => Self::$variant),+ } } }
    }
}
boundary_enum!(
    WaddlePlanningKind,
    PlanningKind,
    Event,
    Link,
    Task,
    Itinerary,
    Decision,
    Travel,
    Expense,
    Poll,
    Vote,
    Attendance
);
boundary_enum!(
    WaddleAttendanceStatus,
    AttendanceStatus,
    Invited,
    Interested,
    Going,
    Declined
);
boundary_enum!(
    WaddlePlanningStatus,
    PlanningStatus,
    Proposed,
    Confirmed,
    Cancelled
);
#[derive(Clone, Debug, uniffi::Record)]
pub struct WaddlePlanningOption {
    pub id: String,
    pub label: String,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct WaddlePlanningItem {
    pub id: String,
    pub kind: WaddlePlanningKind,
    pub author: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub url: Option<String>,
    pub category: Option<String>,
    pub location: Option<String>,
    pub timezone: Option<String>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub assignee: Option<String>,
    pub completed: bool,
    pub pinned: bool,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub participants: Vec<String>,
    pub options: Vec<WaddlePlanningOption>,
    pub selections: Vec<String>,
    pub subject: Option<String>,
    pub attendance: Option<WaddleAttendanceStatus>,
    pub status: Option<WaddlePlanningStatus>,
}
fn into_core(value: WaddlePlanningItem) -> Result<PlanningItem, WaddleError> {
    let mut item = PlanningItem::new(value.id, value.kind.into(), value.title);
    item.description = value.description;
    item.category = value.category;
    item.location = value.location;
    item.url = value
        .url
        .map(|url| url.parse())
        .transpose()
        .map_err(|_| WaddleError::InvalidArgument)?;
    item.timezone = value
        .timezone
        .map(|zone| zone.parse())
        .transpose()
        .map_err(|_| WaddleError::InvalidArgument)?;
    item.starts_at = value
        .starts_at
        .map(|ms| DateTime::from_timestamp_millis(ms).ok_or(WaddleError::InvalidArgument))
        .transpose()?;
    item.ends_at = value
        .ends_at
        .map(|ms| DateTime::from_timestamp_millis(ms).ok_or(WaddleError::InvalidArgument))
        .transpose()?;
    item.assignee = value
        .assignee
        .map(|jid| jid.parse())
        .transpose()
        .map_err(|_| WaddleError::InvalidJid)?;
    item.completed = value.completed;
    item.pinned = value.pinned;
    item.amount_minor = value.amount_minor;
    item.currency = value.currency;
    item.participants = value
        .participants
        .into_iter()
        .map(|jid| jid.parse())
        .collect::<Result<_, _>>()
        .map_err(|_| WaddleError::InvalidJid)?;
    item.options = value
        .options
        .into_iter()
        .map(|option| PlanningOption {
            id: option.id,
            label: option.label,
        })
        .collect();
    item.selections = value.selections;
    item.subject = value.subject;
    item.attendance = value.attendance.map(Into::into);
    item.status = value.status.map(Into::into);
    item.validate().map_err(|_| WaddleError::InvalidArgument)?;
    Ok(item)
}
fn into_ffi(item: PlanningItem) -> WaddlePlanningItem {
    WaddlePlanningItem {
        id: item.id,
        kind: item.kind.into(),
        author: item.author.map(|jid| jid.to_string()),
        title: item.title,
        description: item.description,
        url: item.url.map(|url| url.to_string()),
        category: item.category,
        location: item.location,
        timezone: item.timezone.map(|zone| zone.to_string()),
        starts_at: item.starts_at.map(|date| date.timestamp_millis()),
        ends_at: item.ends_at.map(|date| date.timestamp_millis()),
        assignee: item.assignee.map(|jid| jid.to_string()),
        completed: item.completed,
        pinned: item.pinned,
        amount_minor: item.amount_minor,
        currency: item.currency,
        participants: item
            .participants
            .into_iter()
            .map(|jid| jid.to_string())
            .collect(),
        options: item
            .options
            .into_iter()
            .map(|option| WaddlePlanningOption {
                id: option.id,
                label: option.label,
            })
            .collect(),
        selections: item.selections,
        subject: item.subject,
        attendance: item.attendance.map(Into::into),
        status: item.status.map(Into::into),
    }
}
/// Derive response identity from the account rather than a UI-generated ID.
fn authored_item(value: WaddlePlanningItem, author: &BareJid) -> Result<PlanningItem, WaddleError> {
    let mut item = into_core(value)?;
    item.author = Some(author.clone());
    if let Some(id) = item.self_owned_id(author) {
        item.id = id;
    }
    item.validate().map_err(|_| WaddleError::InvalidArgument)?;
    Ok(item)
}

/// Read the exact saved item so canonical attribution is preserved even when
/// the workspace is larger than the collection retrieval limit.
fn planning_item_query(room: &BareJid, item_id: &str) -> minidom::Element {
    let mut query = build_pubsub_items_iq(
        &Uuid::new_v4().to_string(),
        Some(room),
        NODE_EVENT_PLANNING,
        None,
    );
    if let Some(items) = query
        .get_child_mut("pubsub", waddle_xmpp_client::pep::NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child_mut("items", waddle_xmpp_client::pep::NS_PUBSUB))
    {
        items.append_child(
            minidom::Element::builder("item", waddle_xmpp_client::pep::NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
                .build(),
        );
    }
    query
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    pub async fn event_planning_items(
        &self,
        room_jid: String,
    ) -> Result<Vec<WaddlePlanningItem>, WaddleError> {
        let room: BareJid = room_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = build_pubsub_items_iq(
            &Uuid::new_v4().to_string(),
            Some(&room),
            NODE_EVENT_PLANNING,
            None,
        );
        let reply = send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|error| client_error_to_waddle(&error))?;
        parse_pubsub_items_result(&reply)
            .into_iter()
            .map(|item| {
                let payload = item
                    .payload("item", NODE_EVENT_PLANNING)
                    .ok_or(WaddleError::MalformedResponse)?;
                PlanningItem::from_element(payload)
                    .map(into_ffi)
                    .map_err(|_| WaddleError::MalformedResponse)
            })
            .collect()
    }
    pub async fn event_planning_publish(
        &self,
        room_jid: String,
        item: WaddlePlanningItem,
    ) -> Result<WaddlePlanningItem, WaddleError> {
        let room: BareJid = room_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let account = self
            .config
            .jid
            .parse::<jid::Jid>()
            .map_err(|_| WaddleError::InvalidJid)?
            .to_bare();
        let item = authored_item(item, &account)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = build_pubsub_publish_iq(
            &Uuid::new_v4().to_string(),
            Some(&room),
            NODE_EVENT_PLANNING,
            Some(&item.id),
            item.to_element(),
            None,
        );
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|error| client_error_to_waddle(&error))?;
        let query = planning_item_query(&room, &item.id);
        let reply = send_iq_with_timeout(&handle, query)
            .await
            .map_err(|error| client_error_to_waddle(&error))?;
        let stored = parse_pubsub_items_result(&reply);
        let payload = stored
            .iter()
            .find(|stored| stored.id == item.id)
            .and_then(|stored| stored.payload("item", NODE_EVENT_PLANNING))
            .ok_or(WaddleError::MalformedResponse)?;
        PlanningItem::from_element(payload)
            .map(into_ffi)
            .map_err(|_| WaddleError::MalformedResponse)
    }
    pub async fn event_planning_retract(
        &self,
        room_jid: String,
        id: String,
    ) -> Result<(), WaddleError> {
        let room: BareJid = room_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        if id.trim().is_empty() {
            return Err(WaddleError::InvalidArgument);
        }
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = build_pubsub_retract_iq(
            &Uuid::new_v4().to_string(),
            Some(&room),
            NODE_EVENT_PLANNING,
            &id,
        );
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|error| client_error_to_waddle(&error))?;
        Ok(())
    }
    pub async fn create_event_room(
        &self,
        name: String,
        private_room: bool,
    ) -> Result<String, WaddleError> {
        if name.trim().is_empty() {
            return Err(WaddleError::InvalidArgument);
        }
        let account = self
            .config
            .jid
            .parse::<jid::Jid>()
            .map_err(|_| WaddleError::InvalidJid)?
            .to_bare();
        let nick = account
            .node()
            .ok_or(WaddleError::InvalidJid)?
            .as_str()
            .to_string();
        let room = self
            .create_room(
                format!("event-{}", Uuid::new_v4()),
                nick,
                WaddleRoomConfigPatch {
                    name: Some(name.clone()),
                    members_only: Some(private_room),
                    public_room: Some(!private_room),
                    persistent_room: Some(true),
                    ..WaddleRoomConfigPatch::default()
                },
            )
            .await?;
        let room_jid = room
            .parse::<BareJid>()
            .map_err(|_| WaddleError::InvalidJid)?;
        let verified = self.fetch_room_config_form(&room_jid).await;
        let valid = verified.as_ref().is_ok_and(|form| {
            use waddle_xmpp_client::xep::xep0045_owner::{FIELD_MEMBERS_ONLY, FIELD_PUBLIC_ROOM};
            form.bool_value(FIELD_MEMBERS_ONLY) == Some(private_room)
                && form.bool_value(FIELD_PUBLIC_ROOM) == Some(!private_room)
        });
        let persistent = self
            .clone_handle()
            .await
            .ok_or(WaddleError::NotConnected)?
            .discover_info(room_jid.as_str(), None)
            .await
            .is_ok_and(|info| {
                info.has_feature(waddle_xmpp_client::xep::xep0045_owner::FEATURE_MUC_PERSISTENT)
            });
        if !valid || !persistent {
            let _ = self
                .destroy_room(
                    room,
                    Some("Event room configuration was not applied".into()),
                )
                .await;
            return Err(verified.err().unwrap_or(WaddleError::MalformedResponse));
        }
        let bookmark = self
            .set_room_notification_mode(
                room.clone(),
                WaddleNotifyMode::OnMention,
                Some(name),
                false,
            )
            .await;
        match bookmark {
            Ok(crate::WaddleSetRoomNotificationModeOutcome::Ok { .. }) => Ok(room),
            outcome => {
                let _ = self
                    .destroy_room(room, Some("Event setup did not complete".into()))
                    .await;
                Err(outcome.err().unwrap_or(WaddleError::MalformedResponse))
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_boundary_round_trip_preserves_planning_fields() {
        let mut item = PlanningItem::new("london", PlanningKind::Task, "Book dinner");
        item.assignee = Some("david@example.test".parse().expect("jid"));
        item.starts_at = DateTime::from_timestamp_millis(1_800_000_000_000);
        item.pinned = true;
        assert_eq!(
            into_core(into_ffi(item.clone())).expect("valid boundary"),
            item
        );
    }
    #[test]
    fn native_boundary_rejects_unsafe_links_and_invalid_instants() {
        let mut item = into_ffi(PlanningItem::new("link", PlanningKind::Link, "Hotel"));
        item.url = Some("javascript:alert(1)".into());
        assert!(into_core(item).is_err());
        let mut item = into_ffi(PlanningItem::new("trip", PlanningKind::Itinerary, "Dinner"));
        item.starts_at = Some(i64::MAX);
        assert!(into_core(item).is_err());
    }
    #[test]
    fn native_personal_responses_derive_canonical_ids_from_actual_account() {
        let author: BareJid = "alice@example.test".parse().expect("author");
        let mut attendance = PlanningItem::new("random-ui-id", PlanningKind::Attendance, "");
        attendance.attendance = Some(AttendanceStatus::Going);
        attendance.author = Some("bob@example.test".parse().expect("forged author"));
        let result = authored_item(into_ffi(attendance), &author).expect("attendance");
        assert_eq!(result.id, "attendance:alice@example.test");
        assert_eq!(result.author, Some(author.clone()));
        let mut vote = PlanningItem::new("another-random-ui-id", PlanningKind::Vote, "");
        vote.subject = Some("dinner".into());
        vote.selections = vec!["pub".into()];
        let result = authored_item(into_ffi(vote), &author).expect("vote");
        assert_eq!(result.id, "vote:dinner:alice@example.test");
        assert_eq!(result.author, Some(author));
    }

    #[test]
    fn canonical_readback_selects_only_the_saved_item() {
        let room: BareJid = "event-london@muc.example.test".parse().expect("room");
        let query = planning_item_query(&room, "hotel");
        let items = query
            .get_child("pubsub", waddle_xmpp_client::pep::NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", waddle_xmpp_client::pep::NS_PUBSUB))
            .expect("items query");
        assert_eq!(query.attr("to"), Some(room.as_str()));
        assert_eq!(items.attr("node"), Some(NODE_EVENT_PLANNING));
        assert_eq!(items.children().count(), 1);
        assert_eq!(
            items.children().next().expect("selected item").attr("id"),
            Some("hotel")
        );
    }

    #[test]
    fn planning_publish_uses_room_scoped_pubsub_and_typed_payload() {
        let room: BareJid = "event-london@muc.example.test".parse().expect("room");
        let mut item = PlanningItem::new("hotel", PlanningKind::Link, "Hotel");
        item.url = Some("https://example.test/hotel".parse().expect("url"));
        let iq = build_pubsub_publish_iq(
            "publish",
            Some(&room),
            NODE_EVENT_PLANNING,
            Some(&item.id),
            item.to_element(),
            None,
        );
        assert_eq!(iq.attr("to"), Some(room.as_str()));
        let publish = iq
            .get_child("pubsub", waddle_xmpp_client::pep::NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("publish", waddle_xmpp_client::pep::NS_PUBSUB))
            .expect("publish");
        assert_eq!(publish.attr("node"), Some(NODE_EVENT_PLANNING));
        let payload = publish
            .children()
            .next()
            .and_then(|entry| entry.get_child("item", NODE_EVENT_PLANNING))
            .expect("typed payload");
        assert_eq!(
            PlanningItem::from_element(payload).expect("planning payload"),
            item
        );
    }
}
