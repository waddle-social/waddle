//! Protected effects and metadata of a managed room outside the join, the
//! archive and administration: each is decided from the canonical grants,
//! whatever role, affiliation or relationship the room actor, the legacy
//! stores or a deployment role still hold.

use std::sync::Arc;
use std::time::Duration;

use jid::{BareJid, Jid};
use waddle_extensions::host_tools::InvocationKind;
use waddle_extensions::PluginId;
use waddle_server::auth::session::Session;
use waddle_server::channel_space_links::ChannelSpaceLink;
use waddle_server::permissions::host_grant::RoomAffiliation;
use waddle_server::permissions::{Object, ObjectType, Relation, Subject, Tuple, WriteTuple};
use waddle_server::server::extension_host_adapter::{
    ExtensionHostAdapter, ExtensionHostAdapterError, ExtensionInvocation,
};
use waddle_server::space_identity::{space_jid_for_node, SpaceNode};
use waddle_server::test_support::{
    broadcast_room_system_message, extension_bot_groupchat_refusal, host_grant_read_still_admitted,
    persist_group_dm_member,
};
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::stanza::{
    get, is_error, is_result, pubsub_items, result_payload, set, NS_PUBSUB,
};
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::muc::room_registry_actor::CreateRoom;
use waddle_xmpp::pubsub::{Affiliation, NodeConfig, PubSubItem};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType, ToElement};
use waddle_xmpp::xep::xep0272::{Muji, NS_MUJI};
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use waddle_xmpp::xep::xep0424::NS_MESSAGE_RETRACT;
use waddle_xmpp::xep::xep0425::NS_MESSAGE_MODERATE;
use waddle_xmpp::xep::xep0433::{build_search_request, parse_search_results, SearchRequest};
use waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0;
use xmpp_parsers::message::{Lang, Message, MessageType};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::DefinedCondition;

use super::host_grant_support::{bare, OWNER, PEER, SENDER};
use super::{
    error_condition, join, ManagedRoom, CAROL, MUC, NS_DISCO_INFO, NS_MUC_ADMIN, REMOTE, SPACES,
};

const NS_DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const DOMAIN: &str = "example.com";
const NODE_CHANNELS_LIST: &str = "urn:waddle:admin:channels:list:0";
const NODE_CHANNELS_DELETE: &str = "urn:waddle:admin:channels:delete:0";
const NODE_SPACES_LIST: &str = "urn:waddle:admin:spaces:list:0";
const NODE_SPACES_DELETE: &str = "urn:waddle:admin:spaces:delete:0";
/// A Space node no scope enrolls.
const PLAIN_SPACE: &str = "plain-space";
/// The extension the bot and host-adapter cases act for.
const PLUGIN: &str = "board";

fn attribute(name: &str) -> minidom::rxml::NcName {
    minidom::rxml::NcName::try_from(name).expect("valid XML attribute name")
}

fn moderate(target: &str) -> Element {
    Element::builder("moderate", NS_MESSAGE_MODERATE)
        .attr(attribute("id"), target)
        .append(Element::builder("retract", NS_MESSAGE_RETRACT).build())
        .build()
}

fn role_item(nick: &str, role: &str) -> Element {
    Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(attribute("nick"), nick)
                .attr(attribute("role"), role)
                .build(),
        )
        .build()
}

fn pin_query() -> Element {
    Element::builder("query", NS_WADDLE_PIN_V0).build()
}

fn node_disco_info(node: &str) -> Element {
    Element::builder("query", NS_DISCO_INFO)
        .attr(attribute("node"), node)
        .build()
}

fn disco_items() -> Element {
    Element::builder("query", NS_DISCO_ITEMS).build()
}

fn pubsub_retract(node: &str, item_id: &str) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("retract", NS_PUBSUB)
                .attr(attribute("node"), node)
                .append(
                    Element::builder("item", NS_PUBSUB)
                        .attr(attribute("id"), item_id)
                        .build(),
                )
                .build(),
        )
        .build()
}

/// An XEP-0050 execution of the admin command `node` with `fields`.
fn admin_command(node: &str, fields: &[(&str, &str)]) -> Element {
    let mut command = Element::builder("command", NS_COMMANDS)
        .attr(attribute("node"), node)
        .attr(attribute("action"), "execute");
    if !fields.is_empty() {
        let form = fields.iter().fold(
            DataForm::new(FormType::Submit).add_field(Field::form_type(node)),
            |form, (var, value)| form.add_field(Field::text_single(*var, *value)),
        );
        command = command.append(form.to_element());
    }
    command.build()
}

/// The `jid` attributes of a disco#items result's items.
fn item_jids(reply: &xmpp_parsers::iq::Iq) -> Vec<String> {
    result_payload(reply)
        .map(|query| {
            query
                .children()
                .filter_map(|item| item.attr("jid").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The `name` attributes of a disco#items result's items: the item IDs,
/// the rooms' JIDs, of a Space node's listing.
fn item_names(reply: &xmpp_parsers::iq::Iq) -> Vec<String> {
    result_payload(reply)
        .map(|query| {
            query
                .children()
                .filter_map(|item| item.attr("name").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a disco#items result tells the count of its listing
/// (XEP-0059): `None` when it carries no `<set/>`.
fn tells_count(reply: &xmpp_parsers::iq::Iq) -> Option<bool> {
    let set = result_payload(reply)?
        .children()
        .find(|child| child.is("set", "http://jabber.org/protocol/rsm"))?;
    Some(set.children().any(|child| child.name() == "count"))
}

/// The Space node no scope enrolls, public, holding a bookmark of the
/// managed room and an item without a payload at the address of another
/// room, which is no room of the Space.
async fn plain_space_holding_the_managed_room(fixture: &ManagedRoom) {
    let room = fixture.room().clone();
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    let storage = &fixture.server.state.deps.protocol.pubsub_storage;
    storage
        .get_or_create_node(&spaces, PLAIN_SPACE)
        .await
        .expect("Space node");
    storage
        .update_node_config(&spaces, PLAIN_SPACE, &NodeConfig::spaces_public())
        .await
        .expect("Space configuration");
    let bookmark = PubSubItem::new(
        Some(room.to_string()),
        Some(build_bookmark_element(&Bookmark::new(room))),
    );
    storage
        .publish_item(&spaces, PLAIN_SPACE, &bookmark, None, false)
        .await
        .expect("the managed room's bookmark");
    let bare_item = PubSubItem::new(Some(format!("bare@{MUC}")), None);
    storage
        .publish_item(&spaces, PLAIN_SPACE, &bare_item, None, false)
        .await
        .expect("an item without a payload");
}

/// The `disco#items` query of the Space node no scope enrolls.
fn plain_space_items() -> Element {
    Element::builder("query", NS_DISCO_ITEMS)
        .attr(attribute("node"), PLAIN_SPACE)
        .build()
}

/// The `node` attributes of a disco#items result's items.
fn item_nodes(reply: &xmpp_parsers::iq::Iq) -> Vec<String> {
    result_payload(reply)
        .map(|query| {
            query
                .children()
                .filter_map(|item| item.attr("node").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `text` is the text of `element` or of any element below it.
fn mentions(element: &Element, text: &str) -> bool {
    element.text().trim() == text || element.children().any(|child| mentions(child, text))
}

/// Whether `client` receives a groupchat message from `room` whose body is
/// `body` within `wait`.
async fn receives_room_message(
    client: &mut WsXmppClient,
    room: &BareJid,
    body: &str,
    wait: Duration,
) -> bool {
    client
        .recv_matching_within(wait, |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "message"
                    && element.attr("from") == Some(room.as_str())
                    && element
                        .children()
                        .any(|child| child.name() == "body" && child.text() == body)
            })
        })
        .await
        .is_ok()
}

/// Whether `client` receives a PubSub event of the node `node` within
/// `wait`.
async fn receives_node_event(client: &mut WsXmppClient, node: &str, wait: Duration) -> bool {
    client
        .recv_matching_within(wait, |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "message"
                    && element.children().any(|event| {
                        event.name() == "event"
                            && event
                                .children()
                                .any(|items| items.attr("node") == Some(node))
                    })
            })
        })
        .await
        .is_ok()
}

/// Make `account` a community owner: an Owner affiliation on a Spaces node
/// no scope enrolls.
async fn community_owner(fixture: &ManagedRoom, account: &str) {
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    let storage = &fixture.server.state.deps.app_state.pubsub_storage;
    storage
        .get_or_create_node(&spaces, PLAIN_SPACE)
        .await
        .expect("plain Space node");
    storage
        .update_node_config(&spaces, PLAIN_SPACE, &NodeConfig::spaces_public())
        .await
        .expect("plain Space configuration");
    storage
        .set_affiliation(&spaces, PLAIN_SPACE, &bare(account), Affiliation::Owner)
        .await
        .expect("community owner");
}

/// Register the admin channel and Space commands on the fixture's state, as
/// a deployment's startup does.
async fn register_admin_commands(fixture: &ManagedRoom) {
    let state = &fixture.server.state;
    waddle_server::admin::spaces::register(
        &state.deps.protocol.command_registry,
        Arc::clone(&state.deps.app_state),
    )
    .await;
    waddle_server::admin::channels::register(
        &state.deps.protocol.command_registry,
        Arc::clone(&state.deps.app_state),
        Arc::clone(state),
        Arc::clone(&state.deps.protocol.connection_registry),
        Arc::clone(&state.deps.protocol.sm_session_registry),
        None,
    )
    .await;
}

fn invocation(account: &str) -> ExtensionInvocation {
    let localpart = account.split('@').next().expect("account localpart");
    ExtensionInvocation {
        session: Some(Session::new(account, localpart, localpart)),
        actor_jid: format!("{account}/extension")
            .parse()
            .expect("invoking resource"),
        plugin_id: PluginId::try_new(PLUGIN).expect("plugin id"),
        source_room: None,
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    }
}

#[tokio::test]
async fn message_moderation_needs_the_canonical_moderate_action() {
    let Some(fixture) = ManagedRoom::start("hg_sink_moderate").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut owner = fixture.server.client("owner", "phone").await;
    let joined = join(&mut owner, &room, "owner").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "the owner's join: {joined:?}"
    );
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let joined = join(&mut romeo, &room, "romeo").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );

    // The owner seats the member as a moderator, and the room actor's list
    // names the member an owner: neither is a canonical grant.
    let reply = round_trip(
        &mut owner,
        set("voice", room.as_str(), role_item("romeo", "moderator")),
    )
    .await;
    assert!(is_result(&reply), "the moderator role: {reply:?}");
    fixture
        .mirror_affiliation(SENDER, waddle_xmpp::Affiliation::Owner)
        .await;

    let reply = round_trip(
        &mut romeo,
        set("moderate", room.as_str(), moderate("absent-message")),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a member moderated through the actor's role and list: {reply:?}"
    );
    // The canonical owner passes authorization; the message it names does
    // not exist.
    let reply = round_trip(
        &mut owner,
        set("moderate", room.as_str(), moderate("absent-message")),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::ItemNotFound),
        "the owner's moderation: {reply:?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn the_pin_list_is_refused_to_a_seated_occupant_whose_grant_was_revoked() {
    let Some(fixture) = ManagedRoom::start("hg_sink_pins").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut alice = fixture.server.client("alice", "phone").await;
    let joined = join(&mut alice, &room, "alice").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );
    let reply = round_trip(&mut alice, get("pins", room.as_str(), pin_query())).await;
    assert!(is_result(&reply), "a member's pin list: {reply:?}");

    // A ban recorded in the store leaves the occupant seated until the
    // revocation fence ejects it; the pin list is a protected read.
    fixture
        .grants
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban");
    let reply = round_trip(&mut alice, get("pins-again", room.as_str(), pin_query())).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a banned occupant read the pin list: {reply:?}"
    );
    fixture.close().await;
}

/// A Muji presence of the occupant `nick` of `room`.
fn muji_presence(room: &BareJid, nick: &str) -> String {
    String::from(
        &Element::builder("presence", "jabber:client")
            .attr(attribute("to"), format!("{room}/{nick}"))
            .append(Muji::preparing().to_element())
            .build(),
    )
}

/// The next presence `client` receives from the occupant `nick` of `room`
/// that carries a Muji element or an error, within `wait`.
async fn muji_or_error_from(
    client: &mut WsXmppClient,
    room: &BareJid,
    nick: &str,
    wait: Duration,
) -> Option<Element> {
    let occupant = format!("{room}/{nick}");
    let frame = client
        .recv_matching_within(wait, |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "presence"
                    && element.attr("from") == Some(occupant.as_str())
                    && (element.attr("type") == Some("error") || element.has_child("muji", NS_MUJI))
            })
        })
        .await
        .ok()?;
    frame.parse().ok()
}

#[tokio::test]
async fn a_call_is_not_started_by_a_seated_occupant_whose_grant_was_revoked() {
    let Some(fixture) = ManagedRoom::start("hg_sink_muji").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut alice = fixture.server.client("alice", "phone").await;
    let mut romeo = fixture.server.client("romeo", "laptop").await;
    for (client, nick) in [(&mut alice, "alice"), (&mut romeo, "romeo")] {
        let joined = join(client, &room, nick).await;
        assert_eq!(error_condition(&joined), None, "{nick}'s join: {joined:?}");
    }

    // A member may prepare a call: the other occupant sees it.
    romeo
        .send(&muji_presence(&room, "romeo"))
        .await
        .expect("send Muji");
    let seen = muji_or_error_from(&mut alice, &room, "romeo", Duration::from_secs(5)).await;
    assert!(
        seen.as_ref()
            .is_some_and(|presence| error_condition(presence).is_none()),
        "a member's Muji presence was not reflected: {seen:?}"
    );

    // The store revokes Alice's grant; the room actor still seats her until
    // the revocation fence ejects her. Preparing a call is participation
    // and is refused, and nobody sees it.
    fixture
        .grants
        .set(OWNER, PEER, RoomAffiliation::None)
        .await
        .expect("revoke");
    alice
        .send(&muji_presence(&room, "alice"))
        .await
        .expect("send Muji");
    let reply = muji_or_error_from(&mut alice, &room, "alice", Duration::from_secs(5))
        .await
        .expect("a reply to the Muji presence");
    assert_eq!(
        error_condition(&reply).as_deref(),
        Some("forbidden"),
        "a revoked occupant prepared a call: {reply:?}"
    );
    let reflected =
        muji_or_error_from(&mut romeo, &room, "alice", Duration::from_millis(500)).await;
    assert!(
        reflected.is_none(),
        "the refused Muji presence reached another occupant: {reflected:?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_rooms_own_message_reaches_only_occupants_that_may_read_it() {
    let Some(fixture) = ManagedRoom::start("hg_sink_system").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let joined = join(&mut romeo, &room, "romeo").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );
    let mut alice = fixture.server.client("alice", "phone").await;
    let joined = join(&mut alice, &room, "alice").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );
    fixture
        .grants
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban");

    let mut message = Message::new(Some(Jid::from(room.clone())));
    message.from = Some(Jid::from(room.clone()));
    message.type_ = MessageType::Groupchat;
    message
        .bodies
        .insert(Lang(String::new()), "the room speaks".to_owned());
    let archived =
        broadcast_room_system_message(&fixture.server.state, room.clone(), message).await;
    assert!(archived.is_some(), "the room's message was not archived");
    assert!(
        receives_room_message(&mut romeo, &room, "the room speaks", Duration::from_secs(5)).await,
        "a member did not receive the room's message"
    );
    assert!(
        !receives_room_message(&mut alice, &room, "the room speaks", Duration::from_secs(1)).await,
        "a banned occupant received the room's message"
    );
    fixture.close().await;
}

#[tokio::test]
async fn channel_search_finds_a_managed_room_only_for_an_account_that_may_view_it() {
    let Some(fixture) = ManagedRoom::start("hg_sink_search").await else {
        return;
    };
    let room = fixture.room().clone();
    let search = |id: &str| {
        build_search_request(
            Jid::from(MUC.parse::<BareJid>().expect("MUC service")),
            &SearchRequest {
                query: fixture.grants.scope.channel_id.clone(),
                max: None,
                rsm: None,
            },
            id,
        )
    };
    let found = |reply: &xmpp_parsers::iq::Iq| {
        parse_search_results(reply)
            .iter()
            .any(|result| result.address == room.as_str())
    };

    // The catalog row describes the room, but not to an account whose
    // grants do not allow viewing it, whatever legacy relationship it holds.
    fixture.legacy_grants(CAROL).await;
    let mut carol = fixture.server.client("carol", "phone").await;
    let reply = round_trip(&mut carol, search("carol-search")).await;
    assert!(is_result(&reply), "search refused: {reply:?}");
    assert!(!found(&reply), "an ungranted account found the room");

    let mut romeo = fixture.server.client("romeo", "phone").await;
    let reply = round_trip(&mut romeo, search("romeo-search")).await;
    assert!(found(&reply), "a member did not find the room: {reply:?}");
    fixture.close().await;
}

/// The MUC service lists the managed room to an account whose grants allow
/// `view` and to nobody else (XEP-0045 §6.3); the scope's Space node is
/// described and listed to nobody, its node's `disco#items` included.
#[tokio::test]
async fn the_enrolled_space_is_not_described_and_the_managed_room_is_listed_only_to_a_member() {
    let Some(fixture) = ManagedRoom::start("hg_sink_disco").await else {
        return;
    };
    let room = fixture.room().clone();
    let node = fixture.grants.scope.space_node.clone();
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    let storage = &fixture.server.state.deps.protocol.pubsub_storage;
    for listed in [node.as_str(), PLAIN_SPACE] {
        storage
            .get_or_create_node(&spaces, listed)
            .await
            .expect("Space node");
        storage
            .update_node_config(&spaces, listed, &NodeConfig::spaces_public())
            .await
            .expect("Space configuration");
    }
    let mut romeo = fixture.server.client("romeo", "phone").await;

    // A member is told the scope's Space node does not exist.
    let reply = round_trip(
        &mut romeo,
        get("space-info", SPACES, node_disco_info(&node)),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::ItemNotFound),
        "the enrolled Space node was described: {reply:?}"
    );

    // Nor are its children listed.
    let node_items = Element::builder("query", NS_DISCO_ITEMS)
        .attr(attribute("node"), node.as_str())
        .build();
    let reply = round_trip(&mut romeo, get("space-items", SPACES, node_items)).await;
    assert!(
        is_error(&reply, DefinedCondition::ItemNotFound),
        "the enrolled Space node's children were listed: {reply:?}"
    );

    // The MUC service lists the room, whose catalog row is public, to the
    // member, whose grants allow `view`, and not to an account whose
    // grants do not, whatever legacy relationship it holds.
    let reply = round_trip(&mut romeo, get("rooms", MUC, disco_items())).await;
    assert!(is_result(&reply), "MUC disco#items: {reply:?}");
    assert!(
        item_jids(&reply).contains(&room.to_string()),
        "the managed room was not listed to a member: {reply:?}"
    );
    fixture.legacy_grants(CAROL).await;
    let mut carol = fixture.server.client("carol", "phone").await;
    let reply = round_trip(&mut carol, get("carol-rooms", MUC, disco_items())).await;
    assert!(is_result(&reply), "MUC disco#items: {reply:?}");
    assert!(
        !item_jids(&reply).contains(&room.to_string()),
        "the managed room was listed to an ungranted account: {reply:?}"
    );

    // The Spaces service does not list the scope's Space node.
    let reply = round_trip(&mut romeo, get("spaces", SPACES, disco_items())).await;
    assert!(is_result(&reply), "Spaces disco#items: {reply:?}");
    let nodes = item_nodes(&reply);
    assert!(
        !nodes.contains(&node),
        "the enrolled Space node was listed: {reply:?}"
    );
    assert!(
        nodes.contains(&PLAIN_SPACE.to_owned()),
        "an unenrolled public Space was not listed: {reply:?}"
    );
    fixture.close().await;
}

/// XEP-0503, "Advertising space membership", for a session: a Space no
/// scope enrolls lists a managed room it holds only to an account whose
/// canonical grants allow `view`, decided without stream evidence; an
/// account whose grants do not is not told of it, whatever legacy
/// relationship it holds. An item without a payload is no room of the
/// Space.
#[tokio::test]
async fn a_plain_spaces_listing_names_a_managed_room_only_to_a_member() {
    let Some(fixture) = ManagedRoom::start("hg_sink_space_children").await else {
        return;
    };
    let room = fixture.room().clone();
    plain_space_holding_the_managed_room(&fixture).await;

    let mut romeo = fixture.server.client("romeo", "phone").await;
    let reply = round_trip(
        &mut romeo,
        get("romeo-children", SPACES, plain_space_items()),
    )
    .await;
    assert!(is_result(&reply), "Space node disco#items: {reply:?}");
    assert_eq!(item_names(&reply), [room.to_string()], "{reply:?}");

    fixture.legacy_grants(CAROL).await;
    let mut carol = fixture.server.client("carol", "phone").await;
    let reply = round_trip(
        &mut carol,
        get("carol-children", SPACES, plain_space_items()),
    )
    .await;
    assert!(is_result(&reply), "Space node disco#items: {reply:?}");
    assert!(
        item_names(&reply).is_empty(),
        "the managed room was listed to an ungranted account: {reply:?}"
    );
    fixture.close().await;
}

/// A managed room whose grants cannot be read now (the replica holds no
/// attachment to the store) is left out of the MUC service's listing and of
/// a Space node's, even for its member; both listings are still answered,
/// and every other room is listed.
#[tokio::test]
async fn a_managed_room_whose_grants_cannot_be_read_is_left_out_of_the_listings() {
    let Some(fixture) = ManagedRoom::start_unattached("hg_sink_listing_unread").await else {
        return;
    };
    let room = fixture.room().clone();
    fixture.server.channel("plain-room", false).await;
    plain_space_holding_the_managed_room(&fixture).await;
    let mut romeo = fixture.server.client("romeo", "phone").await;

    let reply = round_trip(&mut romeo, get("rooms", MUC, disco_items())).await;
    assert!(is_result(&reply), "MUC disco#items: {reply:?}");
    let listed = item_jids(&reply);
    assert!(
        !listed.contains(&room.to_string()),
        "a room whose grants cannot be read was listed: {reply:?}"
    );
    assert!(
        listed.contains(&format!("plain-room@{MUC}")),
        "an unmanaged public room was not listed: {reply:?}"
    );
    // The listing left a room out: it carries its `<set/>` and no count.
    assert_eq!(tells_count(&reply), Some(false), "{reply:?}");

    let reply = round_trip(&mut romeo, get("children", SPACES, plain_space_items())).await;
    assert!(is_result(&reply), "Space node disco#items: {reply:?}");
    assert!(
        item_names(&reply).is_empty(),
        "a room whose grants cannot be read was listed: {reply:?}"
    );
    assert_eq!(tells_count(&reply), Some(false), "{reply:?}");
    fixture.close().await;
}

#[tokio::test]
async fn a_spaces_retract_does_not_move_a_managed_room() {
    let Some(fixture) = ManagedRoom::start("hg_sink_retract").await else {
        return;
    };
    let room = fixture.room().clone();
    community_owner(&fixture, CAROL).await;
    fixture
        .server
        .state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, PLAIN_SPACE),
                Relation::new("owner"),
                Subject::user(CAROL),
            ),
        })
        .await
        .expect("Space owner");
    let mut carol = fixture.server.client("carol", "phone").await;
    let reply = round_trip(
        &mut carol,
        set(
            "retract",
            SPACES,
            pubsub_retract(PLAIN_SPACE, room.as_str()),
        ),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a Spaces retract moved a managed room: {reply:?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_spaces_items_read_backfills_no_bookmark_of_a_managed_room() {
    let Some(fixture) = ManagedRoom::start("hg_sink_backfill").await else {
        return;
    };
    let room = fixture.room().clone();
    community_owner(&fixture, CAROL).await;
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    let plain_space =
        space_jid_for_node(&spaces, &SpaceNode::from(PLAIN_SPACE)).expect("plain Space JID");
    // Both rooms have a catalog row and a live room actor, and both are
    // linked to a Space no scope enrolls, with no bookmark there and no
    // parent: what a read backfills.
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let joined = join(&mut romeo, &room, "romeo").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );
    const PLAIN_ROOM: &str = "plain-room";
    let plain_room: BareJid = format!("{PLAIN_ROOM}@{MUC}")
        .parse()
        .expect("plain room address");
    fixture.server.channel(PLAIN_ROOM, false).await;
    fixture
        .server
        .state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: plain_room.clone(),
            waddle_id: "test".to_owned(),
            channel_id: PLAIN_ROOM.to_owned(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("plain room actor");
    for channel in [&room, &plain_room] {
        fixture
            .server
            .state
            .deps
            .app_state
            .channel_space_link_store
            .set(&ChannelSpaceLink {
                channel_jid: channel.clone(),
                space_jid: plain_space.clone(),
                space_node: SpaceNode::from(PLAIN_SPACE),
                created_at: 0,
            })
            .await
            .expect("channel-space link");
    }

    let mut carol = fixture.server.client("carol", "phone").await;
    let reply = round_trip(
        &mut carol,
        get("backfill", SPACES, pubsub_items(PLAIN_SPACE)),
    )
    .await;
    assert!(is_result(&reply), "the items read was refused: {reply:?}");
    let listed: Vec<String> = fixture
        .server
        .state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces, PLAIN_SPACE, None, &[])
        .await
        .expect("Space items")
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert!(
        listed.contains(&plain_room.to_string()),
        "the unmanaged room's bookmark was not backfilled: {listed:?}"
    );
    assert!(
        !listed.contains(&room.to_string()),
        "a managed room's bookmark was backfilled: {listed:?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_managed_room_without_a_channel_row_is_not_joined() {
    let Some(fixture) = ManagedRoom::start("hg_sink_rowless").await else {
        return;
    };
    // A second scope's room, whose member is the fixture's sender, has no
    // catalog row.
    let rowless = fixture.grants.enroll_another().await;
    fixture.refresh_enrolled().await;
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let refused = join(&mut romeo, &rowless.room, "romeo").await;
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("service-unavailable"),
        "a managed room was joined as an instant room: {refused:?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_replica_without_an_attachment_admits_nobody_to_a_managed_room() {
    let Some(fixture) = ManagedRoom::start_unattached("hg_sink_unattached").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let refused = join(&mut romeo, &room, "romeo").await;
    assert!(
        error_condition(&refused).is_some(),
        "an unattached replica admitted a member: {refused:?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn extension_bots_are_not_seated_in_a_managed_room() {
    let Some(fixture) = ManagedRoom::start("hg_sink_bot").await else {
        return;
    };
    let room = fixture.room().clone();
    let refusal =
        extension_bot_groupchat_refusal(&fixture.server.state, PLUGIN, &room, "hello").await;
    assert_eq!(
        refusal.as_deref(),
        Some("extension bots hold no grant in a managed room")
    );
    fixture.close().await;
}

#[tokio::test]
async fn the_extension_host_decides_a_managed_room_by_its_canonical_grants() {
    let Some(fixture) = ManagedRoom::start("hg_sink_host").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let joined = join(&mut romeo, &room, "romeo").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );
    let host = ExtensionHostAdapter::new(Arc::clone(&fixture.server.state));

    // A member reads the room's occupants and finds the room.
    let members = host
        .list_muc_members(&invocation(SENDER), &room)
        .await
        .expect("a member's occupant list");
    assert!(!members.is_empty());
    let channels = host
        .list_channels(&invocation(SENDER), 500, 0)
        .await
        .expect("a member's channel list");
    assert!(channels.iter().any(|channel| channel.room == room));

    // A canonical ban denies it, whatever legacy relationship remains.
    fixture.legacy_grants(PEER).await;
    fixture
        .grants
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban");
    let denied = host.list_muc_members(&invocation(PEER), &room).await;
    assert!(
        matches!(denied, Err(ExtensionHostAdapterError::NotAuthorized)),
        "a banned account read the occupants: {denied:?}"
    );

    // Deployment ownership is not a grant of the room.
    fixture
        .server
        .state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, "deployment"),
                Relation::new("owner"),
                Subject::user(CAROL),
            ),
        })
        .await
        .expect("deployment owner");
    let denied = host.list_muc_members(&invocation(CAROL), &room).await;
    assert!(
        matches!(denied, Err(ExtensionHostAdapterError::NotAuthorized)),
        "the deployment owner read the occupants: {denied:?}"
    );
    let channels = host
        .list_channels(&invocation(CAROL), 500, 0)
        .await
        .expect("the deployment owner's channel list");
    assert!(
        !channels.iter().any(|channel| channel.room == room),
        "the deployment owner was shown the managed room"
    );
    fixture.close().await;
}

#[tokio::test]
async fn admin_commands_refuse_a_managed_room_its_space_and_a_cascade_reaching_it() {
    let Some(fixture) = ManagedRoom::start("hg_sink_admin").await else {
        return;
    };
    let room = fixture.room().clone();
    register_admin_commands(&fixture).await;
    community_owner(&fixture, CAROL).await;
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    let plain_space =
        space_jid_for_node(&spaces, &SpaceNode::from(PLAIN_SPACE)).expect("plain Space JID");
    let scope_space = space_jid_for_node(
        &spaces,
        &SpaceNode::from(fixture.grants.scope.space_node.as_str()),
    )
    .expect("scope Space JID");
    let mut carol = fixture.server.client("carol", "phone").await;

    let reply = round_trip(
        &mut carol,
        set(
            "delete-room",
            DOMAIN,
            admin_command(
                NODE_CHANNELS_DELETE,
                &[("channel_jid", room.as_str()), ("confirm", "yes")],
            ),
        ),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::NotAllowed),
        "a managed room was deleted by an admin command: {reply:?}"
    );

    let reply = round_trip(
        &mut carol,
        set(
            "delete-scope-space",
            DOMAIN,
            admin_command(
                NODE_SPACES_DELETE,
                &[("space_jid", scope_space.as_str()), ("confirm", "yes")],
            ),
        ),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::NotAllowed),
        "an enrolled Space was deleted by an admin command: {reply:?}"
    );

    // A Space whose deletion would destroy the managed room is refused
    // whole: the Space node survives.
    fixture
        .server
        .state
        .deps
        .app_state
        .channel_space_link_store
        .set(&ChannelSpaceLink {
            channel_jid: room.clone(),
            space_jid: plain_space.clone(),
            space_node: SpaceNode::from(PLAIN_SPACE),
            created_at: 0,
        })
        .await
        .expect("channel-space link");
    let reply = round_trip(
        &mut carol,
        set(
            "delete-plain-space",
            DOMAIN,
            admin_command(
                NODE_SPACES_DELETE,
                &[("space_jid", plain_space.as_str()), ("confirm", "yes")],
            ),
        ),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::NotAllowed),
        "a Space deletion reached a managed room: {reply:?}"
    );
    assert!(fixture
        .server
        .state
        .deps
        .app_state
        .pubsub_storage
        .get_node(&spaces, PLAIN_SPACE)
        .await
        .expect("Space node read")
        .is_some());
    fixture.close().await;
}

#[tokio::test]
async fn admin_listings_omit_a_managed_room_and_its_space_without_canonical_standing() {
    let Some(fixture) = ManagedRoom::start("hg_sink_admin_lists").await else {
        return;
    };
    let room = fixture.room().clone();
    let node = fixture.grants.scope.space_node.clone();
    register_admin_commands(&fixture).await;
    community_owner(&fixture, CAROL).await;
    community_owner(&fixture, OWNER).await;
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    fixture
        .server
        .state
        .deps
        .app_state
        .pubsub_storage
        .get_or_create_node(&spaces, &node)
        .await
        .expect("scope Space node");

    let listing = |reply: &xmpp_parsers::iq::Iq| {
        result_payload(reply)
            .cloned()
            .unwrap_or_else(|| panic!("the listing was refused: {reply:?}"))
    };
    let mut carol = fixture.server.client("carol", "phone").await;
    let reply = round_trip(
        &mut carol,
        set("spaces", DOMAIN, admin_command(NODE_SPACES_LIST, &[])),
    )
    .await;
    let spaces_listing = listing(&reply);
    assert!(!mentions(&spaces_listing, &node), "{spaces_listing:?}");
    assert!(mentions(&spaces_listing, PLAIN_SPACE), "{spaces_listing:?}");

    // A community owner without a grant is not shown the managed room; its
    // canonical owner is.
    let reply = round_trip(
        &mut carol,
        set("channels", DOMAIN, admin_command(NODE_CHANNELS_LIST, &[])),
    )
    .await;
    assert!(!mentions(&listing(&reply), room.as_str()));
    let mut owner = fixture.server.client("owner", "phone").await;
    let reply = round_trip(
        &mut owner,
        set("channels", DOMAIN, admin_command(NODE_CHANNELS_LIST, &[])),
    )
    .await;
    assert!(mentions(&listing(&reply), room.as_str()), "{reply:?}");
    fixture.close().await;
}

#[tokio::test]
async fn group_dm_member_writers_refuse_a_managed_rooms_channel() {
    let Some(fixture) = ManagedRoom::start("hg_sink_group_dm").await else {
        return;
    };
    let refused = persist_group_dm_member(
        &fixture.server.state.deps.app_state,
        &fixture.grants.scope.channel_id,
        &bare(CAROL),
    )
    .await;
    assert!(refused.is_err(), "a group-DM writer wrote a managed room");
    assert!(fixture.legacy_relations(CAROL).await.is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn a_group_dm_room_is_never_enrolled() {
    use waddle_server::permissions::host_grant::{
        BootstrapGrant, EnrollScope, GrantOperationId, HostGrantError, ManagedRoom as BoundRoom,
        ScopeKey, ScopeLifecycleId,
    };
    let Some(fixture) = ManagedRoom::start("hg_sink_group_dm_enroll").await else {
        return;
    };
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let room = bare(&format!("group-dm-{tag}@{MUC}"));
    let bound = BoundRoom::new(room.clone()).expect("managed room");
    let channel_id = bound.channel_id().to_owned();
    // The catalog row says the channel is a group DM.
    fixture.server.channel(&channel_id, true).await;
    fixture
        .grants
        .ingress
        .execute(
            "UPDATE channels SET channel_type = 'group-dm' WHERE id = ?",
            waddle_server::db_params![channel_id.clone()],
        )
        .await;
    let refused = fixture
        .grants
        .store
        .enroll_scope(
            &EnrollScope {
                operation: GrantOperationId::generate(),
                lifecycle: ScopeLifecycleId::generate(),
                key: ScopeKey::new(bare(SPACES), format!("space-{tag}").as_str())
                    .expect("scope key"),
                rooms: vec![bound],
                bootstrap: vec![BootstrapGrant {
                    room: room.clone(),
                    subject: super::host_grant_support::subject(OWNER),
                    affiliation: RoomAffiliation::Owner,
                }],
            },
            &fixture.grants.policy,
        )
        .await;
    assert!(
        matches!(refused, Err(HostGrantError::GroupDirectMessageRoom)),
        "a group DM was enrolled: {refused:?}"
    );
    fixture.refresh_enrolled().await;
    assert_eq!(
        fixture
            .server
            .state
            .deps
            .app_state
            .host_grants
            .channel_enrolled(&channel_id)
            .await,
        Ok(false)
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_protected_read_is_decided_again_after_a_revoke() {
    let Some(fixture) = ManagedRoom::start("hg_sink_reread").await else {
        return;
    };
    let room = fixture.room().clone();
    let state = &fixture.server.state;
    let alice = bare(PEER);
    assert_eq!(
        host_grant_read_still_admitted(state, &room, Some(&alice)).await,
        Some(true)
    );
    fixture
        .grants
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban");
    assert_eq!(
        host_grant_read_still_admitted(state, &room, Some(&alice)).await,
        Some(false)
    );
    // A room no scope manages keeps its single check.
    let unmanaged: BareJid = format!("unmanaged-room@{MUC}").parse().expect("room JID");
    assert_eq!(
        host_grant_read_still_admitted(state, &unmanaged, Some(&alice)).await,
        None
    );
    fixture.close().await;
}

#[tokio::test]
async fn an_affiliation_change_whose_commit_reply_was_lost_is_read_back() {
    use waddle_server::permissions::host_grant::HostGrantCommitFault;
    use waddle_server::test_support::record_room_affiliation_under_commit_fault;
    let Some(fixture) = ManagedRoom::start("hg_sink_commit").await else {
        return;
    };
    let room = fixture.room().clone();
    let state = &fixture.server.state;

    // The change committed and its acknowledgement was lost: the request
    // reads the operation back and reports it recorded.
    assert_eq!(
        record_room_affiliation_under_commit_fault(
            state,
            &room,
            &bare(OWNER),
            &bare(CAROL),
            waddle_xmpp::Affiliation::Member,
            HostGrantCommitFault::ReplyLostAfterCommit,
        )
        .await,
        Ok(())
    );
    assert_eq!(
        fixture.grants.affiliations(CAROL).await,
        Some((RoomAffiliation::Member, RoomAffiliation::None))
    );

    // The change never committed: the read back finds no operation, the
    // request fails, and nothing is recorded.
    let dave = "dave@example.com";
    assert_eq!(
        record_room_affiliation_under_commit_fault(
            state,
            &room,
            &bare(OWNER),
            &bare(dave),
            waddle_xmpp::Affiliation::Member,
            HostGrantCommitFault::LostBeforeCommit,
        )
        .await,
        Err(DefinedCondition::InternalServerError)
    );
    assert_eq!(fixture.grants.affiliations(dave).await, None);
    fixture.close().await;
}

#[tokio::test]
async fn the_topology_bootstrap_leaves_an_enrolled_default_channel_and_general_space_alone() {
    use waddle_server::permissions::host_grant::{
        BootstrapGrant, EnrollScope, GrantOperationId, ManagedRoom as BoundRoom, MutationReceipt,
        ScopeKey, ScopeLifecycleId,
    };
    use waddle_server::permissions::ListSubjects;
    use waddle_server::test_support::bootstrap_fresh_topology;
    let Some(fixture) = ManagedRoom::start("hg_sink_topology").await else {
        return;
    };
    // A scope whose Space is the General Space and whose room is the
    // default chat channel, which the bootstrap otherwise writes, places in
    // that Space and opens.
    let chat: BareJid = format!("chat@{MUC}").parse().expect("chat room");
    let receipt = fixture
        .grants
        .store
        .enroll_scope(
            &EnrollScope {
                operation: GrantOperationId::generate(),
                lifecycle: ScopeLifecycleId::generate(),
                key: ScopeKey::new(SPACES.parse().expect("Spaces service"), "general")
                    .expect("scope key"),
                rooms: vec![BoundRoom::new(chat.clone()).expect("bound room")],
                bootstrap: vec![BootstrapGrant {
                    room: chat.clone(),
                    subject: super::host_grant_support::subject(OWNER),
                    affiliation: RoomAffiliation::Owner,
                }],
            },
            &fixture.grants.policy,
        )
        .await
        .expect("enroll the default channel");
    assert!(matches!(receipt, MutationReceipt::Recorded(_)));
    fixture.refresh_enrolled().await;

    bootstrap_fresh_topology(&fixture.server.state)
        .await
        .expect("topology bootstrap");

    let catalog = |id: &'static str| {
        let ingress = &fixture.grants.ingress;
        async move {
            // The scalar subquery yields one row, NULL without a catalog row.
            ingress
                .optional_text(&format!(
                    "SELECT (SELECT id FROM channels WHERE id = '{id}')"
                ))
                .await
        }
    };
    // The enrolled default channel gets no catalog row; another default
    // channel does.
    assert_eq!(catalog("chat").await, None);
    assert_eq!(
        catalog("announcements").await.as_deref(),
        Some("announcements")
    );
    // The enrolled General Space node is not created, and the channel gets
    // no parent relationship.
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    assert!(fixture
        .server
        .state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces, "general")
        .await
        .expect("node lookup")
        .is_none());
    let parents = fixture
        .legacy
        .ask(ListSubjects {
            object: Object::new(ObjectType::Channel, "chat"),
            relation: Relation::new("parent"),
        })
        .await
        .expect("parent relationships");
    assert!(
        parents.is_empty(),
        "the bootstrap parented chat: {parents:?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn an_affiliation_change_overtaken_by_another_is_recorded_at_the_current_generation() {
    use waddle_server::permissions::host_grant::{RoomBinding, RoomGate};
    use waddle_server::test_support::record_room_affiliation_with_grants;
    let Some(fixture) = ManagedRoom::start("hg_sink_stale").await else {
        return;
    };
    let room = fixture.room().clone();
    let state = &fixture.server.state;
    let read = || async {
        match state
            .deps
            .app_state
            .host_grants
            .room(RoomBinding::Room(&room))
            .await
        {
            RoomGate::Managed(grants) => grants,
            _ => panic!("the fixture room is managed"),
        }
    };

    // The request read the room's grants; another change committed before
    // its own.
    let stale = read().await;
    fixture
        .grants
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("the overtaking change");
    assert_ne!(read().await.grants.generation, stale.grants.generation);

    // The request is retried at the current generation and recorded; the
    // overtaking change is kept.
    let dave = "dave@example.com";
    assert_eq!(
        record_room_affiliation_with_grants(
            state,
            &stale,
            &bare(OWNER),
            &bare(dave),
            waddle_xmpp::Affiliation::Member,
        )
        .await,
        Ok(())
    );
    assert_eq!(
        fixture
            .grants
            .affiliations(dave)
            .await
            .map(|(recorded, _)| recorded),
        Some(RoomAffiliation::Member)
    );
    assert_eq!(
        fixture
            .grants
            .affiliations(CAROL)
            .await
            .map(|(recorded, _)| recorded),
        Some(RoomAffiliation::Member)
    );
    fixture.close().await;
}

#[tokio::test]
async fn fan_out_from_an_enrolled_space_node_delivers_nothing() {
    use waddle_server::test_support::fan_out_published_item;
    let Some(fixture) = ManagedRoom::start("hg_sink_fanout").await else {
        return;
    };
    let state = &fixture.server.state;
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    let enrolled = fixture.grants.scope.space_node.clone();
    let storage = &state.deps.protocol.pubsub_storage;
    // CAROL holds a subscription on the enrolled Space node, as one taken
    // before enrollment or written by an older binary would be, and on a
    // Space node no scope enrolls.
    for node in [enrolled.as_str(), PLAIN_SPACE] {
        storage
            .get_or_create_node(&spaces, node)
            .await
            .expect("Space node");
        storage
            .update_node_config(&spaces, node, &NodeConfig::spaces_public())
            .await
            .expect("Space node configuration");
        storage
            .subscribe(&spaces, node, &Jid::from(bare(CAROL)))
            .await
            .expect("subscription");
    }
    let mut carol = fixture.server.client("carol", "phone").await;
    let payload = || Element::builder("note", "urn:example:space-note").build();
    // Whatever the request's PEP classification, the enrolled node's event
    // reaches no subscriber.
    for is_pep in [false, true] {
        fan_out_published_item(state, &spaces, &enrolled, "item-1", payload(), is_pep).await;
        assert!(
            !receives_node_event(&mut carol, &enrolled, Duration::from_millis(500)).await,
            "an enrolled Space node's event was delivered (is_pep: {is_pep})"
        );
    }
    // The unenrolled node delivers to the same subscription.
    fan_out_published_item(state, &spaces, PLAIN_SPACE, "item-1", payload(), false).await;
    assert!(
        receives_node_event(&mut carol, PLAIN_SPACE, Duration::from_secs(5)).await,
        "an unenrolled Space node's event was not delivered"
    );
    fixture.close().await;
}

#[tokio::test]
async fn the_moderator_role_is_granted_to_served_accounts_only() {
    use waddle_xmpp::muc::room_actor::{GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation};
    use waddle_xmpp::muc::room_registry_actor::GetRoom;
    let Some(fixture) = ManagedRoom::start("hg_sink_remote_moderator").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut owner = fixture.server.client("owner", "phone").await;
    let joined = join(&mut owner, &room, "owner").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "the owner's join: {joined:?}"
    );

    // An account of a home this deployment does not serve is seated in the
    // room; how it got there (federation) is not what this case decides.
    let actor = fixture
        .server
        .state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("room lookup")
        .expect("room actor");
    let admission_revision = actor
        .ask(GetSnapshot)
        .await
        .expect("room snapshot")
        .admission_revision;
    actor
        .ask(JoinWithAffiliation {
            sender_jid: format!("{REMOTE}/phone")
                .parse()
                .expect("remote occupant JID"),
            nick: "eve".into(),
            affiliation_grant: JoinAffiliationGrant::HostOwned,
            local_domain: DOMAIN.into(),
            admission_revision,
            session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        })
        .await
        .expect("remote occupant seated");

    // The owner holds `moderate`, but the moderator role is not granted to
    // the remote occupant.
    let reply = round_trip(
        &mut owner,
        set("voice", room.as_str(), role_item("eve", "moderator")),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::NotAllowed)
            && mentions(
                &Element::from(reply.clone()),
                "The moderator role is granted to accounts of this deployment and, over a \
                 certificate-authenticated stream, of its trusted servers only."
            ),
        "a remote occupant was made a moderator: {reply:?}"
    );
    fixture.close().await;
}
