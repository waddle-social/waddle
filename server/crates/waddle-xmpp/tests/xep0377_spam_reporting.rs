//! XEP-0377 (Blocking Command Reports) dedicated suite.
//!
//! Covers `<report/>` parsing and building, its embedding in a XEP-0191
//! `<block/>` request, and the operator-facing counter.

use jid::Jid;
use minidom::Element;
use waddle_xmpp::telemetry::test_support;
use waddle_xmpp::xep::xep0191::{
    parse_blocking_request, BlockingBadRequest, BlockingError, BlockingRequest,
};
use waddle_xmpp::xep::{
    build_report_element, is_report_element, parse_report, record_report, Report, ReportError,
    ReportReason, NS_REPORTING,
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::iq::Iq;

fn parse(xml: &str) -> Element {
    xml.parse::<Element>().expect("well-formed test XML")
}

fn jid(value: &str) -> Jid {
    value.parse().expect("valid jid")
}

fn block_iq(item: &str) -> Iq {
    Iq::Set {
        from: None,
        to: None,
        id: "block1".to_owned(),
        payload: parse(&format!("<block xmlns='urn:xmpp:blocking'>{item}</block>")),
    }
}

#[test]
fn namespace_is_exact() {
    assert_eq!(NS_REPORTING, "urn:xmpp:reporting:1");
}

#[test]
fn defined_reasons_parse() {
    let spam = parse("<report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'/>");
    let abuse = parse("<report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:abuse'/>");
    assert_eq!(
        parse_report(&spam).expect("spam").reason,
        ReportReason::Spam
    );
    assert_eq!(
        parse_report(&abuse).expect("abuse").reason,
        ReportReason::Abuse
    );
}

#[test]
fn reason_is_required_and_must_be_a_uri() {
    let none = parse("<report xmlns='urn:xmpp:reporting:1'/>");
    assert_eq!(parse_report(&none), Err(ReportError::MissingReason));
    for bad in ["", "spam", "two words:x"] {
        let elem = parse(&format!(
            "<report xmlns='urn:xmpp:reporting:1' reason='{bad}'/>"
        ));
        assert_eq!(
            parse_report(&elem),
            Err(ReportError::MalformedReason),
            "{bad:?}"
        );
    }
}

#[test]
fn unregistered_reason_uri_is_preserved() {
    let elem = parse("<report xmlns='urn:xmpp:reporting:1' reason='urn:example:phishing'/>");
    let report = parse_report(&elem).expect("report");
    assert_eq!(report.reason.as_str(), "urn:example:phishing");
    assert!(matches!(report.reason, ReportReason::Other(_)));
}

#[test]
fn wrong_namespace_or_name_is_not_a_report() {
    let wrong_ns = parse("<report xmlns='jabber:client' reason='urn:xmpp:reporting:spam'/>");
    assert!(!is_report_element(&wrong_ns));
    assert_eq!(parse_report(&wrong_ns), Err(ReportError::NotReport));
    let wrong_name = parse("<other xmlns='urn:xmpp:reporting:1'/>");
    assert_eq!(parse_report(&wrong_name), Err(ReportError::NotReport));
}

#[test]
fn full_example_from_the_spec_parses() {
    let elem = parse(
        "<report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'>\
           <stanza-id xmlns='urn:xmpp:sid:0' by='romeo@example.net' id='28482-98726-73623'/>\
           <stanza-id xmlns='urn:xmpp:sid:0' by='romeo@example.net' id='38383-38018-18385'/>\
           <text xml:lang='en'>Never came trouble to my house like this.</text>\
           <report-origin/><third-party/>\
         </report>",
    );
    let report = parse_report(&elem).expect("report");
    assert_eq!(report.texts.len(), 1);
    assert_eq!(report.texts[0].lang.as_deref(), Some("en"));
    assert_eq!(
        report.first_text(),
        Some("Never came trouble to my house like this.")
    );
    assert_eq!(
        report.stanza_ids,
        vec![
            StanzaId::new("28482-98726-73623", jid("romeo@example.net")),
            StanzaId::new("38383-38018-18385", jid("romeo@example.net")),
        ]
    );
    assert!(report.report_origin && report.third_party);
}

#[test]
fn malformed_stanza_ids_are_dropped_not_fatal() {
    let elem = parse(
        "<report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'>\
           <stanza-id xmlns='urn:xmpp:sid:0' id='no-by'/>\
           <stanza-id xmlns='urn:xmpp:sid:0' by='a@b'/>\
         </report>",
    );
    assert!(parse_report(&elem).expect("report").stanza_ids.is_empty());
}

#[test]
fn build_parse_round_trips_through_wire_xml() {
    let mut report = Report::abuse()
        .with_text_lang("en", "Threatening messages")
        .with_text_lang("fr", "Messages menaçants")
        .with_stanza_id(StanzaId::new("sid-1", jid("romeo@example.net")));
    report.report_origin = true;
    let wire = String::from(&build_report_element(&report));
    let reparsed = parse_report(&parse(&wire)).expect("round trip");
    assert_eq!(reparsed, report);
}

#[test]
fn block_item_carries_the_report() {
    let iq = block_iq(
        "<item jid='romeo@example.net'>\
           <report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:abuse'/>\
         </item>\
         <item jid='plain@example.net'/>",
    );
    let BlockingRequest::Block(items) = parse_blocking_request(&iq).expect("block") else {
        panic!("expected block request");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].jid, jid("romeo@example.net"));
    assert_eq!(
        items[0].report.as_ref().map(|r| r.reason.clone()),
        Some(ReportReason::Abuse)
    );
    assert_eq!(items[1].report, None);
}

#[test]
fn block_with_malformed_report_is_bad_request() {
    let iq = block_iq("<item jid='a@b'><report xmlns='urn:xmpp:reporting:1'/></item>");
    assert_eq!(
        parse_blocking_request(&iq),
        Err(BlockingError::BadRequest(
            BlockingBadRequest::MalformedReport
        ))
    );
}

#[test]
fn block_item_with_two_reports_is_bad_request() {
    let iq = block_iq(
        "<item jid='a@b'>\
           <report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'/>\
           <report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:abuse'/>\
         </item>",
    );
    assert_eq!(
        parse_blocking_request(&iq),
        Err(BlockingError::BadRequest(
            BlockingBadRequest::MultipleReports
        ))
    );
}

#[test]
fn report_outside_block_item_is_rejected() {
    let unblock = Iq::Set {
        from: None,
        to: None,
        id: "u1".to_owned(),
        payload: parse(
            "<unblock xmlns='urn:xmpp:blocking'><item jid='a@b'>\
               <report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'/>\
             </item></unblock>",
        ),
    };
    assert_eq!(
        parse_blocking_request(&unblock),
        Err(BlockingError::BadRequest(
            BlockingBadRequest::UnexpectedChild
        ))
    );
    let unknown_child = block_iq("<item jid='a@b'><junk xmlns='urn:x'/></item>");
    assert_eq!(
        parse_blocking_request(&unknown_child),
        Err(BlockingError::BadRequest(
            BlockingBadRequest::UnexpectedChild
        ))
    );
}

#[tokio::test]
async fn recording_a_report_counts_it_by_reason() {
    let metrics = test_support::acquire().await;
    let reporter = "juliet@example.com".parse().expect("bare jid");
    let reported = jid("romeo@example.net");
    record_report(&reporter, &reported, &Report::spam());
    record_report(&reporter, &reported, &Report::spam().with_text("ads"));
    record_report(&reporter, &reported, &Report::abuse());
    let other = parse_report(&parse(
        "<report xmlns='urn:xmpp:reporting:1' reason='urn:example:x'/>",
    ))
    .expect("report");
    record_report(&reporter, &reported, &other);

    let count = |reason: &str| {
        metrics.counter_sum(
            "waddle.abuse_reports.received",
            &[("report_reason", reason)],
        )
    };
    assert_eq!(count("spam"), Some(2));
    assert_eq!(count("abuse"), Some(1));
    assert_eq!(count("other"), Some(1));
    assert_eq!(
        metrics
            .metric_unit("waddle.abuse_reports.received")
            .as_deref(),
        Some("{report}")
    );
}
