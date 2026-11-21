//! XEP-0353 call candidates follow the account recipient pass, once per call,
//! whether delivery selects connected resources, detached SM custody, or neither.
#![cfg(feature = "test-support")]

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use jid::{BareJid, Jid};
use minidom::Element;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::notification_outbox::{NotificationOutboxStore, UnansweredCallScan};
use waddle_server::notification_settings_projection::NotificationSettingsProjectionStore;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::ingress::IngressEffectKind;
use waddle_xmpp::mam::SqlxMamStorage;
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
    database_url: String,
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
            database_url,
            _directory: directory,
        }
    }

    async fn call_created_at_ms(&self) -> i64 {
        sqlx::query_scalar(
            "SELECT created_at_ms FROM notification_candidates WHERE class = 'call' AND call_sid = ?",
        )
        .bind(CALL_ID)
        .fetch_one(&self.pool)
        .await
        .expect("invitation receipt time")
    }

    async fn sweep_unanswered(&self) {
        let created_at_ms = self.call_created_at_ms().await;
        self.sweep_unanswered_at(
            &mut UnansweredCallScan::default(),
            created_at_ms + 55_000,
            128,
        )
        .await;
    }

    async fn sweep_unanswered_at(
        &self,
        scan: &mut UnansweredCallScan,
        now_ms: i64,
        batch_size: usize,
    ) {
        let now = DateTime::<Utc>::from_timestamp_millis(now_ms).expect("deadline timestamp");
        let database = Database::from_config(
            "unanswered-live-sweep",
            &DatabaseConfig::new(DatabaseDriver::Sqlite, &self.database_url),
        )
        .await
        .expect("open live database for timed sweep");
        let outbox = NotificationOutboxStore::try_new(database.clone())
            .await
            .expect("attach existing outbox");
        let mam = SqlxMamStorage::from_sqlite_pool(self.pool.clone())
            .await
            .expect("attach live archive");
        let settings = NotificationSettingsProjectionStore::new(database);
        assert!(
            waddle_server::test_support::sweep_unanswered_calls_for_test(
                &outbox, &mam, &settings, scan, now, batch_size,
            )
            .await
        );
    }

    async fn missed_calls(&self) -> Vec<CandidateRow> {
        self.candidates()
            .await
            .into_iter()
            .filter(|row| row.class == "missed_call" && row.call_sid.as_deref() == Some(CALL_ID))
            .collect()
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

    async fn call_with_sid(&self, sid: &str) -> CandidateRow {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(row) = self
                    .candidates()
                    .await
                    .into_iter()
                    .find(|row| row.class == "call" && row.call_sid.as_deref() == Some(sid))
                {
                    return row;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("call candidate committed")
    }

    fn recipient_candidate_effect_count(&self) -> usize {
        std::fs::read_to_string(&self.effects_file)
            .expect("candidate observations")
            .lines()
            .map(|line| {
                serde_json::from_str::<(String, String)>(line).expect("candidate observation")
            })
            .filter(|(owner, _)| owner == recipient().as_str())
            .count()
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
    receive_call_with_sid(client, CALL_ID).await;
}

async fn receive_call_with_sid(client: &mut WsXmppClient, expected_sid: &str) {
    client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .ok()
                .and_then(|element| Message::try_from(element).ok())
                .and_then(|message| jmi_propose(&message))
                .is_some_and(|(sid, _)| sid.0 == expected_sid)
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

#[tokio::test]
async fn call_push_burst_limit_preserves_live_delivery_and_other_callers() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    for sid in ["burst-first", "burst-second"] {
        send(
            &mut alice,
            proposal_with_sid(recipient().into(), CallOffer::audio_only(), sid).into(),
        )
        .await;
        barrier(&mut alice).await;
        receive_call_with_sid(&mut bob, sid).await;
        fixture.call_with_sid(sid).await;
    }

    let mut third = proposal_with_sid(recipient().into(), CallOffer::audio_only(), "burst-third");
    add_origin_id(&mut third, "burst-third-origin");
    send(&mut alice, third.clone().into()).await;
    barrier(&mut alice).await;
    receive_call_with_sid(&mut bob, "burst-third").await;
    let original_third = fixture.call_with_sid("burst-third").await;
    assert_eq!(fixture.candidates().await.len(), 3);
    assert_eq!(fixture.recipient_candidate_effect_count(), 3);

    third.id = Some(Id("burst-third-retry".to_owned()));
    send(&mut alice, third.into()).await;
    barrier(&mut alice).await;
    barrier(&mut bob).await;
    assert_eq!(fixture.call_with_sid("burst-third").await, original_third);
    assert_eq!(fixture.candidates().await.len(), 3);
    // A retry is planned under a fresh archive id, so it counts its own
    // original row and is suppressed. The original row and push stand.
    assert_eq!(fixture.recipient_candidate_effect_count(), 3);

    send(
        &mut alice,
        proposal_with_sid(recipient().into(), CallOffer::audio_only(), "burst-fourth").into(),
    )
    .await;
    barrier(&mut alice).await;
    receive_call_with_sid(&mut bob, "burst-fourth").await;
    let rows = fixture.candidates().await;
    assert_eq!(rows.len(), 3, "the fourth invitation creates no candidate");
    assert!(rows.iter().all(|row| row.class == "call"));
    assert_eq!(fixture.recipient_candidate_effect_count(), 3);
    assert_eq!(fixture.pending_count().await, 0);

    send(
        &mut alice,
        retract_with_sid("burst-fourth-retract", "burst-fourth").into(),
    )
    .await;
    fixture.no_missed_call(&mut alice).await;
    fixture.notification_effects_settled().await;

    let mut charlie = fixture.connect(2, "other-caller").await;
    send(
        &mut charlie,
        proposal_with_sid(recipient().into(), CallOffer::audio_only(), "other-caller").into(),
    )
    .await;
    barrier(&mut charlie).await;
    receive_call_with_sid(&mut bob, "other-caller").await;
    fixture.call_with_sid("other-caller").await;
    let rows = fixture.candidates().await;
    assert_eq!(
        rows.len(),
        5,
        "three Alice calls, control, and Charlie call"
    );
    assert!(rows.iter().all(|row| row.class == "call"));
}

#[tokio::test]
async fn headless_fourth_call_keeps_pending_delivery_without_a_candidate() {
    let fixture = Fixture::start().await;
    let mut alice = fixture.connect(0, "sender").await;
    for sid in ["offline-first", "offline-second", "offline-third"] {
        send(
            &mut alice,
            proposal_with_sid(recipient().into(), CallOffer::audio_only(), sid).into(),
        )
        .await;
        barrier(&mut alice).await;
        fixture.call_with_sid(sid).await;
    }
    assert_eq!(fixture.pending_count().await, 3);
    send(
        &mut alice,
        proposal_with_sid(
            recipient().into(),
            CallOffer::audio_only(),
            "offline-fourth",
        )
        .into(),
    )
    .await;
    barrier(&mut alice).await;
    // The committed positive control also proves execution finished for the fourth call.
    fixture.control_after_retract(&mut alice).await;
    assert_eq!(fixture.pending_count().await, 4);
    let rows = fixture.candidates().await;
    assert_eq!(rows.len(), 4, "three invitations and the positive control");
    assert!(rows.iter().all(|row| row.class == "call"));
    assert!(rows
        .iter()
        .all(|row| row.call_sid.as_deref() != Some("offline-fourth")));
}

fn retract(id: &str) -> Message {
    retract_with_sid(id, CALL_ID)
}

fn retract_with_sid(id: &str, sid: &str) -> Message {
    let mut message = Message::new(Some(recipient().into()));
    message.type_ = MessageType::Chat;
    message.id = Some(Id(id.to_owned()));
    message
        .payloads
        .push(build_retract(SessionId(sid.to_owned())).into());
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

fn callee_outcome(name: &str, tie_break: bool) -> Message {
    let mut message = Message::new(Some("alice@localhost".parse::<Jid>().expect("caller")));
    message.type_ = MessageType::Chat;
    message.id = Some(Id("callee-outcome".to_owned()));
    let mut outcome = Element::builder(name, NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), CALL_ID)
        .build();
    if tie_break {
        outcome.append_child(Element::builder("tie-break", NS_JINGLE_MESSAGE).build());
    }
    message.payloads.push(outcome);
    add_hint(&mut message, Hint::Store);
    message
}

async fn live_invitation(fixture: &Fixture) -> (WsXmppClient, WsXmppClient) {
    let bob = fixture.available("phone").await;
    let mut alice = fixture.connect(0, "sender").await;
    send(
        &mut alice,
        proposal(recipient().into(), CallOffer::audio_only()).into(),
    )
    .await;
    barrier(&mut alice).await;
    fixture.one_call(false).await;
    (alice, bob)
}

#[tokio::test]
async fn unanswered_call_yields_exactly_one_lineage_free_missed_call() {
    let fixture = Fixture::start().await;
    let (_alice, _bob) = live_invitation(&fixture).await;
    fixture.sweep_unanswered().await;
    fixture.sweep_unanswered().await;
    let missed = fixture.missed_calls().await;
    assert_eq!(missed.len(), 1);
    assert_eq!(missed[0].reason, "call_unanswered");
    assert_eq!(missed[0].call_video, None);
    let delivery_id = missed[0]
        .delivery_id
        .as_ref()
        .expect("timer scheduler identity");
    let descendants: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ?",
    )
    .bind(delivery_id)
    .fetch_one(&fixture.pool)
    .await
    .expect("timer ingress ancestry");
    assert_eq!(descendants, 0, "timer work has no ingress lineage");
}

async fn callee_outcome_prevents_unanswered(name: &str, tie_break: bool, second_device: bool) {
    let fixture = Fixture::start().await;
    let (_alice, mut bob) = live_invitation(&fixture).await;
    let mut second = if second_device {
        Some(fixture.available("second-device").await)
    } else {
        None
    };
    let answerer = second.as_mut().unwrap_or(&mut bob);
    send(answerer, callee_outcome(name, tie_break).into()).await;
    barrier(answerer).await;
    fixture.sweep_unanswered().await;
    assert!(fixture.missed_calls().await.is_empty());
}

#[tokio::test]
async fn callee_proceed_prevents_unanswered_missed_call() {
    callee_outcome_prevents_unanswered("proceed", false, false).await;
}

#[tokio::test]
async fn callee_reject_prevents_unanswered_missed_call() {
    callee_outcome_prevents_unanswered("reject", false, false).await;
}

#[tokio::test]
async fn callee_tie_break_reject_prevents_unanswered_missed_call() {
    callee_outcome_prevents_unanswered("reject", true, false).await;
}

#[tokio::test]
async fn answer_from_second_callee_device_prevents_unanswered_missed_call() {
    callee_outcome_prevents_unanswered("proceed", false, true).await;
}

#[tokio::test]
async fn timer_then_retract_retains_exactly_one_missed_call() {
    let fixture = Fixture::start().await;
    let (mut alice, _bob) = live_invitation(&fixture).await;
    fixture.sweep_unanswered().await;
    send(&mut alice, retract("after-timer").into()).await;
    fixture.control_after_retract(&mut alice).await;
    fixture.notification_effects_settled().await;
    let missed = fixture.missed_calls().await;
    assert_eq!(missed.len(), 1);
    assert_eq!(missed[0].reason, "call_unanswered");
}

#[tokio::test]
async fn retract_then_timer_retains_exactly_one_missed_call() {
    let fixture = Fixture::start().await;
    let (mut alice, _bob) = live_invitation(&fixture).await;
    send(&mut alice, retract("before-timer").into()).await;
    barrier(&mut alice).await;
    let original = fixture.one_missed_call().await;
    fixture.sweep_unanswered().await;
    assert_eq!(fixture.missed_calls().await, vec![original]);
}

#[tokio::test]
async fn retract_after_callee_reject_creates_no_missed_call() {
    let fixture = Fixture::start().await;
    let (mut alice, mut bob) = live_invitation(&fixture).await;
    send(&mut bob, callee_outcome("reject", false).into()).await;
    barrier(&mut bob).await;
    send(&mut alice, retract("after-reject").into()).await;
    fixture.no_missed_call(&mut alice).await;
    fixture.sweep_unanswered().await;
    assert!(fixture.missed_calls().await.is_empty());
}

#[tokio::test]
async fn unanswered_sweep_waits_for_the_deadline() {
    let fixture = Fixture::start().await;
    let (_alice, _bob) = live_invitation(&fixture).await;
    let created = fixture.call_created_at_ms().await;
    let mut scan = UnansweredCallScan::default();
    fixture
        .sweep_unanswered_at(&mut scan, created + 54_999, 128)
        .await;
    assert!(fixture.missed_calls().await.is_empty());
    fixture
        .sweep_unanswered_at(&mut scan, created + 55_000, 128)
        .await;
    assert_eq!(fixture.missed_calls().await.len(), 1);
}

#[tokio::test]
async fn one_sweep_pages_through_tied_timestamps_at_batch_size_one() {
    let fixture = Fixture::start().await;
    let (mut alice, _bob) = live_invitation(&fixture).await;
    fixture.control_after_retract(&mut alice).await;
    let created = fixture.call_created_at_ms().await;
    sqlx::query("UPDATE notification_candidates SET created_at_ms = ? WHERE class = 'call'")
        .bind(created)
        .execute(&fixture.pool)
        .await
        .expect("tie invitation timestamps");
    fixture
        .sweep_unanswered_at(&mut UnansweredCallScan::default(), created + 55_000, 1)
        .await;
    assert_eq!(
        fixture
            .candidates()
            .await
            .iter()
            .filter(|row| row.class == "missed_call")
            .count(),
        2
    );
}
