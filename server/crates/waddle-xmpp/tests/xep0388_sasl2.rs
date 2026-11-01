//! XEP-0388: SASL2 typed wire values dedicated suite.

use jid::Jid;
use minidom::Element;
use waddle_xmpp::xep::xep0388::{
    OtherSasl2Mechanism, Sasl2Authenticate, Sasl2Challenge, Sasl2Data, Sasl2Failure,
    Sasl2FailureCondition, Sasl2Feature, Sasl2InlineFeatures, Sasl2Mechanism, Sasl2Response,
    Sasl2Success, NS_SASL2,
};

#[test]
fn xep0388_authentication_feature_round_trips_mechanisms_and_inline_features() {
    let feature = Sasl2Feature {
        mechanisms: vec![Sasl2Mechanism::OAuthBearer, Sasl2Mechanism::ScramSha256],
        inline: Some(Sasl2InlineFeatures {
            features: vec![Element::builder("bind", "urn:xmpp:bind:0").build()],
        }),
    };

    let element = feature.to_element().unwrap();
    let parsed = Sasl2Feature::from_element(&element).unwrap();
    assert_eq!(parsed, feature);
}

#[test]
fn xep0388_authentication_feature_requires_at_least_one_mechanism() {
    let feature = Sasl2Feature {
        mechanisms: Vec::new(),
        inline: None,
    };
    assert!(feature.to_element().is_err());

    let element = Element::builder("authentication", NS_SASL2).build();
    assert!(Sasl2Feature::from_element(&element).is_err());
}

#[test]
fn xep0388_preserves_valid_extension_mechanisms_and_rejects_invalid_names() {
    let extension = OtherSasl2Mechanism::try_new("X-EXAMPLE").unwrap();
    assert_eq!(Sasl2Mechanism::Other(extension).as_str(), "X-EXAMPLE");
    assert!(OtherSasl2Mechanism::try_new("invalid token").is_err());
    assert!(OtherSasl2Mechanism::try_new("OAUTHBEARER").is_err());
}

#[test]
fn xep0388_authenticate_encodes_initial_response_and_does_not_invent_inline_requests() {
    let authenticate = Sasl2Authenticate::new(
        Sasl2Mechanism::OAuthBearer,
        Some(Sasl2Data::new(b"n,,\x01auth=Bearer token\x01\x01".to_vec())),
    );

    let element = authenticate.to_element();
    assert_eq!(element.name(), "authenticate");
    assert_eq!(element.ns(), NS_SASL2);
    assert_eq!(element.attr("mechanism"), Some("OAUTHBEARER"));
    assert!(element.get_child("initial-response", NS_SASL2).is_some());
    assert!(element.get_child("inline", NS_SASL2).is_none());
    assert_eq!(
        Sasl2Authenticate::from_element(&element).unwrap(),
        authenticate
    );
}

#[test]
fn xep0388_challenge_and_response_preserve_decoded_payload() {
    let challenge = Sasl2Challenge {
        data: Sasl2Data::new(br#"{"status":"invalid_token"}"#.to_vec()),
    };
    let parsed = Sasl2Challenge::from_element(&challenge.to_element()).unwrap();
    assert_eq!(parsed.data.as_bytes(), challenge.data.as_bytes());

    let response = Sasl2Response {
        data: Sasl2Data::new(b"\x01".to_vec()),
    };
    let response_element = response.to_element();
    assert_eq!(response_element.name(), "response");
    assert_eq!(response_element.ns(), NS_SASL2);
    assert_eq!(response_element.text(), "AQ==");
}

#[test]
fn xep0388_success_requires_a_valid_authorization_identifier() {
    let success = Sasl2Success {
        additional_data: Some(Sasl2Data::new([0x01])),
        authorization_identifier: "alice@example.com".parse::<Jid>().unwrap(),
        inline_results: Vec::new(),
    };
    assert_eq!(
        Sasl2Success::from_element(&success.to_element()).unwrap(),
        success
    );

    let missing_identity = Element::builder("success", NS_SASL2).build();
    assert!(Sasl2Success::from_element(&missing_identity).is_err());
}

#[test]
fn xep0388_failure_parses_and_builds_standard_conditions() {
    let failure = Sasl2Failure {
        condition: Sasl2FailureCondition::NotAuthorized,
    };
    let element = failure.to_element().unwrap();
    assert_eq!(element.name(), "failure");
    assert_eq!(element.ns(), NS_SASL2);
    assert!(element
        .get_child("not-authorized", waddle_xmpp::xep::xep0388::NS_SASL)
        .is_some());
    assert!(element.get_child("not-authorized", NS_SASL2).is_none());
    assert_eq!(
        Sasl2Failure::from_element(&element).unwrap().condition,
        Sasl2FailureCondition::NotAuthorized
    );
}

#[test]
fn xep0388_failure_uses_the_rfc6120_namespace_for_the_sasl_condition() {
    let failure = Element::builder("failure", NS_SASL2)
        .append(Element::builder("aborted", waddle_xmpp::xep::xep0388::NS_SASL).build())
        .append(Element::builder("text", NS_SASL2).append("aborted").build())
        .build();
    assert_eq!(
        Sasl2Failure::from_element(&failure).unwrap().condition,
        Sasl2FailureCondition::Aborted
    );
}

#[test]
fn xep0388_rejects_malformed_base64_and_wrong_namespaces() {
    let invalid_challenge = Element::builder("challenge", NS_SASL2)
        .append("%%%")
        .build();
    assert!(Sasl2Challenge::from_element(&invalid_challenge).is_err());

    let wrong_namespace = Element::builder("challenge", "urn:wrong:sasl")
        .append("AQ==")
        .build();
    assert!(Sasl2Challenge::from_element(&wrong_namespace).is_err());
}
