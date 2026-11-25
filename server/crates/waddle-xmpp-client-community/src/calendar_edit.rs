//! Fetch and edit one calendar item without replacing unedited xCal fields.

use chrono::Utc;
use jid::BareJid;
use minidom::Element;
use serde::Deserialize;
use uuid::Uuid;
use waddle_xmpp_client::pubsub::{
    build_pubsub_item_iq, build_pubsub_publish_iq, parse_pubsub_items_result,
};
use waddle_xmpp_core::xcal::{
    build_vcalendar_with_event, parse_vcalendar_item, parse_vevent_element, CalendarDateValue,
    VEvent, NS_XCAL, PUBSUB_NODE_EVENTS,
};

use crate::calendar::{vevent_from_input, WaddleCalendarDateValue, WaddleVEventInput};
use crate::{invalid_input, InvalidInput};

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CalendarField {
    Summary,
    Location,
    Description,
    Dtstart,
    Dtend,
    Rrule,
}

impl CalendarField {
    fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Location => "location",
            Self::Description => "description",
            Self::Dtstart => "dtstart",
            Self::Dtend => "dtend",
            Self::Rrule => "rrule",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaddleCalendarEdit {
    pub occurrence: Option<WaddleCalendarDateValue>,
    pub event: Option<WaddleVEventInput>,
    pub fields: Vec<CalendarField>,
}

pub fn fetch_item(service: &BareJid, id: &str) -> Element {
    build_pubsub_item_iq(
        &Uuid::new_v4().to_string(),
        Some(service),
        PUBSUB_NODE_EVENTS,
        id,
    )
}

fn replace_property(component: &mut Element, source: &Element, key: &str) {
    while component.remove_child(key, NS_XCAL).is_some() {}
    for value in source.children().filter(|e| e.is(key, NS_XCAL)) {
        component.append_child(value.clone());
    }
}

pub fn update_item(
    result: &Element,
    service: &BareJid,
    id: &str,
    edit: WaddleCalendarEdit,
) -> Result<Element, InvalidInput> {
    if result
        .attr("from")
        .is_some_and(|from| from != service.as_str())
    {
        return Err(invalid_input("unexpected calendar reply sender"));
    }
    let mut payload = parse_pubsub_items_result(result)
        .into_iter()
        .find(|item| item.id == id)
        .and_then(|item| item.payload("vcalendar", NS_XCAL).cloned())
        .ok_or_else(|| invalid_input("event no longer available"))?;
    let mut stored = parse_vcalendar_item(id, &payload)
        .ok_or_else(|| invalid_input("invalid calendar item"))?
        .master;
    let uid = stored.uid.clone();
    let occurrence: Option<CalendarDateValue> =
        edit.occurrence.map(TryInto::try_into).transpose()?;
    if let Some(slot) = occurrence {
        if stored.rrule.is_none()
            || stored
                .dtstart
                .is_none_or(|start| start.kind() != slot.kind())
        {
            return Err(invalid_input("invalid calendar occurrence"));
        }
    }
    let cancelling = edit.event.is_none();
    let mut fields = edit
        .fields
        .into_iter()
        .map(CalendarField::name)
        .collect::<Vec<_>>();
    let mut draft = if let Some(mut input) = edit.event {
        if input.summary.trim().is_empty() {
            return Err(invalid_input("an event requires a summary"));
        }
        // A title or location edit must keep an RRULE from a more capable calendar.
        if !fields.contains(&"rrule") {
            input.rrule = None;
        }
        let mut event = vevent_from_input(&uid, input)?;
        event.recurrence_id = occurrence;
        if let Some(slot) = occurrence {
            if fields.contains(&"rrule")
                || event
                    .dtstart
                    .is_some_and(|start| start.kind() != slot.kind())
            {
                return Err(invalid_input("invalid occurrence edit"));
            }
        }
        event
    } else {
        let slot = occurrence.ok_or_else(|| invalid_input("missing cancellation occurrence"))?;
        if !stored.exdates.contains(&slot) {
            stored.exdates.push(slot);
        }
        fields = vec!["exdate"];
        stored.clone()
    };
    draft.dtstamp = Some(Utc::now());
    let clear_exceptions = occurrence.is_none()
        && ((fields.contains(&"dtstart") && draft.dtstart != stored.dtstart)
            || (fields.contains(&"rrule") && draft.rrule.is_none()));
    if clear_exceptions {
        fields.push("exdate");
    }
    fields.push("dtstamp");
    let encoded = build_vcalendar_with_event(&draft);
    let template = encoded
        .get_child("vevent", NS_XCAL)
        .ok_or_else(|| invalid_input("invalid event"))?;
    let target = if cancelling { None } else { occurrence };
    let mut found = false;
    for mut node in payload.take_nodes() {
        if let Some(child) = node.as_element_mut().filter(|e| e.is("vevent", NS_XCAL)) {
            let typed = parse_vevent_element(id, child)
                .ok_or_else(|| invalid_input("invalid event component"))?;
            if typed.uid == uid {
                if clear_exceptions && typed.recurrence_id.is_some() {
                    continue;
                }
                if typed.recurrence_id == target {
                    for field in &fields {
                        replace_property(child, template, field);
                    }
                    found = true;
                }
            }
        }
        payload.append_node(node);
    }
    if !found {
        if target.is_none() {
            return Err(invalid_input("missing event master"));
        }
        let mut new_override = VEvent::new(uid, "").with_recurrence_id_value(
            target.ok_or_else(|| invalid_input("missing recurrence ID"))?,
        );
        new_override.dtstamp = draft.dtstamp;
        let empty = build_vcalendar_with_event(&new_override);
        let mut component = empty
            .get_child("vevent", NS_XCAL)
            .ok_or_else(|| invalid_input("invalid override"))?
            .clone();
        for field in &fields {
            replace_property(&mut component, template, field);
        }
        payload.append_child(component);
    }
    Ok(build_pubsub_publish_iq(
        &Uuid::new_v4().to_string(),
        Some(service),
        PUBSUB_NODE_EVENTS,
        Some(id),
        payload,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp_client::pep::NS_PUBSUB;

    const FIXTURE: &str = "<iq xmlns='jabber:client' type='result' from='community.example.test'><pubsub xmlns='http://jabber.org/protocol/pubsub'><items><item id='one'><vcalendar xmlns='urn:ietf:params:xml:ns:xcal'><version>2.0</version><vevent><uid>one</uid><summary>Daily</summary><dtstart><date>2026-11-01</date></dtstart><rrule><freq>DAILY</freq></rrule><workspace xmlns='urn:waddle:event-planning:0' room='trip@muc.example.test'/><private xmlns='urn:example:extension'>preserve</private></vevent></vcalendar></item></items></pubsub></iq>";

    fn edit(
        fields: Vec<CalendarField>,
        occurrence: Option<&str>,
        input: bool,
    ) -> WaddleCalendarEdit {
        WaddleCalendarEdit {
            occurrence: occurrence.map(|date| WaddleCalendarDateValue::Date { date: date.into() }),
            fields,
            event: input.then(|| WaddleVEventInput {
                summary: "Renamed".into(),
                description: None,
                location: None,
                organizer: None,
                dtstart: Some(WaddleCalendarDateValue::Date {
                    date: "2026-11-01".into(),
                }),
                dtend: None,
                rrule: None,
            }),
        }
    }

    fn published(fixture: &Element, change: WaddleCalendarEdit) -> Element {
        let service = "community.example.test".parse().unwrap();
        let iq = update_item(fixture, &service, "one", change).unwrap();
        assert_eq!(iq.attr("to"), Some("community.example.test"));
        iq.get_child("pubsub", NS_PUBSUB)
            .unwrap()
            .get_child("publish", NS_PUBSUB)
            .unwrap()
            .get_child("item", NS_PUBSUB)
            .unwrap()
            .get_child("vcalendar", NS_XCAL)
            .unwrap()
            .clone()
    }

    #[test]
    fn series_rename_preserves_foreign_properties_workspace_room_and_rule() {
        let out = published(
            &FIXTURE.parse().unwrap(),
            edit(vec![CalendarField::Summary], None, true),
        );
        let component = out.get_child("vevent", NS_XCAL).unwrap();
        assert_eq!(
            component
                .get_child("private", "urn:example:extension")
                .unwrap()
                .text(),
            "preserve"
        );
        let event = parse_vcalendar_item("one", &out).unwrap().master;
        assert_eq!(event.summary, "Renamed");
        assert_eq!(
            event.workspace_room.unwrap().as_str(),
            "trip@muc.example.test"
        );
        assert!(event.rrule.is_some());
    }

    #[test]
    fn single_edit_and_cancel_address_the_original_date_without_changing_the_series() {
        let original = FIXTURE.parse().unwrap();
        let out = published(
            &original,
            edit(vec![CalendarField::Summary], Some("2026-11-02"), true),
        );
        let item = parse_vcalendar_item("one", &out).unwrap();
        assert_eq!(item.master.summary, "Daily");
        assert_eq!(item.overrides.len(), 1);
        assert_eq!(item.overrides[0].summary, "Renamed");
        let out = published(&original, edit(vec![], Some("2026-11-02"), false));
        let master = parse_vcalendar_item("one", &out).unwrap().master;
        assert!(master.rrule.is_some());
        assert_eq!(master.exdates.len(), 1);
    }
}
