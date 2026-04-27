use chrono::Utc;
use waddle_xmpp::xep::xep0215::{
    build_credentials_result, build_service_element, build_services_result, is_extdisco_iq,
    parse_extdisco_request, parse_service_element, ExtDiscoRequest, ExternalService,
    ExternalServiceAction, ExternalServiceName, ExternalServiceTransport, ExternalServiceType,
    NS_EXTDISCO,
};
use xmpp_parsers::{
    iq::{Iq, IqType},
    minidom::Element,
};

#[test]
fn xep0215_services_request_parses_type_filter() {
    let iq = Iq {
        from: Some("romeo@example.test/orchard".parse().expect("jid")),
        to: Some("example.test".parse().expect("jid")),
        id: "ext-1".to_string(),
        payload: IqType::Get(
            Element::builder("services", NS_EXTDISCO)
                .attr("type", "turn")
                .build(),
        ),
    };

    assert!(is_extdisco_iq(&iq));
    assert_eq!(
        parse_extdisco_request(&iq),
        Ok(ExtDiscoRequest::Services {
            service_type: Some(ExternalServiceType::Turn)
        })
    );
}

#[test]
fn xep0215_service_element_preserves_credentials_and_action() {
    let expires = Utc::now();
    let mut service = ExternalService::new(ExternalServiceType::Turn, "turn.example.test")
        .with_port(3478)
        .with_transport(ExternalServiceTransport::Udp)
        .with_credentials("user", "pass", Some(expires));
    service.restricted = Some(true);
    service.name = Some(ExternalServiceName::new("LiveKit TURN"));
    service.action = Some(ExternalServiceAction::Add);

    let elem = build_service_element(&service);
    assert_eq!(elem.attr("host"), Some("turn.example.test"));
    assert_eq!(elem.attr("type"), Some("turn"));
    assert_eq!(elem.attr("restricted"), Some("true"));
    assert_eq!(elem.attr("action"), Some("add"));
    assert_eq!(parse_service_element(&elem), Ok(service));
}

#[test]
fn xep0215_result_builders_wrap_service_entries() {
    let iq = Iq {
        from: Some("romeo@example.test/orchard".parse().expect("jid")),
        to: Some("example.test".parse().expect("jid")),
        id: "ext-2".to_string(),
        payload: IqType::Get(Element::builder("services", NS_EXTDISCO).build()),
    };
    let services = vec![ExternalService::new(
        ExternalServiceType::Turn,
        "turn.example.test",
    )];

    let result = build_services_result(&iq, Some(&ExternalServiceType::Turn), &services);
    let IqType::Result(Some(elem)) = result.payload else {
        panic!("expected services result");
    };
    assert_eq!(elem.name(), "services");
    assert_eq!(elem.children().count(), 1);

    let credentials = build_credentials_result(&iq, &services);
    let IqType::Result(Some(elem)) = credentials.payload else {
        panic!("expected credentials result");
    };
    assert_eq!(elem.name(), "credentials");
    assert_eq!(elem.children().count(), 1);
}
