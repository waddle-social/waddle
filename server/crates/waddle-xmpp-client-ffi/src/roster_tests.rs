//! FFI tests for the roster export: the mapping from an RFC 6121 roster
//! result to the FFI records, and the typed failures.

use minidom::Element;

use waddle_xmpp_client::messaging::NS_CLIENT;
use waddle_xmpp_client::ROSTER_NS;

use crate::roster::{roster_contacts_from_result, WaddleRosterContact};
use crate::test_support::offline_client;
use crate::WaddleError;

fn attr(name: &'static str) -> minidom::rxml::NcName {
    name.try_into().expect("attribute name")
}

fn item(jid: &str, name: Option<&str>, subscription: &str) -> Element {
    let item = Element::builder("item", ROSTER_NS)
        .attr(attr("jid"), jid)
        .attr(attr("subscription"), subscription);
    match name {
        Some(name) => item.attr(attr("name"), name),
        None => item,
    }
    .build()
}

fn result(query: Option<Element>) -> Element {
    let iq = Element::builder("iq", NS_CLIENT).attr(attr("type"), "result");
    match query {
        Some(query) => iq.append(query),
        None => iq,
    }
    .build()
}

fn roster(items: Vec<Element>) -> Element {
    result(Some(
        Element::builder("query", ROSTER_NS)
            .append_all(items)
            .build(),
    ))
}

#[test]
fn contacts_carry_their_address_and_saved_name() {
    let contacts = roster_contacts_from_result(&roster(vec![
        item("romeo@waddle.test", Some("Romeo"), "both"),
        item("nurse@waddle.test", None, "to"),
        item("friar@waddle.test", Some("  "), "none"),
    ]))
    .expect("roster reads");
    assert_eq!(
        contacts,
        vec![
            WaddleRosterContact {
                jid: "romeo@waddle.test".to_string(),
                name: Some("Romeo".to_string()),
            },
            WaddleRosterContact {
                jid: "nurse@waddle.test".to_string(),
                name: None,
            },
            WaddleRosterContact {
                jid: "friar@waddle.test".to_string(),
                name: None,
            },
        ]
    );
}

#[test]
fn an_account_with_no_contacts_has_an_empty_roster() {
    assert_eq!(roster_contacts_from_result(&roster(vec![])), Ok(vec![]));
}

#[test]
fn a_removed_contact_is_left_out() {
    let contacts = roster_contacts_from_result(&roster(vec![
        item("romeo@waddle.test", None, "both"),
        item("paris@waddle.test", None, "remove"),
    ]))
    .expect("roster reads");
    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0].jid, "romeo@waddle.test");
}

/// A reply the roster cannot be read from is not "no contacts".
#[test]
fn a_result_that_cannot_be_read_is_an_error() {
    assert_eq!(
        roster_contacts_from_result(&result(None)),
        Err(WaddleError::MalformedResponse)
    );
    assert_eq!(
        roster_contacts_from_result(&roster(vec![Element::builder("item", ROSTER_NS).build()])),
        Err(WaddleError::MalformedResponse)
    );
}

#[tokio::test]
async fn fetch_roster_contacts_requires_connection() {
    assert_eq!(
        offline_client().fetch_roster_contacts().await.unwrap_err(),
        WaddleError::NotConnected
    );
}
