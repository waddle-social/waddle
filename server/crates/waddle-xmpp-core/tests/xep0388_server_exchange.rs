//! XEP-0388 non-inline server exchange with an explicitly simulated verifier.
//! These sans-I/O tests do not exercise a WebSocket, TLS, or a real verifier.

use minidom::Element;
use waddle_xmpp_core::xep0388::{
    Sasl2Authenticate, Sasl2Data, Sasl2FailureCondition, Sasl2Mechanism, Sasl2Response, NS_SASL2,
};
use waddle_xmpp_core::xep0388_server::{
    Sasl2ServerAction, Sasl2ServerError, Sasl2ServerExchange, Sasl2ServerFrame, Sasl2ServerInput,
    Sasl2Verification, Sasl2VerifierAssertion, Sasl2VerifierProvenance,
};

fn request(data: Option<&[u8]>) -> Sasl2ServerInput {
    Sasl2ServerInput::Authenticate(Sasl2Authenticate::new(
        Sasl2Mechanism::OAuthBearer,
        data.map(Sasl2Data::new),
    ))
}

fn response(data: &[u8]) -> Sasl2ServerInput {
    Sasl2ServerInput::Response(Sasl2Response {
        data: Sasl2Data::new(data),
    })
}

fn assertion() -> Sasl2Verification {
    Sasl2Verification::Authorized(Sasl2VerifierAssertion {
        bare_jid: "alice@example.com".parse().unwrap(),
        provenance: Sasl2VerifierProvenance::OAuthBearer,
    })
}

fn expect_failure(action: Sasl2ServerAction, expected: Sasl2FailureCondition) {
    let Sasl2ServerAction::Send(Sasl2ServerFrame::Failure(failure)) = action else {
        panic!("expected failure, got {action:?}");
    };
    assert_eq!(failure.condition, expected);
    let element = failure.to_element().unwrap();
    assert!(element.is("failure", NS_SASL2));
}

fn start_verification(exchange: &mut Sasl2ServerExchange) {
    let result = exchange.on_input(request(Some(b"credential"))).unwrap();
    assert_eq!(
        result,
        Sasl2ServerAction::VerifyOAuthBearer(Sasl2Data::new(b"credential"))
    );
}

#[test]
fn success_requires_verifier_assertion_and_is_a_single_bare_wire_result() {
    let mut exchange = Sasl2ServerExchange::new();
    assert_eq!(
        exchange.on_verification(assertion()),
        Err(Sasl2ServerError::NotAwaitingVerification)
    );
    start_verification(&mut exchange);
    let Sasl2ServerAction::Send(Sasl2ServerFrame::Success(success)) =
        exchange.on_verification(assertion()).unwrap()
    else {
        panic!("expected success");
    };
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    assert!(success.additional_data.is_none());
    assert!(success.inline_results.is_empty());
    let element = success.to_element();
    assert!(element.is("success", NS_SASL2));
    assert_eq!(
        element
            .get_child("authorization-identifier", NS_SASL2)
            .unwrap()
            .text(),
        "alice@example.com"
    );
    assert_eq!(
        exchange.on_verification(assertion()),
        Err(Sasl2ServerError::NotAwaitingVerification)
    );
    let Sasl2ServerAction::StreamError(error) = exchange.on_input(request(Some(b"again"))).unwrap()
    else {
        panic!("expected stream error");
    };
    assert!(error.is("error", xmpp_parsers::ns::STREAM));
    assert!(error
        .get_child("not-authorized", xmpp_parsers::ns::XMPP_STREAMS)
        .is_some());
    assert_eq!(
        exchange.on_input(request(None)),
        Err(Sasl2ServerError::AlreadyFinished)
    );
}

#[test]
fn absent_initial_response_gets_empty_challenge_but_present_empty_is_verified() {
    let mut absent = Sasl2ServerExchange::new();
    let Sasl2ServerAction::Send(Sasl2ServerFrame::Challenge(challenge)) =
        absent.on_input(request(None)).unwrap()
    else {
        panic!("expected empty challenge");
    };
    assert!(challenge.data.as_bytes().is_empty());
    assert!(challenge.to_element().is("challenge", NS_SASL2));
    assert_eq!(
        absent.on_input(response(b"credential")),
        Ok(Sasl2ServerAction::VerifyOAuthBearer(Sasl2Data::new(
            b"credential"
        )))
    );

    let mut empty = Sasl2ServerExchange::new();
    assert_eq!(
        empty.on_input(request(Some(b""))),
        Ok(Sasl2ServerAction::VerifyOAuthBearer(Sasl2Data::new([])))
    );
}

#[test]
fn rejected_token_sends_json_challenge_then_accepts_exactly_one_error_ack() {
    let mut exchange = Sasl2ServerExchange::new();
    start_verification(&mut exchange);
    let Sasl2ServerAction::Send(Sasl2ServerFrame::Challenge(challenge)) = exchange
        .on_verification(Sasl2Verification::InvalidToken)
        .unwrap()
    else {
        panic!("expected error challenge");
    };
    assert_eq!(challenge.data.as_bytes(), br#"{"status":"invalid_token"}"#);
    expect_failure(
        exchange.on_input(response(&[0x01])).unwrap(),
        Sasl2FailureCondition::NotAuthorized,
    );
    assert_eq!(
        exchange.on_input(response(&[0x01])),
        Err(Sasl2ServerError::AlreadyFinished)
    );
    assert_eq!(
        exchange.on_verification(assertion()),
        Err(Sasl2ServerError::NotAwaitingVerification)
    );
}

#[test]
fn abort_and_bad_error_ack_end_exchange_without_late_success() {
    let mut abort = Sasl2ServerExchange::new();
    start_verification(&mut abort);
    abort
        .on_verification(Sasl2Verification::InvalidToken)
        .unwrap();
    expect_failure(
        abort.on_input(Sasl2ServerInput::Abort).unwrap(),
        Sasl2FailureCondition::Aborted,
    );
    assert_eq!(
        abort.on_input(response(&[0x01])),
        Err(Sasl2ServerError::AlreadyFinished)
    );

    let mut bad_ack = Sasl2ServerExchange::new();
    start_verification(&mut bad_ack);
    bad_ack
        .on_verification(Sasl2Verification::InvalidToken)
        .unwrap();
    expect_failure(
        bad_ack
            .on_input(response(b"not the acknowledgement"))
            .unwrap(),
        Sasl2FailureCondition::MalformedRequest,
    );

    let mut pending = Sasl2ServerExchange::new();
    start_verification(&mut pending);
    expect_failure(
        pending.on_input(Sasl2ServerInput::Abort).unwrap(),
        Sasl2FailureCondition::Aborted,
    );
    assert_eq!(
        pending.on_verification(assertion()),
        Err(Sasl2ServerError::NotAwaitingVerification)
    );
}

#[test]
fn non_inline_exchange_rejects_extension_requests_and_unoffered_mechanisms() {
    for child in [
        Element::builder("bind", "urn:xmpp:bind:0").build(),
        Element::builder("resume", "urn:xmpp:sm:3").build(),
        Element::builder("future", "urn:example:extension").build(),
    ] {
        let mut request = Sasl2Authenticate::new(
            Sasl2Mechanism::OAuthBearer,
            Some(Sasl2Data::new(b"credential")),
        );
        request.push_additional_child(child).unwrap();
        let mut exchange = Sasl2ServerExchange::new();
        expect_failure(
            exchange
                .on_input(Sasl2ServerInput::Authenticate(request))
                .unwrap(),
            Sasl2FailureCondition::MalformedRequest,
        );
        assert_eq!(
            exchange.on_verification(assertion()),
            Err(Sasl2ServerError::NotAwaitingVerification)
        );
    }
    let mut wrong_mechanism = Sasl2ServerExchange::new();
    expect_failure(
        wrong_mechanism
            .on_input(Sasl2ServerInput::Authenticate(Sasl2Authenticate::new(
                Sasl2Mechanism::Plain,
                Some(Sasl2Data::new(b"credential")),
            )))
            .unwrap(),
        Sasl2FailureCondition::InvalidMechanism,
    );
}

#[test]
fn user_agent_is_permitted_but_unexpected_traffic_disconnects_pending_exchange() {
    let mut request = Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, None);
    request
        .push_additional_child(Element::builder("user-agent", NS_SASL2).build())
        .unwrap();
    let mut exchange = Sasl2ServerExchange::new();
    assert!(matches!(
        exchange.on_input(Sasl2ServerInput::Authenticate(request)),
        Ok(Sasl2ServerAction::Send(Sasl2ServerFrame::Challenge(_)))
    ));
    assert_eq!(
        exchange.on_input(Sasl2ServerInput::Other),
        Ok(Sasl2ServerAction::Disconnect)
    );
    assert_eq!(
        exchange.on_input(response(b"late")),
        Err(Sasl2ServerError::AlreadyFinished)
    );
    assert_eq!(
        exchange.on_verification(assertion()),
        Err(Sasl2ServerError::NotAwaitingVerification)
    );

    let mut validating = Sasl2ServerExchange::new();
    start_verification(&mut validating);
    assert_eq!(
        validating.on_input(response(b"unexpected")),
        Ok(Sasl2ServerAction::Disconnect)
    );
    assert_eq!(
        validating.on_verification(assertion()),
        Err(Sasl2ServerError::NotAwaitingVerification)
    );
}
