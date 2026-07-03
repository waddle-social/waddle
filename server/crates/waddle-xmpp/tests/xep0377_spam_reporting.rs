//! XEP-0377: Spam Reporting dedicated suite.
//!
//! Round trips the `<report/>` payload, verifies namespace exactness
//! against `urn:xmpp:reporting:1`, and exercises the XEP-0191 block
//! item shape a client sends when blocking with a report attached.

use minidom::Element;
use waddle_xmpp::xep::{
    build_report_element, is_report_element, parse_report, Report, ReportReason, ReportRecord,
    ReportStore, NS_REPORTING,
};

fn reparse(elem: &Element) -> Element {
    String::from(elem)
        .parse::<Element>()
        .expect("serialized report is well-formed XML")
}

#[test]
fn xep0377_namespace_is_exact() {
    assert_eq!(NS_REPORTING, "urn:xmpp:reporting:1");
}

#[test]
fn xep0377_spam_report_round_trips_through_wire_xml() {
    let elem = reparse(&build_report_element(&Report::spam()));
    assert!(is_report_element(&elem));
    assert_eq!(elem.attr("reason"), Some("urn:xmpp:reporting:spam"));

    let parsed = parse_report(&elem).expect("parses");
    assert_eq!(parsed.reason, ReportReason::Spam);
    assert!(parsed.text.is_none());
}

#[test]
fn xep0377_abuse_report_with_text_round_trips() {
    let report = Report::abuse().with_text("Sent me threatening messages");
    let parsed = parse_report(&reparse(&build_report_element(&report))).expect("parses");
    assert_eq!(parsed, report);
}

#[test]
fn xep0377_report_text_child_is_in_reporting_namespace() {
    let elem = build_report_element(&Report::spam().with_text("Never stops"));
    let text = elem
        .get_child("text", NS_REPORTING)
        .expect("text child in urn:xmpp:reporting:1");
    assert_eq!(text.text(), "Never stops");
}

#[test]
fn xep0377_unknown_reason_is_rejected() {
    assert_eq!(ReportReason::from_str_attr("phishing"), None);
    // Bare tokens are not the registered URN values from XEP-0377 §3.
    assert_eq!(ReportReason::from_str_attr("spam"), None);
    assert_eq!(ReportReason::from_str_attr("abuse"), None);

    let elem: Element = "<report xmlns='urn:xmpp:reporting:1' reason='phishing'/>"
        .parse()
        .expect("valid xml");
    assert!(parse_report(&elem).is_none());
}

#[test]
fn xep0377_report_without_reason_is_rejected() {
    let elem: Element = "<report xmlns='urn:xmpp:reporting:1'/>"
        .parse()
        .expect("valid xml");
    assert!(parse_report(&elem).is_none());
}

#[test]
fn xep0377_report_in_wrong_namespace_is_not_detected() {
    let elem: Element = "<report xmlns='urn:xmpp:reporting:0' reason='urn:xmpp:reporting:spam'/>"
        .parse()
        .expect("valid xml");
    assert!(!is_report_element(&elem));
    assert!(parse_report(&elem).is_none());
}

#[test]
fn xep0377_report_extracts_from_blocking_item_shape() {
    // XEP-0377 §3.2: the report rides inside a XEP-0191 block <item/>.
    let block: Element = "<block xmlns='urn:xmpp:blocking'>\
                          <item jid='spammer@badhost.example'>\
                          <report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'>\
                          <text xmlns='urn:xmpp:reporting:1'>Never came trouble to my house like this.</text>\
                          </report></item></block>"
        .parse()
        .expect("valid xml");

    let item = block
        .get_child("item", "urn:xmpp:blocking")
        .expect("item child");
    let report_elem = item
        .children()
        .find(|c| is_report_element(c))
        .expect("report child");
    let report = parse_report(report_elem).expect("parses");

    assert_eq!(report.reason, ReportReason::Spam);
    assert_eq!(
        report.text.as_deref(),
        Some("Never came trouble to my house like this.")
    );
    assert_eq!(item.attr("jid"), Some("spammer@badhost.example"));
}

#[test]
fn xep0377_reason_wire_values_round_trip() {
    for reason in [ReportReason::Spam, ReportReason::Abuse] {
        assert_eq!(ReportReason::from_str_attr(reason.as_str()), Some(reason));
        assert_eq!(reason.to_string(), reason.as_str());
    }
}

#[test]
fn xep0377_report_store_tracks_per_jid_counts_and_reasons() {
    let mut store = ReportStore::new();
    store.file_report(
        ReportRecord::new(
            "alice@example.com",
            "spammer@badhost.example",
            Report::spam(),
        )
        .with_timestamp("2024-06-01T12:00:00Z"),
    );
    store.file_report(ReportRecord::new(
        "bob@example.com",
        "spammer@badhost.example",
        Report::spam().with_text("ads"),
    ));
    store.file_report(ReportRecord::new(
        "alice@example.com",
        "troll@badhost.example",
        Report::abuse(),
    ));

    assert_eq!(store.total(), 3);
    assert_eq!(store.report_count("spammer@badhost.example"), 2);
    assert_eq!(store.reports_for("troll@badhost.example").len(), 1);
    assert_eq!(store.spam_reports().len(), 2);
    assert_eq!(store.report_count("innocent@example.com"), 0);
    assert_eq!(
        store.reports_for("spammer@badhost.example")[0]
            .timestamp
            .as_deref(),
        Some("2024-06-01T12:00:00Z")
    );
}
