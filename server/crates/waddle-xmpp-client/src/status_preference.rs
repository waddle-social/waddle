//! Cross-device manual-status sync, receive side (ADR-010 decision 5).
//!
//! The user's picked presence mode lives as one PEP item (`id = current`)
//! on their own account in the Waddle-custom
//! `urn:waddle:status-preference:0` node (XEP-0223 private storage). A
//! resource reads it with an items request on ready and follows the
//! XEP-0163 `+notify` event (advertised in [`crate::caps`]) when another
//! resource of the same account publishes a new pick.
//!
//! Both paths accept only the account's own PEP service (XEP-0223
//! §Security Considerations, CVE-2023-28686): a contact must not be able
//! to inject a forged preference into the user's devices.

use jid::{BareJid, Jid};
use minidom::Element;
use waddle_xmpp_core::waddle_status_preference::{
    StatusPreference, NS_WADDLE_STATUS_PREFERENCE, PEP_ITEM_WADDLE_STATUS_PREFERENCE,
    PEP_NODE_WADDLE_STATUS_PREFERENCE,
};

use crate::pep::NS_PUBSUB;
use crate::pubsub_event::{PubsubEvent, PubsubEventPayload};

const NS_CLIENT: &str = "jabber:client";
const STATUS_PREFERENCE_ELEMENT: &str = "status-preference";

/// XEP-0060 §6.5.7 request for the single stored item.
pub fn build_status_preference_fetch_iq(iq_id: &str) -> Element {
    let items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            PEP_NODE_WADDLE_STATUS_PREFERENCE,
        )
        .attr(minidom::rxml::xml_ncname!("max_items").to_owned(), "1")
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id)
        .append(pubsub)
        .build()
}

/// Whether a fetch result may be trusted: an absent `from` is the
/// account's own PEP service; anything but the account's bare JID is
/// treated as spoofed (XEP-0223 §Security Considerations).
pub fn fetch_result_from_matches_account(result_from: Option<&Jid>, account: &BareJid) -> bool {
    result_from.is_none_or(|from| is_own_bare(from, account))
}

/// The stored preference from a fetch result. `None` when nothing is
/// stored or the payload is malformed, which means Automatic: absence
/// must never pin a status.
pub fn parse_status_preference_fetch_result(iq: &Element) -> Option<StatusPreference> {
    let items = iq
        .get_child("pubsub", NS_PUBSUB)?
        .get_child("items", NS_PUBSUB)
        .filter(|items| items.attr("node") == Some(PEP_NODE_WADDLE_STATUS_PREFERENCE))?;
    items
        .children()
        .filter(|item| item.is("item", NS_PUBSUB))
        .find_map(|item| {
            item.get_child(STATUS_PREFERENCE_ELEMENT, NS_WADDLE_STATUS_PREFERENCE)
                .and_then(|payload| StatusPreference::parse(payload).ok())
        })
}

/// A live pick from another resource of [`account`], from a PEP
/// notification on the status-preference node. A retraction of the
/// `current` item means Automatic. Notifications from anyone else, and
/// malformed payloads, yield `None`.
pub fn parse_status_preference_event(
    event: &PubsubEvent,
    account: &BareJid,
) -> Option<StatusPreference> {
    if event.node != PEP_NODE_WADDLE_STATUS_PREFERENCE {
        return None;
    }
    // XEP-0163 §4.3: PEP notifications come from the owner's bare JID.
    if !event
        .from
        .as_ref()
        .is_some_and(|from| is_own_bare(from, account))
    {
        return None;
    }
    event.items.iter().rev().find_map(|item| {
        if item.retracted {
            return (item.id.as_deref() == Some(PEP_ITEM_WADDLE_STATUS_PREFERENCE))
                .then_some(StatusPreference::Automatic);
        }
        let PubsubEventPayload::Opaque { element } = &item.payload else {
            return None;
        };
        if !element.is(STATUS_PREFERENCE_ELEMENT, NS_WADDLE_STATUS_PREFERENCE) {
            return None;
        }
        StatusPreference::parse(element).ok()
    })
}

fn is_own_bare(from: &Jid, account: &BareJid) -> bool {
    from.resource().is_none() && from.to_bare() == *account
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pubsub_event::parse_pubsub_events;
    use waddle_xmpp_core::waddle_status_preference::ManualStatus;

    fn account() -> BareJid {
        "alice@example.com".parse().expect("bare JID")
    }

    fn event_message(from: &str, item: Element) -> Element {
        let items = Element::builder("items", crate::pep::NS_PUBSUB_EVENT)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                PEP_NODE_WADDLE_STATUS_PREFERENCE,
            )
            .append(item)
            .build();
        let event = Element::builder("event", crate::pep::NS_PUBSUB_EVENT)
            .append(items)
            .build();
        Element::builder("message", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("from").to_owned(), from)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "headline")
            .append(event)
            .build()
    }

    fn published(pref: StatusPreference) -> Element {
        Element::builder("item", crate::pep::NS_PUBSUB_EVENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                PEP_ITEM_WADDLE_STATUS_PREFERENCE,
            )
            .append(pref.build_element())
            .build()
    }

    fn retracted() -> Element {
        Element::builder("retract", crate::pep::NS_PUBSUB_EVENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                PEP_ITEM_WADDLE_STATUS_PREFERENCE,
            )
            .build()
    }

    fn parse_event(message: &Element) -> Option<StatusPreference> {
        parse_pubsub_events(message)
            .iter()
            .find_map(|event| parse_status_preference_event(event, &account()))
    }

    #[test]
    fn own_pep_event_carries_the_manual_pick() {
        let message = event_message(
            "alice@example.com",
            published(StatusPreference::Manual(ManualStatus::Away)),
        );
        assert_eq!(
            parse_event(&message),
            Some(StatusPreference::Manual(ManualStatus::Away))
        );
    }

    #[test]
    fn reset_to_automatic_is_published_and_retraction_means_automatic() {
        let reset = event_message("alice@example.com", published(StatusPreference::Automatic));
        assert_eq!(parse_event(&reset), Some(StatusPreference::Automatic));
        let retract = event_message("alice@example.com", retracted());
        assert_eq!(parse_event(&retract), Some(StatusPreference::Automatic));
    }

    #[test]
    fn events_from_anyone_but_the_own_bare_jid_are_ignored() {
        for from in [
            "mallory@example.com",
            "alice@example.com/phone",
            "alice@evil.example",
        ] {
            let message =
                event_message(from, published(StatusPreference::Manual(ManualStatus::Dnd)));
            assert_eq!(parse_event(&message), None, "from {from}");
        }
    }

    #[test]
    fn fetch_iq_asks_for_the_single_item() {
        let iq = build_status_preference_fetch_iq("sp-1");
        let items = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .expect("items");
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("id"), Some("sp-1"));
        assert_eq!(items.attr("node"), Some(PEP_NODE_WADDLE_STATUS_PREFERENCE));
        assert_eq!(items.attr("max_items"), Some("1"));
    }

    #[test]
    fn fetch_result_parses_the_stored_pick_and_absence_is_none() {
        let item = Element::builder("item", NS_PUBSUB)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                PEP_ITEM_WADDLE_STATUS_PREFERENCE,
            )
            .append(StatusPreference::Manual(ManualStatus::Chat).build_element())
            .build();
        let items = Element::builder("items", NS_PUBSUB)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                PEP_NODE_WADDLE_STATUS_PREFERENCE,
            )
            .append(item)
            .build();
        let result = Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(Element::builder("pubsub", NS_PUBSUB).append(items).build())
            .build();
        assert_eq!(
            parse_status_preference_fetch_result(&result),
            Some(StatusPreference::Manual(ManualStatus::Chat))
        );
        let empty = Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .build();
        assert_eq!(parse_status_preference_fetch_result(&empty), None);
    }

    #[test]
    fn fetch_result_from_guard_accepts_only_the_own_account() {
        let own: Jid = "alice@example.com".parse().expect("jid");
        let other: Jid = "mallory@example.com".parse().expect("jid");
        let full: Jid = "alice@example.com/phone".parse().expect("jid");
        assert!(fetch_result_from_matches_account(None, &account()));
        assert!(fetch_result_from_matches_account(Some(&own), &account()));
        assert!(!fetch_result_from_matches_account(Some(&other), &account()));
        assert!(!fetch_result_from_matches_account(Some(&full), &account()));
    }
}
