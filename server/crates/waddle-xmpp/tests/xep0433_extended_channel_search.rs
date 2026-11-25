//! XEP-0433: Extended Channel Search dedicated suite.
//!
//! Pins the conformant XEP-0004 search parameter form, XEP-0059 RSM, result
//! addressing, and `<item address='...'>` metadata shape.

use minidom::Element;
use waddle_xmpp::xep::{
    build_search_form_response, build_search_request, build_search_response,
    is_search_form_request, is_search_request, parse_search_request, parse_search_results,
    parse_search_rsm_response, ChannelResult, SearchRequest, SearchRequestError, SearchScope,
    Searchable, SortKey, FIELD_QUERY, NS_CHANNEL_SEARCH, NS_CHANNEL_SEARCH_PARAMS, NS_DATA_FORMS,
    NS_RSM, SORT_KEY_ADDRESS,
};
use xmpp_parsers::iq::Iq;

fn wire_round_trip(iq: Iq) -> Iq {
    let elem = Element::from(iq);
    let xml = String::from(&elem);
    Iq::try_from(xml.parse::<Element>().expect("well-formed XML")).expect("valid IQ")
}

fn muc_service() -> jid::Jid {
    "muc.example.com".parse().expect("valid jid")
}

#[test]
fn xep0433_namespace_is_exact() {
    assert_eq!(NS_CHANNEL_SEARCH, "urn:xmpp:channel-search:0:search");
    assert_eq!(
        NS_CHANNEL_SEARCH_PARAMS,
        "urn:xmpp:channel-search:0:search-params"
    );
}

#[test]
fn xep0433_search_request_is_data_form_plus_rsm() {
    let request = SearchRequest::new("general").with_max(20);
    let iq = wire_round_trip(build_search_request(muc_service(), &request, "search-1"));

    assert!(is_search_request(&iq));
    assert_eq!(iq.id(), "search-1");
    let parsed = parse_search_request(&iq).expect("parses");
    assert_eq!(parsed.query, "general");
    assert_eq!(parsed.max, Some(20));

    let Iq::Get { payload, .. } = &iq else {
        panic!("request must be IQ get");
    };
    let rsm = payload.get_child("set", NS_RSM).expect("RSM set");
    assert_eq!(
        rsm.get_child("max", NS_RSM).map(|value| value.text()),
        Some("20".to_owned())
    );
    let form = payload.get_child("x", NS_DATA_FORMS).expect("data form");
    assert_eq!(form.attr("type"), Some("submit"));
    assert_eq!(
        form.children()
            .find(|field| field.attr("var") == Some("FORM_TYPE"))
            .and_then(|field| field.get_child("value", NS_DATA_FORMS))
            .map(|value| value.text()),
        Some(NS_CHANNEL_SEARCH_PARAMS.to_owned())
    );
    assert_eq!(
        form.children()
            .find(|field| field.attr("var") == Some(FIELD_QUERY))
            .and_then(|field| field.get_child("value", NS_DATA_FORMS))
            .map(|value| value.text()),
        Some("general".to_owned())
    );
    assert!(payload.get_child("query", NS_CHANNEL_SEARCH).is_none());
    assert!(payload.get_child("max", NS_CHANNEL_SEARCH).is_none());
}

#[test]
fn xep0433_empty_search_requests_form_template() {
    let iq = Iq::try_from(
        "<iq xmlns='jabber:client' type='get' id='s0' to='muc.example.com'>\
           <search xmlns='urn:xmpp:channel-search:0:search'/>\
         </iq>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid iq");

    assert!(is_search_request(&iq));
    assert!(is_search_form_request(&iq));

    let response = wire_round_trip(build_search_form_response(&iq));
    let Iq::Result {
        payload: Some(payload),
        ..
    } = response
    else {
        panic!("form response must be an IQ result");
    };
    let form = payload.get_child("x", NS_DATA_FORMS).expect("data form");
    assert_eq!(form.attr("type"), Some("form"));
    assert_eq!(
        form.children()
            .find(|field| field.attr("var") == Some("FORM_TYPE"))
            .and_then(|field| field.get_child("value", NS_DATA_FORMS))
            .map(|value| value.text()),
        Some(NS_CHANNEL_SEARCH_PARAMS.to_owned())
    );
    assert!(form
        .children()
        .any(|field| field.attr("var") == Some(FIELD_QUERY)));
}

#[test]
fn xep0433_request_requires_form_type_but_not_rsm() {
    let no_form_type = "<iq xmlns='jabber:client' type='get' id='s1'>\
        <search xmlns='urn:xmpp:channel-search:0:search'>\
          <set xmlns='http://jabber.org/protocol/rsm'><max>5</max></set>\
          <x xmlns='jabber:x:data' type='submit'><field var='q'><value>room</value></field></x>\
        </search>\
      </iq>";
    let iq = Iq::try_from(no_form_type.parse::<Element>().expect("valid xml")).expect("valid iq");
    assert_eq!(
        parse_search_request(&iq),
        Err(SearchRequestError::Malformed)
    );

    let no_rsm = "<iq xmlns='jabber:client' type='get' id='s2'>\
        <search xmlns='urn:xmpp:channel-search:0:search'>\
          <x xmlns='jabber:x:data' type='submit'>\
            <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:channel-search:0:search-params</value></field>\
            <field var='q'><value>room</value></field>\
          </x>\
        </search>\
      </iq>";
    let iq = Iq::try_from(no_rsm.parse::<Element>().expect("valid xml")).expect("valid iq");
    let parsed = parse_search_request(&iq).expect("no-RSM request is legal");
    assert_eq!(parsed.query, "room");
    assert_eq!(parsed.rsm, None);
}

#[test]
fn xep0433_response_mirrors_id_and_swaps_addressing() {
    let requester: jid::Jid = "hag66@shakespeare.lit/pda".parse().expect("valid jid");
    let mut request_iq = build_search_request(muc_service(), &SearchRequest::new(""), "search-9");
    if let Iq::Get { from, .. } = &mut request_iq {
        *from = Some(requester.clone());
    }

    let response = build_search_response(&request_iq, &[]);
    let Iq::Result { from, to, id, .. } = &response else {
        panic!("search response must be an IQ result");
    };
    assert_eq!(id, "search-9");
    assert_eq!(from.as_ref(), Some(&muc_service()));
    assert_eq!(to.as_ref(), Some(&requester));
}

#[test]
fn xep0433_full_search_round_trip_with_metadata() {
    let request_iq = build_search_request(
        muc_service(),
        &SearchRequest::new("general").with_max(20),
        "search-1",
    );
    let results = vec![
        ChannelResult::new("general@muc.example.com")
            .with_name("General")
            .with_description("Main discussion")
            .with_language("en")
            .with_nusers(42),
        ChannelResult::new("random@muc.example.com").with_name("Random"),
    ];

    let response = wire_round_trip(build_search_response(&request_iq, &results));
    let parsed = parse_search_results(&response);
    assert_eq!(parsed, results);
    let rsm = parse_search_rsm_response(&response).expect("RSM response");
    assert_eq!(rsm.first.as_deref(), Some("general@muc.example.com"));
    assert_eq!(rsm.last.as_deref(), Some("random@muc.example.com"));
    assert_eq!(rsm.count, Some(2));
}

#[test]
fn xep0433_result_items_use_address_and_children() {
    let xml = "<iq xmlns='jabber:client' type='result' id='s1'>\
       <result xmlns='urn:xmpp:channel-search:0:search'>\
         <item address='kept@muc.example.com'>\
           <name>Kept</name>\
           <description>Room description</description>\
           <language>en</language>\
           <nusers>7</nusers>\
         </item>\
       </result>\
     </iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    let parsed = parse_search_results(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].address, "kept@muc.example.com");
    assert_eq!(parsed[0].name.as_deref(), Some("Kept"));
    assert_eq!(parsed[0].description.as_deref(), Some("Room description"));
    assert_eq!(parsed[0].language.as_deref(), Some("en"));
    assert_eq!(parsed[0].nusers, Some(7));
}

#[test]
fn xep0433_item_without_address_is_dropped() {
    let xml = "<iq xmlns='jabber:client' type='result' id='s1'>\
       <result xmlns='urn:xmpp:channel-search:0:search'>\
         <item><name>No address</name></item>\
         <item address='kept@muc.example.com'/>\
       </result>\
     </iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    let parsed = parse_search_results(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].address, "kept@muc.example.com");
}

#[test]
fn xep0433_non_numeric_nusers_is_ignored_not_fatal() {
    let xml = "<iq xmlns='jabber:client' type='result' id='s1'>\
       <result xmlns='urn:xmpp:channel-search:0:search'>\
         <item address='room@muc.example.com'><nusers>many</nusers></item>\
       </result>\
     </iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    let parsed = parse_search_results(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].nusers, None);
}

#[test]
fn xep0433_result_in_wrong_namespace_yields_no_channels() {
    let xml = "<iq xmlns='jabber:client' type='result' id='s1'>\
       <result xmlns='urn:xmpp:channel-search:1'>\
         <item address='room@muc.example.com'/>\
       </result>\
     </iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    assert!(parse_search_results(&iq).is_empty());
}

#[test]
fn xep0433_searchable_matches_name_description_and_address_but_not_language() {
    let channel = ChannelResult::new("general@muc.example.com")
        .with_name("General Discussion")
        .with_description("Main chat for everyone")
        .with_language("fr");
    let all = SearchScope::default();

    assert!(channel.matches_query("GENERAL", all));
    assert!(channel.matches_query("discussion", all));
    assert!(channel.matches_query("everyone", all));
    assert!(channel.matches_query("muc.example.com", all));
    assert!(
        !channel.matches_query("fr", all),
        "language is not a searchable field"
    );
    assert!(!channel.matches_query("random", all));
}

#[test]
fn xep0433_display_name_falls_back_to_address_localpart() {
    assert_eq!(
        ChannelResult::new("room@muc.example.com")
            .with_name("My Room")
            .display_name(),
        "My Room"
    );
    assert_eq!(
        ChannelResult::new("room@muc.example.com").display_name(),
        "room"
    );
}

fn form_request(fields: &str) -> Iq {
    let xml = format!(
        "<iq xmlns='jabber:client' type='get' id='f1'>\
           <search xmlns='urn:xmpp:channel-search:0:search'>\
             <x xmlns='jabber:x:data' type='submit'>\
               <field var='FORM_TYPE' type='hidden'>\
                 <value>urn:xmpp:channel-search:0:search-params</value></field>\
               {fields}\
             </x>\
           </search>\
         </iq>"
    );
    Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq")
}

/// XEP-0433 "No Search Conditions": a form without `q` is `bad-request`.
#[test]
fn xep0433_form_without_q_is_no_search_conditions() {
    assert_eq!(
        parse_search_request(&form_request("")),
        Err(SearchRequestError::NoSearchConditions)
    );
    assert_eq!(
        parse_search_request(&form_request(
            "<field var='sinname'><value>1</value></field>"
        )),
        Err(SearchRequestError::NoSearchConditions)
    );
}

/// XEP-0433 "Invalid Search Terms": `q` present without a usable term.
#[test]
fn xep0433_blank_q_is_invalid_search_terms() {
    for fields in [
        "<field var='q'/>",
        "<field var='q'><value/></field>",
        "<field var='q'><value>   </value></field>",
    ] {
        assert_eq!(
            parse_search_request(&form_request(fields)),
            Err(SearchRequestError::InvalidSearchTerms),
            "{fields}"
        );
    }
}

/// XEP-0433 "Unsupported Sort Key": an unknown `key` value is rejected,
/// a known one parses.
#[test]
fn xep0433_unknown_sort_key_is_invalid_sort_key() {
    let bad = "<field var='q'><value>x</value></field>\
               <field var='key'><value>{urn:example}bogus</value></field>";
    assert_eq!(
        parse_search_request(&form_request(bad)),
        Err(SearchRequestError::InvalidSortKey)
    );
    let good = format!(
        "<field var='q'><value>x</value></field>\
         <field var='key'><value>{SORT_KEY_ADDRESS}</value></field>"
    );
    let parsed = parse_search_request(&form_request(&good)).expect("valid key");
    assert_eq!(parsed.key, SortKey::Address);
}

#[test]
fn xep0433_request_reads_search_scope_types_and_trims_q() {
    let parsed = parse_search_request(&form_request(
        "<field var='q'><value>  room  </value></field>\
         <field var='sinname'><value>false</value></field>\
         <field var='sindescription'><value>0</value></field>\
         <field var='types'><value>xep-0045</value><value>xep-0369</value></field>",
    ))
    .expect("parses");
    assert_eq!(parsed.query, "room");
    assert_eq!(
        parsed.scope,
        SearchScope {
            address: true,
            name: false,
            description: false
        }
    );
    assert_eq!(
        parsed.types,
        Some(vec!["xep-0045".to_owned(), "xep-0369".to_owned()])
    );
}

#[test]
fn xep0433_built_request_round_trips_scope_key_and_types() {
    let mut request = SearchRequest::new("room");
    request.scope.description = false;
    request.types = Some(vec!["xep-0045".to_owned()]);
    let parsed = parse_search_request(&wire_round_trip(build_search_request(
        muc_service(),
        &request,
        "rt",
    )))
    .expect("parses");
    assert_eq!(parsed.scope, request.scope);
    assert_eq!(parsed.types, request.types);
    assert_eq!(parsed.key, SortKey::Address);
}

/// XEP-0433 business rule: the form template "MUST include all fields it
/// supports with their respective default values"; `sinname`,
/// `sindescription` and `key` are REQUIRED, `sinaddress` and `types`
/// RECOMMENDED.
#[test]
fn xep0433_form_template_lists_every_supported_field_with_defaults() {
    let iq = Iq::try_from(
        "<iq xmlns='jabber:client' type='get' id='t1'>\
           <search xmlns='urn:xmpp:channel-search:0:search'/></iq>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid iq");
    let Iq::Result {
        payload: Some(payload),
        ..
    } = wire_round_trip(build_search_form_response(&iq))
    else {
        panic!("form response must be an IQ result");
    };
    let form = payload.get_child("x", NS_DATA_FORMS).expect("data form");
    let field = |var: &str| {
        form.children()
            .find(|field| field.attr("var") == Some(var))
            .unwrap_or_else(|| panic!("template lacks {var}"))
    };
    let value = |var: &str| {
        field(var)
            .get_child("value", NS_DATA_FORMS)
            .map(|value| value.text())
    };
    for var in ["sinaddress", "sinname", "sindescription"] {
        assert_eq!(field(var).attr("type"), Some("boolean"), "{var}");
        assert!(matches!(value(var).as_deref(), Some("1" | "true")), "{var}");
    }
    assert_eq!(field("types").attr("type"), Some("list-multi"));
    assert_eq!(value("types").as_deref(), Some("xep-0045"));
    assert_eq!(field("key").attr("type"), Some("list-single"));
    assert_eq!(value("key").as_deref(), Some(SORT_KEY_ADDRESS));
    let options: Vec<String> = field("key")
        .children()
        .filter(|child| child.name() == "option")
        .filter_map(|option| option.get_child("value", NS_DATA_FORMS))
        .map(|value| value.text())
        .collect();
    assert_eq!(options, vec![SORT_KEY_ADDRESS.to_owned()]);
}

#[test]
fn xep0433_scope_limits_keyword_matching() {
    let ch = ChannelResult::new("ops@muc.example.com")
        .with_name("Operators")
        .with_description("Federation chat");
    let description_only = SearchScope {
        address: false,
        name: false,
        description: true,
    };
    assert!(ch.matches_query("federation", description_only));
    assert!(!ch.matches_query("operators", description_only));
    assert!(!ch.matches_query("ops", description_only));
}
