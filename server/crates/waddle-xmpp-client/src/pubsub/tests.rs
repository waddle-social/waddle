//! Dedicated test suite for the XEP-0060 Publish-Subscribe client
//! helpers: items/publish/subscribe/retract IQ wire shapes, XEP-0223
//! publish-options preconditions, items/publish result parsing, and
//! error tolerance for malformed results.

use jid::BareJid;
use minidom::Element;

use super::*;

const SERVICE: &str = "community.waddle.test";
const NODE: &str = "urn:xmpp:pubsub-social-feed:1";

fn service() -> BareJid {
    SERVICE.parse().expect("valid service jid")
}

fn payload() -> Element {
    Element::builder("entry", "http://www.w3.org/2005/Atom")
        .append("hello")
        .build()
}

fn form_field_value(form: &Element, var: &str) -> Option<String> {
    form.children()
        .filter(|child| child.is("field", "jabber:x:data"))
        .find(|child| child.attr("var") == Some(var))
        .and_then(|field| field.get_child("value", "jabber:x:data"))
        .map(Element::text)
}

// ── Namespace / service constants ────────────────────────────────────

#[test]
fn publish_options_form_type_pins_the_disco_feature_var() {
    assert_eq!(
        NS_PUBSUB_PUBLISH_OPTIONS,
        crate::mds::NS_PUBSUB_PUBLISH_OPTIONS_FEATURE
    );
}

#[test]
fn community_service_jid_is_the_community_subdomain() {
    assert_eq!(
        community_service_jid("waddle.test"),
        "community.waddle.test"
    );
    assert!(community_service_jid("waddle.test")
        .parse::<BareJid>()
        .is_ok());
}

// ── Items fetch (§6.5) ───────────────────────────────────────────────

#[test]
fn items_iq_targets_service_node_without_max_items() {
    let iq = build_pubsub_items_iq("iq-1", Some(&service()), NODE, None);
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("id"), Some("iq-1"));
    assert_eq!(iq.attr("to"), Some(SERVICE));
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items element");
    assert_eq!(items.attr("node"), Some(NODE));
    assert_eq!(items.attr("max_items"), None);
}

#[test]
fn items_iq_carries_max_items_when_bounded() {
    let iq = build_pubsub_items_iq("iq-2", Some(&service()), NODE, Some(50));
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items element");
    assert_eq!(items.attr("max_items"), Some("50"));
}

#[test]
fn items_iq_without_service_routes_to_own_pep() {
    let iq = build_pubsub_items_iq("iq-3", None, NODE, Some(1));
    assert_eq!(iq.attr("to"), None);
}

// ── Publish (§7.1) ───────────────────────────────────────────────────

#[test]
fn publish_iq_wraps_payload_under_item_on_node() {
    let iq = build_pubsub_publish_iq(
        "iq-4",
        Some(&service()),
        NODE,
        Some("post-1"),
        payload(),
        None,
    );
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some(SERVICE));
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish element");
    assert_eq!(publish.attr("node"), Some(NODE));
    let item = publish.get_child("item", NS_PUBSUB).expect("item element");
    assert_eq!(item.attr("id"), Some("post-1"));
    let entry = item
        .get_child("entry", "http://www.w3.org/2005/Atom")
        .expect("payload survives");
    assert_eq!(entry.text(), "hello");
}

#[test]
fn publish_iq_omits_item_id_for_service_generated_ids() {
    let iq = build_pubsub_publish_iq("iq-5", Some(&service()), NODE, None, payload(), None);
    let item = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .and_then(|publish| publish.get_child("item", NS_PUBSUB))
        .expect("item element");
    assert_eq!(item.attr("id"), None);
}

#[test]
fn publish_iq_has_no_publish_options_unless_requested() {
    let iq = build_pubsub_publish_iq(
        "iq-6",
        Some(&service()),
        NODE,
        Some("post-1"),
        payload(),
        None,
    );
    let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub element");
    assert!(pubsub.get_child("publish-options", NS_PUBSUB).is_none());
}

// ── Publish options (§7.1.5 / XEP-0223) ──────────────────────────────

#[test]
fn publish_options_form_carries_every_configured_precondition() {
    let options = PubsubPublishOptions {
        persist_items: Some(true),
        access_model: Some(PubsubAccessModel::Whitelist),
        send_last_published_item: Some(PubsubSendLastPublishedItem::Never),
        max_items: Some(1),
    };
    let iq = build_pubsub_publish_iq(
        "iq-7",
        None,
        NODE,
        Some("current"),
        payload(),
        Some(&options),
    );
    let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub element");
    let form = pubsub
        .get_child("publish-options", NS_PUBSUB)
        .and_then(|options| options.get_child("x", "jabber:x:data"))
        .expect("submit form");
    assert_eq!(form.attr("type"), Some("submit"));
    assert_eq!(
        form_field_value(form, "FORM_TYPE").as_deref(),
        Some(NS_PUBSUB_PUBLISH_OPTIONS)
    );
    assert_eq!(
        form_field_value(form, "pubsub#persist_items").as_deref(),
        Some("true")
    );
    assert_eq!(
        form_field_value(form, "pubsub#access_model").as_deref(),
        Some("whitelist")
    );
    assert_eq!(
        form_field_value(form, "pubsub#send_last_published_item").as_deref(),
        Some("never")
    );
    assert_eq!(
        form_field_value(form, "pubsub#max_items").as_deref(),
        Some("1")
    );
}

#[test]
fn publish_options_form_type_field_is_hidden() {
    let form_owner = PubsubPublishOptions::default().to_element();
    let form = form_owner
        .get_child("x", "jabber:x:data")
        .expect("submit form");
    let form_type = form
        .children()
        .find(|child| child.attr("var") == Some("FORM_TYPE"))
        .expect("FORM_TYPE field");
    assert_eq!(form_type.attr("type"), Some("hidden"));
}

#[test]
fn publish_options_omit_unset_fields() {
    let options = PubsubPublishOptions {
        max_items: Some(1),
        ..PubsubPublishOptions::default()
    };
    let element = options.to_element();
    let form = element.get_child("x", "jabber:x:data").expect("form");
    assert!(form_field_value(form, "pubsub#persist_items").is_none());
    assert!(form_field_value(form, "pubsub#access_model").is_none());
    assert!(form_field_value(form, "pubsub#send_last_published_item").is_none());
    assert_eq!(
        form_field_value(form, "pubsub#max_items").as_deref(),
        Some("1")
    );
}

// ── Subscribe (§6.1) ─────────────────────────────────────────────────

#[test]
fn subscribe_iq_carries_node_and_subscriber_bare_jid() {
    let subscriber: BareJid = "romeo@waddle.test".parse().expect("valid jid");
    let iq = build_pubsub_subscribe_iq("iq-8", &service(), NODE, &subscriber);
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some(SERVICE));
    let subscribe = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("subscribe", NS_PUBSUB))
        .expect("subscribe element");
    assert_eq!(subscribe.attr("node"), Some(NODE));
    assert_eq!(subscribe.attr("jid"), Some("romeo@waddle.test"));
}

// ── Retract (§7.2) ───────────────────────────────────────────────────

#[test]
fn retract_iq_always_requests_subscriber_notification() {
    let iq = build_pubsub_retract_iq("iq-9", Some(&service()), NODE, "post-1");
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some(SERVICE));
    let retract = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("retract", NS_PUBSUB))
        .expect("retract element");
    assert_eq!(retract.attr("node"), Some(NODE));
    assert_eq!(retract.attr("notify"), Some("true"));
    let item = retract.get_child("item", NS_PUBSUB).expect("item element");
    assert_eq!(item.attr("id"), Some("post-1"));
}

// ── Items result parsing (§6.5.2) ────────────────────────────────────

#[test]
fn parse_items_result_returns_ids_with_typed_payloads() {
    let xml = "<iq xmlns='jabber:client' type='result' id='x'>\
        <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
          <items node='urn:xmpp:pubsub-social-feed:1'>\
            <item id='post-1'>\
              <entry xmlns='http://www.w3.org/2005/Atom'>first</entry>\
            </item>\
            <item id='post-2'>\
              <entry xmlns='http://www.w3.org/2005/Atom'>second</entry>\
            </item>\
          </items>\
        </pubsub>\
      </iq>";
    let iq: Element = xml.parse().expect("valid xml");
    let items = parse_pubsub_items_result(&iq);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, "post-1");
    assert_eq!(items[1].id, "post-2");
    let entry = items[0]
        .payload("entry", "http://www.w3.org/2005/Atom")
        .expect("entry payload");
    assert_eq!(entry.text(), "first");
}

#[test]
fn parse_items_result_finds_payload_among_multiple_children() {
    let xml = "<iq xmlns='jabber:client' type='result' id='x'>\
        <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
          <items node='n'>\
            <item id='post-1'>\
              <decoration xmlns='urn:example:decoration'/>\
              <entry xmlns='http://www.w3.org/2005/Atom'>real</entry>\
            </item>\
          </items>\
        </pubsub>\
      </iq>";
    let iq: Element = xml.parse().expect("valid xml");
    let items = parse_pubsub_items_result(&iq);
    let entry = items[0]
        .payload("entry", "http://www.w3.org/2005/Atom")
        .expect("entry payload");
    assert_eq!(entry.text(), "real");
    assert!(items[0].payload("missing", "urn:example:none").is_none());
}

#[test]
fn parse_items_result_skips_items_without_ids() {
    let xml = "<iq xmlns='jabber:client' type='result' id='x'>\
        <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
          <items node='n'>\
            <item><entry xmlns='http://www.w3.org/2005/Atom'/></item>\
            <item id='kept'/>\
          </items>\
        </pubsub>\
      </iq>";
    let iq: Element = xml.parse().expect("valid xml");
    let items = parse_pubsub_items_result(&iq);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "kept");
    assert!(items[0].payloads.is_empty());
}

#[test]
fn parse_items_result_tolerates_missing_wrappers() {
    let bare: Element = "<iq xmlns='jabber:client' type='result' id='x'/>"
        .parse()
        .expect("valid xml");
    assert!(parse_pubsub_items_result(&bare).is_empty());

    let empty: Element = "<iq xmlns='jabber:client' type='result' id='x'>\
        <pubsub xmlns='http://jabber.org/protocol/pubsub'/></iq>"
        .parse()
        .expect("valid xml");
    assert!(parse_pubsub_items_result(&empty).is_empty());
}

#[test]
fn parse_items_result_tolerates_error_iqs() {
    let error: Element = "<iq xmlns='jabber:client' type='error' id='x'>\
        <error type='cancel'>\
          <item-not-found xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
        </error></iq>"
        .parse()
        .expect("valid xml");
    assert!(parse_pubsub_items_result(&error).is_empty());
}

// ── Publish result parsing (§7.1.2) ──────────────────────────────────

#[test]
fn parse_publish_result_returns_echoed_item_id() {
    let xml = "<iq xmlns='jabber:client' type='result' id='x'>\
        <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
          <publish node='n'><item id='generated-1'/></publish>\
        </pubsub>\
      </iq>";
    let iq: Element = xml.parse().expect("valid xml");
    assert_eq!(
        parse_pubsub_publish_result_item_id(&iq).as_deref(),
        Some("generated-1")
    );
}

#[test]
fn parse_publish_result_tolerates_bare_success() {
    let iq: Element = "<iq xmlns='jabber:client' type='result' id='x'/>"
        .parse()
        .expect("valid xml");
    assert_eq!(parse_pubsub_publish_result_item_id(&iq), None);
}
