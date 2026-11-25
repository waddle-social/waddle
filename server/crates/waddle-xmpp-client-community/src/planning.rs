use chrono::{DateTime, Utc};
use jid::BareJid;
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::pubsub::{
    build_pubsub_item_iq, build_pubsub_items_iq, build_pubsub_publish_iq, build_pubsub_retract_iq,
    parse_pubsub_items_result,
};
use waddle_xmpp_core::event_planning::NODE_EVENT_PLANNING;
use waddle_xmpp_core::event_planning::{
    AttendanceStatus, PlanningItem, PlanningKind, PlanningOption, PlanningStatus,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanningOptionInput {
    id: String,
    label: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddlePlanningItem {
    id: String,
    kind: String,
    author: Option<String>,
    title: String,
    description: Option<String>,
    url: Option<String>,
    category: Option<String>,
    location: Option<String>,
    timezone: Option<String>,
    starts_at: Option<String>,
    ends_at: Option<String>,
    assignee: Option<String>,
    #[serde(default)]
    completed: bool,
    #[serde(default)]
    pinned: bool,
    amount_minor: Option<i64>,
    currency: Option<String>,
    #[serde(default)]
    participants: Vec<String>,
    #[serde(default)]
    options: Vec<PlanningOptionInput>,
    #[serde(default)]
    selections: Vec<String>,
    subject: Option<String>,
    attendance: Option<String>,
    status: Option<String>,
}

impl WaddlePlanningItem {
    fn into_core(self) -> Result<PlanningItem, String> {
        let kind = PlanningKind::from_token(&self.kind).ok_or("unknown planning item kind")?;
        let mut item = PlanningItem::new(self.id, kind, self.title);
        item.author = self
            .author
            .map(|value| value.parse())
            .transpose()
            .map_err(|_| "invalid author JID")?;
        item.description = self.description;
        item.url = self
            .url
            .map(|value| value.parse())
            .transpose()
            .map_err(|_| "invalid URL")?;
        item.category = self.category;
        item.location = self.location;
        item.timezone = self
            .timezone
            .map(|value| value.parse())
            .transpose()
            .map_err(|_| "invalid IANA time zone")?;
        item.starts_at = parse_timestamp(self.starts_at)?;
        item.ends_at = parse_timestamp(self.ends_at)?;
        item.assignee = self
            .assignee
            .map(|value| value.parse())
            .transpose()
            .map_err(|_| "invalid assignee JID")?;
        item.completed = self.completed;
        item.pinned = self.pinned;
        item.amount_minor = self.amount_minor;
        item.currency = self.currency;
        item.participants = self
            .participants
            .into_iter()
            .map(|value| value.parse())
            .collect::<Result<_, _>>()
            .map_err(|_| "invalid participant JID")?;
        item.options = self
            .options
            .into_iter()
            .map(|value| PlanningOption {
                id: value.id,
                label: value.label,
            })
            .collect();
        item.selections = self.selections;
        item.subject = self.subject;
        item.attendance = self
            .attendance
            .map(|value| AttendanceStatus::from_token(&value).ok_or("invalid attendance status"))
            .transpose()?;
        item.status = self
            .status
            .map(|value| PlanningStatus::from_token(&value).ok_or("invalid planning status"))
            .transpose()?;
        item.validate().map_err(|error| error.to_string())?;
        Ok(item)
    }
}

fn parse_timestamp(value: Option<String>) -> Result<Option<DateTime<Utc>>, String> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| "invalid RFC3339 timestamp".to_string())
        })
        .transpose()
}

impl From<PlanningItem> for WaddlePlanningItem {
    fn from(item: PlanningItem) -> Self {
        Self {
            id: item.id,
            kind: item.kind.as_str().to_string(),
            author: item.author.map(|value| value.to_string()),
            title: item.title,
            description: item.description,
            url: item.url.map(|value| value.to_string()),
            category: item.category,
            location: item.location,
            timezone: item.timezone.map(|value| value.to_string()),
            starts_at: item.starts_at.map(|value| value.to_rfc3339()),
            ends_at: item.ends_at.map(|value| value.to_rfc3339()),
            assignee: item.assignee.map(|value| value.to_string()),
            completed: item.completed,
            pinned: item.pinned,
            amount_minor: item.amount_minor,
            currency: item.currency,
            participants: item
                .participants
                .into_iter()
                .map(|value| value.to_string())
                .collect(),
            options: item
                .options
                .into_iter()
                .map(|value| PlanningOptionInput {
                    id: value.id,
                    label: value.label,
                })
                .collect(),
            selections: item.selections,
            subject: item.subject,
            attendance: item.attendance.map(|value| value.as_str().to_string()),
            status: item.status.map(|value| value.as_str().to_string()),
        }
    }
}

pub fn items(room: &BareJid, id: Option<&str>) -> Element {
    let request = Uuid::new_v4().to_string();
    match id {
        Some(id) => build_pubsub_item_iq(&request, Some(room), NODE_EVENT_PLANNING, id),
        None => build_pubsub_items_iq(&request, Some(room), NODE_EVENT_PLANNING, None),
    }
}

pub fn parse_items(reply: &Element, room: &BareJid) -> Result<Vec<WaddlePlanningItem>, String> {
    if reply.attr("from").is_some_and(|from| from != room.as_str()) {
        return Err("unexpected planning service".into());
    }
    let valid_node = reply
        .get_child("pubsub", waddle_xmpp_client::pep::NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", waddle_xmpp_client::pep::NS_PUBSUB))
        .and_then(|items| items.attr("node"));
    if valid_node != Some(NODE_EVENT_PLANNING) {
        return Err("missing planning items node".into());
    }
    parse_pubsub_items_result(reply)
        .into_iter()
        .map(|entry| {
            let payload = entry
                .payload("item", NODE_EVENT_PLANNING)
                .ok_or("missing planning item")?;
            let item = PlanningItem::from_element(payload).map_err(|err| err.to_string())?;
            if item.id != entry.id {
                return Err("planning item identity mismatch".into());
            }
            Ok(item.into())
        })
        .collect()
}

pub fn publish(
    room: &BareJid,
    owner: BareJid,
    input: WaddlePlanningItem,
) -> Result<(String, Element), String> {
    let mut item = input.into_core()?;
    if let Some(id) = item.self_owned_id(&owner) {
        item.id = id;
    }
    item.author = Some(owner);
    item.validate().map_err(|err| err.to_string())?;
    let iq = build_pubsub_publish_iq(
        &Uuid::new_v4().to_string(),
        Some(room),
        NODE_EVENT_PLANNING,
        Some(&item.id),
        item.to_element(),
        None,
    );
    Ok((item.id, iq))
}

pub fn retract(room: &BareJid, id: &str) -> Result<Element, String> {
    if id.is_empty() {
        return Err("missing planning item id".into());
    }
    Ok(build_pubsub_retract_iq(
        &Uuid::new_v4().to_string(),
        Some(room),
        NODE_EVENT_PLANNING,
        id,
    ))
}

pub fn published(reply: &Element, room: &BareJid, id: &str) -> Result<WaddlePlanningItem, String> {
    parse_items(reply, room)?
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| "saved planning item unavailable".into())
}
#[cfg(test)]
mod tests {
    use super::WaddlePlanningItem;
    use waddle_xmpp_core::event_planning::{PlanningItem, PlanningKind};

    #[test]
    fn js_round_trip_preserves_event_schedule_and_time_zone() {
        let mut event =
            PlanningItem::new("event", PlanningKind::Event, "London maintainers meetup");
        event.timezone = Some("Europe/London".parse().expect("zone"));
        event.starts_at = Some("2027-02-12T10:00:00Z".parse().expect("instant"));
        event.author = Some("david@example.com".parse().expect("jid"));
        assert_eq!(
            WaddlePlanningItem::from(event.clone())
                .into_core()
                .expect("round trip"),
            event
        );
    }

    #[test]
    fn invalid_boundary_fields_are_rejected() {
        let item = PlanningItem::new("link-1", PlanningKind::Link, "Hotel");
        let mut wire = WaddlePlanningItem::from(item);
        wire.url = Some("javascript:alert(1)".to_string());
        assert!(wire.into_core().is_err());
        let mut wire =
            WaddlePlanningItem::from(PlanningItem::new("event", PlanningKind::Event, "Meetup"));
        wire.timezone = Some("Atlantis/Nowhere".to_string());
        assert!(wire.into_core().is_err());
    }
}
