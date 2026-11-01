//! XEP-0388 SASL2 client bootstrap values dedicated suite.

use jid::{BareJid, Jid};
use minidom::Element;
use waddle_xmpp_client::AccessToken;
use waddle_xmpp_client::{
    AuthMechanism, AuthenticationRequest, BootstrapElement, OAuthBearerRequest, SaslChallenge,
    SaslFailureCondition, StreamFeatures, NS_SASL, NS_SASL2, NS_STREAMS,
};

#[test]
fn xep0388_stream_features_parse_sasl2_mechanisms_and_inline_presence() {
    let features = Element::builder("features", NS_STREAMS)
        .append(
            Element::builder("authentication", NS_SASL2)
                .append(
                    Element::builder("mechanism", NS_SASL2)
                        .append("OAUTHBEARER")
                        .build(),
                )
                .append(Element::builder("inline", NS_SASL2).build())
                .build(),
        )
        .build();

    let parsed = StreamFeatures::from_element(&features).unwrap();
    assert!(parsed.supports_sasl2(AuthMechanism::OAuthBearer));
    assert!(parsed
        .sasl2_authentication
        .as_ref()
        .unwrap()
        .inline
        .is_some());
    assert!(!parsed.supports(AuthMechanism::OAuthBearer));
}

#[test]
fn xep0388_authenticate_has_initial_response_and_omits_inline_requests() {
    let request = AuthenticationRequest::Sasl2OAuthBearer(OAuthBearerRequest {
        account: "alice@example.com".parse::<BareJid>().unwrap(),
        access_token: AccessToken::new("test-token"),
    });
    let element = request.to_element();

    assert_eq!(element.name(), "authenticate");
    assert_eq!(element.ns(), NS_SASL2);
    assert_eq!(element.attr("mechanism"), Some("OAUTHBEARER"));
    assert!(element.get_child("initial-response", NS_SASL2).is_some());
    assert!(element.get_child("inline", NS_SASL2).is_none());
}

#[test]
fn xep0388_success_requires_and_parses_authorization_identifier() {
    let success = Element::builder("success", NS_SASL2)
        .append(
            Element::builder("additional-data", NS_SASL2)
                .append("AQ==")
                .build(),
        )
        .append(
            Element::builder("authorization-identifier", NS_SASL2)
                .append("alice@example.com")
                .build(),
        )
        .build();
    let Some(Ok(BootstrapElement::Sasl2Success(success))) = BootstrapElement::parse(&success)
    else {
        panic!("expected parsed SASL2 success");
    };
    assert_eq!(
        success.authorization_identifier,
        "alice@example.com".parse::<Jid>().unwrap()
    );
    assert_eq!(success.additional_data.unwrap().as_bytes(), &[0x01]);
    assert!(success.inline_results.is_empty());

    let missing_identifier = Element::builder("success", NS_SASL2).build();
    assert!(matches!(
        BootstrapElement::parse(&missing_identifier),
        Some(Err(_))
    ));

    let duplicate_identifier = Element::builder("success", NS_SASL2)
        .append(
            Element::builder("authorization-identifier", NS_SASL2)
                .append("alice@example.com")
                .build(),
        )
        .append(
            Element::builder("authorization-identifier", NS_SASL2)
                .append("mallory@example.com")
                .build(),
        )
        .build();
    assert!(matches!(
        BootstrapElement::parse(&duplicate_identifier),
        Some(Err(_))
    ));

    let success_with_inline_result = Element::builder("success", NS_SASL2)
        .append(
            Element::builder("authorization-identifier", NS_SASL2)
                .append("alice@example.com")
                .build(),
        )
        .append(Element::builder("resumed", waddle_xmpp_client::NS_SM).build())
        .build();
    let Some(Ok(BootstrapElement::Sasl2Success(success))) =
        BootstrapElement::parse(&success_with_inline_result)
    else {
        panic!("expected SASL2 success with preserved inline result");
    };
    assert_eq!(success.inline_results.len(), 1);
}

#[test]
fn xep0388_failure_is_terminally_typed_and_oauth_error_challenge_responds_with_aq() {
    let failure = Element::builder("failure", NS_SASL2)
        .append(Element::builder("not-authorized", NS_SASL).build())
        .build();
    let Some(Ok(BootstrapElement::Sasl2Failure(failure))) = BootstrapElement::parse(&failure)
    else {
        panic!("expected parsed SASL2 failure");
    };
    assert_eq!(failure.condition, SaslFailureCondition::NotAuthorized);

    let challenge = Element::builder("challenge", NS_SASL2)
        .append("eyJzdGF0dXMiOiJpbnZhbGlkX3Rva2VuIn0=")
        .build();
    let parsed = SaslChallenge::from_element(&challenge).unwrap();
    assert!(!parsed.data.as_bytes().is_empty());
    let response = parsed.oauthbearer_error_response_element();
    assert_eq!(response.name(), "response");
    assert_eq!(response.ns(), NS_SASL2);
    assert_eq!(response.text(), "AQ==");
}

#[test]
fn xep0388_sasl1_challenge_keeps_legacy_response_namespace() {
    let challenge = Element::builder("challenge", NS_SASL)
        .append("eyJzdGF0dXMiOiJpbnZhbGlkX3Rva2VuIn0=")
        .build();
    let response = SaslChallenge::from_element(&challenge)
        .unwrap()
        .oauthbearer_error_response_element();
    assert_eq!(response.ns(), NS_SASL);
}
