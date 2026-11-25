//! XEP-0513 explicit mention integration tests over WebSocket.

use waddle_ws_test_support as ws_common;

use std::str::FromStr;
use tokio::sync::Mutex;
use waddle_xmpp::xep::NS_EXPLICIT_MENTIONS;
use waddle_xmpp::Stanza;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0513-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

async fn join_room(client: &mut WsXmppClient, room: &str) -> String {
    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    let frames = client
        .recv_until(|frame| frame.contains("urn:xmpp:occupant-id:0"))
        .await
        .expect("join responses");
    frames
        .iter()
        .find_map(|frame| occupant_id_from_frame(frame))
        .expect("self presence occupant id")
}

fn occupant_id_from_frame(frame: &str) -> Option<String> {
    let element = Element::from_str(frame).ok()?;
    find_occupant_id(&element)
}

fn find_occupant_id(element: &Element) -> Option<String> {
    if element.name() == "occupant-id" && element.ns() == "urn:xmpp:occupant-id:0" {
        return element.attr("id").map(str::to_string);
    }
    element.children().find_map(find_occupant_id)
}

/// Serialize a typed `Stanza` to its wire-form XML string. Mirrors
/// the helper in xep0503_spaces_ws.rs so XEP wire-shape tests follow
/// the same "typed builder + serializer" pattern (CLAUDE.md XML
/// generation hard rule — never construct XML with `format!`).
fn stanza_to_xml(stanza: &Stanza) -> String {
    let mut buf = Vec::new();
    stanza
        .to_element()
        .write_to(&mut buf)
        .expect("serializing stanza to Vec<u8> should not fail");
    String::from_utf8(buf).expect("xmpp_parsers serializes valid UTF-8")
}

/// Build a typed `<iq type='get'>` envelope addressed at `to` with
/// the given `payload` and return its wire-form XML string. Used by
/// the mentions-query tests so the test author doesn't have
/// to hand-roll attribute quoting or namespace declarations.
fn iq_get_xml(id: &str, to: &str, payload: Element) -> String {
    let iq = Iq::Get {
        from: None,
        to: Some(to.parse().expect("valid iq destination JID")),
        id: id.to_string(),
        payload,
    };
    stanza_to_xml(&Stanza::Iq(Box::new(iq)))
}

/// Build a typed `<iq type='set'>` envelope addressed at `to` with
/// the given `payload` and return its wire-form XML string. Used by
/// the mentions-query tests.
fn iq_set_xml(id: &str, to: &str, payload: Element) -> String {
    let iq = Iq::Set {
        from: None,
        to: Some(to.parse().expect("valid iq destination JID")),
        id: id.to_string(),
        payload,
    };
    stanza_to_xml(&Stanza::Iq(Box::new(iq)))
}

/// Build an empty `<query xmlns='urn:xmpp:mentions:0'/>` payload via the typed `Element::builder`.
fn mentions_query_empty() -> Element {
    Element::builder("query", NS_EXPLICIT_MENTIONS).build()
}

#[tokio::test]
async fn explicit_mentions_route_and_replay_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mentions-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let occupant_id = join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="mention-1">
                <body>@admin please check this</body>
                <markup xmlns="urn:xmpp:markup:0">
                    <span start="0" end="6">
                        <mention xmlns="urn:xmpp:mentions:0"
                            type="urn:xmpp:mentions:0#occupant" target="{occupant_id}"/>
                    </span>
                </markup>
            </message>"#
        ))
        .await
        .expect("send mention");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:mentions:0"))
        .await
        .expect("mention echo");
    assert!(
        echo.contains(&occupant_id),
        "missing mentioned occupant id: {echo}"
    );
    assert!(
        !echo.contains("jid='admin@localhost") && !echo.contains("jid='admin@localhost"),
        "MUC mention payload leaked a JID despite occupant-id support: {echo}"
    );
    // XEP-0513 wire shape: <markup><span start end><mention type target/>.
    let echoed = Element::from_str(&echo).expect("echo parses as XML");
    let markup = echoed
        .get_child("markup", "urn:xmpp:markup:0")
        .unwrap_or_else(|| panic!("mention rides in XEP-0394 markup: {echo}"));
    let span = markup
        .get_child("span", "urn:xmpp:markup:0")
        .expect("markup span");
    assert_eq!(span.attr("start"), Some("0"));
    assert_eq!(span.attr("end"), Some("6"));
    let mention = span
        .get_child("mention", NS_EXPLICIT_MENTIONS)
        .expect("XEP-0513 mention");
    assert_eq!(mention.attr("type"), Some("urn:xmpp:mentions:0#occupant"));
    assert_eq!(mention.attr("target"), Some(occupant_id.as_str()));
    assert!(
        mention.attr("occupantid").is_none() && mention.attr("begin").is_none(),
        "no pre-XEP attributes in the official namespace: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-mention" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-mention") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    // XEP-0513: the replayed mention MUST still identify its target by
    // occupant-id, never by a raw JID inside the `<mention/>` element.
    let mention_frame = frames
        .iter()
        .find(|frame| frame.contains("urn:xmpp:mentions:0") && frame.contains(&occupant_id))
        .unwrap_or_else(|| panic!("MAM did not replay mention: {frames:?}"));
    // XEP-0313 §MUC Archives: for a non-anonymous room the *archived*
    // result additionally discloses the sender's real JID via a
    // room-authored `<x xmlns='muc#user'><item jid=…/></x>` (#1268).
    // The disclosure lives in that item, NOT in the `<mention/>`
    // element — assert the mention element itself stays JID-free while
    // the muc#user item carries the real JID.
    let mention_element_start = mention_frame
        .find("<mention ")
        .expect("mention element present in replay");
    let mention_element_end = mention_frame[mention_element_start..]
        .find("/>")
        .map(|offset| mention_element_start + offset)
        .expect("mention element is self-closing");
    let mention_element = &mention_frame[mention_element_start..mention_element_end];
    assert!(
        !mention_element.contains("jid="),
        "MUC mention element leaked a JID despite occupant-id support: {mention_element}"
    );
    assert!(
        mention_frame.contains("http://jabber.org/protocol/muc#user")
            && mention_frame.contains("jid='admin@localhost"),
        "XEP-0313 §MUC Archives: non-anonymous MAM replay must disclose the real JID \
         via the muc#user item: {mention_frame}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn mention_without_target_attribute_returns_bad_request() {
    // XEP-0513: a '<mention/>' must identify its target via 'jid',
    // 'occupantid', or 'mentions' (group). Decorative mentions with
    // none of these are not interpretable by receivers and are
    // rejected with bad-request.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mention-bad-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let _occupant_id = join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="bad-mention">
                <body>broken mention</body>
                <markup xmlns="urn:xmpp:markup:0">
                    <span start="0" end="6">
                        <mention xmlns="urn:xmpp:mentions:0" target="no-type"/>
                    </span>
                </markup>
            </message>"#
        ))
        .await
        .expect("send malformed mention");
    let error = client
        .recv_matching(|frame| frame.contains("<bad-request"))
        .await
        .expect("bad-request error");
    assert!(
        error.contains("type='error'"),
        "not an error stanza: {error}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn muc_jid_mention_returns_bad_request_when_occupant_ids_supported() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mention-jid-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let _occupant_id = join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="jid-mention">
                <body>jid mention</body>
                <markup xmlns="urn:xmpp:markup:0">
                    <span start="0" end="3">
                        <mention xmlns="urn:waddle:mentions:0" jid="admin@localhost"/>
                    </span>
                </markup>
            </message>"#
        ))
        .await
        .expect("send jid mention");
    let error = client
        .recv_matching(|frame| frame.contains("<bad-request"))
        .await
        .expect("bad-request error");
    assert!(
        error.contains("type='error'"),
        "not an error stanza: {error}"
    );

    let _ = client.close().await;
}

/// The current XEP-0513 (0.1.1) removed the permissions form: a `<query/>`
/// in `urn:xmpp:mentions:0` is no longer a defined IQ, so the room answers
/// it like any unknown request instead of returning a form.
#[tokio::test]
async fn mentions_query_iq_is_not_implemented() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("perms-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let _occupant_id = join_room(&mut client, &room).await;

    for (id, xml) in [
        (
            "mentions-query-get",
            iq_get_xml("mentions-query-get", &room, mentions_query_empty()),
        ),
        (
            "mentions-query-set",
            iq_set_xml("mentions-query-set", &room, mentions_query_empty()),
        ),
    ] {
        client.send(&xml).await.expect("send mentions query");
        let frame = client
            .recv_matching(|frame| frame.contains(id))
            .await
            .expect("mentions query response");
        assert!(
            frame.contains("type='error'") || frame.contains("type=\"error\""),
            "mentions query must be an error, got: {frame}"
        );
        assert!(
            frame.contains("service-unavailable") || frame.contains("feature-not-implemented"),
            "mentions query must be unsupported, got: {frame}"
        );
        assert!(
            !frame.contains("jabber:x:data"),
            "no permissions form may be returned, got: {frame}"
        );
    }

    let _ = client.close().await;
}

/// Serialize a message the shared client builds for `body`, with the room's
/// broadcast labels enabled, to its wire form.
fn client_built_group_message(room: &str, body: &str) -> (String, String) {
    use waddle_xmpp_client::messaging::{
        build_outbound_message, MentionDirectory, SendMessageOptions,
    };
    let options = SendMessageOptions {
        mention_directory: Some(MentionDirectory {
            targets: Vec::new(),
            broadcasts: true,
        }),
        ..SendMessageOptions::default()
    };
    let (id, message) =
        build_outbound_message(room, "groupchat", body, &options).expect("build message");
    (id.to_string(), mention_element_to_xml(&message))
}

/// A message built by the shared client for `@everyone` / `@here` carries
/// XEP-0513 `#channel` mentions (xep-0513.xml:161: no 'target'; xep-0513.xml:126:
/// `active='true'` for "@here"), which the room accepts, reflects and
/// replays from MAM unchanged.
#[tokio::test]
async fn client_built_broadcast_mentions_are_accepted_reflected_and_archived() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mentions-bc-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let _occupant_id = join_room(&mut client, &room).await;

    let (id, xml) = client_built_group_message(&room, "@everyone and @here, hi");
    client.send(&xml).await.expect("send broadcast mentions");
    let echo = client
        .recv_matching(|frame| frame.contains(&id) && frame.contains("urn:xmpp:mentions:0"))
        .await
        .expect("broadcast echo");
    assert!(
        !echo.contains("<bad-request"),
        "client-built channel mention rejected: {echo}"
    );

    let channel_mentions = |frame: &str| -> Vec<(String, String, Option<String>)> {
        let element = Element::from_str(frame).expect("frame parses as XML");
        let mut found = Vec::new();
        collect_channel_mentions(&element, &mut found);
        found
    };
    let expected = vec![
        ("0".to_string(), "9".to_string(), None),
        ("14".to_string(), "19".to_string(), Some("true".to_string())),
    ];
    assert_eq!(channel_mentions(&echo), expected, "reflected: {echo}");
    assert!(
        !echo.contains("xmpp:@everyone") && !echo.contains("target="),
        "no pseudo-URI reference and no 'target' on a channel mention: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-bc" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-bc") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    let archived = frames
        .iter()
        .find(|frame| frame.contains(&id) && frame.contains("urn:xmpp:mentions:0"))
        .unwrap_or_else(|| panic!("MAM did not replay broadcast mentions: {frames:?}"));
    assert_eq!(channel_mentions(archived), expected, "archived: {archived}");

    let _ = client.close().await;
}

/// `(start, end, active)` of every XEP-0513 `#channel` mention in `element`.
fn collect_channel_mentions(element: &Element, into: &mut Vec<(String, String, Option<String>)>) {
    if element.is("span", "urn:xmpp:markup:0") {
        for mention in element
            .children()
            .filter(|child| child.is("mention", NS_EXPLICIT_MENTIONS))
            .filter(|mention| mention.attr("type") == Some("urn:xmpp:mentions:0#channel"))
        {
            into.push((
                element.attr("start").unwrap_or_default().to_string(),
                element.attr("end").unwrap_or_default().to_string(),
                mention.attr("active").map(str::to_string),
            ));
        }
    }
    for child in element.children() {
        collect_channel_mentions(child, into);
    }
}

/// Recovery must retain the recipient-specific occupant mention and No Ping semantics.
#[test]
fn recovery_envelope_preserves_occupant_mention_and_noping() {
    use waddle_server::notification_outbox::{
        candidate_from_envelope, GroupchatCandidateIdentity, NotificationClass,
        NotificationThreadId,
    };
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;
    use waddle_xmpp::xep::{build_mention_markup, generate_occupant_id, ExplicitMention};

    let owner = "juliet@example.com".parse().expect("owner");
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    let sender = room.with_resource_str("romeo").expect("occupant").into();
    let archive = waddle_xmpp_core::xep0359::StanzaId::new("frozen", room.clone().into());
    let secret = OccupantIdSecret::try_new(vec![7; 32]).expect("secret");
    let occupant = generate_occupant_id(&owner, &room, &secret);
    for noping in [false, true] {
        let mut mention = ExplicitMention::occupant_id(occupant.as_str());
        mention.noping = noping;
        let mut envelope = xmpp_parsers::message::Message::new(None);
        envelope.payloads.push(build_mention_markup(&mention));
        let candidate = candidate_from_envelope(
            &envelope,
            GroupchatCandidateIdentity {
                owner: &owner,
                room: &room,
                sender: &sender,
                thread_id: NotificationThreadId::root(),
                archive_stanza_id: &archive,
                is_live_occupant: false,
                sender_can_broadcast_channel_mention: false,
            },
            &secret,
        )
        .expect("rebuild candidate");
        assert_eq!(candidate.noping(), noping);
        assert_eq!(
            candidate.class(),
            if noping {
                NotificationClass::NotifyAll
            } else {
                NotificationClass::PersonalMention
            }
        );
    }
}

// ---------------------------------------------------------------------------
// Personal mentions of non-members (XEP-0430 inbox fan-out)
// ---------------------------------------------------------------------------

const MENTION_PEER: &str = "peer";
const MENTION_PEER_PASSWORD: &str = "peer-password-1";
const MENTION_OTHER: &str = "carol";
const MENTION_OTHER_PASSWORD: &str = "carol-password-1";
const NS_INBOX: &str = "urn:xmpp:inbox:1";

fn mention_element_to_xml(element: &xmpp_parsers::minidom::Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML");
    String::from_utf8(buf).expect("UTF-8")
}

/// `unread` of the caller's XEP-0430 inbox row for `room`, if any.
async fn inbox_unread(client: &mut WsXmppClient, room: &str, id: &str) -> Option<String> {
    use xmpp_parsers::minidom::Element;
    let query = Element::builder("iq", "jabber:client")
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
            "get",
        )
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
            id,
        )
        .append(
            Element::builder("inbox", NS_INBOX)
                .attr(
                    xmpp_parsers::minidom::rxml::xml_ncname!("messages").to_owned(),
                    "false",
                )
                .build(),
        )
        .build();
    client
        .send(&mention_element_to_xml(&query))
        .await
        .expect("send inbox query");
    let frames = client
        .recv_until(|frame| frame.contains("<iq") && frame.contains(id) && frame.contains("<fin"))
        .await
        .expect("inbox response");
    frames
        .iter()
        .filter_map(|frame| frame.parse::<Element>().ok())
        .filter(|message| message.name() == "message" && message.attr("type") != Some("headline"))
        .flat_map(|message| {
            message
                .children()
                .filter(|child| child.name() == "entry" && child.ns() == NS_INBOX)
                .cloned()
                .collect::<Vec<_>>()
        })
        .find(|entry| entry.attr("jid") == Some(room))
        .and_then(|entry| entry.attr("unread").map(str::to_string))
}

/// Poll a fresh session for `user` until its inbox has a row for `room`.
async fn wait_for_inbox_unread(
    server: &TestServer,
    user: &str,
    password: &str,
    room: &str,
) -> Option<String> {
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        user,
        password,
        &format!("inbox-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect inbox client");
    let mut unread = None;
    for attempt in 0..50 {
        unread = inbox_unread(&mut client, room, &format!("inbox-poll-{attempt}")).await;
        if unread.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let _ = client.close().await;
    unread
}

/// `unread` for `user`'s row in `room`, read once.
async fn inbox_unread_once(
    server: &TestServer,
    user: &str,
    password: &str,
    room: &str,
) -> Option<String> {
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        user,
        password,
        &format!("inbox-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect inbox client");
    let unread = inbox_unread(&mut client, room, "inbox-once").await;
    let _ = client.close().await;
    unread
}

/// XEP-0513 §304 bounds who a message can reach: personal mentions of
/// non-members (XEP-0372 references, since a room that supports
/// occupant-ids rejects `jid=` mentions) reach an offline non-member only
/// while the message's combined XEP-0513 + XEP-0372 target count stays
/// within `mentions#count`.
#[tokio::test]
async fn mention_count_cap_bounds_offline_non_member_inbox_rows() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        (MENTION_PEER, MENTION_PEER_PASSWORD),
        (MENTION_OTHER, MENTION_OTHER_PASSWORD),
    ]);
    let password = server.fixed_account_password().to_string();
    let mut client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, "xep0513-nm")
            .await
            .expect("connect and auth");
    let room = format!("mentions-nm-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let occupant_id = join_room(&mut client, &room).await;

    // Five XEP-0513 occupant-id targets plus one XEP-0372 reference to
    // carol exceed the cap of five, so every mention is ignored.
    let over_cap = format!(
        r#"<markup xmlns="urn:xmpp:markup:0">{}</markup>"#,
        (0..5)
            .map(|_| format!(
                r#"<span start="0" end="6"><mention xmlns="urn:xmpp:mentions:0" type="urn:xmpp:mentions:0#occupant" target="{occupant_id}"/></span>"#
            ))
            .collect::<String>()
    );
    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="mention-nm-1">
                <body>@carol and friends</body>
                {over_cap}
                <reference xmlns="urn:xmpp:reference:0" type="mention" uri="xmpp:carol@localhost"/>
            </message>"#
        ))
        .await
        .expect("send over-cap mention");
    client
        .recv_matching(|frame| frame.contains("mention-nm-1"))
        .await
        .expect("over-cap echo");
    // Within the cap: one occupant-id mention plus a reference to the peer.
    // Room dispatch is serialized, so once the peer's row exists the
    // earlier message has been fully projected.
    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="mention-nm-2">
                <body>@admin @peer please check</body>
                <markup xmlns="urn:xmpp:markup:0">
                    <span start="0" end="6"><mention xmlns="urn:xmpp:mentions:0" type="urn:xmpp:mentions:0#occupant" target="{occupant_id}"/></span>
                </markup>
                <reference xmlns="urn:xmpp:reference:0" type="mention" uri="xmpp:peer@localhost"/>
            </message>"#
        ))
        .await
        .expect("send within-cap mention");
    client
        .recv_matching(|frame| frame.contains("mention-nm-2"))
        .await
        .expect("within-cap echo");

    assert_eq!(
        wait_for_inbox_unread(&server, MENTION_PEER, MENTION_PEER_PASSWORD, &room).await,
        Some("1".to_string()),
        "a within-cap mention must give the offline non-member one unread row"
    );
    assert_eq!(
        inbox_unread_once(&server, MENTION_OTHER, MENTION_OTHER_PASSWORD, &room).await,
        None,
        "an over-cap message must not project a row for a mentioned non-member"
    );
    let _ = client.close().await;
}
