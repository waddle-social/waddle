//! Two-node suite for XEP-0045 joins, occupant presence and leaves carried
//! to a remote room owner as room requests in the durable mailbox (#2174).
//!
//! Node B owns the room and node A hosts the joining resource. Both select
//! the mailbox and the shared database is at the mailbox protocol epoch, so
//! the join travels as a `RoomJoin` entry: the origin commits the request,
//! the owner decides it with its frozen response and durable occupancy, and
//! the origin hands the response over in the transaction that consumes the
//! entry.
//!
//! The owner-side seams of `clustering::acceptance_seams` stop the owner
//! before and after the transaction in which it decides a request. A join's
//! request stanza is rebuilt by the origin's handler and carries no `id`, so
//! its seam events name the stanza `-`.

#![cfg(all(feature = "clustering", feature = "test-support"))]

#[path = "cluster_support/mod.rs"]
pub mod cluster_support;

use std::path::PathBuf;
use std::time::Duration;

use cluster_support::{
    cluster_e2e_serial_lock, fanout_message, free_tcp_port, generate_pool, join_fanout_room,
    open_control_db, reset_and_enroll, reset_node_lease_tables, spawn_cluster_server_with_envs,
    wait_for_node_lease_lapse, wait_for_readiness, EnrolledPool, CLUSTER_PEER_PASSWORD,
    CLUSTER_PEER_USERNAME,
};
use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_server::clustering::claims::PostgresClaimStore;
use waddle_server::db::Database;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType};

const DOMAIN: &str = "localhost";
const SEAM_DIRECTORY_ENV: &str = "WADDLE_TEST_SEAM_REMOTE_ACCEPTANCE_DIRECTORY";
const ACCEPTANCE_ENV: &str = "WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE";
const ORPHAN_REAPER_INTERVAL_ENV: &str = "WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS";
const HOLD_BEFORE_COMMIT: &str = "hold-before-commit";
const HOLD_AFTER_COMMIT: &str = "hold-after-commit";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";

/// Control directory of the owner's seams.
struct OwnerSeams {
    directory: PathBuf,
}

impl OwnerSeams {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "waddle-room-request-seams-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).expect("create seam directory");
        Self { directory }
    }

    fn directory(&self) -> String {
        self.directory.display().to_string()
    }

    fn arm(&self, seam: &str) {
        std::fs::write(self.directory.join(seam), b"").expect("arm seam");
    }

    fn release(&self, seam: &str) {
        std::fs::remove_file(self.directory.join(seam)).expect("release seam");
    }

    fn events(&self) -> Vec<String> {
        std::fs::read_to_string(self.directory.join("events"))
            .map(|events| events.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    async fn wait_for(&self, event: &str, budget: Duration) {
        let waited = tokio::time::timeout(budget, async {
            while !self.events().iter().any(|line| line == event) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "owner seam never recorded {event:?} within {budget:?}; recorded {:?}",
            self.events()
        );
    }
}

impl Drop for OwnerSeams {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Advance the shared database to the mailbox protocol epoch.
async fn activate_mailbox_epoch(db: &Database) {
    let conn = db.guard().await.expect("epoch guard");
    for _ in 0..2 {
        conn.execute(
            "UPDATE ingress_protocol_epoch SET epoch = epoch + 1, activated_at = now(), \
             lineage_uuid = (SELECT lineage_uuid::uuid FROM _lineage WHERE id = 1) \
             WHERE id = 1 AND epoch < 2",
            (),
        )
        .await
        .expect("activate the mailbox epoch");
    }
}

/// The one value `sql` yields for `room`, its only parameter.
///
/// The ingress relations are shared by every scenario of the database and
/// cannot be emptied between them, so each query names the scenario's room.
async fn scalar(db: &Database, room: &jid::BareJid, sql: &str) -> i64 {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(sql, waddle_server::db_params![room.to_string()])
        .await
        .expect("scalar query");
    rows.next()
        .await
        .expect("scalar row")
        .expect("one row")
        .get::<i64>(0)
        .expect("scalar value")
}

/// Wait until `sql` yields `expected` for `room`.
async fn wait_for_scalar(
    db: &Database,
    room: &jid::BareJid,
    sql: &str,
    expected: i64,
    budget: Duration,
) {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let value = scalar(db, room, sql).await;
        if value == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{sql} stayed at {value}, expected {expected}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn join_presence(room: &jid::BareJid, nick: &str) -> Element {
    let occupant = room.with_resource_str(nick).expect("occupant JID");
    Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(xml_ncname!("to").to_owned(), occupant.to_string())
        .append(Element::builder("x", NS_MUC).build())
        .build()
}

fn parse(frames: &[String]) -> Vec<Element> {
    frames
        .iter()
        .filter_map(|frame| frame.parse::<Element>().ok())
        .collect()
}

fn is_presence_from(frame: &Element, occupant: &jid::FullJid) -> bool {
    frame.is("presence", waddle_xmpp::ns::JABBER_CLIENT)
        && frame.attr("from") == Some(occupant.to_string().as_str())
        && frame.attr("type").is_none()
}

fn is_self_presence(frame: &Element, occupant: &jid::FullJid) -> bool {
    is_presence_from(frame, occupant)
        && frame.get_child("x", NS_MUC_USER).is_some_and(|x| {
            x.children()
                .any(|child| child.is("status", NS_MUC_USER) && child.attr("code") == Some("110"))
        })
}

fn is_subject(frame: &Element) -> bool {
    frame.is("message", waddle_xmpp::ns::JABBER_CLIENT)
        && frame
            .get_child("subject", waddle_xmpp::ns::JABBER_CLIENT)
            .is_some()
}

fn is_error(frame: &Element) -> bool {
    frame.attr("type") == Some("error")
}

/// Frames a client receives until `budget` passes without one.
async fn drain(client: &mut WsXmppClient, budget: Duration) -> Vec<Element> {
    let mut frames = Vec::new();
    while let Ok(frame) = client.recv_timeout(budget).await {
        frames.push(frame);
    }
    parse(&frames)
}

/// Two meshed nodes at the mailbox epoch and a room owned by node B, with a
/// witness occupant there.
struct Scenario {
    postgres_url: String,
    db: Database,
    pool: EnrolledPool,
    port_a: u16,
    server_a: TestServer,
    server_b: Option<TestServer>,
    /// Node id of the running owner, or of the one stopped last.
    node_b: String,
    seams: OwnerSeams,
    room: jid::BareJid,
    witness: Option<WsXmppClient>,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Scenario {
    async fn start() -> Self {
        Self::start_with("mailbox", &[]).await
    }

    /// Start with the origin node's remote room acceptance selector and
    /// extra environment for both nodes. The owner always selects the
    /// mailbox.
    async fn start_with(origin_acceptance: &str, extra: &[(&str, &str)]) -> Self {
        let postgres_url = waddle_ws_test_support::require_test_postgres_url()
            .expect("the room request suite needs PostgreSQL: set WADDLE_TEST_POSTGRES_URL");
        let serial = cluster_e2e_serial_lock().lock().await;
        let db = open_control_db(&postgres_url).await;
        let pool = generate_pool();
        reset_and_enroll(&db, &pool).await;
        reset_node_lease_tables(&db).await;
        let seams = OwnerSeams::new();
        let port_a = free_tcp_port();
        let port_b = free_tcp_port();
        let seam_directory = seams.directory();
        // The origin's orphan reaper is slowed so that a replacement owner,
        // not the origin, takes a killed owner's room claim: every scenario
        // here is about an owner that is remote from the requester.
        let mut envs_a = vec![
            (ACCEPTANCE_ENV, origin_acceptance),
            (ORPHAN_REAPER_INTERVAL_ENV, "600000"),
        ];
        envs_a.extend_from_slice(extra);
        let mut envs_b = vec![
            (SEAM_DIRECTORY_ENV, seam_directory.as_str()),
            (ACCEPTANCE_ENV, "mailbox"),
        ];
        envs_b.extend_from_slice(extra);
        let (server_a, _, _) = spawn_cluster_server_with_envs(
            &postgres_url,
            &pool.pool_env,
            port_a,
            &[port_b],
            &envs_a,
        )
        .await;
        let (server_b, node_b, _) = spawn_cluster_server_with_envs(
            &postgres_url,
            &pool.pool_env,
            port_b,
            &[port_a],
            &envs_b,
        )
        .await;
        wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
        wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;
        activate_mailbox_epoch(&db).await;

        let room: jid::BareJid = format!("requests-{}@muc.{DOMAIN}", uuid::Uuid::new_v4())
            .parse()
            .expect("unique room JID");
        // The first join creates the room, so the witness's node owns it.
        let mut witness = WsXmppClient::connect_and_auth(
            &server_b.ws_url(),
            DOMAIN,
            "admin",
            server_b.fixed_account_password(),
            &format!("witness-{}", uuid::Uuid::new_v4()),
        )
        .await
        .expect("witness binds on the owner node");
        join_fanout_room(&mut witness, &room, "witness").await;
        let claim = PostgresClaimStore::new(db.clone())
            .current_claim(&Entity::new(EntityType::RoomActor, room.to_string()))
            .await
            .expect("room claim lookup")
            .expect("live room claim");
        assert_eq!(claim.owner.node_id, node_b, "node B must own the room");
        Self {
            postgres_url,
            db,
            pool,
            port_a,
            server_a,
            server_b: Some(server_b),
            node_b,
            seams,
            room,
            witness: Some(witness),
            _serial: serial,
        }
    }

    /// A client of the cluster peer account bound on the origin node.
    async fn origin_client(&self, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(
            &self.server_a.ws_url(),
            DOMAIN,
            CLUSTER_PEER_USERNAME,
            CLUSTER_PEER_PASSWORD,
            resource,
        )
        .await
        .expect("client binds on the origin node")
    }

    fn occupant(&self, nick: &str) -> jid::FullJid {
        self.room.with_resource_str(nick).expect("occupant JID")
    }

    /// SIGKILL the owner and start its replacement, a new incarnation.
    async fn replace_owner(&mut self) {
        self.stop_owner().await;
        self.start_owner().await;
    }

    /// SIGKILL the owner and wait until its node lease has lapsed, so the
    /// origin node keeps serving when a replacement appears.
    async fn stop_owner(&mut self) {
        drop(self.witness.take());
        drop(self.server_b.take());
        wait_for_node_lease_lapse(&self.db, &self.node_b).await;
    }

    /// Start a new owner incarnation after [`Self::stop_owner`] and wait
    /// until its orphan reaper has taken the room's claim.
    async fn start_owner(&mut self) {
        let port_b = free_tcp_port();
        let (server_b, node_b, _) = spawn_cluster_server_with_envs(
            &self.postgres_url,
            &self.pool.pool_env,
            port_b,
            &[self.port_a],
            &[
                (SEAM_DIRECTORY_ENV, &self.seams.directory()),
                (ACCEPTANCE_ENV, "mailbox"),
            ],
        )
        .await;
        wait_for_readiness(&server_b, true, Duration::from_secs(30)).await;
        let claims = PostgresClaimStore::new(self.db.clone());
        let room = Entity::new(EntityType::RoomActor, self.room.to_string());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let claim = claims
                .current_claim(&room)
                .await
                .expect("room claim lookup");
            if claim.is_some_and(|claim| claim.owner.node_id == node_b) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the replacement owner must take the room's claim"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.server_b = Some(server_b);
        self.node_b = node_b;
    }
}

const REQUESTS: &str = "SELECT count(*) FROM ingress_room_requests WHERE room_jid = ?";
const JOINS: &str =
    "SELECT count(*) FROM ingress_room_requests WHERE room_jid = ? AND request_kind = 1";
const SETTLED_JOINS: &str = "SELECT count(*) FROM ingress_room_requests \
     WHERE room_jid = ? AND request_kind = 1 AND owner_decision = 1 AND settled_at IS NOT NULL";
const CONSUMED_JOIN_ENTRIES: &str = "SELECT count(*) FROM ingress_mailbox_entries e \
     JOIN ingress_room_requests r ON r.request_key = e.request_key \
     WHERE r.room_jid = ? AND e.operation_kind = 2 AND e.disposition = 3";
/// Occupancy rows of resources bound on the origin node. The witness is
/// connected to the owner and has a row of its own.
const OCCUPANTS: &str = "SELECT count(*) FROM ingress_room_occupants \
     WHERE room_jid = ? AND occupant_jid NOT LIKE 'admin@%'";
const WITNESS_OCCUPANTS: &str = "SELECT count(*) FROM ingress_room_occupants \
     WHERE room_jid = ? AND occupant_jid LIKE 'admin@%'";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";

fn occupant_presence(room: &jid::BareJid, nick: &str, kind: Option<&str>, status: &str) -> Element {
    let mut presence = Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(xml_ncname!("to").to_owned(), format!("{room}/{nick}"));
    if let Some(kind) = kind {
        presence = presence.attr(xml_ncname!("type").to_owned(), kind);
    }
    presence
        .append(
            Element::builder("status", waddle_xmpp::ns::JABBER_CLIENT)
                .append(status)
                .build(),
        )
        .build()
}

fn kick_iq(room: &jid::BareJid, nick: &str, id: &str) -> Element {
    Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(xml_ncname!("type").to_owned(), "set")
        .attr(xml_ncname!("id").to_owned(), id)
        .attr(xml_ncname!("to").to_owned(), room.to_string())
        .append(
            Element::builder("query", NS_MUC_ADMIN)
                .append(
                    Element::builder("item", NS_MUC_ADMIN)
                        .attr(xml_ncname!("nick").to_owned(), nick)
                        .attr(xml_ncname!("role").to_owned(), "none")
                        .build(),
                )
                .build(),
        )
        .build()
}

fn muji_iq(room: &jid::BareJid, action: &str, sid: &str) -> Element {
    use waddle_xmpp::xep::xep0166::NS_JINGLE;
    use waddle_xmpp::xep::xep0167::NS_JINGLE_RTP;
    use waddle_xmpp::xep::xep0272::NS_MUJI;
    use waddle_xmpp::xep::xep_waddle_livekit_transport::NS_WADDLE_LIVEKIT_TRANSPORT;
    let mut jingle = Element::builder("jingle", NS_JINGLE)
        .attr(xml_ncname!("action").to_owned(), action)
        .attr(xml_ncname!("sid").to_owned(), sid);
    if action == "session-initiate" {
        let payload_type = Element::builder("payload-type", NS_JINGLE_RTP)
            .attr(xml_ncname!("id").to_owned(), "111")
            .attr(xml_ncname!("name").to_owned(), "opus")
            .attr(xml_ncname!("clockrate").to_owned(), "48000")
            .attr(xml_ncname!("channels").to_owned(), "2")
            .build();
        let description = Element::builder("description", NS_JINGLE_RTP)
            .attr(xml_ncname!("media").to_owned(), "audio")
            .append(payload_type)
            .build();
        jingle = jingle.append(
            Element::builder("content", NS_JINGLE)
                .attr(xml_ncname!("creator").to_owned(), "initiator")
                .attr(xml_ncname!("name").to_owned(), "audio")
                .append(description)
                .append(Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT).build())
                .build(),
        );
    }
    let jingle = jingle
        .append(
            Element::builder("muji", NS_MUJI)
                .attr(xml_ncname!("room").to_owned(), room.to_string())
                .build(),
        )
        .build();
    Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(xml_ncname!("type").to_owned(), "set")
        .attr(xml_ncname!("id").to_owned(), format!("{action}-{sid}"))
        .attr(xml_ncname!("to").to_owned(), "calls.localhost")
        .append(jingle)
        .build()
}

/// XEP-0045 §7.2: the joining resource receives the other occupants'
/// presence, then its self-presence with status 110, then the subject. The
/// response is frozen by the owner in that order, handed over once, and the
/// join seats the requester in durable occupancy.
#[tokio::test]
async fn join_response_is_handed_over_once_in_section_7_2_order() {
    let scenario = Scenario::start().await;
    let mut joiner = scenario.origin_client("joiner").await;
    joiner
        .send(&String::from(&join_presence(&scenario.room, "joiner")))
        .await
        .expect("send join");
    let frames = tokio::time::timeout(
        Duration::from_secs(20),
        joiner.recv_until(|frame| frame.contains("<subject")),
    )
    .await
    .expect("bounded join")
    .expect("join replies");
    let frames = parse(&frames);
    let position = |matches: &dyn Fn(&Element) -> bool| frames.iter().position(matches);
    let witness = position(&|frame| is_presence_from(frame, &scenario.occupant("witness")))
        .expect("the other occupant's presence");
    let own = position(&|frame| is_self_presence(frame, &scenario.occupant("joiner")))
        .expect("self-presence with status 110");
    let subject = position(&is_subject).expect("room subject");
    assert!(
        witness < own && own < subject,
        "§7.2 order is occupants, self-presence, subject: {frames:?}"
    );

    // No second copy of the response follows.
    let later = drain(&mut joiner, Duration::from_secs(2)).await;
    assert!(
        !later
            .iter()
            .any(|frame| is_self_presence(frame, &scenario.occupant("joiner"))),
        "the response is handed over once: {later:?}"
    );
    assert_eq!(scalar(&scenario.db, &scenario.room, REQUESTS).await, 1);
    assert_eq!(scalar(&scenario.db, &scenario.room, SETTLED_JOINS).await, 1);
    assert_eq!(
        scalar(&scenario.db, &scenario.room, CONSUMED_JOIN_ENTRIES).await,
        1
    );
    assert_eq!(
        scalar(
            &scenario.db,
            &scenario.room,
            "SELECT count(*) FROM ingress_room_occupants WHERE room_jid = ? AND nick = 'joiner' \
             AND join_request IS NOT NULL AND presence IS NOT NULL"
        )
        .await,
        1
    );
    // The stored response is contiguous from position zero.
    assert_eq!(
        scalar(
            &scenario.db,
            &scenario.room,
            "SELECT (max(s.position) + 1 - count(*))::bigint FROM ingress_room_responses s \
             JOIN ingress_room_requests r ON r.request_key = s.request_key WHERE r.room_jid = ?"
        )
        .await,
        0
    );
}

/// The owner refuses a groupchat stanza from a sender it has not seated and
/// accepts it once that sender's join was decided.
#[tokio::test]
async fn groupchat_is_refused_before_the_senders_join_and_accepted_after() {
    let scenario = Scenario::start().await;
    let mut sender = scenario.origin_client("sender").await;
    sender
        .send(&String::from(&fanout_message(
            &scenario.room,
            "before-join",
            Some("before the join"),
        )))
        .await
        .expect("send before join");
    let refused = drain(&mut sender, Duration::from_secs(10)).await;
    assert!(
        refused
            .iter()
            .any(|frame| frame.attr("id") == Some("before-join") && is_error(frame)),
        "a sender without occupancy is refused: {refused:?}"
    );
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 0);

    join_fanout_room(&mut sender, &scenario.room, "sender").await;
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 1);
    sender
        .send(&String::from(&fanout_message(
            &scenario.room,
            "after-join",
            Some("after the join"),
        )))
        .await
        .expect("send after join");
    let reflected = tokio::time::timeout(
        Duration::from_secs(20),
        sender.recv_until(|frame| frame.contains("after the join")),
    )
    .await
    .expect("bounded reflection")
    .expect("reflection");
    assert!(
        !parse(&reflected)
            .iter()
            .any(|frame| frame.attr("id") == Some("after-join") && is_error(frame)),
        "a seated sender is accepted: {reflected:?}"
    );
}

/// A join committed while the owner is stopped is not lost with its hint:
/// the entry stays pending, the replacement's poller adopts the lane after
/// loading the room's occupancy, decides the join once and the requester
/// receives one response.
#[tokio::test]
async fn join_committed_while_the_owner_is_down_is_decided_by_its_replacement() {
    let mut scenario = Scenario::start().await;
    scenario.seams.arm(HOLD_BEFORE_COMMIT);
    let mut joiner = scenario.origin_client("joiner").await;
    joiner
        .send(&String::from(&join_presence(&scenario.room, "joiner")))
        .await
        .expect("send join");
    scenario
        .seams
        .wait_for("held before-commit -", Duration::from_secs(10))
        .await;
    assert_eq!(
        scalar(
            &scenario.db,
            &scenario.room,
            "SELECT count(*) FROM ingress_room_requests WHERE room_jid = ? AND owner_decision = 0"
        )
        .await,
        1,
        "the owner stopped before its decision"
    );
    scenario.replace_owner().await;
    scenario.seams.release(HOLD_BEFORE_COMMIT);

    let frames = tokio::time::timeout(
        Duration::from_secs(60),
        joiner.recv_until(|frame| frame.contains("<subject")),
    )
    .await
    .expect("the replacement owner decides the pending join")
    .expect("join replies");
    let own = scenario.occupant("joiner");
    assert_eq!(
        parse(&frames)
            .iter()
            .filter(|frame| is_self_presence(frame, &own))
            .count(),
        1
    );
    assert_eq!(
        scalar(&scenario.db, &scenario.room, REQUESTS).await,
        1,
        "decided once"
    );
    assert_eq!(scalar(&scenario.db, &scenario.room, SETTLED_JOINS).await, 1);
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 1);
}

/// An owner killed after its decision committed has already frozen the
/// response. The origin reads it from the database without the owner, hands
/// it over once, and the replacement decides nothing again.
#[tokio::test]
async fn owner_killed_after_commit_hands_the_frozen_response_over_once() {
    let mut scenario = Scenario::start().await;
    scenario.seams.arm(HOLD_AFTER_COMMIT);
    let mut joiner = scenario.origin_client("joiner").await;
    joiner
        .send(&String::from(&join_presence(&scenario.room, "joiner")))
        .await
        .expect("send join");
    scenario
        .seams
        .wait_for("held after-commit -", Duration::from_secs(10))
        .await;
    scenario.stop_owner().await;
    scenario.seams.release(HOLD_AFTER_COMMIT);

    let frames = tokio::time::timeout(
        Duration::from_secs(20),
        joiner.recv_until(|frame| frame.contains("<subject")),
    )
    .await
    .expect("the origin reads the frozen response without the owner")
    .expect("join replies");
    let own = scenario.occupant("joiner");
    assert_eq!(
        parse(&frames)
            .iter()
            .filter(|frame| is_self_presence(frame, &own))
            .count(),
        1
    );
    wait_for_scalar(
        &scenario.db,
        &scenario.room,
        SETTLED_JOINS,
        1,
        Duration::from_secs(10),
    )
    .await;

    scenario.start_owner().await;
    let later = drain(&mut joiner, Duration::from_secs(5)).await;
    assert!(
        !later.iter().any(|frame| is_self_presence(frame, &own)),
        "the replacement owner does not answer a decided join again: {later:?}"
    );
    assert_eq!(scalar(&scenario.db, &scenario.room, REQUESTS).await, 1);
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 1);
}

/// After the room's claim moves to a new owner incarnation, an occupant that
/// joined under the predecessor is recovered from its durable row: its
/// groupchat stanza is accepted without a rejoin. A sender with no row is
/// still refused, so nothing is inferred for occupants the table does not
/// hold.
#[tokio::test]
async fn owner_transfer_recovers_occupancy_and_refuses_unrecorded_senders() {
    let mut scenario = Scenario::start().await;
    let mut seated = scenario.origin_client("seated").await;
    join_fanout_room(&mut seated, &scenario.room, "seated").await;
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 1);
    scenario.replace_owner().await;

    seated
        .send(&String::from(&fanout_message(
            &scenario.room,
            "after-transfer",
            Some("after the transfer"),
        )))
        .await
        .expect("send after transfer");
    let reflected = tokio::time::timeout(
        Duration::from_secs(60),
        seated.recv_until(|frame| frame.contains("after the transfer")),
    )
    .await
    .expect("the successor accepts the recovered occupant's stanza")
    .expect("reflection");
    assert!(
        !parse(&reflected)
            .iter()
            .any(|frame| frame.attr("id") == Some("after-transfer") && is_error(frame)),
        "a recovered occupant is not asked to rejoin: {reflected:?}"
    );

    let mut stranger = scenario.origin_client("stranger").await;
    stranger
        .send(&String::from(&fanout_message(
            &scenario.room,
            "never-joined",
            Some("never joined"),
        )))
        .await
        .expect("send without occupancy");
    let refused = drain(&mut stranger, Duration::from_secs(10)).await;
    assert!(
        refused
            .iter()
            .any(|frame| frame.attr("id") == Some("never-joined") && is_error(frame)),
        "the successor seats only recorded occupants: {refused:?}"
    );
}

/// A decision reached for a connection that has ended is not handed to a
/// later connection of the same full JID: the later connection receives
/// exactly the response to its own join.
#[tokio::test]
async fn decision_for_an_ended_connection_is_not_handed_to_its_successor() {
    let scenario = Scenario::start().await;
    scenario.seams.arm(HOLD_BEFORE_COMMIT);
    let mut first = scenario.origin_client("same-resource").await;
    first
        .send(&String::from(&join_presence(&scenario.room, "joiner")))
        .await
        .expect("send first join");
    scenario
        .seams
        .wait_for("held before-commit -", Duration::from_secs(10))
        .await;
    drop(first);
    scenario.seams.release(HOLD_BEFORE_COMMIT);
    // The ended connection's leave is a request of its own, so the first
    // join is named by its kind.
    wait_for_scalar(
        &scenario.db,
        &scenario.room,
        "SELECT count(*) FROM ingress_room_requests \
         WHERE room_jid = ? AND request_kind = 1 AND owner_decision <> 0",
        1,
        Duration::from_secs(30),
    )
    .await;

    let mut second = scenario.origin_client("same-resource").await;
    second
        .send(&String::from(&join_presence(&scenario.room, "joiner")))
        .await
        .expect("send second join");
    let mut frames = parse(
        &tokio::time::timeout(
            Duration::from_secs(20),
            second.recv_until(|frame| frame.contains("<subject")),
        )
        .await
        .expect("bounded second join")
        .expect("second join replies"),
    );
    frames.extend(drain(&mut second, Duration::from_secs(2)).await);
    let own = scenario.occupant("joiner");
    assert_eq!(
        frames
            .iter()
            .filter(|frame| is_self_presence(frame, &own))
            .count(),
        1,
        "only the second connection's own response is delivered: {frames:?}"
    );
    assert_eq!(scalar(&scenario.db, &scenario.room, JOINS).await, 2);
    // The occupancy row belongs to the later connection's join.
    assert_eq!(
        scalar(
            &scenario.db,
            &scenario.room,
            "WITH requests AS (SELECT * FROM ingress_room_requests WHERE room_jid = ?) \
             SELECT count(*) FROM ingress_room_occupants o JOIN requests r \
             ON r.request_key = o.join_request \
             WHERE r.created_at = (SELECT max(created_at) FROM requests)"
        )
        .await,
        1
    );
}

/// A resource connected to the room's owner has an occupancy row of its
/// own, so the owner's successor seats it and a later joiner is told about
/// it. The row names the connection generation that joined.
#[tokio::test]
async fn occupant_connected_to_the_owner_is_recovered_by_its_successor() {
    let mut scenario = Scenario::start().await;
    assert_eq!(
        scalar(&scenario.db, &scenario.room, WITNESS_OCCUPANTS).await,
        1
    );
    assert_eq!(
        scalar(
            &scenario.db,
            &scenario.room,
            "SELECT count(*) FROM ingress_room_occupants o JOIN xmpp_occupancy_authority a \
             ON a.full_jid = o.occupant_jid AND a.generation::text = o.occupancy_generation \
             WHERE o.room_jid = ? AND o.occupant_jid LIKE 'admin@%'"
        )
        .await,
        1,
        "the row carries the generation the occupancy authority holds for the resource"
    );
    scenario.replace_owner().await;

    let mut joiner = scenario.origin_client("after-transfer").await;
    joiner
        .send(&String::from(&join_presence(&scenario.room, "joiner")))
        .await
        .expect("send join");
    let frames = parse(
        &tokio::time::timeout(
            Duration::from_secs(60),
            joiner.recv_until(|frame| frame.contains("<subject")),
        )
        .await
        .expect("the successor decides the join")
        .expect("join replies"),
    );
    assert!(
        frames
            .iter()
            .any(|frame| is_presence_from(frame, &scenario.occupant("witness"))),
        "the successor seats the occupant that was connected to its predecessor: {frames:?}"
    );
}

/// An occupant the owner kicked loses its occupancy row with the kick, so
/// the owner's successor does not seat it again.
#[tokio::test]
async fn kicked_occupant_is_not_seated_by_a_successor() {
    let mut scenario = Scenario::start().await;
    let mut kicked = scenario.origin_client("kicked").await;
    join_fanout_room(&mut kicked, &scenario.room, "kicked").await;
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 1);

    let witness = scenario.witness.as_mut().expect("witness");
    witness
        .send(&String::from(&kick_iq(&scenario.room, "kicked", "kick-1")))
        .await
        .expect("send kick");
    wait_for_scalar(
        &scenario.db,
        &scenario.room,
        OCCUPANTS,
        0,
        Duration::from_secs(20),
    )
    .await;
    let _ = drain(&mut kicked, Duration::from_secs(2)).await;
    scenario.replace_owner().await;

    kicked
        .send(&String::from(&fanout_message(
            &scenario.room,
            "after-kick",
            Some("after the kick"),
        )))
        .await
        .expect("send after kick");
    let refused = drain(&mut kicked, Duration::from_secs(30)).await;
    assert!(
        refused
            .iter()
            .any(|frame| frame.attr("id") == Some("after-kick") && is_error(frame)),
        "a kicked occupant is not recovered: {refused:?}"
    );
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 0);
}

/// An owner that carries room requests through the mailbox refuses a join
/// that still arrives on the ordered relay: nothing is seated and no
/// request exists.
#[tokio::test]
async fn relayed_join_is_refused_by_an_owner_at_the_mailbox_epoch() {
    let scenario = Scenario::start_with("relay", &[]).await;
    let mut joiner = scenario.origin_client("relayed").await;
    joiner
        .send(&String::from(&join_presence(&scenario.room, "relayed")))
        .await
        .expect("send join");
    let frames = drain(&mut joiner, Duration::from_secs(10)).await;
    assert!(
        !frames
            .iter()
            .any(|frame| is_self_presence(frame, &scenario.occupant("relayed"))),
        "a relayed join is not accepted at the mailbox epoch: {frames:?}"
    );
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 0);
    assert_eq!(scalar(&scenario.db, &scenario.room, REQUESTS).await, 0);
}

/// A presence update and a leave of a resource on the origin node reach the
/// owner as room requests: the other occupants see each, the occupancy row
/// follows, and the leaving resource receives its own unavailable presence.
#[tokio::test]
async fn presence_update_and_leave_cross_nodes_as_room_requests() {
    let mut scenario = Scenario::start().await;
    let mut occupant = scenario.origin_client("updates").await;
    join_fanout_room(&mut occupant, &scenario.room, "updates").await;
    let own = scenario.occupant("updates");
    let witness = scenario.witness.as_mut().expect("witness");
    let _ = drain(witness, Duration::from_secs(1)).await;

    occupant
        .send(&String::from(&occupant_presence(
            &scenario.room,
            "updates",
            None,
            "stepped out",
        )))
        .await
        .expect("send presence update");
    let seen = tokio::time::timeout(
        Duration::from_secs(20),
        witness.recv_until(|frame| frame.contains("stepped out")),
    )
    .await
    .expect("the owner fans the update out")
    .expect("update frames");
    assert!(parse(&seen)
        .iter()
        .any(|frame| is_presence_from(frame, &own)));
    wait_for_scalar(
        &scenario.db,
        &scenario.room,
        "SELECT count(*) FROM ingress_room_requests \
         WHERE room_jid = ? AND request_kind = 2 AND owner_decision = 1 AND settled_at IS NOT NULL",
        1,
        Duration::from_secs(20),
    )
    .await;
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 1);

    occupant
        .send(&String::from(&occupant_presence(
            &scenario.room,
            "updates",
            Some("unavailable"),
            "gone",
        )))
        .await
        .expect("send leave");
    let left = parse(
        &tokio::time::timeout(
            Duration::from_secs(20),
            occupant.recv_until(|frame| frame.contains("unavailable")),
        )
        .await
        .expect("the leave is answered")
        .expect("leave frames"),
    );
    assert!(
        left.iter()
            .any(|frame| frame.attr("type") == Some("unavailable")
                && frame.attr("from") == Some(own.to_string().as_str())),
        "the leaving resource receives its own unavailable presence: {left:?}"
    );
    wait_for_scalar(
        &scenario.db,
        &scenario.room,
        OCCUPANTS,
        0,
        Duration::from_secs(20),
    )
    .await;
    wait_for_scalar(
        &scenario.db,
        &scenario.room,
        "SELECT count(*) FROM ingress_room_requests \
         WHERE room_jid = ? AND request_kind = 3 AND owner_decision = 1 AND settled_at IS NOT NULL",
        1,
        Duration::from_secs(20),
    )
    .await;
    let departed = tokio::time::timeout(
        Duration::from_secs(20),
        witness.recv_until(|frame| frame.contains("unavailable")),
    )
    .await
    .expect("the owner fans the leave out")
    .expect("leave fan-out");
    assert!(parse(&departed)
        .iter()
        .any(|frame| is_presence_from(frame, &own)
            || frame.attr("from") == Some(own.to_string().as_str())));
}

/// A Muji `session-initiate` and `session-terminate` of a resource on the
/// origin node are decided by the room owner as requests on the room's Muji
/// lane, and the requester receives the IQ result and the `session-accept`.
#[tokio::test]
async fn muji_exchange_crosses_nodes_on_the_muji_lane() {
    let livekit = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&livekit)
        .await;
    let livekit_ws_url = livekit.uri().replacen("http://", "ws://", 1);
    let scenario = Scenario::start_with(
        "mailbox",
        &[
            ("LIVEKIT_API_KEY", "APItestkeyrequests"),
            (
                "LIVEKIT_API_SECRET",
                "test-secret-with-at-least-32-bytes-of-payload",
            ),
            ("LIVEKIT_WS_URL", &livekit_ws_url),
            ("LIVEKIT_TURN_HOST", "turn.example.test"),
            (
                "LIVEKIT_TURN_SHARED_SECRET",
                "turn-shared-secret-value-also-long-enough",
            ),
            (
                "LIVEKIT_WEBHOOK_SECRET",
                "test-webhook-secret-with-at-least-32-bytes",
            ),
        ],
    )
    .await;
    let mut caller = scenario.origin_client("caller").await;
    join_fanout_room(&mut caller, &scenario.room, "caller").await;

    caller
        .send(&String::from(&muji_iq(
            &scenario.room,
            "session-initiate",
            "call-1",
        )))
        .await
        .expect("send session-initiate");
    let accepted = tokio::time::timeout(
        Duration::from_secs(30),
        caller.recv_until(|frame| frame.contains("session-accept")),
    )
    .await
    .expect("the owner answers the initiate")
    .expect("initiate replies");
    assert!(
        !parse(&accepted).iter().any(is_error),
        "a seated occupant's initiate is accepted: {accepted:?}"
    );
    caller
        .send(&String::from(&muji_iq(
            &scenario.room,
            "session-terminate",
            "call-1",
        )))
        .await
        .expect("send session-terminate");
    let terminated = tokio::time::timeout(
        Duration::from_secs(30),
        caller.recv_until(|frame| frame.contains("session-terminate-call-1")),
    )
    .await
    .expect("the owner answers the terminate")
    .expect("terminate replies");
    assert!(
        !parse(&terminated).iter().any(is_error),
        "a hangup does not fail: {terminated:?}"
    );
    wait_for_scalar(
        &scenario.db,
        &scenario.room,
        "SELECT count(*) FROM ingress_room_requests \
         WHERE room_jid = ? AND request_kind = 4 AND owner_decision = 1 AND settled_at IS NOT NULL",
        2,
        Duration::from_secs(20),
    )
    .await;
    // Both requests sit on one lane, and it is not the join's lane.
    assert_eq!(
        scalar(
            &scenario.db,
            &scenario.room,
            "SELECT count(DISTINCT e.lane_id) FROM ingress_mailbox_entries e \
             JOIN ingress_room_requests r ON r.request_key = e.request_key \
             WHERE r.room_jid = ? AND e.operation_kind = 5"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &scenario.db,
            &scenario.room,
            "WITH entries AS (SELECT e.lane_id, e.operation_kind FROM ingress_mailbox_entries e \
             JOIN ingress_room_requests r ON r.request_key = e.request_key WHERE r.room_jid = ?) \
             SELECT count(*) FROM entries m WHERE m.operation_kind = 5 \
             AND m.lane_id IN (SELECT lane_id FROM entries WHERE operation_kind = 2)"
        )
        .await,
        0
    );
}

/// A row whose connection generation the occupancy authority no longer
/// holds for the full JID is not seated by the owner's successor, and the
/// successor removes it from the table when it seats the room.
///
/// The connection ends while no owner is running: with an owner, the leave
/// its origin commits at connection end would remove the row first.
#[tokio::test]
async fn row_of_a_superseded_connection_is_not_seated_and_is_removed() {
    let mut scenario = Scenario::start().await;
    let mut first = scenario.origin_client("rebound").await;
    join_fanout_room(&mut first, &scenario.room, "rebound").await;
    assert_eq!(scalar(&scenario.db, &scenario.room, OCCUPANTS).await, 1);
    scenario.stop_owner().await;
    // A later connection binds the same resource and does not join. The
    // row keeps the earlier connection's generation.
    drop(first);
    let mut second = scenario.origin_client("rebound").await;
    wait_for_scalar(
        &scenario.db,
        &scenario.room,
        "SELECT count(*) FROM ingress_room_occupants o JOIN xmpp_occupancy_authority a \
         ON a.full_jid = o.occupant_jid \
         WHERE o.room_jid = ? AND o.occupant_jid NOT LIKE 'admin@%' \
         AND a.generation <> o.occupancy_generation",
        1,
        Duration::from_secs(20),
    )
    .await;
    scenario.start_owner().await;

    second
        .send(&String::from(&fanout_message(
            &scenario.room,
            "not-rejoined",
            Some("without a join"),
        )))
        .await
        .expect("send without a join");
    let refused = drain(&mut second, Duration::from_secs(30)).await;
    assert!(
        refused
            .iter()
            .any(|frame| frame.attr("id") == Some("not-rejoined") && is_error(frame)),
        "the later connection is not an occupant: {refused:?}"
    );
    wait_for_scalar(
        &scenario.db,
        &scenario.room,
        OCCUPANTS,
        0,
        Duration::from_secs(20),
    )
    .await;
}
