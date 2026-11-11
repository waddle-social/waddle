//! XEP-0386 Bind 2 wire conformance: feature advertisement, bind request and
//! `<bound/>` result, including XEP-0198 stream management enabled inline.

use minidom::Element;
use waddle_xmpp_core::disco::Feature;
use waddle_xmpp_core::xep0388::{Sasl2Feature, Sasl2InlineFeatures, Sasl2Success};
use waddle_xmpp_core::xep0388_inline::{
    Bind2Bound, Bind2Feature, Bind2Request, Bind2StreamManagementResult, Bind2Tag,
    Sasl2InlineError, Sasl2InlineOffer, Sasl2InlineResults, SmInlineEnable, SmInlineEnabled,
    SmInlineFailed, SmStreamId, NS_BIND2, NS_SM,
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

#[test]
fn xep0386_feature_parses_the_specification_advertisement() {
    let feature = Sasl2Feature::from_element(&parse(
        "<authentication xmlns='urn:xmpp:sasl:2'>
           <mechanism>SCRAM-SHA-1</mechanism>
           <mechanism>SCRAM-SHA-1-PLUS</mechanism>
           <inline>
             <bind xmlns='urn:xmpp:bind:0'>
               <inline>
                 <feature var='urn:xmpp:carbons:2' />
                 <feature var='urn:xmpp:csi:0' />
                 <feature var='urn:xmpp:sm:3' />
               </inline>
             </bind>
             <sm xmlns='urn:xmpp:sm:3'/>
           </inline>
         </authentication>",
    ))
    .unwrap();
    let offer = Sasl2InlineOffer::from_inline_features(&feature.inline.unwrap()).unwrap();

    assert!(offer.stream_resumption);
    let bind2 = offer.bind2.expect("Bind 2 feature");
    assert_eq!(
        bind2.inline_features,
        vec![
            Feature::new("urn:xmpp:carbons:2"),
            Feature::new("urn:xmpp:csi:0"),
            Feature::new(NS_SM),
        ]
    );
    assert!(bind2.supports_stream_management());
}

#[test]
fn xep0386_feature_round_trips_with_and_without_inline_features() {
    let bare = Bind2Feature::default();
    let wire = reparse(&bare.to_element());
    assert!(wire.is("bind", NS_BIND2));
    assert!(wire.nodes().next().is_none());
    assert_eq!(Bind2Feature::from_element(&wire), Ok(bare.clone()));
    assert!(!bare.supports_stream_management());

    let with_features = Bind2Feature {
        inline_features: vec![Feature::new(NS_SM), Feature::new("urn:xmpp:carbons:2")],
    };
    let wire = reparse(&with_features.to_element());
    let inline = wire.get_child("inline", NS_BIND2).expect("inline list");
    assert_eq!(
        inline
            .children()
            .map(|feature| (feature.name(), feature.ns(), feature.attr("var")))
            .collect::<Vec<_>>(),
        vec![
            ("feature", NS_BIND2.to_owned(), Some(NS_SM)),
            ("feature", NS_BIND2.to_owned(), Some("urn:xmpp:carbons:2")),
        ]
    );
    assert_eq!(Bind2Feature::from_element(&wire), Ok(with_features));
}

#[test]
fn xep0386_feature_lists_a_repeated_var_once() {
    let parsed = Bind2Feature::from_element(&parse(
        "<bind xmlns='urn:xmpp:bind:0'><inline>\
           <feature var='urn:xmpp:sm:3'/><feature var='urn:xmpp:sm:3'/>\
         </inline></bind>",
    ))
    .unwrap();

    assert_eq!(parsed.inline_features, vec![Feature::new(NS_SM)]);
}

#[test]
fn xep0386_feature_rejects_malformed_advertisements() {
    for xml in [
        "<bind xmlns='urn:xmpp:bind:0' version='1'/>",
        "<bind xmlns='urn:xmpp:bind:0'>text</bind>",
        "<bind xmlns='urn:xmpp:bind:0'><inline/><inline/></bind>",
        "<bind xmlns='urn:xmpp:bind:0'><tag>phone</tag></bind>",
        "<bind xmlns='urn:xmpp:bind:0'><feature var='urn:xmpp:sm:3'/></bind>",
        "<bind xmlns='urn:xmpp:bind:0'><inline><feature/></inline></bind>",
        "<bind xmlns='urn:xmpp:bind:0'><inline><feature var=''/></inline></bind>",
        "<bind xmlns='urn:xmpp:bind:0'><inline><feature var='a' type='b'/></inline></bind>",
        "<bind xmlns='urn:xmpp:bind:0'><inline><feature var='a'>x</feature></inline></bind>",
        "<bind xmlns='urn:xmpp:bind:0'><inline><sm xmlns='urn:xmpp:sm:3'/></inline></bind>",
        "<bind xmlns='urn:xmpp:bind:0'><inline>\
           <feature xmlns='http://jabber.org/protocol/disco#info' var='a'/>\
         </inline></bind>",
    ] {
        assert_eq!(
            Bind2Feature::from_element(&parse(xml)),
            Err(Sasl2InlineError::InvalidBind2Feature),
            "{xml}"
        );
    }
}

#[test]
fn xep0386_feature_requires_the_bind_2_namespace() {
    for xml in [
        "<bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/>",
        "<bind xmlns='urn:xmpp:bind2:1'/>",
        "<bound xmlns='urn:xmpp:bind:0'/>",
    ] {
        assert_eq!(
            Bind2Feature::from_element(&parse(xml)),
            Err(Sasl2InlineError::InvalidElement),
            "{xml}"
        );
    }
}

#[test]
fn xep0386_inline_offer_rejects_duplicate_and_misplaced_bind_2_features() {
    let bind = "<bind xmlns='urn:xmpp:bind:0'/>";
    for (features, expected) in [
        (vec![bind, bind], Sasl2InlineError::DuplicateBind2Feature),
        (
            vec!["<bound xmlns='urn:xmpp:bind:0'/>"],
            Sasl2InlineError::UnexpectedBind2Element,
        ),
        (
            vec!["<bind xmlns='urn:xmpp:bind:0'><inline><feature/></inline></bind>"],
            Sasl2InlineError::InvalidBind2Feature,
        ),
    ] {
        let inline = Sasl2InlineFeatures {
            features: features.iter().map(|xml| parse(xml)).collect(),
        };
        assert_eq!(
            Sasl2InlineOffer::from_inline_features(&inline),
            Err(expected),
            "{features:?}"
        );
    }
}

#[test]
fn xep0386_request_parses_the_specification_example() {
    let parsed = Bind2Request::from_element(&parse(
        "<bind xmlns='urn:xmpp:bind:0'>
           <tag>AwesomeXMPP</tag>
           <enable xmlns='urn:xmpp:carbons:2'/>
           <enable xmlns='urn:xmpp:sm:3'/>
           <inactive xmlns='urn:xmpp:csi:0'/>
         </bind>",
    ))
    .unwrap();

    assert_eq!(parsed.tag, Some(Bind2Tag::new("AwesomeXMPP")));
    assert_eq!(parsed.stream_management, Some(SmInlineEnable::default()));
    assert_eq!(
        parsed
            .other_features
            .iter()
            .map(|feature| (feature.name(), feature.ns()))
            .collect::<Vec<_>>(),
        vec![
            ("enable", "urn:xmpp:carbons:2".to_owned()),
            ("inactive", "urn:xmpp:csi:0".to_owned()),
        ]
    );
}

#[test]
fn xep0386_request_round_trips() {
    for request in [
        Bind2Request::default(),
        Bind2Request {
            tag: Some(Bind2Tag::new("Waddle")),
            stream_management: Some(SmInlineEnable {
                resume: true,
                max: Some(300),
            }),
            other_features: vec![Element::builder("enable", "urn:xmpp:carbons:2").build()],
        },
    ] {
        let wire = reparse(&request.to_element());
        assert!(wire.is("bind", NS_BIND2));
        assert_eq!(Bind2Request::from_element(&wire), Ok(request));
    }

    let empty = Bind2Request::default().to_element();
    assert!(empty.nodes().next().is_none());
}

#[test]
fn xep0386_request_rejects_malformed_bind_children() {
    for (xml, expected) in [
        (
            "<bind xmlns='urn:xmpp:bind:0' resource='x'/>",
            Sasl2InlineError::InvalidBindRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'>phone</bind>",
            Sasl2InlineError::InvalidBindRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'><tag>a</tag><tag>b</tag></bind>",
            Sasl2InlineError::InvalidBindRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'><tag><b xmlns='urn:example'/></tag></bind>",
            Sasl2InlineError::InvalidBindRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'><tag lang='en'>a</tag></bind>",
            Sasl2InlineError::InvalidBindRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'><resource>phone</resource></bind>",
            Sasl2InlineError::InvalidBindRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'><inline/></bind>",
            Sasl2InlineError::InvalidBindRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'>\
               <enable xmlns='urn:xmpp:sm:3'/><enable xmlns='urn:xmpp:sm:3'/>\
             </bind>",
            Sasl2InlineError::InvalidEnableRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'><enable xmlns='urn:xmpp:sm:3' resume='yes'/></bind>",
            Sasl2InlineError::InvalidEnableRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'><enable xmlns='urn:xmpp:sm:3' max='0'/></bind>",
            Sasl2InlineError::InvalidEnableRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'><enable xmlns='urn:xmpp:sm:3' id='x'/></bind>",
            Sasl2InlineError::InvalidEnableRequest,
        ),
        (
            "<bind xmlns='urn:xmpp:bind:0'>\
               <resume xmlns='urn:xmpp:sm:3' h='1' previd='a'/>\
             </bind>",
            Sasl2InlineError::UnexpectedStreamManagementElement,
        ),
    ] {
        assert_eq!(
            Bind2Request::from_element(&parse(xml)),
            Err(expected),
            "{xml}"
        );
    }
    assert_eq!(
        Bind2Request::from_element(&parse("<bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/>")),
        Err(Sasl2InlineError::InvalidElement)
    );
}

#[test]
fn xep0198_inline_enable_round_trips_and_accepts_xsd_booleans() {
    let enable = SmInlineEnable {
        resume: true,
        max: Some(120),
    };
    let wire = reparse(&enable.to_element());
    assert!(wire.is("enable", NS_SM));
    assert_eq!(wire.attr("resume"), Some("true"));
    assert_eq!(wire.attr("max"), Some("120"));
    assert_eq!(SmInlineEnable::from_element(&wire), Ok(enable));

    assert!(SmInlineEnable::default()
        .to_element()
        .attrs()
        .iter()
        .next()
        .is_none());
    for (value, expected) in [("1", true), ("true", true), ("0", false), ("false", false)] {
        let wire = Element::builder("enable", NS_SM)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), value)
            .build();
        assert_eq!(
            SmInlineEnable::from_element(&wire).map(|enable| enable.resume),
            Ok(expected)
        );
    }
}

#[test]
fn xep0386_bound_parses_the_specification_example_and_keeps_archive_metadata() {
    let parsed = Bind2Bound::from_element(&parse(
        "<bound xmlns='urn:xmpp:bind:0'>
           <metadata xmlns='urn:xmpp:mam:2'>
             <start id='YWxwaGEg' timestamp='2008-08-22T21:09:04Z' />
             <end id='b21lZ2Eg' timestamp='2020-04-20T14:34:21Z' />
           </metadata>
         </bound>",
    ))
    .unwrap();

    assert_eq!(parsed.stream_management, None);
    assert_eq!(parsed.other_results.len(), 1);
    assert!(parsed.other_results[0].is("metadata", "urn:xmpp:mam:2"));
}

#[test]
fn xep0386_bound_round_trips_inline_stream_management_results() {
    for stream_management in [
        Bind2StreamManagementResult::Enabled(SmInlineEnabled {
            id: Some(SmStreamId::new("some-long-sm-id").unwrap()),
            resume: true,
            max: Some(600),
            location: Some("[2001:41D0:1:A49b::1]:9222".to_owned()),
        }),
        Bind2StreamManagementResult::Enabled(SmInlineEnabled {
            id: None,
            resume: false,
            max: None,
            location: None,
        }),
        Bind2StreamManagementResult::Failed(SmInlineFailed {
            h: None,
            condition: Some(DefinedCondition::InternalServerError),
        }),
    ] {
        let bound = Bind2Bound {
            stream_management: Some(stream_management),
            other_results: vec![Element::builder("metadata", "urn:xmpp:mam:2").build()],
        };
        let wire = reparse(&bound.to_element());
        assert!(wire.is("bound", NS_BIND2));
        assert_eq!(Bind2Bound::from_element(&wire), Ok(bound));
    }
}

#[test]
fn xep0386_bound_rejects_malformed_and_duplicate_results() {
    let enabled = "<enabled xmlns='urn:xmpp:sm:3'/>";
    let failed = "<failed xmlns='urn:xmpp:sm:3'/>";
    for (children, expected) in [
        (
            vec![enabled, enabled],
            Sasl2InlineError::DuplicateEnableResult,
        ),
        (
            vec![enabled, failed],
            Sasl2InlineError::DuplicateEnableResult,
        ),
        (
            vec!["<enabled xmlns='urn:xmpp:sm:3' resume='true'/>"],
            Sasl2InlineError::InvalidEnabled,
        ),
        (
            vec!["<enabled xmlns='urn:xmpp:sm:3' resume='true' id=''/>"],
            Sasl2InlineError::InvalidEnabled,
        ),
        (
            vec!["<enabled xmlns='urn:xmpp:sm:3' resume='maybe' id='a'/>"],
            Sasl2InlineError::InvalidEnabled,
        ),
        (
            vec!["<enabled xmlns='urn:xmpp:sm:3' max='0'/>"],
            Sasl2InlineError::InvalidEnabled,
        ),
        (
            vec!["<enabled xmlns='urn:xmpp:sm:3' h='1'/>"],
            Sasl2InlineError::InvalidEnabled,
        ),
        (
            vec!["<enabled xmlns='urn:xmpp:sm:3'>x</enabled>"],
            Sasl2InlineError::InvalidEnabled,
        ),
        (
            vec!["<failed xmlns='urn:xmpp:sm:3' h='x'/>"],
            Sasl2InlineError::InvalidFailed,
        ),
        (
            vec!["<resumed xmlns='urn:xmpp:sm:3' h='1' previd='a'/>"],
            Sasl2InlineError::UnexpectedStreamManagementElement,
        ),
        (
            vec!["<latest-id xmlns='urn:xmpp:bind:0'>0312a1b8</latest-id>"],
            Sasl2InlineError::InvalidBound,
        ),
    ] {
        let mut bound = Element::builder("bound", NS_BIND2);
        for xml in &children {
            bound = bound.append(parse(xml));
        }
        assert_eq!(
            Bind2Bound::from_element(&bound.build()),
            Err(expected),
            "{children:?}"
        );
    }
    assert_eq!(
        Bind2Bound::from_element(
            &Element::builder("bound", NS_BIND2)
                .append("stray text")
                .build()
        ),
        Err(Sasl2InlineError::InvalidBound)
    );
    assert_eq!(
        Bind2Bound::from_element(&parse("<bound xmlns='urn:xmpp:bind:0' jid='a@b/c'/>")),
        Err(Sasl2InlineError::InvalidBound)
    );
    assert_eq!(
        Bind2Bound::from_element(&parse("<bound xmlns='urn:xmpp:bind2:1'/>")),
        Err(Sasl2InlineError::InvalidElement)
    );
}

#[test]
fn xep0386_success_reports_failed_resume_then_bound_with_inline_enable() {
    let success = Sasl2Success::from_element(&parse(
        "<success xmlns='urn:xmpp:sasl:2'>
           <authorization-identifier>user@example.com/AwesomeXMPP.4232f4d4</authorization-identifier>
           <failed xmlns='urn:xmpp:sm:3' h='7'>
             <item-not-found xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
           </failed>
           <bound xmlns='urn:xmpp:bind:0'>
             <enabled xmlns='urn:xmpp:sm:3' id='new-sm-id' resume='true'/>
           </bound>
         </success>",
    ))
    .unwrap();
    let results = Sasl2InlineResults::from_success(&success).unwrap();

    assert_eq!(
        success
            .authorization_identifier
            .resource()
            .map(|r| r.as_str()),
        Some("AwesomeXMPP.4232f4d4")
    );
    assert_eq!(
        results.bound.and_then(|bound| bound.stream_management),
        Some(Bind2StreamManagementResult::Enabled(SmInlineEnabled {
            id: Some(SmStreamId::new("new-sm-id").unwrap()),
            resume: true,
            max: None,
            location: None,
        }))
    );
}
