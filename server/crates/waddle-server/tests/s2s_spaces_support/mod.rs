//! A deployment that serves its Spaces to accounts at another server, for
//! the Spaces suites (XEP-0503 version 0.3.0 over XEP-0060 across S2S).
//!
//! `example.com` is the account-traffic fixture's deployment with its
//! Spaces PubSub service (`spaces.example.com`) among its S2S domains and
//! the production Spaces host attached to its router as startup attaches
//! it; `home-b.test` sends what a case hands it and keeps what it receives.
//! Spaces, their rooms and their bookmarks are written into the stores the
//! handlers read; the `federated` setting of a Space is turned on through
//! its store, or through the owner's configuration form where a case says
//! so. No PostgreSQL, SpiceDB or second process is involved.

#[path = "../s2s_account_support/mod.rs"]
pub mod accounts;

use jid::{BareJid, Jid};
use minidom::Element;
use waddle_server::db::actor::DbExecute;
use waddle_server::db::s2s_spaces::FederatedSpaces;
use waddle_server::permissions::{Object, ObjectType, Relation, Subject, Tuple, WriteTuple};
use waddle_xmpp::pubsub::{Affiliation, NodeConfig, PubSubItem};
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use xmpp_parsers::iq::Iq;

pub use accounts::{
    error_condition, federated_with, jid, wait_for, Adjust, Federated, Pki, Spec, ARRIVES, BOB,
    BOB_PHONE, LOCAL, MALLORY, PEER, QUIET, SPACES,
};

/// The MUC service of `example.com`.
pub const MUC: &str = "muc.example.com";

pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
pub const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
pub const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
pub const NS_PUBSUB_ERRORS: &str = "http://jabber.org/protocol/pubsub#errors";
pub const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
pub const NS_DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";
pub const NS_DATA: &str = "jabber:x:data";
pub const NS_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

/// The field of a Space's configuration form that opens it to other
/// servers.
pub const FIELD_FEDERATED: &str = "urn:waddle:spaces:federated";

/// `example.com` serving its Spaces to `home-b.test`.
pub async fn hosting(pki: &Pki) -> Federated {
    hosting_with(pki, Adjust::default()).await
}

/// [`hosting`], adjusted.
pub async fn hosting_with(pki: &Pki, adjust: Adjust) -> Federated {
    federated_with(
        pki,
        Adjust {
            hosts_spaces: true,
            ..adjust
        },
    )
    .await
}

pub fn spaces() -> BareJid {
    SPACES.parse().expect("the Spaces service")
}

/// The room JID of the channel `id`.
pub fn room(id: &str) -> BareJid {
    [id, "@", MUC].concat().parse().expect("a room JID")
}

/// Create the Space `node` with `config`.
pub async fn space(federated: &Federated, node: &str, config: NodeConfig) {
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;
    storage
        .get_or_create_node(&spaces(), node)
        .await
        .expect("the Space node");
    storage
        .update_node_config(&spaces(), node, &config)
        .await
        .expect("the Space's configuration");
}

/// Turn the `federated` setting of the Space `node` on, through its store.
pub async fn federate(federated: &Federated, node: &str) {
    let state = &federated.loopback.state;
    let stored = state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces(), node)
        .await
        .expect("the node is read")
        .expect("the Space exists");
    let enabled = FederatedSpaces::new(state.deps.app_state.db_pool.global().clone())
        .enable(
            &spaces(),
            node,
            stored.created_at.timestamp_millis(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .expect("the setting is written");
    assert!(enabled);
}

/// A public Space `node` that is open to other servers.
pub async fn federated_space(federated: &Federated, node: &str) {
    space(federated, node, NodeConfig::spaces_public()).await;
    federate(federated, node).await;
}

/// Write the catalog row of the channel `id`, and whether its room is open
/// to other servers.
pub async fn channel(federated: &Federated, id: &str, open: bool) {
    federated.loopback.channel(id, false).await;
    federated
        .loopback
        .state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: "UPDATE channels SET federated = ? WHERE id = ?".to_owned(),
            params: vec![open.into(), id.into()],
        })
        .await
        .expect("the channel's federated setting");
}

/// The XEP-0402 bookmark item of the channel `id`.
pub fn bookmark(id: &str) -> PubSubItem {
    let room = room(id);
    PubSubItem::new(
        Some(room.to_string()),
        Some(build_bookmark_element(&Bookmark::new(room).with_name(id))),
    )
}

/// Store the bookmark of the channel `id` in the Space `node` directly,
/// without a publish and its placement, as `publisher` if given.
pub async fn list_in(federated: &Federated, node: &str, id: &str, publisher: Option<&BareJid>) {
    federated
        .loopback
        .state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces(), node, &bookmark(id), publisher, false)
        .await
        .expect("the stored bookmark");
}

/// Give `account` the PubSub affiliation `affiliation` in the Space
/// `node`, through the store.
pub async fn affiliate(federated: &Federated, node: &str, account: &str, affiliation: Affiliation) {
    federated
        .loopback
        .state
        .deps
        .protocol
        .pubsub_storage
        .set_affiliation(
            &spaces(),
            node,
            &account.parse().expect("an account"),
            affiliation,
        )
        .await
        .expect("the affiliation");
}

/// Write the permission relationship `relation` of `account` on `object`.
pub async fn relate(federated: &Federated, object: Object, relation: &str, account: &str) {
    federated
        .loopback
        .state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(object, Relation::new(relation), Subject::user(account)),
        })
        .await
        .expect("the permission relationship");
}

/// Make `account` an owner of the Space `node` and of the channel `id`, in
/// PubSub and in the permission relationships a publish is decided by.
pub async fn owner_of(federated: &Federated, node: &str, id: &str, account: &str) {
    affiliate(federated, node, account, Affiliation::Owner).await;
    relate(
        federated,
        Object::new(ObjectType::Space, node),
        "owner",
        account,
    )
    .await;
    relate(
        federated,
        Object::new(ObjectType::Channel, id),
        "owner",
        account,
    )
    .await;
}

/// A request of `from` for the Spaces service.
pub fn request(from: &str, id: &str, set: bool, payload: Element) -> Iq {
    let (from, to) = (Some(jid(from)), Some(Jid::from(spaces())));
    let id = id.to_owned();
    if set {
        Iq::Set {
            from,
            to,
            id,
            payload,
        }
    } else {
        Iq::Get {
            from,
            to,
            id,
            payload,
        }
    }
}

/// A `disco#info` or `disco#items` query, of `node` if given.
pub fn disco(namespace: &str, node: Option<&str>) -> Element {
    let mut query = Element::builder("query", namespace);
    if let Some(node) = node {
        query = query.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    query.build()
}

/// A `pubsub` element in `namespace` holding `child`.
pub fn pubsub(namespace: &str, child: Element) -> Element {
    Element::builder("pubsub", namespace).append(child).build()
}

/// A child of a `pubsub` element in `namespace` that names `node`.
pub fn of_node(name: &str, namespace: &str, node: &str) -> Element {
    Element::builder(name, namespace)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .build()
}

/// `<subscribe/>` or `<unsubscribe/>` of `jid` for `node`.
pub fn subscription(name: &str, node: &str, jid: &str) -> Element {
    pubsub(
        NS_PUBSUB,
        Element::builder(name, NS_PUBSUB)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
            .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
            .build(),
    )
}

/// `<items/>` of `node`.
pub fn items(node: &str) -> Element {
    pubsub(NS_PUBSUB, of_node("items", NS_PUBSUB, node))
}

/// `<publish/>` of the bookmark of the channel `id` to `node`.
pub fn publish(node: &str, id: &str) -> Element {
    let item = bookmark(id);
    let mut element = Element::builder("item", NS_PUBSUB).attr(
        minidom::rxml::xml_ncname!("id").to_owned(),
        item.id.unwrap_or_default(),
    );
    if let Some(payload) = item.payload {
        element = element.append(payload);
    }
    pubsub(
        NS_PUBSUB,
        Element::builder("publish", NS_PUBSUB)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
            .append(element.build())
            .build(),
    )
}

/// `<retract/>` of the bookmark of the channel `id` from `node`.
pub fn retract(node: &str, id: &str) -> Element {
    pubsub(
        NS_PUBSUB,
        Element::builder("retract", NS_PUBSUB)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
            .append(
                Element::builder("item", NS_PUBSUB)
                    .attr(
                        minidom::rxml::xml_ncname!("id").to_owned(),
                        room(id).to_string(),
                    )
                    .build(),
            )
            .build(),
    )
}

/// A field of a submitted data form.
pub fn field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(Element::builder("value", NS_DATA).append(value).build())
        .build()
}

/// A `pubsub#owner` configuration form for `node` submitting `fields`.
pub fn configure(node: &str, fields: Vec<Element>) -> Element {
    let mut form = Element::builder("x", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", NS_DATA)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(
                    Element::builder("value", NS_DATA)
                        .append("http://jabber.org/protocol/pubsub#node_config")
                        .build(),
                )
                .build(),
        );
    for field in fields {
        form = form.append(field);
    }
    pubsub(
        NS_PUBSUB_OWNER,
        Element::builder("configure", NS_PUBSUB_OWNER)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
            .append(form.build())
            .build(),
    )
}

/// A `pubsub#owner` change of `account`'s affiliation in `node`.
pub fn affiliation(node: &str, account: &str, affiliation: &str) -> Element {
    pubsub(
        NS_PUBSUB_OWNER,
        Element::builder("affiliations", NS_PUBSUB_OWNER)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
            .append(
                Element::builder("affiliation", NS_PUBSUB_OWNER)
                    .attr(minidom::rxml::xml_ncname!("jid").to_owned(), account)
                    .attr(
                        minidom::rxml::xml_ncname!("affiliation").to_owned(),
                        affiliation,
                    )
                    .build(),
            )
            .build(),
    )
}

/// A `pubsub#owner` change that subscribes `account` to `node`.
pub fn owner_subscription(node: &str, account: &str) -> Element {
    pubsub(
        NS_PUBSUB_OWNER,
        Element::builder("subscriptions", NS_PUBSUB_OWNER)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
            .append(
                Element::builder("subscription", NS_PUBSUB_OWNER)
                    .attr(minidom::rxml::xml_ncname!("jid").to_owned(), account)
                    .attr(
                        minidom::rxml::xml_ncname!("subscription").to_owned(),
                        "subscribed",
                    )
                    .build(),
            )
            .build(),
    )
}

/// Send `iq` from `home-b.test` to the Spaces service and return its
/// answer.
pub async fn ask(federated: &Federated, iq: Iq) -> Element {
    let id = iq.id().to_owned();
    federated.to_spaces(iq).await;
    federated.peer_receives(&id).await
}

/// The `<error/>` of an answer, with nothing of the request in it.
pub fn error_of(answer: &Element) -> Option<Element> {
    answer
        .children()
        .find(|child| child.name() == "error")
        .cloned()
}

/// The first descendant of `element` named `name`, depth first.
pub fn descendant<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    element.children().find_map(|child| {
        if child.name() == name {
            Some(child)
        } else {
            descendant(child, name)
        }
    })
}

/// The ids of the `<item/>` elements anywhere in `element`.
pub fn item_ids(element: &Element) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for child in element.children() {
        if child.name() == "item" {
            ids.extend(child.attr("id").map(ToOwned::to_owned));
        }
        ids.extend(item_ids(child));
    }
    ids
}

/// The value of the field `var` of a data form anywhere in `element`.
pub fn form_value(element: &Element, var: &str) -> Option<String> {
    for child in element.children() {
        if child.name() == "field" && child.attr("var") == Some(var) {
            return child
                .children()
                .find(|value| value.name() == "value")
                .map(Element::text);
        }
        if let Some(value) = form_value(child, var) {
            return Some(value);
        }
    }
    None
}

/// Whether `stanza` is an event notification of `node` from the Spaces
/// service to `to`.
pub fn is_event(stanza: &Element, node: &str, to: &str) -> bool {
    stanza.name() == "message"
        && stanza.attr("from") == Some(SPACES)
        && stanza.attr("to") == Some(to)
        && stanza
            .children()
            .filter(|child| child.is("event", NS_PUBSUB_EVENT))
            .flat_map(Element::children)
            .any(|items| items.name() == "items" && items.attr("node") == Some(node))
}

/// How many rows of `table` there are, in the deployment's global database.
pub async fn rows(federated: &Federated, table: &str) -> i64 {
    let row = federated
        .loopback
        .state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(waddle_server::db::actor::DbQueryOne {
            sql: format!("SELECT COUNT(*) FROM {table}"),
            params: Vec::new(),
        })
        .await
        .expect("count query")
        .expect("count row");
    match row.first() {
        Some(waddle_server::db::Value::Integer(count)) => *count,
        other => panic!("unexpected count {other:?}"),
    }
}
