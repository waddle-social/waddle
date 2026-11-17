//! Native xCal event records.
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError};
use chrono::{DateTime, NaiveDate, Utc};
use jid::BareJid;
use uuid::Uuid;
use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, build_pubsub_retract_iq,
    parse_pubsub_items_result,
};
use waddle_xmpp_core::xcal::{
    build_vcalendar_with_item, parse_vcalendar_item, Attendee, CalendarDateValue, CalendarItem,
    PartStat, VEvent, NS_XCAL, PUBSUB_NODE_EVENTS,
};
#[derive(Clone, Debug, uniffi::Enum)]
pub enum WaddleCalendarDate {
    Date { date: String },
    DateTime { timestamp_ms: i64 },
}
impl TryFrom<WaddleCalendarDate> for CalendarDateValue {
    type Error = WaddleError;
    fn try_from(value: WaddleCalendarDate) -> Result<Self, Self::Error> {
        match value {
            WaddleCalendarDate::Date { date } => NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map(Self::Date)
                .map_err(|_| WaddleError::InvalidArgument),
            WaddleCalendarDate::DateTime { timestamp_ms } => {
                DateTime::from_timestamp_millis(timestamp_ms)
                    .map(Self::DateTime)
                    .ok_or(WaddleError::InvalidArgument)
            }
        }
    }
}
impl From<CalendarDateValue> for WaddleCalendarDate {
    fn from(value: CalendarDateValue) -> Self {
        match value {
            CalendarDateValue::Date(date) => Self::Date {
                date: date.to_string(),
            },
            CalendarDateValue::DateTime(date) => Self::DateTime {
                timestamp_ms: date.timestamp_millis(),
            },
        }
    }
}
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum WaddleEventParticipation {
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
}
impl From<WaddleEventParticipation> for PartStat {
    fn from(value: WaddleEventParticipation) -> Self {
        match value {
            WaddleEventParticipation::NeedsAction => Self::NeedsAction,
            WaddleEventParticipation::Accepted => Self::Accepted,
            WaddleEventParticipation::Declined => Self::Declined,
            WaddleEventParticipation::Tentative => Self::Tentative,
        }
    }
}
impl From<PartStat> for WaddleEventParticipation {
    fn from(value: PartStat) -> Self {
        match value {
            PartStat::NeedsAction => Self::NeedsAction,
            PartStat::Accepted => Self::Accepted,
            PartStat::Declined => Self::Declined,
            PartStat::Tentative => Self::Tentative,
        }
    }
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct WaddleEventAttendee {
    pub uri: String,
    pub status: WaddleEventParticipation,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct WaddleEvent {
    pub id: String,
    pub uid: String,
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub organizer: Option<String>,
    pub workspace_room: Option<String>,
    pub start: Option<WaddleCalendarDate>,
    pub end: Option<WaddleCalendarDate>,
    pub attendees: Vec<WaddleEventAttendee>,
}
fn to_ffi(id: String, event: VEvent) -> WaddleEvent {
    WaddleEvent {
        id,
        uid: event.uid,
        summary: event.summary,
        description: event.description,
        location: event.location,
        organizer: event.organizer,
        workspace_room: event.workspace_room.map(|room| room.to_string()),
        start: event.dtstart.map(Into::into),
        end: event.dtend.map(Into::into),
        attendees: event
            .attendees
            .into_iter()
            .map(|attendee| WaddleEventAttendee {
                uri: attendee.uri,
                status: attendee.partstat.into(),
            })
            .collect(),
    }
}
fn apply_input(mut item: CalendarItem, event: WaddleEvent) -> Result<CalendarItem, WaddleError> {
    if event.summary.trim().is_empty() {
        return Err(WaddleError::InvalidArgument);
    }
    let start: Option<CalendarDateValue> = event.start.map(TryInto::try_into).transpose()?;
    let end: Option<CalendarDateValue> = event.end.map(TryInto::try_into).transpose()?;
    if let (Some(start), Some(end)) = (start, end) {
        if start.kind() != end.kind() || end <= start {
            return Err(WaddleError::InvalidArgument);
        }
    }
    if start.is_none() && end.is_some() {
        return Err(WaddleError::InvalidArgument);
    }
    if item.master.rrule.is_some() && start != item.master.dtstart {
        return Err(WaddleError::InvalidArgument);
    }
    item.master.summary = event.summary.trim().to_string();
    item.master.description = event.description;
    item.master.location = event.location;
    item.master.workspace_room = event
        .workspace_room
        .map(|room| room.parse())
        .transpose()
        .map_err(|_| WaddleError::InvalidJid)?;
    item.master.dtstart = start;
    item.master.dtend = end;
    item.master.dtstamp = Some(Utc::now());
    Ok(item)
}
#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    pub async fn fetch_community_events(
        &self,
        community_jid: String,
    ) -> Result<Vec<WaddleEvent>, WaddleError> {
        let service: BareJid = community_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = build_pubsub_items_iq(
            &Uuid::new_v4().to_string(),
            Some(&service),
            PUBSUB_NODE_EVENTS,
            None,
        );
        let reply = send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|error| client_error_to_waddle(&error))?;
        Ok(parse_pubsub_items_result(&reply)
            .into_iter()
            .filter_map(|item| {
                item.payload("vcalendar", NS_XCAL)
                    .and_then(|payload| parse_vcalendar_item(&item.id, payload))
                    .map(|calendar| to_ffi(item.id.clone(), calendar.master))
            })
            .collect())
    }
    /// Empty id creates; updates preserve recurrence, overrides and attendees.
    pub async fn publish_community_event(
        &self,
        community_jid: String,
        event: WaddleEvent,
    ) -> Result<WaddleEvent, WaddleError> {
        let service: BareJid = community_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let id = if event.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            event.id.clone()
        };
        let item = if event.id.is_empty() {
            let mut master = VEvent::new(&id, &event.summary);
            master.organizer = Some(format!(
                "xmpp:{}",
                self.config
                    .jid
                    .parse::<jid::Jid>()
                    .map_err(|_| WaddleError::InvalidJid)?
                    .to_bare()
            ));
            CalendarItem::new(master)
        } else {
            let iq = build_pubsub_items_iq(
                &Uuid::new_v4().to_string(),
                Some(&service),
                PUBSUB_NODE_EVENTS,
                None,
            );
            let reply = send_iq_with_timeout(&handle, iq)
                .await
                .map_err(|error| client_error_to_waddle(&error))?;
            parse_pubsub_items_result(&reply)
                .into_iter()
                .find(|item| item.id == id)
                .and_then(|item| {
                    item.payload("vcalendar", NS_XCAL)
                        .and_then(|payload| parse_vcalendar_item(&id, payload))
                })
                .ok_or(WaddleError::InvalidArgument)?
        };
        let item = apply_input(item, event)?;
        let iq = build_pubsub_publish_iq(
            &Uuid::new_v4().to_string(),
            Some(&service),
            PUBSUB_NODE_EVENTS,
            Some(&id),
            build_vcalendar_with_item(&item),
            None,
        );
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|error| client_error_to_waddle(&error))?;
        Ok(to_ffi(id, item.master))
    }
    pub async fn retract_community_event(
        &self,
        community_jid: String,
        item_id: String,
    ) -> Result<(), WaddleError> {
        let service: BareJid = community_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        if item_id.trim().is_empty() {
            return Err(WaddleError::InvalidArgument);
        }
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = build_pubsub_retract_iq(
            &Uuid::new_v4().to_string(),
            Some(&service),
            PUBSUB_NODE_EVENTS,
            &item_id,
        );
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|error| client_error_to_waddle(&error))?;
        Ok(())
    }
    pub async fn rsvp_community_event(
        &self,
        community_jid: String,
        uid: String,
        self_jid: String,
        status: WaddleEventParticipation,
    ) -> Result<(), WaddleError> {
        let service: BareJid = community_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let account = self
            .config
            .jid
            .parse::<jid::Jid>()
            .map_err(|_| WaddleError::InvalidJid)?
            .to_bare();
        let supplied: BareJid = self_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        if account != supplied || uid.trim().is_empty() {
            return Err(WaddleError::InvalidArgument);
        }
        let localpart = account.node().ok_or(WaddleError::InvalidJid)?.as_str();
        let item_id = format!("{uid}-rsvp-{localpart}");
        let master = VEvent::new(uid, "")
            .with_dtstamp(Utc::now())
            .with_attendees([Attendee::new(format!("xmpp:{account}"), status.into())]);
        let iq = build_pubsub_publish_iq(
            &Uuid::new_v4().to_string(),
            Some(&service),
            PUBSUB_NODE_EVENTS,
            Some(&item_id),
            build_vcalendar_with_item(&CalendarItem::new(master)),
            None,
        );
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|error| client_error_to_waddle(&error))?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> WaddleEvent {
        to_ffi("trip".into(), VEvent::new("trip", "London"))
    }
    #[test]
    fn accepts_undated_planning_but_rejects_invalid_ranges() {
        assert!(apply_input(CalendarItem::new(VEvent::new("trip", "London")), input()).is_ok());
        let mut event = input();
        event.start = Some(WaddleCalendarDate::Date {
            date: "2027-02-12".into(),
        });
        event.end = Some(WaddleCalendarDate::Date {
            date: "2027-02-11".into(),
        });
        assert!(apply_input(CalendarItem::new(VEvent::new("trip", "London")), event).is_err());
    }
    #[test]
    fn dates_round_trip_and_mixed_types_are_rejected() {
        let date = CalendarDateValue::try_from(WaddleCalendarDate::Date {
            date: "2027-02-12".into(),
        })
        .expect("valid date");
        assert_eq!(
            CalendarDateValue::try_from(WaddleCalendarDate::from(date)).expect("round trip"),
            date
        );
        let mut event = input();
        event.start = Some(WaddleCalendarDate::from(date));
        event.end = Some(WaddleCalendarDate::DateTime {
            timestamp_ms: 1_900_000_000_000,
        });
        assert!(apply_input(CalendarItem::new(VEvent::new("trip", "London")), event).is_err());
    }
    #[test]
    fn editing_preserves_recurrence_and_attendees() {
        use waddle_xmpp_core::xcal::{Freq, Rrule};
        let master = VEvent::new("trip", "London")
            .with_rrule(Rrule::new(Freq::Weekly))
            .with_attendees([Attendee::new("xmpp:alex@example.test", PartStat::Accepted)]);
        let original = CalendarItem::new(master);
        let mut event = to_ffi("trip".into(), original.master.clone());
        event.summary = "London maintainers".into();
        let changed = apply_input(original.clone(), event).expect("valid edit");
        assert_eq!(changed.master.rrule, original.master.rrule);
        assert_eq!(changed.master.attendees, original.master.attendees);
        assert_eq!(changed.master.summary, "London maintainers");
    }
}
