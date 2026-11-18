//! XEP-0353 call candidates follow the account recipient pass, once per call,
//! whether delivery selects connected resources, detached SM custody, or neither.
#![cfg(feature = "test-support")]

use std::path::PathBuf;
use std::time::Duration;

use jid::{BareJid, Jid};
use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::ingress::IngressEffectKind;
use waddle_xmpp::stream_management::SM_NS;
use waddle_xmpp::xep::xep0191::NS_BLOCKING;
use waddle_xmpp::xep::xep0334::{add_hint, remove_hint, Hint};
use waddle_xmpp::xep::xep0353::{
    build_propose, build_retract, jmi_propose, CallOffer, NS_JINGLE_MESSAGE,
};
use waddle_xmpp::xep::xep0492::build_notify_element;
use waddle_xmpp::xep::xep_waddle_dm_bookmarks::{
    build_dm_bookmark_element, PEP_NODE_WADDLE_DM_BOOKMARKS,
};
use waddle_xmpp::xep::NotificationLevel;
use waddle_xmpp_core::xep0359::add_origin_id;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jingle::SessionId;
use xmpp_parsers::message::{Id, Message, MessageType};
use xmpp_parsers::ping::Ping;
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::pubsub::pubsub::{Item, PubSub, Publish};
use xmpp_parsers::pubsub::{ItemId, NodeName};

const DOMAIN: &str = "localhost";
const ACCOUNTS: &[(&str, &str)] = &[
    ("alice", "live-call-alice-password"),
    ("bob", "live-call-bob-password"),
    ("charlie", "live-call-charlie-password"),
];
const CALL_ID: &str = "live-call-session";
const CONTROL_CALL_ID: &str = "positive-control-session";

struct Fixture {
    server: TestServer,
    pool: sqlx::SqlitePool,
    effects_file: PathBuf,
    _directory: tempfile::TempDir,
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct CandidateRow {
    class: String,
    reason: String,
    stanza_id_by: String,
    stanza_id: String,
    call_sid: Option<String>,
    call_video: Option<i64>,
    delivery_id: Option<String>,
}

impl Fixture {
    async fn start() -> Self {
        let directory = tempfile::tempdir().expect("call test directory");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("calls.sqlite").display()
        );
        let effects_file = directory.path().join("candidate-effects.jsonl");
        let server = TestServer::start_persistent_with_extra_envs(
            &database_url,
            ACCOUNTS,
            &[
                ("WADDLE_XMPP_DURABLE_SM", "durable"),
                (
                    "WADDLE_TEST_CALL_CANDIDATE_EFFECTS_FILE",
                    effects_file.to_str().expect("effect observation path"),
                ),
            ],
        );
        let pool = sqlx::SqlitePool::connect(&database_url)
            .await
            .expect("inspect call database");
        Self {
            server,
            pool,
            effects_file,
            _directory: directory,
        }
    }

    async fn connect(&self, account: usize, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(
            &self.server.ws_url(),
            DOMAIN,
            ACCOUNTS[account].0,
            ACCOUNTS[account].1,
            resource,
        )
        .await
        .expect("authenticated WebSocket")
    }

    async fn available(&self, resource: &str) -> WsXmppClient {
        let mut client = self.connect(1, resource).await;
        send(&mut client, Presence::new(PresenceType::None).into()).await;
        barrier(&mut client).await;
        client
    }

    async fn detach(&self, resource: &str) -> Jid {
        let mut client = self.available(resource).await;
        send(
            &mut client,
            Element::builder("enable", SM_NS)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .build(),
        )
        .await;
        let enabled = client
            .recv_matching(|frame| {
                frame
                    .parse::<Element>()
                    .is_ok_and(|element| element.is("enabled", SM_NS))
            })
            .await
            .expect("resumable stream enabled");
        assert!(enabled
            .parse::<Element>()
            .expect("enabled XML")
            .attr("id")
            .is_some());
        let jid: Jid = client
            .full_jid
            .as_ref()
            .expect("bound resource")
            .parse()
            .expect("typed resource JID");
        drop(client);
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let detached: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM sm_durable_sessions WHERE full_jid = ? AND state = 2",
                )
                .bind(jid.to_string())
                .fetch_one(&self.pool)
                .await
                .expect("detached session state");
                if detached == 1 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("transport loss hibernates the recipient session");
        jid
    }

    async fn candidates(&self) -> Vec<CandidateRow> {
        sqlx::query_as(
            "SELECT class, reason, stanza_id_by, stanza_id, call_sid, call_video, delivery_id \
             FROM notification_candidates ORDER BY stanza_id",
        )
        .fetch_all(&self.pool)
        .await
        .expect("call candidates")
    }

    async fn one_call(&self, video: bool) -> CandidateRow {
        self.one_call_for(&recipient(), CALL_ID, video).await
    }

    async fn one_call_for(&self, recipient: &BareJid, sid: &str, video: bool) -> CandidateRow {
        let rows = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let rows = self.candidates().await;
                if rows.iter().any(|row| row.call_sid.as_deref() == Some(sid)) {
                    return rows;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("call candidate committed");
        assert_eq!(rows.len(), 1, "one recipient-level candidate: {rows:?}");
        let row = rows.into_iter().next().expect("one candidate");
        assert_eq!(row.class, "call");
        assert_eq!(row.reason, "offline_call");
        assert_eq!(row.stanza_id_by, recipient.as_str());
        assert_eq!(row.call_sid.as_deref(), Some(sid));
        assert_eq!(row.call_video, Some(i64::from(video)));
        assert!(row.delivery_id.is_some(), "canonical notification custody");
        // All planning completes before this candidate can commit. Count raw
        // effects rather than unique candidate/receipt rows, which conceal a
        // per-resource double fire. Origin-id replay may plan a fresh stamp
        // before restamping, so count the original archived ingress identity.
        let observations = std::fs::read_to_string(&self.effects_file)
            .expect("candidate effect observer must have run");
        let effect_count = observations
            .lines()
            .map(|line| {
                serde_json::from_str::<(String, String)>(line).expect("candidate observation")
            })
            .filter(|(owner, stanza_id)| owner == recipient.as_str() && stanza_id == &row.stanza_id)
            .count();
        assert_eq!(
            effect_count, 1,
            "one raw candidate effect per recipient pass"
        );
        row
    }

    /// The control is queued after the negative stanza on the same sender stream.
    /// Observing its committed candidate proves candidate execution is working and
    /// prevents an early empty read from making the negative assertion vacuous.
    async fn only_positive_control(&self, sender: &mut WsXmppClient) {
        let control_recipient: BareJid = "charlie@localhost".parse().expect("control recipient");
        let control = proposal_with_sid(
            control_recipient.clone().into(),
            CallOffer::audio_only(),
            CONTROL_CALL_ID,
        );
        send(sender, control.into()).await;
        barrier(sender).await;
        self.one_call_for(&control_recipient, CONTROL_CALL_ID, false)
            .await;
    }

    async fn one_missed_call(&self) -> CandidateRow {
        let rows = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let rows = self.candidates().await;
                if rows.iter().any(|row| row.class == "missed_call") {
                    return rows;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("missed-call candidate committed");
        assert_eq!(
            rows.len(),
            2,
            "one invitation and one missed call: {rows:?}"
        );
        let row = rows
            .into_iter()
            .find(|row| row.class == "missed_call")
            .expect("one missed-call candidate");
        assert_eq!(row.reason, "call_retracted");
        assert_eq!(row.stanza_id_by, recipient().as_str());
        assert_eq!(row.call_sid.as_deref(), Some(CALL_ID));
        assert_eq!(
            row.call_video, None,
            "missed calls have no invitation media"
        );
        assert!(row.delivery_id.is_some(), "canonical notification custody");
        let observations = std::fs::read_to_string(&self.effects_file)
            .expect("candidate effect observer must have run");
        let effect_count = observations
            .lines()
            .map(|line| {
                serde_json::from_str::<(String, String)>(line).expect("candidate observation")
            })
            .filter(|(owner, stanza_id)| {
                owner == recipient().as_str() && stanza_id == &row.stanza_id
            })
            .count();
        assert_eq!(
            effect_count, 1,
            "one raw missed-call candidate effect per recipient pass"
        );
        row
    }

    async fn control_after_retract(&self, sender: &mut WsXmppClient) {
        let to: BareJid = "charlie@localhost".parse().expect("control recipient");
        send(
            sender,
            proposal_with_sid(to.into(), CallOffer::audio_only(), CONTROL_CALL_ID).into(),
        )
        .await;
        barrier(sender).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if self
                    .candidates()
                    .await
                    .iter()
                    .any(|row| row.call_sid.as_deref() == Some(CONTROL_CALL_ID))
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("positive control committed after retract");
    }

    async fn no_missed_call(&self, sender: &mut WsXmppClient) {
        self.control_after_retract(sender).await;
        assert!(self
            .candidates()
            .await
            .iter()
            .all(|row| row.class != "missed_call"));
    }

    async fn notification_effects_settled(&self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let pending: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM ingress_effect_intents i WHERE i.kind = ? AND NOT EXISTS \
                     (SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key \
                      AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash)",
                ).bind(IngressEffectKind::NotificationActivityPreview.storage_tag())
                    .fetch_one(&self.pool).await.expect("notification effect receipts");
                if pending == 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("every notification effect, including duplicate retract, settles");
    }

    async fn pending_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM pending_delivery WHERE recipient_jid = ?")
            .bind(recipient().as_str())
            .fetch_one(&self.pool)
            .await
            .expect("pending delivery count")
    }
}

fn recipient() -> BareJid {
    "bob@localhost".parse().expect("recipient JID")
}

fn proposal(to: Jid, offer: CallOffer) -> Message {
    proposal_with_sid(to, offer, CALL_ID)
}

fn proposal_with_sid(to: Jid, offer: CallOffer, sid: &str) -> Message {
    let mut message = Message::new(Some(to));
    message.type_ = MessageType::Chat;
    message.id = Some(Id(sid.to_owned()));
    message
        .payloads
        .push(build_propose(SessionId(sid.to_owned()), offer));
    add_hint(&mut message, Hint::Store);
    message
}

async fn send(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    client
        .send(&String::from_utf8(bytes).expect("XML UTF-8"))
        .await
        .expect("send XML");
}

/// A pong on the same stream proves the preceding routing pass completed.
async fn barrier(client: &mut WsXmppClient) {
    let mut ping = Iq::from_get("call-routing-barrier", Ping);
    *ping.to_mut() = Some(DOMAIN.parse().expect("server JID"));
    send(client, ping.into()).await;
    let pong = client
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("iq", xmpp_parsers::ns::JABBER_CLIENT)
                    && element.attr("id") == Some("call-routing-barrier")
            })
        })
        .await
        .expect("routing barrier pong");
    assert_eq!(
        pong.parse::<Element>().expect("pong XML").attr("type"),
        Some("result")
    );
}

async fn receive_call(client: &mut WsXmppClient) {
    client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .ok()
                .and_then(|element| Message::try_from(element).ok())
                .and_then(|message| jmi_propose(&message))
                .is_some_and(|(sid, _)| sid.0 == CALL_ID)
        })
        .await
        .expect("recipient receives call proposal");
    barrier(client).await;
}

#[tokio::test]
async fn connected_recipient_gets_one_call_candidate_without_pending_delivery() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    receive_call(&mut bob).await;
    fixture.one_call(false).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn detached_recipient_gets_one_call_candidate_without_pending_delivery() {
    let fixture = Fixture::start().await;
    fixture.detach("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_video()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(true).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn two_connected_resources_share_one_call_candidate_without_pending_delivery() {
    let fixture = Fixture::start().await;
    let mut phone = fixture.available("phone").await;
    let mut tablet = fixture.available("tablet").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_video()).into(),
    )
    .await;
    barrier(&mut alice).await;
    receive_call(&mut phone).await;
    receive_call(&mut tablet).await;
    fixture.one_call(true).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn connected_and_detached_resources_share_one_call_candidate_without_pending_delivery() {
    let fixture = Fixture::start().await;
    fixture.detach("phone").await;
    let mut tablet = fixture.available("tablet").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_video()).into(),
    )
    .await;
    barrier(&mut alice).await;
    receive_call(&mut tablet).await;
    fixture.one_call(true).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn connected_recipient_gets_no_call_candidate_without_store_hint() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    let mut message = proposal(recipient().into(), CallOffer::audio_only());
    remove_hint(&mut message, Hint::Store);
    send(&mut alice, message.into()).await;
    barrier(&mut alice).await;
    receive_call(&mut bob).await;
    fixture.only_positive_control(&mut alice).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn connected_recipient_who_muted_caller_gets_no_call_candidate() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    let publish = PubSub::Publish {
        publish: Publish {
            node: NodeName(PEP_NODE_WADDLE_DM_BOOKMARKS.to_owned()),
            items: vec![Item {
                id: Some(ItemId("alice@localhost".to_owned())),
                publisher: None,
                payload: Some(build_dm_bookmark_element(&build_notify_element(
                    NotificationLevel::Never,
                ))),
            }],
        },
        publish_options: None,
    };
    send(&mut bob, Iq::from_set("mute-caller", publish).into()).await;
    let muted = bob
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("iq", xmpp_parsers::ns::JABBER_CLIENT)
                    && element.attr("id") == Some("mute-caller")
            })
        })
        .await
        .expect("mute caller result");
    assert_eq!(
        muted.parse::<Element>().expect("mute XML").attr("type"),
        Some("result")
    );
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    receive_call(&mut bob).await;
    fixture.only_positive_control(&mut alice).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn detached_full_jid_gets_one_call_candidate_without_pending_delivery() {
    let fixture = Fixture::start().await;
    let full = fixture.detach("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(&mut alice, proposal(full, CallOffer::audio_only()).into()).await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn blocked_sender_gets_no_call_candidate() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    send(
        &mut bob,
        Element::builder("iq", xmpp_parsers::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "block-caller")
            .append(
                Element::builder("block", NS_BLOCKING)
                    .append(
                        Element::builder("item", NS_BLOCKING)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@localhost",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
    .await;
    let blocked = bob
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.attr("id") == Some("block-caller"))
        })
        .await
        .expect("block result");
    assert_eq!(
        blocked.parse::<Element>().expect("block XML").attr("type"),
        Some("result")
    );
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.only_positive_control(&mut alice).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn self_addressed_proposal_gets_no_call_candidate() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    send(
        &mut bob,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut bob).await;
    fixture.only_positive_control(&mut bob).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn headless_recipient_still_gets_one_call_candidate_through_offline_delivery() {
    let fixture = Fixture::start().await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    assert_eq!(fixture.pending_count().await, 1);
}

#[tokio::test]
async fn proposal_without_media_and_retract_get_no_call_candidate() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(
            recipient().into(),
            CallOffer {
                audio: false,
                video: false,
            },
        )
        .into(),
    )
    .await;
    let mut retract = Message::new(Some(recipient().into()));
    retract.type_ = MessageType::Chat;
    retract.id = Some(Id("call-retract".to_owned()));
    retract
        .payloads
        .push(build_retract(SessionId(CALL_ID.to_owned())).into());
    send(&mut alice, retract.into()).await;
    barrier(&mut alice).await;
    barrier(&mut bob).await;
    fixture.only_positive_control(&mut alice).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn origin_id_retry_reuses_the_live_call_candidate_archive_identity() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    let mut message = proposal(recipient().into(), CallOffer::audio_only());
    add_origin_id(&mut message, "retry-call-origin");
    send(&mut alice, message.clone().into()).await;
    barrier(&mut alice).await;
    receive_call(&mut bob).await;
    let first = fixture.one_call(false).await;

    message.id = Some(Id("call-envelope-retry".to_owned()));
    send(&mut alice, message.into()).await;
    barrier(&mut alice).await;
    barrier(&mut bob).await;
    assert_eq!(fixture.one_call(false).await, first);
    assert_eq!(fixture.pending_count().await, 0);
}

fn retract(id: &str) -> Message {
    let mut message = Message::new(Some(recipient().into()));
    message.type_ = MessageType::Chat;
    message.id = Some(Id(id.to_owned()));
    message
        .payloads
        .push(build_retract(SessionId(CALL_ID.to_owned())).into());
    add_hint(&mut message, Hint::Store);
    message
}

async fn propose_then_retract(fixture: &Fixture) {
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_video()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(true).await;
    send(&mut alice, retract("missed-call-retract").into()).await;
    barrier(&mut alice).await;
    fixture.one_missed_call().await;
    fixture.notification_effects_settled().await;
}

#[tokio::test]
async fn connected_recipient_gets_one_missed_call_candidate() {
    let fixture = Fixture::start().await;
    let _bob = fixture.available("phone").await;
    propose_then_retract(&fixture).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn detached_recipient_gets_one_missed_call_candidate() {
    let fixture = Fixture::start().await;
    fixture.detach("phone").await;
    propose_then_retract(&fixture).await;
    assert_eq!(fixture.pending_count().await, 0);
}

#[tokio::test]
async fn headless_recipient_gets_one_missed_call_candidate() {
    let fixture = Fixture::start().await;
    propose_then_retract(&fixture).await;
    assert_eq!(fixture.pending_count().await, 2);
}

#[tokio::test]
async fn propose_immediately_followed_by_retract_on_one_stream_gets_missed_call() {
    let fixture = Fixture::start().await;
    let _bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    // Deliberately no barrier or database wait between the two stanzas.
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    send(&mut alice, retract("immediate-retract").into()).await;
    barrier(&mut alice).await;
    fixture.one_missed_call().await;
    fixture.notification_effects_settled().await;
}

#[tokio::test]
async fn retract_from_another_caller_resource_gets_one_missed_call() {
    let fixture = Fixture::start().await;
    let _bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    let mut other_resource = fixture.connect(0, "other-resource").await;
    send(
        &mut other_resource,
        retract("other-resource-retract").into(),
    )
    .await;
    barrier(&mut other_resource).await;
    fixture.one_missed_call().await;
}

#[tokio::test]
async fn retract_without_prior_call_candidate_gets_no_missed_call() {
    let fixture = Fixture::start().await;
    let _bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(&mut alice, retract("no-prior-retract").into()).await;
    fixture.no_missed_call(&mut alice).await;
}

#[tokio::test]
async fn different_account_cannot_retract_another_call_for_a_missed_call_alert() {
    let fixture = Fixture::start().await;
    let _bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    let mut charlie = fixture.connect(2, "forger").await;
    send(&mut charlie, retract("forged-retract").into()).await;
    barrier(&mut charlie).await;
    fixture.no_missed_call(&mut alice).await;
}

#[tokio::test]
async fn tie_break_retract_gets_no_missed_call() {
    let fixture = Fixture::start().await;
    let _bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    let mut message = retract("tie-break-retract");
    message.payloads[0].append_child(Element::builder("tie-break", NS_JINGLE_MESSAGE).build());
    send(&mut alice, message.into()).await;
    fixture.no_missed_call(&mut alice).await;
}

#[tokio::test]
async fn retract_more_than_sixty_seconds_after_call_candidate_gets_no_missed_call() {
    let fixture = Fixture::start().await;
    let _bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    // Move only the durable invitation's receipt time; avoid a minute-long sleep.
    sqlx::query("UPDATE notification_candidates SET created_at_ms = created_at_ms - 61000 WHERE class = 'call'")
        .execute(&fixture.pool).await.expect("age the invitation");
    send(&mut alice, retract("late-retract").into()).await;
    fixture.no_missed_call(&mut alice).await;
}

#[tokio::test]
async fn second_distinct_retract_is_duplicate_and_its_canonical_receipt_settles() {
    let fixture = Fixture::start().await;
    let _bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    send(&mut alice, retract("first-retract").into()).await;
    barrier(&mut alice).await;
    let first = fixture.one_missed_call().await;
    fixture.notification_effects_settled().await;
    send(&mut alice, retract("second-retract").into()).await;
    fixture.control_after_retract(&mut alice).await;
    fixture.notification_effects_settled().await;
    let missed = fixture
        .candidates()
        .await
        .into_iter()
        .filter(|row| row.class == "missed_call")
        .collect::<Vec<_>>();
    assert_eq!(
        missed,
        vec![first],
        "the first candidate and delivery identity are retained"
    );
    let observations =
        std::fs::read_to_string(&fixture.effects_file).expect("candidate observations");
    let recipient_effects = observations
        .lines()
        .map(|line| serde_json::from_str::<(String, String)>(line).expect("candidate observation"))
        .filter(|(owner, _)| owner == recipient().as_str())
        .count();
    assert_eq!(
        recipient_effects, 3,
        "proposal and both distinct retracts were planned"
    );
}

async fn mute_caller(client: &mut WsXmppClient) {
    let publish = PubSub::Publish {
        publish: Publish {
            node: NodeName(PEP_NODE_WADDLE_DM_BOOKMARKS.to_owned()),
            items: vec![Item {
                id: Some(ItemId("alice@localhost".to_owned())),
                publisher: None,
                payload: Some(build_dm_bookmark_element(&build_notify_element(
                    NotificationLevel::Never,
                ))),
            }],
        },
        publish_options: None,
    };
    send(client, Iq::from_set("mute-before-retract", publish).into()).await;
    let response = client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.attr("id") == Some("mute-before-retract"))
        })
        .await
        .expect("mute caller response");
    assert_eq!(
        response.parse::<Element>().expect("mute XML").attr("type"),
        Some("result")
    );
}

#[tokio::test]
async fn muted_conversation_gets_no_missed_call_even_with_prior_call_candidate() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    mute_caller(&mut bob).await;
    send(&mut alice, retract("muted-retract").into()).await;
    fixture.no_missed_call(&mut alice).await;
}

#[tokio::test]
async fn caller_blocked_after_proposal_gets_no_missed_call() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    let block = Element::builder("iq", xmpp_parsers::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            "block-before-retract",
        )
        .append(
            Element::builder("block", NS_BLOCKING)
                .append(
                    Element::builder("item", NS_BLOCKING)
                        .attr(
                            minidom::rxml::xml_ncname!("jid").to_owned(),
                            "alice@localhost",
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    send(&mut bob, block).await;
    let response = bob
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.attr("id") == Some("block-before-retract"))
        })
        .await
        .expect("block caller response");
    assert_eq!(
        response.parse::<Element>().expect("block XML").attr("type"),
        Some("result")
    );
    send(&mut alice, retract("blocked-retract").into()).await;
    fixture.no_missed_call(&mut alice).await;
}
