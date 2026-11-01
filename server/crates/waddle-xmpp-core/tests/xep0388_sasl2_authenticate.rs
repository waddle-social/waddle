//! XEP-0388 authenticate direct-child wire conformance.

use minidom::Element;
use waddle_xmpp_core::xep0388::{Sasl2Authenticate, Sasl2Data, Sasl2Mechanism, NS_SASL2};

#[test]
fn xep0388_authenticate_preserves_direct_inline_requests_and_user_agent() {
    let user_agent = Element::builder("user-agent", NS_SASL2)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            "d4565fa7-4d72-4749-b3d3-740edbf87770",
        )
        .append(
            Element::builder("software", NS_SASL2)
                .append("Waddle")
                .build(),
        )
        .build();
    let resume = Element::builder("resume", "urn:xmpp:sm:3")
        .attr(minidom::rxml::xml_ncname!("previd").to_owned(), "stream-1")
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), "7")
        .build();
    let bind = Element::builder("bind", "urn:xmpp:bind:0")
        .append(
            Element::builder("tag", "urn:xmpp:bind:0")
                .append("phone")
                .build(),
        )
        .build();
    let unknown = Element::builder("future", "urn:example:feature").build();
    let mut request =
        Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, Some(Sasl2Data::new(b"secret")));
    for child in [&user_agent, &resume, &bind, &unknown] {
        request.push_additional_child(child.clone()).unwrap();
    }

    let wire = request.to_element();
    assert!(wire.get_child("inline", NS_SASL2).is_none());
    for child in [&user_agent, &resume, &bind, &unknown] {
        assert_eq!(
            wire.get_child(child.name(), child.ns().as_str()),
            Some(child)
        );
    }
    let mut bytes = Vec::new();
    wire.write_to(&mut bytes).unwrap();
    let serialized = String::from_utf8(bytes).unwrap();
    let parsed_element: Element = serialized.parse().unwrap();
    let parsed = Sasl2Authenticate::from_element(&parsed_element).unwrap();
    assert_eq!(parsed, request);
    assert_eq!(
        parsed.additional_children(),
        &[user_agent, resume, bind, unknown]
    );
}

#[test]
fn xep0388_authenticate_distinguishes_absent_from_empty_initial_response() {
    let absent = Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, None);
    let empty = Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, Some(Sasl2Data::new([])));
    assert!(absent
        .to_element()
        .get_child("initial-response", NS_SASL2)
        .is_none());
    assert!(empty
        .to_element()
        .get_child("initial-response", NS_SASL2)
        .is_some());
    assert_eq!(
        Sasl2Authenticate::from_element(&absent.to_element()).unwrap(),
        absent
    );
    assert_eq!(
        Sasl2Authenticate::from_element(&empty.to_element()).unwrap(),
        empty
    );
}

#[test]
fn xep0388_authenticate_rejects_malformed_or_duplicate_protocol_children() {
    let root = || {
        Element::builder("authenticate", NS_SASL2).attr(
            minidom::rxml::xml_ncname!("mechanism").to_owned(),
            "OAUTHBEARER",
        )
    };
    let initial = Element::builder("initial-response", NS_SASL2)
        .append("AQ==")
        .build();
    let duplicate_initial = root().append(initial.clone()).append(initial).build();
    assert!(Sasl2Authenticate::from_element(&duplicate_initial).is_err());

    let inline_wrapper = root()
        .append(Element::builder("inline", NS_SASL2).build())
        .build();
    assert!(Sasl2Authenticate::from_element(&inline_wrapper).is_err());
    let mut request = Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, None);
    assert!(request
        .push_additional_child(Element::builder("inline", NS_SASL2).build())
        .is_err());
    assert!(request
        .push_additional_child(Element::builder("initial-response", NS_SASL2).build())
        .is_err());

    let agent = Element::builder("user-agent", NS_SASL2).build();
    let duplicate_agent = root().append(agent.clone()).append(agent).build();
    assert!(Sasl2Authenticate::from_element(&duplicate_agent).is_err());

    let nested = Element::builder("initial-response", NS_SASL2)
        .append(Element::builder("unexpected", "urn:example:feature").build())
        .build();
    assert!(Sasl2Authenticate::from_element(&root().append(nested).build()).is_err());
    assert!(Sasl2Authenticate::from_element(&root().append("unexpected").build()).is_err());
}
