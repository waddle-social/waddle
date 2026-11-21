//! Stanzas and payloads the flows send, built as `minidom` elements, and
//! readers for the answers. Addresses are text, as for stream headers.

use minidom::rxml::{xml_ncname, NcNameStr};
use minidom::{Element, ElementBuilder};

use crate::xml::ns;

fn attrs(
    mut builder: ElementBuilder,
    pairs: &[(&'static NcNameStr, Option<&str>)],
) -> ElementBuilder {
    for (name, value) in pairs {
        if let Some(value) = value {
            builder = builder.attr((*name).to_owned(), *value);
        }
    }
    builder
}

/// An IQ in `namespace` (`jabber:client` or `jabber:server`).
pub fn iq(
    namespace: &str,
    kind: &str,
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    payload: Option<Element>,
) -> Element {
    let builder = attrs(
        Element::builder("iq", namespace),
        &[
            (xml_ncname!("type"), Some(kind)),
            (xml_ncname!("id"), Some(id)),
            (xml_ncname!("from"), from),
            (xml_ncname!("to"), to),
        ],
    );
    match payload {
        Some(payload) => builder.append(payload).build(),
        None => builder.build(),
    }
}

pub fn ping_payload() -> Element {
    Element::builder("ping", ns::PING).build()
}

/// A client IQ get carrying `payload`.
pub fn get(id: &str, to: &str, payload: Element) -> Element {
    iq(ns::CLIENT, "get", id, None, Some(to), Some(payload))
}

/// A client IQ set carrying `payload`.
pub fn set(id: &str, to: Option<&str>, payload: Element) -> Element {
    iq(ns::CLIENT, "set", id, None, to, Some(payload))
}

pub fn ping(id: &str, to: &str) -> Element {
    get(id, to, ping_payload())
}

pub fn disco_info(id: &str, to: &str) -> Element {
    get(id, to, Element::builder("query", ns::DISCO_INFO).build())
}

pub fn disco_items(id: &str, to: &str) -> Element {
    get(id, to, Element::builder("query", ns::DISCO_ITEMS).build())
}

/// A message in `namespace` with an optional body.
pub fn message(
    namespace: &str,
    kind: &str,
    id: &str,
    from: Option<&str>,
    to: &str,
    body: Option<&str>,
) -> Element {
    let builder = attrs(
        Element::builder("message", namespace),
        &[
            (xml_ncname!("type"), Some(kind)),
            (xml_ncname!("id"), Some(id)),
            (xml_ncname!("from"), from),
            (xml_ncname!("to"), Some(to)),
        ],
    );
    match body {
        Some(body) => builder
            .append(Element::builder("body", namespace).append(body).build())
            .build(),
        None => builder.build(),
    }
}

pub fn chat(id: &str, to: &str, body: &str) -> Element {
    message(ns::CLIENT, "chat", id, None, to, Some(body))
}

pub fn presence(namespace: &str, to: Option<&str>, kind: Option<&str>) -> Element {
    attrs(
        Element::builder("presence", namespace),
        &[(xml_ncname!("to"), to), (xml_ncname!("type"), kind)],
    )
    .build()
}

/// Join `room/nick` (XEP-0045 section 7.2), asking for no history.
pub fn muc_join(room_nick: &str) -> Element {
    let history = Element::builder("history", ns::MUC)
        .attr(xml_ncname!("maxstanzas").to_owned(), "0")
        .build();
    attrs(
        Element::builder("presence", ns::CLIENT),
        &[(xml_ncname!("to"), Some(room_nick))],
    )
    .append(Element::builder("x", ns::MUC).append(history).build())
    .build()
}

pub fn groupchat(id: &str, room: &str, body: &str) -> Element {
    message(ns::CLIENT, "groupchat", id, None, room, Some(body))
}

/// A MAM query (XEP-0313) for at most `max` messages after `after`.
pub fn mam_query(id: &str, to: Option<&str>, max: u32, after: Option<&str>) -> Element {
    let mut set = Element::builder("set", ns::RSM).append(
        Element::builder("max", ns::RSM)
            .append(max.to_string())
            .build(),
    );
    if let Some(after) = after {
        set = set.append(Element::builder("after", ns::RSM).append(after).build());
    }
    let query = Element::builder("query", ns::MAM)
        .attr(xml_ncname!("queryid").to_owned(), id)
        .append(set.build())
        .build();
    iq(ns::CLIENT, "set", id, None, to, Some(query))
}

/// Change an occupant's role (XEP-0045 section 8).
pub fn muc_role(id: &str, room: &str, nick: &str, role: &str) -> Element {
    let item = Element::builder("item", ns::MUC_ADMIN)
        .attr(xml_ncname!("nick").to_owned(), nick)
        .attr(xml_ncname!("role").to_owned(), role)
        .build();
    set(
        id,
        Some(room),
        Element::builder("query", ns::MUC_ADMIN)
            .append(item)
            .build(),
    )
}

/// A `pubsub#owner` read of `node` at `service`: `what` is `configure`
/// (XEP-0060 section 8.2), `subscriptions` (8.8.1) or `affiliations`
/// (8.9.1).
pub fn pubsub_owner_get(id: &str, service: &str, what: &str, node: &str) -> Element {
    let child = Element::builder(what, ns::PUBSUB_OWNER)
        .attr(xml_ncname!("node").to_owned(), node)
        .build();
    get(
        id,
        service,
        Element::builder("pubsub", ns::PUBSUB_OWNER)
            .append(child)
            .build(),
    )
}

/// Change affiliations with `node` at `service` (XEP-0060 section 8.9.2):
/// `(jid, affiliation)` pairs.
pub fn pubsub_affiliations_set(
    id: &str,
    service: &str,
    node: &str,
    changes: &[(&str, &str)],
) -> Element {
    let mut list = Element::builder("affiliations", ns::PUBSUB_OWNER)
        .attr(xml_ncname!("node").to_owned(), node);
    for (jid, affiliation) in changes {
        list = list.append(
            Element::builder("affiliation", ns::PUBSUB_OWNER)
                .attr(xml_ncname!("jid").to_owned(), *jid)
                .attr(xml_ncname!("affiliation").to_owned(), *affiliation)
                .build(),
        );
    }
    set(
        id,
        Some(service),
        Element::builder("pubsub", ns::PUBSUB_OWNER)
            .append(list.build())
            .build(),
    )
}

/// The first element at or below `element` named `name` in `namespace`
/// that `matches` accepts.
pub fn find<'a>(
    element: &'a Element,
    name: &str,
    namespace: &str,
    matches: &dyn Fn(&Element) -> bool,
) -> Option<&'a Element> {
    if element.is(name, namespace) && matches(element) {
        return Some(element);
    }
    element
        .children()
        .find_map(|child| find(child, name, namespace, matches))
}

/// Change the affiliation of `jid` with a room (XEP-0045 sections 9 and
/// 10).
pub fn muc_affiliation(id: &str, room: &str, jid: &str, affiliation: &str) -> Element {
    let item = Element::builder("item", ns::MUC_ADMIN)
        .attr(xml_ncname!("affiliation").to_owned(), affiliation)
        .attr(xml_ncname!("jid").to_owned(), jid)
        .build();
    set(
        id,
        Some(room),
        Element::builder("query", ns::MUC_ADMIN)
            .append(item)
            .build(),
    )
}

/// Retract a room message as a moderator (XEP-0425).
pub fn moderate(id: &str, room: &str, stanza_id: &str) -> Element {
    let moderate = Element::builder("moderate", ns::MODERATION)
        .attr(xml_ncname!("id").to_owned(), stanza_id)
        .append(Element::builder("retract", ns::RETRACT).build())
        .build();
    set(id, Some(room), moderate)
}

pub fn carbons_enable(id: &str) -> Element {
    set(id, None, Element::builder("enable", ns::CARBONS).build())
}

pub fn roster_get(id: &str) -> Element {
    iq(
        ns::CLIENT,
        "get",
        id,
        None,
        None,
        Some(Element::builder("query", ns::ROSTER).build()),
    )
}

fn pubsub(child: Element) -> Element {
    Element::builder("pubsub", ns::PUBSUB).append(child).build()
}

pub fn pubsub_create(id: &str, service: &str, node: &str) -> Element {
    set(
        id,
        Some(service),
        pubsub(
            Element::builder("create", ns::PUBSUB)
                .attr(xml_ncname!("node").to_owned(), node)
                .build(),
        ),
    )
}

pub fn pubsub_publish(
    id: &str,
    service: Option<&str>,
    node: &str,
    item_id: &str,
    payload: Element,
) -> Element {
    let item = Element::builder("item", ns::PUBSUB)
        .attr(xml_ncname!("id").to_owned(), item_id)
        .append(payload)
        .build();
    set(
        id,
        service,
        pubsub(
            Element::builder("publish", ns::PUBSUB)
                .attr(xml_ncname!("node").to_owned(), node)
                .append(item)
                .build(),
        ),
    )
}

pub fn pubsub_items(id: &str, service: Option<&str>, node: &str) -> Element {
    let items = Element::builder("items", ns::PUBSUB)
        .attr(xml_ncname!("node").to_owned(), node)
        .build();
    iq(ns::CLIENT, "get", id, None, service, Some(pubsub(items)))
}

pub fn pubsub_subscribe(id: &str, service: &str, node: &str, jid: &str) -> Element {
    set(
        id,
        Some(service),
        pubsub(
            Element::builder("subscribe", ns::PUBSUB)
                .attr(xml_ncname!("node").to_owned(), node)
                .attr(xml_ncname!("jid").to_owned(), jid)
                .build(),
        ),
    )
}

/// A XEP-0402 conference bookmark for `room`.
pub fn bookmark(name: &str, nick: &str) -> Element {
    Element::builder("conference", ns::BOOKMARKS)
        .attr(xml_ncname!("name").to_owned(), name)
        .attr(xml_ncname!("autojoin").to_owned(), "true")
        .append(Element::builder("nick", ns::BOOKMARKS).append(nick).build())
        .build()
}

/// A payload nested `depth` elements deep.
pub fn nested(depth: usize) -> Element {
    let mut element = Element::builder("leaf", "urn:example:nest").build();
    for _ in 1..depth {
        element = Element::builder("n", "urn:example:nest")
            .append(element)
            .build();
    }
    element
}

/// A body of `bytes` ASCII bytes.
pub fn filler(bytes: usize) -> String {
    "x".repeat(bytes)
}

/// Whether `element` is an IQ of `kind` answering `id`.
pub fn is_iq(element: &Element, kind: &str, id: &str) -> bool {
    element.name() == "iq" && element.attr("type") == Some(kind) && element.attr("id") == Some(id)
}

/// The defined condition of a stanza error, if `element` carries one.
pub fn error_condition(element: &Element) -> Option<String> {
    let error = element.children().find(|child| child.name() == "error")?;
    error
        .children()
        .find(|child| child.ns() == ns::STANZAS && child.name() != "text")
        .map(|condition| condition.name().to_owned())
}

/// The defined condition of a stream error element.
pub fn stream_error_condition(element: &Element) -> Option<String> {
    if element.name() != "error" || element.ns() != ns::STREAM {
        return None;
    }
    element
        .children()
        .find(|child| child.ns() == ns::STREAM_ERRORS && child.name() != "text")
        .map(|condition| condition.name().to_owned())
}

/// The `stanza-id` an archive or room assigned (XEP-0359).
pub fn stanza_id(element: &Element, by: &str) -> Option<String> {
    element
        .children()
        .find(|child| child.name() == "stanza-id" && child.attr("by") == Some(by))
        .and_then(|child| child.attr("id"))
        .map(str::to_owned)
}

pub fn body(element: &Element) -> Option<String> {
    element
        .children()
        .find(|child| child.name() == "body")
        .map(Element::text)
}

/// Accept the default configuration of a newly created room (XEP-0045
/// section 10.1.2).
pub fn muc_instant(id: &str, room: &str) -> Element {
    let form = Element::builder("x", ns::DATA_FORMS)
        .attr(xml_ncname!("type").to_owned(), "submit")
        .build();
    set(
        id,
        Some(room),
        Element::builder("query", ns::MUC_OWNER)
            .append(form)
            .build(),
    )
}

/// A submitted data form (XEP-0004) of `form_type` with `fields`, each a
/// variable and its single value.
fn submitted_form(form_type: &str, fields: &[(&str, &str)]) -> Element {
    let field = |var: &str, value: &str| {
        Element::builder("field", ns::DATA_FORMS)
            .attr(xml_ncname!("var").to_owned(), var)
            .append(
                Element::builder("value", ns::DATA_FORMS)
                    .append(value)
                    .build(),
            )
            .build()
    };
    let mut form = Element::builder("x", ns::DATA_FORMS)
        .attr(xml_ncname!("type").to_owned(), "submit")
        .append(field("FORM_TYPE", form_type));
    for (var, value) in fields {
        form = form.append(field(var, value));
    }
    form.build()
}

/// Submit the owner configuration form of `room` with `fields` (XEP-0045
/// section 10.2), as an owner does for a newly created room it configures
/// instead of accepting the defaults.
pub fn muc_configure(id: &str, room: &str, fields: &[(&str, &str)]) -> Element {
    set(
        id,
        Some(room),
        Element::builder("query", ns::MUC_OWNER)
            .append(submitted_form(ns::MUC_ROOMCONFIG, fields))
            .build(),
    )
}

/// Submit the node configuration form of `node` on `service` with
/// `fields` (XEP-0060 section 8.2.4).
pub fn pubsub_configure(id: &str, service: &str, node: &str, fields: &[(&str, &str)]) -> Element {
    let configure = Element::builder("configure", ns::PUBSUB_OWNER)
        .attr(xml_ncname!("node").to_owned(), node)
        .append(submitted_form(ns::PUBSUB_NODE_CONFIG, fields))
        .build();
    set(
        id,
        Some(service),
        Element::builder("pubsub", ns::PUBSUB_OWNER)
            .append(configure)
            .build(),
    )
}

/// A XEP-0503 image metadata item payload pointing at `url`.
pub fn image_metadata(url: &str) -> Element {
    let info = Element::builder("info", "urn:xmpp:avatar:metadata")
        .attr(xml_ncname!("bytes").to_owned(), "420")
        .attr(xml_ncname!("height").to_owned(), "64")
        .attr(xml_ncname!("width").to_owned(), "64")
        .attr(xml_ncname!("type").to_owned(), "image/png")
        .attr(xml_ncname!("url").to_owned(), url)
        .attr(xml_ncname!("id").to_owned(), "harness-image")
        .build();
    Element::builder("metadata", "urn:xmpp:avatar:metadata")
        .append(info)
        .build()
}

/// A correction of message `replaces` (XEP-0308).
pub fn correction(id: &str, room: &str, replaces: &str, body: &str) -> Element {
    let mut message = groupchat(id, room, body);
    message.append_child(
        Element::builder("replace", "urn:xmpp:message-correct:0")
            .attr(xml_ncname!("id").to_owned(), replaces)
            .build(),
    );
    message
}

/// A retraction of the sender's own message `stanza_id` (XEP-0424).
pub fn retraction(id: &str, room: &str, stanza_id: &str) -> Element {
    let mut message = message(ns::CLIENT, "groupchat", id, None, room, None);
    message.append_child(
        Element::builder("retract", ns::RETRACT)
            .attr(xml_ncname!("id").to_owned(), stanza_id)
            .build(),
    );
    message
}

/// Whether `element` is a MUC self-presence (status code 110).
pub fn is_self_presence(element: &Element) -> bool {
    element.name() == "presence"
        && element
            .children()
            .filter(|child| child.is("x", ns::MUC_USER))
            .flat_map(|x| x.children())
            .any(|child| child.name() == "status" && child.attr("code") == Some("110"))
}

/// The status codes of a MUC presence (XEP-0045 section 15.6).
pub fn muc_status_codes(element: &Element) -> Vec<String> {
    element
        .children()
        .filter(|child| child.is("x", ns::MUC_USER))
        .flat_map(|x| x.children())
        .filter(|child| child.name() == "status")
        .filter_map(|status| status.attr("code").map(str::to_owned))
        .collect()
}

/// The role in a MUC presence's item.
pub fn muc_role_of(element: &Element) -> Option<String> {
    element
        .children()
        .filter(|child| child.is("x", ns::MUC_USER))
        .flat_map(|x| x.children())
        .find(|child| child.name() == "item")
        .and_then(|item| item.attr("role"))
        .map(str::to_owned)
}
