//! XEP-0202: Entity Time dedicated suite.

use chrono::{FixedOffset, TimeZone, Utc};
use minidom::Element;
use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::xep::{build_time_response, parse_time_response, EntityTime, NS_TIME};
use xmpp_parsers::iq::{Iq, IqType};

fn make_time_query() -> Iq {
    Iq {
        from: Some("alice@localhost/web".parse().expect("valid jid")),
        to: Some("localhost".parse().expect("valid jid")),
        id: "xep0202-1".to_string(),
        payload: IqType::Get(Element::builder("time", NS_TIME).build()),
    }
}

#[test]
fn xep0202_server_disco_advertises_entity_time() {
    assert!(server_features().contains(&Feature::entity_time()));
}

#[test]
fn xep0202_roundtrips_typed_utc_and_tzo_values() {
    let query = make_time_query();
    let entity_time = EntityTime {
        utc: Utc
            .with_ymd_and_hms(2024, 7, 4, 16, 30, 0)
            .single()
            .expect("valid time"),
        tzo: FixedOffset::west_opt(7 * 3600).expect("valid offset"),
    };

    let response = build_time_response(&query, &entity_time);
    let parsed = parse_time_response(&response).expect("typed parse");

    assert_eq!(parsed, entity_time);
}

#[test]
fn xep0202_rejects_non_utc_utc_children() {
    let iq = Iq {
        from: None,
        to: None,
        id: "xep0202-invalid-utc".to_string(),
        payload: IqType::Result(Some(
            Element::builder("time", NS_TIME)
                .append(Element::builder("tzo", NS_TIME).append("+01:00").build())
                .append(
                    Element::builder("utc", NS_TIME)
                        .append("2024-07-04T16:30:00+01:00")
                        .build(),
                )
                .build(),
        )),
    };

    assert!(parse_time_response(&iq).is_none());
}
