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
use waddle_xmpp_client::ClientHandle;
use waddle_xmpp_core::xcal::{
    build_vcalendar_with_item, parse_vcalendar_item, Attendee, CalendarDateValue, CalendarItem,
    Freq, PartStat, Rrule, RruleEnd, VEvent, Weekday, NS_XCAL, PUBSUB_NODE_EVENTS,
};
use waddle_xmpp_core::xcal_expansion::{expand_occurrences, Occurrence};
/// Separates an event's uid from the account in the id of an RSVP item.
const RSVP_ITEM_MARKER: &str = "-rsvp-";
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleRecurrenceFrequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}
impl From<WaddleRecurrenceFrequency> for Freq {
    fn from(value: WaddleRecurrenceFrequency) -> Self {
        match value {
            WaddleRecurrenceFrequency::Daily => Self::Daily,
            WaddleRecurrenceFrequency::Weekly => Self::Weekly,
            WaddleRecurrenceFrequency::Monthly => Self::Monthly,
            WaddleRecurrenceFrequency::Yearly => Self::Yearly,
        }
    }
}
impl From<Freq> for WaddleRecurrenceFrequency {
    fn from(value: Freq) -> Self {
        match value {
            Freq::Daily => Self::Daily,
            Freq::Weekly => Self::Weekly,
            Freq::Monthly => Self::Monthly,
            Freq::Yearly => Self::Yearly,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleWeekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}
impl From<WaddleWeekday> for Weekday {
    fn from(value: WaddleWeekday) -> Self {
        match value {
            WaddleWeekday::Sunday => Self::Sunday,
            WaddleWeekday::Monday => Self::Monday,
            WaddleWeekday::Tuesday => Self::Tuesday,
            WaddleWeekday::Wednesday => Self::Wednesday,
            WaddleWeekday::Thursday => Self::Thursday,
            WaddleWeekday::Friday => Self::Friday,
            WaddleWeekday::Saturday => Self::Saturday,
        }
    }
}
impl From<Weekday> for WaddleWeekday {
    fn from(value: Weekday) -> Self {
        match value {
            Weekday::Sunday => Self::Sunday,
            Weekday::Monday => Self::Monday,
            Weekday::Tuesday => Self::Tuesday,
            Weekday::Wednesday => Self::Wednesday,
            Weekday::Thursday => Self::Thursday,
            Weekday::Friday => Self::Friday,
            Weekday::Saturday => Self::Saturday,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleRecurrenceEnd {
    Never,
    /// The series has this many occurrences, cancelled ones included.
    Count {
        count: u32,
    },
    /// The last occurrence starts at or before this instant.
    Until {
        timestamp_ms: i64,
    },
}
/// How an event repeats. Occurrences are worked out by the caller, in UTC.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WaddleRecurrence {
    pub frequency: WaddleRecurrenceFrequency,
    /// Every `interval` days, weeks, months or years; at least 1.
    pub interval: u32,
    /// Empty means the weekday of the event's start.
    pub weekdays: Vec<WaddleWeekday>,
    /// 1 to 31, or -1 to -31 counted from the end of the month.
    pub month_days: Vec<i32>,
    pub end: WaddleRecurrenceEnd,
}
impl TryFrom<WaddleRecurrence> for Rrule {
    type Error = WaddleError;
    fn try_from(value: WaddleRecurrence) -> Result<Self, Self::Error> {
        let mut rule = Rrule::new(value.frequency.into())
            .with_by_day(value.weekdays.into_iter().map(Into::into))
            .with_by_month_day(value.month_days);
        if value.interval > 1 {
            rule = rule.with_interval(value.interval);
        }
        match value.end {
            WaddleRecurrenceEnd::Never => Ok(rule),
            WaddleRecurrenceEnd::Count { count } => Ok(rule.with_count(count)),
            WaddleRecurrenceEnd::Until { timestamp_ms } => {
                DateTime::from_timestamp_millis(timestamp_ms)
                    .map(|until| rule.with_until(until))
                    .ok_or(WaddleError::InvalidArgument)
            }
        }
    }
}
/// The rule to store for a recurrence a caller wrote. Stricter than what is
/// read: only rules every client shows the same way are written.
fn authored_rule(recurrence: WaddleRecurrence) -> Result<Rrule, WaddleError> {
    let weekly = recurrence.frequency == WaddleRecurrenceFrequency::Weekly;
    let monthly = recurrence.frequency == WaddleRecurrenceFrequency::Monthly;
    let days_fit = (weekly || recurrence.weekdays.is_empty())
        && (monthly || recurrence.month_days.is_empty())
        && recurrence
            .month_days
            .iter()
            .all(|day| (1..=31).contains(&day.abs()));
    let counted = recurrence.end != WaddleRecurrenceEnd::Count { count: 0 };
    if recurrence.interval == 0 || !days_fit || !counted {
        return Err(WaddleError::InvalidArgument);
    }
    Rrule::try_from(recurrence)
}
/// Whether a series that starts at `start` can follow `rule`. Holds for
/// every rule written, a stored one handed back unchanged included.
fn rule_fits_start(rule: &Rrule, start: Option<CalendarDateValue>) -> bool {
    match (&rule.end, start) {
        (_, None) => false,
        // An all-day series cannot end on an instant: readers drop the item.
        (Some(RruleEnd::Until(_)), Some(CalendarDateValue::Date(_))) => false,
        (Some(RruleEnd::Until(until)), Some(CalendarDateValue::DateTime(start))) => *until >= start,
        _ => true,
    }
}
impl From<Rrule> for WaddleRecurrence {
    fn from(rule: Rrule) -> Self {
        Self {
            frequency: rule.freq.into(),
            interval: rule.interval.unwrap_or(1).max(1),
            weekdays: rule.by_day.into_iter().map(Into::into).collect(),
            month_days: rule.by_month_day,
            end: match rule.end {
                None => WaddleRecurrenceEnd::Never,
                Some(RruleEnd::Count(count)) => WaddleRecurrenceEnd::Count { count },
                Some(RruleEnd::Until(until)) => WaddleRecurrenceEnd::Until {
                    timestamp_ms: until.timestamp_millis(),
                },
            },
        }
    }
}
/// One occurrence of a repeating event that was changed on its own.
/// A missing field means the occurrence keeps the event's value.
#[derive(Clone, Debug, uniffi::Record)]
pub struct WaddleEventOverride {
    /// The start the occurrence would have had.
    pub recurrence_id: WaddleCalendarDate,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: Option<WaddleCalendarDate>,
    pub end: Option<WaddleCalendarDate>,
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
    /// Each account's latest RSVP. Publishing does not change them.
    pub attendees: Vec<WaddleEventAttendee>,
    pub recurrence: Option<WaddleRecurrence>,
    /// Starts of the cancelled occurrences. Publishing does not change them;
    /// `cancel_community_event_occurrence` adds one.
    pub excluded: Vec<WaddleCalendarDate>,
    /// Occurrences changed on their own. Publishing does not change them.
    pub overrides: Vec<WaddleEventOverride>,
}
/// One occurrence of an event, as `expand_community_event` works it out.
#[derive(Clone, Debug, uniffi::Record)]
pub struct WaddleEventOccurrence {
    /// The start the recurrence gives the occurrence: the value to cancel it
    /// by. `None` when the event does not repeat.
    pub recurrence_id: Option<WaddleCalendarDate>,
    pub start: WaddleCalendarDate,
    pub end: Option<WaddleCalendarDate>,
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
}
impl From<Occurrence> for WaddleEventOccurrence {
    fn from(occurrence: Occurrence) -> Self {
        Self {
            recurrence_id: occurrence.recurrence_id.map(Into::into),
            start: occurrence.start.into(),
            end: occurrence.end.map(Into::into),
            summary: occurrence.summary,
            description: occurrence.description,
            location: occurrence.location,
        }
    }
}
/// The stored shape of a fetched event, for working out its occurrences.
impl TryFrom<WaddleEvent> for CalendarItem {
    type Error = WaddleError;
    fn try_from(event: WaddleEvent) -> Result<Self, Self::Error> {
        let date =
            |value: Option<WaddleCalendarDate>| value.map(CalendarDateValue::try_from).transpose();
        let mut master = VEvent::new(&event.uid, event.summary);
        master.description = event.description;
        master.location = event.location;
        master.dtstart = date(event.start)?;
        master.dtend = date(event.end)?;
        master.rrule = event.recurrence.map(TryInto::try_into).transpose()?;
        master.exdates = event
            .excluded
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<_, _>>()?;
        let overrides = event
            .overrides
            .into_iter()
            .map(|changed| {
                let mut occurrence = VEvent::new(&event.uid, changed.summary.unwrap_or_default());
                occurrence.recurrence_id = Some(changed.recurrence_id.try_into()?);
                occurrence.description = changed.description;
                occurrence.location = changed.location;
                occurrence.dtstart = date(changed.start)?;
                occurrence.dtend = date(changed.end)?;
                Ok(occurrence)
            })
            .collect::<Result<_, WaddleError>>()?;
        Ok(Self { master, overrides })
    }
}
/// The occurrences of `event` that overlap `from_ms..to_ms`, earliest first.
/// Recurrences are walked in UTC, the same on every client. An all-day
/// occurrence sits at midnight UTC of its date, so a caller showing another
/// time zone should ask for a day more on each side.
#[uniffi::export]
pub fn expand_community_event(
    event: WaddleEvent,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<WaddleEventOccurrence>, WaddleError> {
    let instant = |ms| DateTime::from_timestamp_millis(ms).ok_or(WaddleError::InvalidArgument);
    let item = CalendarItem::try_from(event)?;
    Ok(
        expand_occurrences(&item, instant(from_ms)?, instant(to_ms)?)
            .into_iter()
            .map(Into::into)
            .collect(),
    )
}
fn to_ffi(id: String, item: CalendarItem) -> WaddleEvent {
    let event = item.master;
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
        recurrence: event.rrule.map(Into::into),
        excluded: event.exdates.into_iter().map(Into::into).collect(),
        overrides: item
            .overrides
            .into_iter()
            .filter_map(|changed| {
                Some(WaddleEventOverride {
                    recurrence_id: changed.recurrence_id?.into(),
                    summary: Some(changed.summary).filter(|summary| !summary.is_empty()),
                    description: changed.description,
                    location: changed.location,
                    start: changed.dtstart.map(Into::into),
                    end: changed.dtend.map(Into::into),
                })
            })
            .collect(),
    }
}
/// The uid of the event an RSVP item answers, if `item` is one. The server
/// accepts an RSVP only with one attendee and no summary.
fn rsvp_target<'a>(id: &'a str, item: &CalendarItem) -> Option<&'a str> {
    let (uid, account) = id.rsplit_once(RSVP_ITEM_MARKER)?;
    let is_rsvp = !uid.is_empty()
        && !account.is_empty()
        && item.master.summary.is_empty()
        && item.master.attendees.len() == 1;
    is_rsvp.then_some(uid)
}
/// Moves each account's RSVP item into the attendees of the event it
/// answers. An RSVP replaces what the event itself says about that attendee.
fn fold_rsvps(items: Vec<(String, CalendarItem)>) -> Vec<(String, CalendarItem)> {
    let (rsvps, mut events): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|(id, item)| rsvp_target(id, item).is_some());
    for (id, rsvp) in rsvps {
        let Some(uid) = rsvp_target(&id, &rsvp) else {
            continue;
        };
        let Some((_, event)) = events.iter_mut().find(|(_, event)| event.master.uid == uid) else {
            continue;
        };
        for answer in rsvp.master.attendees {
            match event
                .master
                .attendees
                .iter_mut()
                .find(|attendee| attendee.uri == answer.uri)
            {
                Some(attendee) => *attendee = answer,
                None => event.master.attendees.push(answer),
            }
        }
    }
    events
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
    // A rule the caller hands back as it was fetched is kept as stored, so
    // an edit of the title does not fail on a rule another client wrote.
    let stored_rule = item
        .master
        .rrule
        .take()
        .filter(|rule| Some(WaddleRecurrence::from(rule.clone())) == event.recurrence);
    let recurrence = match stored_rule {
        Some(rule) => Some(rule),
        None => event.recurrence.map(authored_rule).transpose()?,
    };
    if !recurrence
        .as_ref()
        .is_none_or(|rule| rule_fits_start(rule, start))
    {
        return Err(WaddleError::InvalidArgument);
    }
    // Cancelled and changed occurrences are named by the start they had.
    // Once the series moves, those names would point at other occurrences.
    if start != item.master.dtstart || recurrence.is_none() {
        item.master.exdates.clear();
        item.overrides.clear();
    }
    item.master.rrule = recurrence;
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
/// `item` with the occurrence that starts at `occurrence` cancelled.
fn cancel_occurrence(
    mut item: CalendarItem,
    occurrence: CalendarDateValue,
) -> Result<CalendarItem, WaddleError> {
    let same_kind = item
        .master
        .dtstart
        .is_some_and(|start| start.kind() == occurrence.kind());
    if item.master.rrule.is_none() || !same_kind {
        return Err(WaddleError::InvalidArgument);
    }
    if !item.master.exdates.contains(&occurrence) {
        item.master.exdates.push(occurrence);
        item.master.exdates.sort();
    }
    item.master.dtstamp = Some(Utc::now());
    Ok(item)
}
async fn fetch_calendar_items(
    handle: &ClientHandle,
    service: &BareJid,
) -> Result<Vec<(String, CalendarItem)>, WaddleError> {
    let iq = build_pubsub_items_iq(
        &Uuid::new_v4().to_string(),
        Some(service),
        PUBSUB_NODE_EVENTS,
        None,
    );
    let reply = send_iq_with_timeout(handle, iq)
        .await
        .map_err(|error| client_error_to_waddle(&error))?;
    Ok(parse_pubsub_items_result(&reply)
        .into_iter()
        .filter_map(|item| {
            let calendar = parse_vcalendar_item(&item.id, item.payload("vcalendar", NS_XCAL)?)?;
            Some((item.id, calendar))
        })
        .collect())
}
#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// Every event of the community, each with the RSVPs it has received.
    pub async fn fetch_community_events(
        &self,
        community_jid: String,
    ) -> Result<Vec<WaddleEvent>, WaddleError> {
        let service: BareJid = community_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let items = fetch_calendar_items(&handle, &service).await?;
        Ok(fold_rsvps(items)
            .into_iter()
            .map(|(id, item)| to_ffi(id, item))
            .collect())
    }
    /// Empty id creates. An update replaces the recurrence with the one given
    /// and keeps the RSVPs. Cancelled and changed occurrences are kept too,
    /// unless the start moves or the event stops repeating: they are named
    /// by the start they had.
    pub async fn publish_community_event(
        &self,
        community_jid: String,
        event: WaddleEvent,
    ) -> Result<WaddleEvent, WaddleError> {
        let service: BareJid = community_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        if !event.id.is_empty() {
            return self
                .rewrite_event(&service, &event.id.clone(), |stored| {
                    apply_input(stored, event)
                })
                .await;
        }
        let id = Uuid::new_v4().to_string();
        let mut master = VEvent::new(&id, &event.summary);
        master.organizer = Some(format!(
            "xmpp:{}",
            self.config
                .jid
                .parse::<jid::Jid>()
                .map_err(|_| WaddleError::InvalidJid)?
                .to_bare()
        ));
        let item = apply_input(CalendarItem::new(master), event)?;
        publish_calendar_item(&handle, &service, &id, &item).await?;
        Ok(to_ffi(id, item))
    }
    /// Cancels the one occurrence of a repeating event that starts at
    /// `occurrence` (its `recurrence_id`), keeping the rest of the series.
    pub async fn cancel_community_event_occurrence(
        &self,
        community_jid: String,
        item_id: String,
        occurrence: WaddleCalendarDate,
    ) -> Result<WaddleEvent, WaddleError> {
        let service: BareJid = community_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let occurrence = CalendarDateValue::try_from(occurrence)?;
        self.rewrite_event(&service, &item_id, |stored| {
            cancel_occurrence(stored, occurrence)
        })
        .await
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
impl WaddleClient {
    /// Fetches the stored event, publishes what `change` makes of it and
    /// returns it with its RSVPs. Works from the server's copy so a change
    /// from another device, made since the caller last fetched, is kept.
    async fn rewrite_event(
        &self,
        service: &BareJid,
        item_id: &str,
        change: impl FnOnce(CalendarItem) -> Result<CalendarItem, WaddleError>,
    ) -> Result<WaddleEvent, WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let mut items = fetch_calendar_items(&handle, service).await?;
        let index = items
            .iter()
            .position(|(id, item)| id == item_id && rsvp_target(id, item).is_none())
            .ok_or(WaddleError::InvalidArgument)?;
        let (id, stored) = items.swap_remove(index);
        let item = change(stored)?;
        publish_calendar_item(&handle, service, &id, &item).await?;
        items.push((id.clone(), item));
        fold_rsvps(items)
            .into_iter()
            .find(|(folded, _)| *folded == id)
            .map(|(id, item)| to_ffi(id, item))
            .ok_or(WaddleError::InvalidArgument)
    }
}
async fn publish_calendar_item(
    handle: &ClientHandle,
    service: &BareJid,
    id: &str,
    item: &CalendarItem,
) -> Result<(), WaddleError> {
    let iq = build_pubsub_publish_iq(
        &Uuid::new_v4().to_string(),
        Some(service),
        PUBSUB_NODE_EVENTS,
        Some(id),
        build_vcalendar_with_item(item),
        None,
    );
    send_iq_with_timeout(handle, iq)
        .await
        .map_err(|error| client_error_to_waddle(&error))?;
    Ok(())
}
#[cfg(test)]
#[path = "calendar_tests.rs"]
mod tests;
