use super::*;

use chrono::Datelike;

fn make_mam_result_message(
    mam_id: &str,
    query_id: &str,
    stamp: &str,
    from: &str,
    msg_type: &str,
    body: &str,
) -> Element {
    let inner_message = Element::builder("message", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), from)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), msg_type)
        .append(Element::builder("body", CLIENT_NS).append(body).build())
        .build();

    let delay = Element::builder("delay", DELAY_NS)
        .attr(minidom::rxml::xml_ncname!("stamp").to_owned(), stamp)
        .build();

    let forwarded = Element::builder("forwarded", FORWARD_NS)
        .append(delay)
        .append(inner_message)
        .build();

    let result = Element::builder("result", MAM_NS)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), mam_id)
        .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), query_id)
        .append(forwarded)
        .build();

    Element::builder("message", CLIENT_NS)
        .append(result)
        .build()
}

fn make_mam_fin(
    complete: bool,
    first: Option<&str>,
    last: Option<&str>,
    count: Option<u32>,
) -> Element {
    let mut set_builder = Element::builder("set", RSM_NS);

    if let Some(f) = first {
        set_builder = set_builder.append(Element::builder("first", RSM_NS).append(f).build());
    }
    if let Some(l) = last {
        set_builder = set_builder.append(Element::builder("last", RSM_NS).append(l).build());
    }
    if let Some(c) = count {
        set_builder = set_builder.append(
            Element::builder("count", RSM_NS)
                .append(c.to_string())
                .build(),
        );
    }

    let mut fin_builder = Element::builder("fin", MAM_NS);
    if complete {
        fin_builder = fin_builder.attr(minidom::rxml::xml_ncname!("complete").to_owned(), "true");
    }
    fin_builder.append(set_builder.build()).build()
}

#[test]
fn parse_mam_result_happy_path() {
    let el = make_mam_result_message(
        "mam-id-1",
        "qid-42",
        "2024-01-01T12:00:00Z",
        "alice@example.com/res",
        "chat",
        "Hello world",
    );

    let archived = parse_mam_result(&el).expect("should parse");

    assert_eq!(archived.mam_id, "mam-id-1");
    assert_eq!(archived.query_id.as_deref(), Some("qid-42"));
    assert_eq!(archived.message_type, "chat");
    assert_eq!(archived.body.as_deref(), Some("Hello world"));
    assert_eq!(archived.from.as_deref(), Some("alice@example.com/res"));
    assert!(archived.id.is_none());
    assert!(archived.origin_id.is_none());
    assert!(archived.timestamp.is_some());
    let ts = archived.timestamp.unwrap();
    assert_eq!(ts.year(), 2024);
}

#[test]
fn parse_mam_result_preserves_multiple_typed_stanza_ids() {
    let inner_message = Element::builder("message", CLIENT_NS)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "room@conf.example/alice",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
        .append(Element::builder("body", CLIENT_NS).append("Hello").build())
        .append(
            Element::builder("stanza-id", XEP0359_NS)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "foreign-sid")
                .attr(
                    minidom::rxml::xml_ncname!("by").to_owned(),
                    "archive.example",
                )
                .build(),
        )
        .append(
            Element::builder("stanza-id", XEP0359_NS)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "room-sid")
                .attr(
                    minidom::rxml::xml_ncname!("by").to_owned(),
                    "room@conf.example",
                )
                .build(),
        )
        .append(
            Element::builder("stanza-id", XEP0359_NS)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "bad-sid")
                .attr(minidom::rxml::xml_ncname!("by").to_owned(), "")
                .build(),
        )
        .build();
    let forwarded = Element::builder("forwarded", FORWARD_NS)
        .append(inner_message)
        .build();
    let result = Element::builder("result", MAM_NS)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "mam-id-1")
        .append(forwarded)
        .build();
    let el = Element::builder("message", CLIENT_NS)
        .append(result)
        .build();

    let archived = parse_mam_result(&el).expect("should parse");

    let stanza_ids: Vec<(String, String)> = archived
        .stanza_ids
        .iter()
        .map(|stanza_id| (stanza_id.id.clone(), stanza_id.by.to_string()))
        .collect();
    assert_eq!(
        stanza_ids,
        vec![
            ("foreign-sid".to_string(), "archive.example".to_string()),
            ("room-sid".to_string(), "room@conf.example".to_string()),
        ]
    );
    assert_eq!(
        archived.stanza_id.as_ref().map(|id| id.as_str()),
        Some("foreign-sid")
    );
}

#[test]
fn parse_mam_result_preserves_inner_message_and_origin_ids() {
    let inner = Element::builder("message", CLIENT_NS)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "alice@example.com/desktop",
        )
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            "bob@example.com",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            "direct-message-id",
        )
        .append(
            Element::builder("origin-id", XEP0359_NS)
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    "direct-origin-id",
                )
                .build(),
        )
        .append(Element::builder("body", CLIENT_NS).append("hi").build())
        .build();
    let forwarded = Element::builder("forwarded", FORWARD_NS)
        .append(
            Element::builder("delay", DELAY_NS)
                .attr(
                    minidom::rxml::xml_ncname!("stamp").to_owned(),
                    "2024-01-01T12:00:00Z",
                )
                .build(),
        )
        .append(inner)
        .build();
    let result = Element::builder("result", MAM_NS)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            "mam-id-with-business-id",
        )
        .attr(
            minidom::rxml::xml_ncname!("queryid").to_owned(),
            "qid-business",
        )
        .append(forwarded)
        .build();
    let el = Element::builder("message", CLIENT_NS)
        .append(result)
        .build();

    let archived = parse_mam_result(&el).expect("should parse");

    assert_eq!(
        archived.id.as_ref().map(|id| id.as_str()),
        Some("direct-message-id")
    );
    assert_eq!(
        archived.origin_id.as_ref().map(|id| id.as_str()),
        Some("direct-origin-id")
    );
}

#[test]
fn parse_mam_result_extracts_archived_author_real_jid() {
    let inner = Element::builder("message", CLIENT_NS)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "room@muc.example.com/alice",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
        .append(
            Element::builder("body", CLIENT_NS)
                .append("Hello world")
                .build(),
        )
        .append(
            Element::builder("x", NS_MUC_USER)
                .append(
                    Element::builder("item", NS_MUC_USER)
                        .attr(
                            minidom::rxml::xml_ncname!("jid").to_owned(),
                            "alice@example.com/phone",
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    let delay = Element::builder("delay", DELAY_NS)
        .attr(
            minidom::rxml::xml_ncname!("stamp").to_owned(),
            "2024-01-01T12:00:00Z",
        )
        .build();
    let forwarded = Element::builder("forwarded", FORWARD_NS)
        .append(delay)
        .append(inner)
        .build();
    let result = Element::builder("result", MAM_NS)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "mam-id-2")
        .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "qid-43")
        .append(forwarded)
        .build();
    let el = Element::builder("message", CLIENT_NS)
        .append(result)
        .build();

    let archived = parse_mam_result(&el).expect("should parse");

    assert_eq!(
        archived.author_real_jid.as_deref(),
        Some("alice@example.com")
    );
}

#[test]
fn xep_0201_parses_archived_message_with_thread_parent() {
    // Locks the typed-parent surface on the client `ArchivedMessage`:
    // a MAM result carrying `<thread parent='X'>id</thread>` populates
    // both `thread` and `parent_thread_id` instead of dropping parent.
    // FFI consumers (Swift/Kotlin) read `archived.parent_thread_id`
    // directly via `archived_to_ffi`.
    let inner = Element::builder("message", CLIENT_NS)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "alice@example.com/web",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .append(Element::builder("body", CLIENT_NS).append("hi").build())
        .append(
            Element::builder("thread", CLIENT_NS)
                .attr(
                    minidom::rxml::xml_ncname!("parent").to_owned(),
                    "root-thread",
                )
                .append("child-thread")
                .build(),
        )
        .build();
    let forwarded = Element::builder("forwarded", "urn:xmpp:forward:0")
        .append(
            Element::builder("delay", "urn:xmpp:delay")
                .attr(
                    minidom::rxml::xml_ncname!("stamp").to_owned(),
                    "2024-01-01T12:00:00Z",
                )
                .build(),
        )
        .append(inner)
        .build();
    let result = Element::builder("message", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "normal")
        .append(
            Element::builder("result", MAM_NS)
                .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q1")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "mam-1")
                .append(forwarded)
                .build(),
        )
        .build();

    let archived = parse_mam_result(&result).expect("should parse");
    assert_eq!(archived.thread.as_deref(), Some("child-thread"));
    assert_eq!(archived.parent_thread_id.as_deref(), Some("root-thread"));
}

#[test]
fn xep_0201_parses_archived_message_without_thread_parent() {
    // Root-only thread: parent_thread_id stays None.
    let inner = Element::builder("message", CLIENT_NS)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "alice@example.com/web",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .append(Element::builder("body", CLIENT_NS).append("hi").build())
        .append(
            Element::builder("thread", CLIENT_NS)
                .append("root-thread")
                .build(),
        )
        .build();
    let forwarded = Element::builder("forwarded", "urn:xmpp:forward:0")
        .append(
            Element::builder("delay", "urn:xmpp:delay")
                .attr(
                    minidom::rxml::xml_ncname!("stamp").to_owned(),
                    "2024-01-01T12:00:00Z",
                )
                .build(),
        )
        .append(inner)
        .build();
    let result = Element::builder("message", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "normal")
        .append(
            Element::builder("result", MAM_NS)
                .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q1")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "mam-1")
                .append(forwarded)
                .build(),
        )
        .build();

    let archived = parse_mam_result(&result).expect("should parse");
    assert_eq!(archived.thread.as_deref(), Some("root-thread"));
    assert_eq!(archived.parent_thread_id, None);
}

#[test]
fn parse_mam_result_ignores_non_mam_message() {
    let plain = Element::builder("message", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .append(Element::builder("body", CLIENT_NS).append("plain").build())
        .build();

    assert!(parse_mam_result(&plain).is_none());
}

#[test]
fn parse_mam_fin_complete() {
    let fin = make_mam_fin(true, Some("first-id"), Some("last-id"), Some(42));
    let (rsm, is_complete) = parse_mam_fin(&fin).expect("should parse");

    assert!(is_complete);
    assert!(rsm.is_complete);
    assert_eq!(rsm.first.as_deref(), Some("first-id"));
    assert_eq!(rsm.last.as_deref(), Some("last-id"));
    assert_eq!(rsm.count, Some(42));
}

#[test]
fn parse_mam_fin_partial() {
    let fin = make_mam_fin(false, Some("a"), Some("b"), None);
    let (rsm, is_complete) = parse_mam_fin(&fin).expect("should parse");

    assert!(!is_complete);
    assert!(!rsm.is_complete);
    assert_eq!(rsm.first.as_deref(), Some("a"));
    assert_eq!(rsm.last.as_deref(), Some("b"));
    assert_eq!(rsm.count, None);
}

#[test]
fn rsm_page_info_defaults() {
    let info = RsmPageInfo::default();
    assert!(info.first.is_none());
    assert!(info.last.is_none());
    assert!(info.count.is_none());
    assert!(info.index.is_none());
    assert!(!info.is_complete);
}

#[test]
fn build_mam_iq_extended_supports_thread_fulltext_and_after() {
    let iq = MamIqBuilder::new("iq-1", "query-1", 25)
        .after("after-1")
        .with_jid("alice@example.com")
        .to_jid("room@muc.example.com")
        .thread_id("thread-42")
        .fulltext("needle")
        .start("2024-01-01T00:00:00Z")
        .end("2024-01-31T23:59:59Z")
        .build();
    let query = iq.get_child("query", MAM_NS).expect("query child");
    let form = query.get_child("x", DATA_FORMS_NS).expect("form child");
    let fields: Vec<(String, String)> = form
        .children()
        .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
        .filter_map(|child| {
            Some((
                child.attr("var")?.to_string(),
                child.get_child("value", DATA_FORMS_NS)?.text(),
            ))
        })
        .collect();
    assert!(fields.contains(&("with".to_string(), "alice@example.com".to_string())));
    assert!(fields.contains(&(WADDLE_MAM_THREAD_FIELD.to_string(), "thread-42".to_string())));
    assert!(fields.contains(&(FULLTEXT_MAM_FIELD.to_string(), "needle".to_string())));
    assert!(fields.contains(&(
        MAM_START_FIELD.to_string(),
        "2024-01-01T00:00:00Z".to_string()
    )));
    assert!(fields.contains(&(
        MAM_END_FIELD.to_string(),
        "2024-01-31T23:59:59Z".to_string()
    )));
    let set = query.get_child("set", RSM_NS).expect("rsm set");
    assert_eq!(
        set.get_child("after", RSM_NS).map(|child| child.text()),
        Some("after-1".to_string())
    );
    assert_eq!(iq.attr("to"), Some("room@muc.example.com"));
}

#[test]
fn build_mam_iq_extended_supports_latest_before_marker() {
    let iq = MamIqBuilder::new("iq-2", "query-2", 10).before("").build();
    let before = iq
        .get_child("query", MAM_NS)
        .and_then(|query| query.get_child("set", RSM_NS))
        .and_then(|set| set.get_child("before", RSM_NS))
        .expect("before child");
    assert_eq!(before.text(), "");
}

#[test]
fn build_mam_iq_preserves_existing_before_behavior() {
    let iq = build_mam_iq(
        "iq-3",
        "query-3",
        50,
        Some("last-id"),
        Some("bob@example.com"),
        None,
    );
    let query = iq.get_child("query", MAM_NS).expect("query child");
    let set = query.get_child("set", RSM_NS).expect("set child");
    assert_eq!(
        set.get_child("before", RSM_NS).map(|child| child.text()),
        Some("last-id".to_string())
    );
    let fields: Vec<(String, String)> = query
        .get_child("x", DATA_FORMS_NS)
        .expect("form child")
        .children()
        .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
        .filter_map(|child| {
            Some((
                child.attr("var")?.to_string(),
                child.get_child("value", DATA_FORMS_NS)?.text(),
            ))
        })
        .collect();
    assert!(fields.contains(&("with".to_string(), "bob@example.com".to_string())));
}

// ── XEP-0313 full-text search builder tests ──────────────────────────────

fn mam_form_value(iq: &Element, field_var: &str) -> Option<String> {
    iq.get_child("query", MAM_NS)
        .and_then(|query| query.get_child("x", DATA_FORMS_NS))
        .and_then(|form| {
            form.children()
                .find(|field| field.name() == "field" && field.attr("var") == Some(field_var))
        })
        .and_then(|field| field.get_child("value", DATA_FORMS_NS))
        .map(|element| element.text())
}

fn rsm_before(iq: &Element) -> Option<String> {
    iq.get_child("query", MAM_NS)
        .and_then(|query| query.get_child("set", RSM_NS))
        .and_then(|set| set.get_child("before", RSM_NS))
        .map(|element| element.text())
}

#[test]
fn room_search_history_targets_room_archive_with_fulltext() {
    let iq = build_room_search_history_iq("iq-1", "query-1", 10, "room@muc.example.com", "needle");

    assert_eq!(iq.attr("to"), Some("room@muc.example.com"));
    assert_eq!(
        mam_form_value(&iq, FULLTEXT_MAM_FIELD).as_deref(),
        Some("needle")
    );
    // Empty `<before/>` = newest matching page first (XEP-0059 §2.5).
    assert_eq!(rsm_before(&iq).as_deref(), Some(""));
    assert!(
        mam_form_value(&iq, "with").is_none(),
        "room search must not carry a with filter"
    );
}

#[test]
fn dm_search_history_targets_account_archive_and_filters_peer() {
    let iq = build_dm_search_history_iq(
        "iq-1",
        "query-1",
        10,
        "alice@example.com",
        "bob@example.com",
        "quarterly report",
    );

    assert_eq!(iq.attr("to"), Some("alice@example.com"));
    assert_eq!(
        mam_form_value(&iq, "with").as_deref(),
        Some("bob@example.com")
    );
    assert_eq!(
        mam_form_value(&iq, FULLTEXT_MAM_FIELD).as_deref(),
        Some("quarterly report")
    );
    assert_eq!(rsm_before(&iq).as_deref(), Some(""));
}

// ── XEP-0359 stanza-id filter tests ─────────────────────────────────────

#[cfg(test)]
mod stanza_id_filter_tests {
    use minidom::Element;
    use waddle_xmpp_core::mam::{MAM_NS, RSM_NS, STANZA_ID_FILTER_FIELD};

    use super::super::{MamIqBuilder, DATA_FORMS_NS};

    #[test]
    fn builder_emits_minimal_iq_with_no_setters() {
        // No to_jid, no before/after, no filter fields — only FORM_TYPE.
        let iq = MamIqBuilder::new("iq1", "q1", 10).build();
        assert!(
            iq.attr("to").is_none(),
            "no to attribute when to_jid setter not called"
        );
        let query = iq.get_child("query", MAM_NS).expect("query child");
        let form = query.get_child("x", DATA_FORMS_NS).expect("form");
        let vars: Vec<&str> = form
            .children()
            .filter(|c| c.name() == "field")
            .filter_map(|c| c.attr("var"))
            .collect();
        assert_eq!(vars, vec!["FORM_TYPE"], "only FORM_TYPE field present");
        let rsm = query.get_child("set", RSM_NS).expect("rsm set");
        assert!(
            rsm.get_child("before", RSM_NS).is_none(),
            "no before element when before setter not called"
        );
        assert!(
            rsm.get_child("after", RSM_NS).is_none(),
            "no after element when after setter not called"
        );
    }

    #[test]
    fn build_mam_iq_round_trips_through_parse_mam_query() {
        use std::str::FromStr;
        use waddle_xmpp_core::mam::{parse_mam_query, MamFilterStanzaId};

        let iq_elem = MamIqBuilder::new("iq1", "q1", 10)
            .to_jid("room@conf.example")
            .stanza_ids(&["sid-A", "sid-B"])
            .build();

        // Serialize to bytes, parse back as minidom Element, then as Iq.
        let mut bytes = Vec::<u8>::new();
        iq_elem
            .write_to(&mut bytes)
            .expect("element serializes to bytes");
        let xml = String::from_utf8(bytes).expect("minidom emits valid UTF-8");
        let reparsed = Element::from_str(&xml).expect("element round-trips through XML");
        let iq = xmpp_parsers::iq::Iq::try_from(reparsed).expect("Iq parse from element");

        let (_query_id, query) = parse_mam_query(&iq).expect("query parses");
        assert_eq!(
            query.stanza_ids,
            vec![
                MamFilterStanzaId::new("sid-A").unwrap(),
                MamFilterStanzaId::new("sid-B").unwrap(),
            ],
            "stanza-id filter values must survive builder → serialize → parse round-trip"
        );
    }

    #[test]
    fn builder_appends_stanza_id_filter_when_provided() {
        let iq = MamIqBuilder::new("iq1", "q1", 10)
            .to_jid("room@conf.example")
            .stanza_ids(&["sid-A", "sid-B"])
            .build();
        let query = iq.get_child("query", MAM_NS).expect("query child");
        let form = query.get_child("x", DATA_FORMS_NS).expect("form");
        let field = form
            .children()
            .find(|c| c.name() == "field" && c.attr("var") == Some(STANZA_ID_FILTER_FIELD))
            .expect("stanza-id filter field present");
        let values: Vec<String> = field
            .children()
            .filter(|c| c.name() == "value")
            .map(Element::text)
            .collect();
        assert_eq!(values, vec!["sid-A".to_string(), "sid-B".to_string()]);
    }

    #[test]
    fn builder_omits_stanza_id_filter_when_none() {
        let iq = MamIqBuilder::new("iq1", "q1", 10)
            .to_jid("room@conf.example")
            .build();
        let query = iq.get_child("query", MAM_NS).expect("query child");
        let form = query.get_child("x", DATA_FORMS_NS).expect("form");
        assert!(form
            .children()
            .all(|c| c.attr("var") != Some(STANZA_ID_FILTER_FIELD)));
    }

    #[test]
    fn builder_omits_stanza_id_filter_when_empty_slice() {
        let iq = MamIqBuilder::new("iq1", "q1", 10)
            .to_jid("room@conf.example")
            .stanza_ids(&[])
            .build();
        let query = iq.get_child("query", MAM_NS).expect("query child");
        let form = query.get_child("x", DATA_FORMS_NS).expect("form");
        assert!(form
            .children()
            .all(|c| c.attr("var") != Some(STANZA_ID_FILTER_FIELD)));
    }

    #[test]
    fn builder_emits_stanza_id_filter_alongside_thread_and_fulltext() {
        use waddle_xmpp_core::mam::{FULLTEXT_MAM_FIELD, WADDLE_MAM_THREAD_FIELD};

        let iq = MamIqBuilder::new("iq1", "q1", 10)
            .to_jid("room@conf.example")
            .thread_id("thread-X")
            .fulltext("hello")
            .stanza_ids(&["sid-A", "sid-B"])
            .build();
        let query = iq.get_child("query", MAM_NS).expect("query child");
        let form = query.get_child("x", DATA_FORMS_NS).expect("form");

        let vars: Vec<&str> = form
            .children()
            .filter(|c| c.name() == "field")
            .filter_map(|c| c.attr("var"))
            .collect();

        assert!(
            vars.contains(&STANZA_ID_FILTER_FIELD),
            "missing stanza-id field; got {vars:?}"
        );
        assert!(
            vars.contains(&WADDLE_MAM_THREAD_FIELD),
            "missing thread field; got {vars:?}"
        );
        assert!(
            vars.contains(&FULLTEXT_MAM_FIELD),
            "missing fulltext field; got {vars:?}"
        );
    }
}

// ── Query orchestration integration tests ────────────────────────────────

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod query {
    use std::sync::{Arc, RwLock};
    use std::time::Duration;

    use minidom::Element;
    use tokio::sync::{broadcast, mpsc, oneshot};
    use tokio::time::timeout;

    use super::super::{ArchivedMessage, ArchivedPayload, MamExt, CLIENT_NS};
    use crate::client::ClientHandle;
    use crate::command::XmppCommand;
    use crate::event::ClientEvent;
    use crate::state::{SessionBinding, SessionPhase, SessionSnapshot};
    use waddle_xmpp_core::mam::{FULLTEXT_MAM_FIELD, MAM_NS, RSM_NS};

    fn make_handle() -> (
        ClientHandle,
        mpsc::Receiver<XmppCommand>,
        broadcast::Sender<ClientEvent>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<XmppCommand>(4);
        let (evt_tx, _) = broadcast::channel::<ClientEvent>(64);
        let state = Arc::new(RwLock::new(SessionSnapshot::new()));
        let handle = ClientHandle::from_parts(cmd_tx, evt_tx.clone(), state);
        (handle, cmd_rx, evt_tx)
    }

    /// Handle with an established session bound to `alice@example.com/res`,
    /// as `search_dm_history` derives the personal-archive address from the
    /// bound JID.
    fn make_bound_handle() -> (
        ClientHandle,
        mpsc::Receiver<XmppCommand>,
        broadcast::Sender<ClientEvent>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<XmppCommand>(4);
        let (evt_tx, _) = broadcast::channel::<ClientEvent>(64);
        let mut snapshot = SessionSnapshot::new();
        snapshot.phase = SessionPhase::Established;
        snapshot.binding = Some(SessionBinding {
            jid: "alice@example.com/res".parse().expect("valid bound jid"),
            stream_id: None,
            resumable: false,
        });
        let state = Arc::new(RwLock::new(snapshot));
        let handle = ClientHandle::from_parts(cmd_tx, evt_tx.clone(), state);
        (handle, cmd_rx, evt_tx)
    }

    fn build_archived(mam_id: &str, query_id: &str, body: &str) -> ArchivedMessage {
        let stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
            mam_id,
            "room@muc.example.com".parse().expect("valid archive jid"),
        );
        ArchivedMessage {
            mam_id: mam_id.to_string(),
            query_id: Some(query_id.to_string()),
            id: None,
            stanza_id: Some(stanza_id.clone()),
            origin_id: None,
            timestamp: None,
            from: Some("room@muc.example.com/alice".to_string()),
            to: Some("alice@example.com/res".to_string()),
            stanza_ids: vec![stanza_id],
            parent_thread_id: None,
            message_type: "groupchat".to_string(),
            body: Some(body.to_string()),
            thread: None,
            author_real_jid: None,
            inner: Element::builder("message", CLIENT_NS).build(),
            payload: ArchivedPayload::default(),
        }
    }

    fn build_fin_iq(iq_id: &str, first: &str, last: &str, count: u32) -> Element {
        let set = Element::builder("set", RSM_NS)
            .append(Element::builder("first", RSM_NS).append(first).build())
            .append(Element::builder("last", RSM_NS).append(last).build())
            .append(
                Element::builder("count", RSM_NS)
                    .append(count.to_string())
                    .build(),
            )
            .build();

        let fin = Element::builder("fin", MAM_NS)
            .attr(minidom::rxml::xml_ncname!("complete").to_owned(), "true")
            .append(set)
            .build();

        Element::builder("iq", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id)
            .append(fin)
            .build()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_room_history_collects_results_then_resolves_on_fin() {
        let (handle, mut cmd_rx, evt_tx) = make_handle();

        tokio::spawn(async move {
            let cmd = cmd_rx.recv().await.expect("driver received cmd");
            let (stanza, responder) = match cmd {
                XmppCommand::SendIq { stanza, responder } => (stanza, responder),
                other => panic!("unexpected command: {other:?}"),
            };

            let query_id = stanza
                .get_child("query", MAM_NS)
                .and_then(|q| q.attr("queryid"))
                .expect("queryid attribute on <query>")
                .to_string();
            let iq_id = stanza.attr("id").expect("id attribute on <iq>").to_string();

            // Broadcast MAM results carrying the correct queryid, plus one
            // result with a foreign queryid that must be filtered out.
            for i in 0..5u32 {
                evt_tx
                    .send(ClientEvent::MamResult(Box::new(build_archived(
                        &format!("mam-{i}"),
                        &query_id,
                        &format!("hello {i}"),
                    ))))
                    .expect("broadcast MAM result");
            }

            evt_tx
                .send(ClientEvent::MamResult(Box::new(build_archived(
                    "mam-other",
                    "some-other-query",
                    "noise",
                ))))
                .expect("broadcast foreign MAM result");

            // Resolve send_iq with the <fin/> IQ — must be after the
            // MAM broadcasts to match XEP-0313 server behaviour.
            responder
                .send(Ok(build_fin_iq(&iq_id, "mam-0", "mam-4", 5)))
                .expect("responder not dropped");
        });

        let page = timeout(
            Duration::from_secs(2),
            handle.fetch_room_history("room@muc.example.com", 50, None),
        )
        .await
        .expect("run_mam_query must resolve once <fin/> arrives (not block for 30s)")
        .expect("fetch_room_history succeeds");

        assert_eq!(
            page.messages.len(),
            5,
            "only messages matching the query_id must be collected"
        );
        for (i, msg) in page.messages.iter().enumerate() {
            assert_eq!(msg.mam_id, format!("mam-{i}"));
            assert_eq!(msg.body.as_deref(), Some(format!("hello {i}").as_str()));
        }
        assert!(page.is_complete, "<fin complete='true'/> must propagate");
        assert_eq!(page.rsm.first.as_deref(), Some("mam-0"));
        assert_eq!(page.rsm.last.as_deref(), Some("mam-4"));
        assert_eq!(page.rsm.count, Some(5));
    }

    #[test]
    fn collector_dedups_replayed_results_and_filters_foreign_query_ids() {
        let mut collector = super::super::MamResultCollector::new("query-1");

        // Original delivery.
        collector.collect(build_archived("mam-0", "query-1", "hello 0"));
        collector.collect(build_archived("mam-1", "query-1", "hello 1"));
        // XEP-0198 resume replays the unacked tail: same queryid, same mam_id.
        collector.collect(build_archived("mam-1", "query-1", "hello 1"));
        collector.collect(build_archived("mam-0", "query-1", "hello 0"));
        // A result for another open query must never be collected.
        collector.collect(build_archived("mam-9", "some-other-query", "noise"));

        let messages = collector.into_messages();
        assert_eq!(
            messages.len(),
            2,
            "replayed results with an already-collected mam_id must be dropped"
        );
        assert_eq!(messages[0].mam_id, "mam-0");
        assert_eq!(messages[1].mam_id, "mam-1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_room_history_dedups_replayed_results_by_mam_id() {
        let (handle, mut cmd_rx, evt_tx) = make_handle();

        tokio::spawn(async move {
            let cmd = cmd_rx.recv().await.expect("driver received cmd");
            let (stanza, responder) = match cmd {
                XmppCommand::SendIq { stanza, responder } => (stanza, responder),
                other => panic!("unexpected command: {other:?}"),
            };

            let query_id = stanza
                .get_child("query", MAM_NS)
                .and_then(|q| q.attr("queryid"))
                .expect("queryid attribute on <query>")
                .to_string();
            let iq_id = stanza.attr("id").expect("id attribute on <iq>").to_string();

            // Original delivery.
            for i in 0..3u32 {
                evt_tx
                    .send(ClientEvent::MamResult(Box::new(build_archived(
                        &format!("mam-{i}"),
                        &query_id,
                        &format!("hello {i}"),
                    ))))
                    .expect("broadcast MAM result");
            }

            // XEP-0198 resume replays the unacked tail: same queryid, same
            // mam_id, delivered again before the (also replayed) <fin/>.
            for i in 1..3u32 {
                evt_tx
                    .send(ClientEvent::MamResult(Box::new(build_archived(
                        &format!("mam-{i}"),
                        &query_id,
                        &format!("hello {i}"),
                    ))))
                    .expect("broadcast replayed MAM result");
            }

            responder
                .send(Ok(build_fin_iq(&iq_id, "mam-0", "mam-2", 3)))
                .expect("responder not dropped");
        });

        let page = timeout(
            Duration::from_secs(2),
            handle.fetch_room_history("room@muc.example.com", 50, None),
        )
        .await
        .expect("run_mam_query must resolve once <fin/> arrives")
        .expect("fetch_room_history succeeds");

        assert_eq!(
            page.messages.len(),
            3,
            "replayed results with an already-collected mam_id must be dropped"
        );
        for (i, msg) in page.messages.iter().enumerate() {
            assert_eq!(msg.mam_id, format!("mam-{i}"));
        }
        assert!(page.is_complete);
        assert_eq!(page.rsm.first.as_deref(), Some("mam-0"));
        assert_eq!(page.rsm.last.as_deref(), Some("mam-2"));
        assert_eq!(page.rsm.count, Some(3));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_room_history_propagates_send_iq_error() {
        let (handle, mut cmd_rx, _evt_tx) = make_handle();

        tokio::spawn(async move {
            let cmd = cmd_rx.recv().await.expect("driver received cmd");
            let XmppCommand::SendIq { responder, .. } = cmd else {
                panic!("expected SendIq");
            };
            // Drop the responder to simulate a disconnected driver.
            drop(responder);
        });

        let result = timeout(
            Duration::from_secs(2),
            handle.fetch_room_history("room@muc.example.com", 50, None),
        )
        .await
        .expect("must not hang the full 30s timeout");

        assert!(result.is_err(), "dropped responder must surface as error");
    }

    /// Mock driver for the search verbs: answers the first `SendIq` with a
    /// complete empty `<fin/>` and hands the sent stanza back for
    /// wire-shape assertions in the test body.
    fn spawn_fin_responder(mut cmd_rx: mpsc::Receiver<XmppCommand>) -> oneshot::Receiver<Element> {
        let (stanza_tx, stanza_rx) = oneshot::channel();
        tokio::spawn(async move {
            let cmd = cmd_rx.recv().await.expect("driver received cmd");
            let (stanza, responder) = match cmd {
                XmppCommand::SendIq { stanza, responder } => (stanza, responder),
                other => panic!("unexpected command: {other:?}"),
            };
            let iq_id = stanza.attr("id").expect("id attribute on <iq>").to_string();
            responder
                .send(Ok(build_fin_iq(&iq_id, "", "", 0)))
                .expect("responder not dropped");
            stanza_tx.send(stanza).expect("test still listening");
        });
        stanza_rx
    }

    fn form_field_value(iq: &Element, field_var: &str) -> Option<String> {
        iq.get_child("query", MAM_NS)
            .and_then(|query| query.get_child("x", "jabber:x:data"))
            .and_then(|form| {
                form.children()
                    .find(|field| field.name() == "field" && field.attr("var") == Some(field_var))
            })
            .and_then(|field| field.get_child("value", "jabber:x:data"))
            .map(|element| element.text())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_room_history_sends_fulltext_query_to_room_archive() {
        let (handle, cmd_rx, _evt_tx) = make_handle();
        let stanza_rx = spawn_fin_responder(cmd_rx);

        let page = timeout(
            Duration::from_secs(2),
            handle.search_room_history("room@muc.example.com", "needle", 25),
        )
        .await
        .expect("search must resolve once <fin/> arrives")
        .expect("search_room_history succeeds");

        assert!(page.is_complete);
        assert!(page.messages.is_empty());

        let stanza = stanza_rx.await.expect("driver captured the IQ");
        assert_eq!(stanza.attr("to"), Some("room@muc.example.com"));
        assert_eq!(
            form_field_value(&stanza, FULLTEXT_MAM_FIELD).as_deref(),
            Some("needle")
        );
        let before = stanza
            .get_child("query", MAM_NS)
            .and_then(|query| query.get_child("set", RSM_NS))
            .and_then(|set| set.get_child("before", RSM_NS))
            .map(|element| element.text());
        assert_eq!(
            before.as_deref(),
            Some(""),
            "search must request the newest page via an empty <before/>"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_dm_history_targets_account_archive_and_filters_peer() {
        let (handle, cmd_rx, _evt_tx) = make_bound_handle();
        let stanza_rx = spawn_fin_responder(cmd_rx);

        let page = timeout(
            Duration::from_secs(2),
            handle.search_dm_history("bob@example.com", "quarterly report", 25),
        )
        .await
        .expect("search must resolve once <fin/> arrives")
        .expect("search_dm_history succeeds");

        assert!(page.is_complete);

        let stanza = stanza_rx.await.expect("driver captured the IQ");
        // Personal archive = the session's bound bare JID.
        assert_eq!(stanza.attr("to"), Some("alice@example.com"));
        assert_eq!(
            form_field_value(&stanza, "with").as_deref(),
            Some("bob@example.com")
        );
        assert_eq!(
            form_field_value(&stanza, FULLTEXT_MAM_FIELD).as_deref(),
            Some("quarterly report")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_dm_history_without_bound_session_errors() {
        // No binding in the snapshot: the personal-archive address is
        // unknown, so the verb must fail without sending anything.
        let (handle, mut cmd_rx, _evt_tx) = make_handle();

        let result = timeout(
            Duration::from_secs(2),
            handle.search_dm_history("bob@example.com", "needle", 25),
        )
        .await
        .expect("must fail fast, not wait for the query timeout");

        assert!(result.is_err(), "unbound session must surface as error");
        assert!(
            cmd_rx.try_recv().is_err(),
            "no IQ may be sent without a bound JID"
        );
    }
}
