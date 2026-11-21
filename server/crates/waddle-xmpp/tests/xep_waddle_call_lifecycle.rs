//! Dedicated wire and exported-metrics suite for Waddle call lifecycle reports.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jid::FullJid;
use minidom::rxml::{xml_ncname, Namespace};
use minidom::Element;
use waddle_xmpp::protocol::dispatch::StanzaDispatcher;
use waddle_xmpp::protocol::event::{OutboundEvent, StanzaContext};
use waddle_xmpp::protocol::handlers::call_lifecycle::CallLifecycleHandler;
use waddle_xmpp::protocol::handlers::session_initiate_rate_limit::CallLifecycleRateLimit;
use waddle_xmpp::protocol::IqHandler;
use waddle_xmpp::telemetry::test_support;
use waddle_xmpp::xep::{
    CallClientPlatform, CallDurationBucket, CallEndReason, CallLifecycleKind, CallLifecycleReport,
    CallReconnectBucket, CallSetupOutcome, NS_WADDLE_CALL_LIFECYCLE,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

fn report(setup_outcome: CallSetupOutcome) -> CallLifecycleReport {
    CallLifecycleReport {
        kind: CallLifecycleKind::Dm,
        setup_outcome,
        end_reason: CallEndReason::PeerLeft,
        duration: CallDurationBucket::Under1m,
        reconnects: CallReconnectBucket::Once,
        platform: CallClientPlatform::Ios,
    }
}

fn session_jid() -> FullJid {
    "alice@waddle.test/phone".parse().unwrap()
}

fn ctx(jid: &FullJid) -> StanzaContext<'_> {
    StanzaContext {
        participant_registration: None,
        domain: "waddle.test",
        full_jid: jid,
        occupant_session: None,
        media_capabilities: None,
    }
}

fn report_iq(payload: Element, to: Option<&str>) -> Iq {
    Iq::Set {
        from: Some(session_jid().into()),
        to: to.map(|jid| jid.parse().unwrap()),
        id: "report-1".into(),
        payload,
    }
}

fn reply(events: Vec<OutboundEvent>) -> Iq {
    assert_eq!(events.len(), 1, "reports must only produce an IQ reply");
    let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
        panic!("expected stanza reply");
    };
    let Stanza::Iq(iq) = *stanza else {
        panic!("expected IQ reply");
    };
    *iq
}

fn assert_result(original: &Iq, events: Vec<OutboundEvent>) {
    let result = reply(events);
    assert!(matches!(result, Iq::Result { payload: None, .. }));
    assert_eq!(result.id(), original.id());
    assert_eq!(result.to(), original.from());
    assert_eq!(result.from(), original.to());
}

fn assert_error(events: Vec<OutboundEvent>, condition: DefinedCondition) {
    let Iq::Error { error, .. } = reply(events) else {
        panic!("expected IQ error");
    };
    assert_eq!(error.defined_condition, condition);
    assert_eq!(error.type_, ErrorType::Cancel);
}

#[tokio::test]
async fn absent_and_server_domain_destinations_receive_empty_result() {
    // Counting binds the process-wide instruments; hold the reader first.
    let _metrics = test_support::acquire().await;
    let handler = CallLifecycleHandler::new();
    let jid = session_jid();
    assert_eq!(handler.namespace(), NS_WADDLE_CALL_LIFECYCLE);
    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_iq(Arc::new(handler));
    for to in [None, Some("waddle.test")] {
        let iq = report_iq(report(CallSetupOutcome::Accepted).build_element(), to);
        assert_result(&iq, dispatcher.dispatch_iq(&iq, &ctx(&jid)));
    }
}

#[test]
fn other_destinations_that_reach_handler_are_unavailable() {
    let handler = CallLifecycleHandler::new();
    let jid = session_jid();
    for to in [
        "foreign.test",
        "bob@waddle.test",
        "waddle.test/resource",
        "bob@foreign.test/resource",
    ] {
        let iq = report_iq(report(CallSetupOutcome::Accepted).build_element(), Some(to));
        assert_error(
            handler.handle(&iq, &ctx(&jid)),
            DefinedCondition::ServiceUnavailable,
        );
    }
}

#[test]
fn get_is_bad_request() {
    let handler = CallLifecycleHandler::new();
    let jid = session_jid();
    let iq = Iq::Get {
        from: Some(jid.clone().into()),
        to: None,
        id: "get-report".into(),
        payload: report(CallSetupOutcome::Accepted).build_element(),
    };
    assert_error(
        handler.handle(&iq, &ctx(&jid)),
        DefinedCondition::BadRequest,
    );
}

#[test]
fn each_missing_or_unknown_attribute_is_bad_request() {
    let handler = CallLifecycleHandler::new();
    let jid = session_jid();
    for attribute in [
        xml_ncname!("kind"),
        xml_ncname!("setup-outcome"),
        xml_ncname!("end-reason"),
        xml_ncname!("duration"),
        xml_ncname!("reconnects"),
        xml_ncname!("platform"),
    ] {
        let mut missing = report(CallSetupOutcome::Accepted).build_element();
        missing.attrs_mut().remove(&Namespace::NONE, attribute);
        assert_error(
            handler.handle(&report_iq(missing, None), &ctx(&jid)),
            DefinedCondition::BadRequest,
        );
        let mut unknown = report(CallSetupOutcome::Accepted).build_element();
        unknown.set_attr(Namespace::NONE, attribute.to_owned(), "unknown");
        assert_error(
            handler.handle(&report_iq(unknown, None), &ctx(&jid)),
            DefinedCondition::BadRequest,
        );
    }
}

#[tokio::test]
async fn unknown_extensions_are_ignored_but_wrong_roots_rejected() {
    // Counting binds the process-wide instruments; hold the reader first.
    let _metrics = test_support::acquire().await;
    let handler = CallLifecycleHandler::new();
    let jid = session_jid();
    let mut payload = report(CallSetupOutcome::Accepted).build_element();
    payload.set_attr(
        Namespace::NONE,
        xml_ncname!("future").to_owned(),
        "new-value",
    );
    payload.append_child(Element::builder("future", NS_WADDLE_CALL_LIFECYCLE).build());
    let iq = report_iq(payload, None);
    assert_result(&iq, handler.handle(&iq, &ctx(&jid)));
    for root in [
        Element::builder("other", NS_WADDLE_CALL_LIFECYCLE).build(),
        Element::builder("report", "urn:waddle:other:0").build(),
    ] {
        assert_error(
            handler.handle(&report_iq(root, None), &ctx(&jid)),
            DefinedCondition::BadRequest,
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn thirteenth_report_is_limited_across_resources_but_another_account_is_unaffected() {
    let metrics = test_support::acquire().await;
    let handler = CallLifecycleHandler::new();
    let alice = session_jid();
    // A forged from must neither select the limiter bucket nor change the reporter.
    let mut iq = report_iq(report(CallSetupOutcome::Accepted).build_element(), None);
    *iq.from_mut() = Some("forged@waddle.test/other".parse().unwrap());
    for _ in 0..12 {
        assert_result(&iq, handler.handle(&iq, &ctx(&alice)));
    }
    let other_resource: FullJid = "alice@waddle.test/desktop".parse().unwrap();
    assert_error(
        handler.handle(&iq, &ctx(&other_resource)),
        DefinedCondition::PolicyViolation,
    );
    let bob: FullJid = "bob@waddle.test/phone".parse().unwrap();
    assert_result(&iq, handler.handle(&iq, &ctx(&bob)));
    for name in [
        "waddle.call.client.setup",
        "waddle.call.client.kind",
        "waddle.call.client.end",
    ] {
        assert_eq!(metrics.counter_sum(name, &[]), Some(13), "{name}");
    }
    assert_eq!(
        metrics.counter_sum(
            "waddle.call.control.rate_limited",
            &[("surface", "lifecycle_report")]
        ),
        Some(1)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn setup_and_kind_count_every_outcome_end_only_counts_connected_media() {
    let metrics = test_support::acquire().await;
    let handler = CallLifecycleHandler::new();
    let jid = session_jid();
    for setup_outcome in CallSetupOutcome::ALL {
        let iq = report_iq(report(setup_outcome).build_element(), None);
        assert_result(&iq, handler.handle(&iq, &ctx(&jid)));
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.client.setup",
                &[
                    ("platform", "ios"),
                    ("setup_outcome", setup_outcome.as_str())
                ]
            ),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.client.kind",
                &[("kind", "dm"), ("setup_outcome", setup_outcome.as_str())]
            ),
            Some(1)
        );
    }
    assert_eq!(
        metrics.counter_sum("waddle.call.client.setup", &[]),
        Some(5)
    );
    assert_eq!(metrics.counter_sum("waddle.call.client.kind", &[]), Some(5));
    assert_eq!(
        metrics.counter_sum(
            "waddle.call.client.end",
            &[("platform", "ios"), ("end_reason", "peer_left")]
        ),
        Some(1)
    );
    for name in [
        "waddle.call.client.setup",
        "waddle.call.client.kind",
        "waddle.call.client.end",
    ] {
        assert_eq!(metrics.metric_unit(name).as_deref(), Some("1"));
        for (_, attributes) in metrics.counter_samples(name).unwrap() {
            assert_eq!(attributes.len(), 2, "{name}: {attributes:?}");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_reports_emit_no_client_metrics_and_do_not_consume_budget() {
    let metrics = test_support::acquire().await;
    let handler = CallLifecycleHandler::with_rate_limit(Arc::new(CallLifecycleRateLimit::new(
        1,
        Duration::from_secs(600),
    )));
    let jid = session_jid();
    let malformed = report_iq(
        Element::builder("report", NS_WADDLE_CALL_LIFECYCLE).build(),
        None,
    );
    assert_error(
        handler.handle(&malformed, &ctx(&jid)),
        DefinedCondition::BadRequest,
    );
    let foreign = report_iq(
        report(CallSetupOutcome::Accepted).build_element(),
        Some("foreign.test"),
    );
    assert_error(
        handler.handle(&foreign, &ctx(&jid)),
        DefinedCondition::ServiceUnavailable,
    );
    for name in [
        "waddle.call.client.setup",
        "waddle.call.client.kind",
        "waddle.call.client.end",
    ] {
        assert_eq!(metrics.counter_sum(name, &[]), None, "{name}");
    }
    let iq = report_iq(report(CallSetupOutcome::Failed).build_element(), None);
    assert_result(&iq, handler.handle(&iq, &ctx(&jid)));
    assert_error(
        handler.handle(&iq, &ctx(&jid)),
        DefinedCondition::PolicyViolation,
    );
    assert_eq!(
        metrics.counter_sum("waddle.call.client.setup", &[]),
        Some(1)
    );
    assert_eq!(metrics.counter_sum("waddle.call.client.kind", &[]), Some(1));
    assert_eq!(metrics.counter_sum("waddle.call.client.end", &[]), None);
}

#[derive(Clone, Default)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn counted_report_logs_all_fields_and_authenticated_bare_jid_once() {
    // Counting binds the process-wide instruments; hold the reader first.
    let _metrics = test_support::acquire().await;
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(CaptureWriter(buffer.clone()))
            .finish(),
    );
    let handler = CallLifecycleHandler::new();
    let jid = session_jid();
    let mut iq = report_iq(report(CallSetupOutcome::Accepted).build_element(), None);
    *iq.from_mut() = Some("forged@waddle.test/other".parse().unwrap());
    assert_result(&iq, handler.handle(&iq, &ctx(&jid)));
    let logs = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert_eq!(logs.lines().count(), 1, "{logs}");
    let log: serde_json::Value = serde_json::from_str(logs.trim()).unwrap();
    assert_eq!(log["level"], "INFO");
    let fields = &log["fields"];
    for (key, value) in [
        ("user", "alice@waddle.test"),
        ("kind", "dm"),
        ("setup_outcome", "accepted"),
        ("end_reason", "peer-left"),
        ("duration", "under-1m"),
        ("reconnects", "once"),
        ("platform", "ios"),
    ] {
        assert_eq!(fields[key], value, "{key}: {logs}");
    }
}
