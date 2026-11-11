//! XEP-0388 inline extension points carrying XEP-0198 inline stream
//! resumption: advertisement, `<authenticate/>` request, and `<success/>`
//! results.

use minidom::Element;
use waddle_xmpp_core::xep0388::{
    Sasl2Authenticate, Sasl2Data, Sasl2Feature, Sasl2InlineFeatures, Sasl2Mechanism, Sasl2Success,
};
use waddle_xmpp_core::xep0388_inline::{
    Bind2Bound, Bind2Feature, Bind2Request, Sasl2InlineError, Sasl2InlineOffer,
    Sasl2InlineRequests, Sasl2InlineResults, SmInlineFailed, SmInlineResume, SmInlineResumeResult,
    SmInlineResumed, SmStreamId, NS_BIND2, NS_SM,
};
use xmpp_parsers::stanza_error::DefinedCondition;

fn parse(xml: &str) -> Element {
    xml.parse().expect("test XML parses")
}

fn reparse(element: &Element) -> Element {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("element serialises");
    String::from_utf8(bytes)
        .expect("serialised XML is UTF-8")
        .parse()
        .expect("serialised XML parses")
}

fn offer(xml: &str) -> Result<Sasl2InlineOffer, Sasl2InlineError> {
    let feature = Sasl2Feature::from_element(&parse(xml)).expect("SASL2 feature parses");
    Sasl2InlineOffer::from_inline_features(&feature.inline.expect("inline container"))
}

fn inline_offer(features: &[&str]) -> Result<Sasl2InlineOffer, Sasl2InlineError> {
    Sasl2InlineOffer::from_inline_features(&Sasl2InlineFeatures {
        features: features.iter().map(|xml| parse(xml)).collect(),
    })
}

fn results(children: &[&str]) -> Result<Sasl2InlineResults, Sasl2InlineError> {
    let success = Sasl2Success {
        additional_data: None,
        authorization_identifier: "juliet@montague.example".parse().expect("JID parses"),
        inline_results: children.iter().map(|xml| parse(xml)).collect(),
    };
    let wire =
        Sasl2Success::from_element(&reparse(&success.to_element())).expect("SASL2 success parses");
    Sasl2InlineResults::from_success(&wire)
}

fn requests(children: &[&str]) -> Result<Sasl2InlineRequests, Sasl2InlineError> {
    let mut authenticate = Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, None);
    for xml in children {
        authenticate
            .push_additional_child(parse(xml))
            .expect("extension child is accepted");
    }
    let wire = Sasl2Authenticate::from_element(&reparse(&authenticate.to_element()))
        .expect("SASL2 authenticate parses");
    Sasl2InlineRequests::from_authenticate(&wire)
}

fn stream_id(value: &str) -> SmStreamId {
    SmStreamId::new(value).expect("non-empty stream id")
}

#[test]
fn xep0388_inline_offer_parses_the_specification_example() {
    let parsed = offer(
        "<authentication xmlns='urn:xmpp:sasl:2'>\
           <mechanism>SCRAM-SHA-1</mechanism>\
           <inline>\
             <sm xmlns='urn:xmpp:sm:3'/>\
             <bind xmlns='urn:xmpp:bind:0'/>\
           </inline>\
         </authentication>",
    )
    .unwrap();

    assert!(parsed.stream_resumption);
    assert_eq!(parsed.bind2, Some(Bind2Feature::default()));
}

#[test]
fn xep0388_inline_offer_round_trips_through_the_authentication_feature() {
    let offer = Sasl2InlineOffer {
        stream_resumption: true,
        bind2: Some(Bind2Feature::default()),
    };
    let feature = Sasl2Feature {
        mechanisms: vec![Sasl2Mechanism::OAuthBearer],
        inline: Some(offer.to_inline_features()),
    };

    let wire = reparse(&feature.to_element().unwrap());
    let inline = wire
        .get_child("inline", "urn:xmpp:sasl:2")
        .expect("inline container");
    assert!(inline.get_child("sm", NS_SM).is_some());
    assert!(inline.get_child("bind", NS_BIND2).is_some());

    let reparsed = Sasl2Feature::from_element(&wire).unwrap();
    assert_eq!(
        Sasl2InlineOffer::from_inline_features(&reparsed.inline.unwrap()),
        Ok(offer)
    );
}

#[test]
fn xep0388_inline_offer_without_known_features_is_empty() {
    let parsed = offer(
        "<authentication xmlns='urn:xmpp:sasl:2'>\
           <mechanism>OAUTHBEARER</mechanism>\
           <inline><megabind xmlns='urn:example:megabind'/></inline>\
         </authentication>",
    )
    .unwrap();

    assert_eq!(parsed, Sasl2InlineOffer::default());
    assert!(Sasl2InlineOffer::default()
        .to_inline_features()
        .features
        .is_empty());
}

#[test]
fn xep0388_inline_offer_rejects_duplicates_and_malformed_stream_management_features() {
    let sm = "<sm xmlns='urn:xmpp:sm:3'/>";
    for (features, expected) in [
        (
            vec![sm, sm],
            Sasl2InlineError::DuplicateStreamManagementFeature,
        ),
        (
            vec!["<sm xmlns='urn:xmpp:sm:3' resume='true'/>"],
            Sasl2InlineError::InvalidStreamManagementFeature,
        ),
        (
            vec!["<sm xmlns='urn:xmpp:sm:3'><optional/></sm>"],
            Sasl2InlineError::InvalidStreamManagementFeature,
        ),
        (
            vec!["<resume xmlns='urn:xmpp:sm:3' h='1' previd='a'/>"],
            Sasl2InlineError::UnexpectedStreamManagementElement,
        ),
    ] {
        assert_eq!(inline_offer(&features), Err(expected), "{features:?}");
    }
}

#[test]
fn xep0388_inline_offer_ignores_stream_management_in_an_older_namespace() {
    let parsed = offer(
        "<authentication xmlns='urn:xmpp:sasl:2'>\
           <mechanism>OAUTHBEARER</mechanism>\
           <inline><sm xmlns='urn:xmpp:sm:2'/></inline>\
         </authentication>",
    )
    .unwrap();

    assert!(!parsed.stream_resumption);
}

#[test]
fn xep0198_inline_resume_matches_the_specification_wire_shape() {
    let resume = SmInlineResume {
        previd: stream_id("some-long-sm-id"),
        h: 7,
    };

    let wire = reparse(&resume.to_element());
    assert!(wire.is("resume", NS_SM));
    assert_eq!(wire.attr("previd"), Some("some-long-sm-id"));
    assert_eq!(wire.attr("h"), Some("7"));
    assert_eq!(wire.attrs().iter().count(), 2);
    assert!(wire.nodes().next().is_none());
    assert_eq!(SmInlineResume::from_element(&wire), Ok(resume));
}

#[test]
fn xep0198_inline_resume_rejects_malformed_attributes_and_content() {
    for xml in [
        "<resume xmlns='urn:xmpp:sm:3' previd='a'/>",
        "<resume xmlns='urn:xmpp:sm:3' h='1'/>",
        "<resume xmlns='urn:xmpp:sm:3' h='1' previd=''/>",
        "<resume xmlns='urn:xmpp:sm:3' h='-1' previd='a'/>",
        "<resume xmlns='urn:xmpp:sm:3' h='1.0' previd='a'/>",
        "<resume xmlns='urn:xmpp:sm:3' h='4294967296' previd='a'/>",
        "<resume xmlns='urn:xmpp:sm:3' h='' previd='a'/>",
        "<resume xmlns='urn:xmpp:sm:3' h='1' previd='a' max='5'/>",
        "<resume xmlns='urn:xmpp:sm:3' h='1' previd='a'>text</resume>",
        "<resume xmlns='urn:xmpp:sm:3' h='1' previd='a'><x xmlns='urn:example'/></resume>",
    ] {
        assert_eq!(
            SmInlineResume::from_element(&parse(xml)),
            Err(Sasl2InlineError::InvalidResumeRequest),
            "{xml}"
        );
    }
    assert_eq!(
        SmInlineResume::from_element(&parse("<resume xmlns='urn:xmpp:sm:2' h='1' previd='a'/>")),
        Err(Sasl2InlineError::InvalidElement)
    );
    assert_eq!(
        SmInlineResume::from_element(&parse("<resumed xmlns='urn:xmpp:sm:3' h='1' previd='a'/>")),
        Err(Sasl2InlineError::InvalidElement)
    );
}

#[test]
fn xep0198_inline_resume_accepts_the_full_unsigned_int_range() {
    let parsed = SmInlineResume::from_element(&parse(
        "<resume xmlns='urn:xmpp:sm:3' h='4294967295' previd='a'/>",
    ))
    .unwrap();
    assert_eq!(parsed.h, u32::MAX);
}

#[test]
fn xep0388_authenticate_carries_resume_before_the_bind_fallback() {
    let requests = Sasl2InlineRequests {
        resume: Some(SmInlineResume {
            previd: stream_id("some-long-sm-id"),
            h: 42,
        }),
        bind: Some(Bind2Request::default()),
    };
    let mut authenticate =
        Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, Some(Sasl2Data::new(b"secret")));
    requests.append_to(&mut authenticate).unwrap();

    let wire = reparse(&authenticate.to_element());
    let children: Vec<(&str, String)> = wire
        .children()
        .map(|child| (child.name(), child.ns()))
        .collect();
    assert_eq!(
        children,
        vec![
            ("initial-response", "urn:xmpp:sasl:2".to_owned()),
            ("resume", NS_SM.to_owned()),
            ("bind", NS_BIND2.to_owned()),
        ]
    );

    let reparsed = Sasl2Authenticate::from_element(&wire).unwrap();
    assert_eq!(
        Sasl2InlineRequests::from_authenticate(&reparsed),
        Ok(requests)
    );
}

#[test]
fn xep0388_authenticate_without_inline_requests_is_empty() {
    let authenticate = Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, None);
    let requests = Sasl2InlineRequests::from_authenticate(&authenticate).unwrap();

    assert!(requests.is_empty());
    assert!(requests.to_elements().is_empty());
}

#[test]
fn xep0388_authenticate_rejects_duplicate_and_misplaced_inline_requests() {
    let resume = "<resume xmlns='urn:xmpp:sm:3' h='1' previd='a'/>";
    let bind = "<bind xmlns='urn:xmpp:bind:0'/>";
    for (children, expected) in [
        (
            vec![resume, resume],
            Sasl2InlineError::DuplicateResumeRequest,
        ),
        (vec![bind, bind], Sasl2InlineError::DuplicateBindRequest),
        (
            vec!["<resume xmlns='urn:xmpp:sm:3' previd='a'/>"],
            Sasl2InlineError::InvalidResumeRequest,
        ),
        (
            vec!["<enable xmlns='urn:xmpp:sm:3'/>"],
            Sasl2InlineError::UnexpectedStreamManagementElement,
        ),
        (
            vec!["<bound xmlns='urn:xmpp:bind:0'/>"],
            Sasl2InlineError::UnexpectedBind2Element,
        ),
    ] {
        assert_eq!(requests(&children), Err(expected), "{children:?}");
    }
}

#[test]
fn xep0388_append_refuses_to_repeat_an_inline_request() {
    let requests = Sasl2InlineRequests {
        resume: Some(SmInlineResume {
            previd: stream_id("a"),
            h: 1,
        }),
        bind: None,
    };
    let mut authenticate = Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, None);
    requests.append_to(&mut authenticate).unwrap();
    let before = authenticate.clone();

    assert_eq!(
        requests.append_to(&mut authenticate),
        Err(Sasl2InlineError::DuplicateResumeRequest)
    );
    assert_eq!(authenticate, before);
}

#[test]
fn xep0388_success_parses_the_specification_resumed_example() {
    let parsed = results(&["<resumed xmlns='urn:xmpp:sm:3' h='345' previd='124'/>"]).unwrap();

    assert_eq!(
        parsed.resume,
        Some(SmInlineResumeResult::Resumed(SmInlineResumed {
            previd: stream_id("124"),
            h: 345,
        }))
    );
    assert_eq!(parsed.bound, None);
    assert!(parsed.other_results.is_empty());
}

#[test]
fn xep0198_failed_resume_carries_handled_count_and_condition() {
    let parsed = results(&[
        "<failed xmlns='urn:xmpp:sm:3' h='21'>\
           <item-not-found xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
         </failed>",
        "<bound xmlns='urn:xmpp:bind:0'/>",
    ])
    .unwrap();

    assert_eq!(
        parsed.resume,
        Some(SmInlineResumeResult::Failed(SmInlineFailed {
            h: Some(21),
            condition: Some(DefinedCondition::ItemNotFound),
        }))
    );
    assert_eq!(parsed.bound, Some(Bind2Bound::default()));
}

#[test]
fn xep0198_failed_resume_allows_omitted_handled_count_and_condition() {
    let parsed = results(&["<failed xmlns='urn:xmpp:sm:3'/>"]).unwrap();

    assert_eq!(
        parsed.resume,
        Some(SmInlineResumeResult::Failed(SmInlineFailed {
            h: None,
            condition: None,
        }))
    );
}

#[test]
fn xep0388_inline_results_round_trip_through_success() {
    for inline in [
        Sasl2InlineResults {
            resume: Some(SmInlineResumeResult::Resumed(SmInlineResumed {
                previd: stream_id("some-long-sm-id"),
                h: 9,
            })),
            bound: None,
            other_results: Vec::new(),
        },
        Sasl2InlineResults {
            resume: Some(SmInlineResumeResult::Failed(SmInlineFailed {
                h: Some(3),
                condition: Some(DefinedCondition::ItemNotFound),
            })),
            bound: Some(Bind2Bound::default()),
            other_results: vec![Element::builder("future", "urn:example:feature").build()],
        },
        Sasl2InlineResults {
            resume: Some(SmInlineResumeResult::Failed(SmInlineFailed {
                h: None,
                condition: None,
            })),
            bound: None,
            other_results: Vec::new(),
        },
    ] {
        let success = Sasl2Success {
            additional_data: None,
            authorization_identifier: "juliet@montague.example".parse().unwrap(),
            inline_results: inline.to_elements(),
        };
        let reparsed = Sasl2Success::from_element(&reparse(&success.to_element())).unwrap();
        assert_eq!(Sasl2InlineResults::from_success(&reparsed), Ok(inline));
    }
}

#[test]
fn xep0388_inline_results_reject_duplicates_and_contradictions() {
    let resumed = "<resumed xmlns='urn:xmpp:sm:3' h='1' previd='a'/>";
    let failed = "<failed xmlns='urn:xmpp:sm:3'/>";
    let bound = "<bound xmlns='urn:xmpp:bind:0'/>";
    for (children, expected) in [
        (
            vec![resumed, resumed],
            Sasl2InlineError::DuplicateResumeResult,
        ),
        (
            vec![resumed, failed],
            Sasl2InlineError::DuplicateResumeResult,
        ),
        (
            vec![failed, failed],
            Sasl2InlineError::DuplicateResumeResult,
        ),
        (vec![failed, bound, bound], Sasl2InlineError::DuplicateBound),
        (vec![resumed, bound], Sasl2InlineError::BoundAfterResumption),
        (vec![bound, resumed], Sasl2InlineError::BoundAfterResumption),
    ] {
        assert_eq!(results(&children), Err(expected), "{children:?}");
    }
}

#[test]
fn xep0388_inline_results_reject_malformed_stream_management_results() {
    for (children, expected) in [
        (
            "<resumed xmlns='urn:xmpp:sm:3' previd='a'/>",
            Sasl2InlineError::InvalidResumed,
        ),
        (
            "<resumed xmlns='urn:xmpp:sm:3' h='1'/>",
            Sasl2InlineError::InvalidResumed,
        ),
        (
            "<resumed xmlns='urn:xmpp:sm:3' h='one' previd='a'/>",
            Sasl2InlineError::InvalidResumed,
        ),
        (
            "<resumed xmlns='urn:xmpp:sm:3' h='1' previd=''/>",
            Sasl2InlineError::InvalidResumed,
        ),
        (
            "<resumed xmlns='urn:xmpp:sm:3' h='1' previd='a' location='x'/>",
            Sasl2InlineError::InvalidResumed,
        ),
        (
            "<failed xmlns='urn:xmpp:sm:3' h='-3'/>",
            Sasl2InlineError::InvalidFailed,
        ),
        (
            "<failed xmlns='urn:xmpp:sm:3' previd='a'/>",
            Sasl2InlineError::InvalidFailed,
        ),
        (
            "<failed xmlns='urn:xmpp:sm:3'>gone</failed>",
            Sasl2InlineError::InvalidFailed,
        ),
        (
            "<failed xmlns='urn:xmpp:sm:3'><item-not-found xmlns='urn:example'/></failed>",
            Sasl2InlineError::InvalidFailed,
        ),
        (
            "<failed xmlns='urn:xmpp:sm:3'>\
               <not-a-condition xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
             </failed>",
            Sasl2InlineError::InvalidFailed,
        ),
        (
            "<failed xmlns='urn:xmpp:sm:3'>\
               <item-not-found xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
               <conflict xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
             </failed>",
            Sasl2InlineError::InvalidFailed,
        ),
        (
            "<enabled xmlns='urn:xmpp:sm:3'/>",
            Sasl2InlineError::UnexpectedStreamManagementElement,
        ),
        (
            "<a xmlns='urn:xmpp:sm:3' h='1'/>",
            Sasl2InlineError::UnexpectedStreamManagementElement,
        ),
    ] {
        assert_eq!(results(&[children]), Err(expected), "{children}");
    }
}

#[test]
fn xep0388_inline_results_keep_other_namespaces_unmodelled() {
    let parsed = results(&[
        "<resumed xmlns='urn:xmpp:sm:2' h='1' previd='a'/>",
        "<bound xmlns='urn:xmpp:bind2:1'/>",
    ])
    .unwrap();

    assert_eq!(parsed.resume, None);
    assert_eq!(parsed.bound, None);
    assert_eq!(parsed.other_results.len(), 2);
}

#[test]
fn xep0198_stream_id_rejects_the_empty_identifier() {
    assert_eq!(SmStreamId::new(""), None);
    assert_eq!(stream_id("abc").as_str(), "abc");
}
