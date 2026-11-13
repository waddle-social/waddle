//! XEP-0045 / XEP-0198: when a remotely owned room accepts a groupchat
//! message relative to the sender's handled count (#1948).
//!
//! Two real `waddle-server` processes share one PostgreSQL control plane.
//! The sender binds on node A with stream management enabled; the room is
//! owned by node B. A groupchat message from the sender is committed on A,
//! handed to B (through the mailbox entry at protocol epoch 2, through the
//! ordered relay below it), committed again by B as the room owner, and
//! answered with the sender's reflection. The suite stops B at fixed points of that
//! exchange through the file-armed seams in `clustering/acceptance_seams.rs`
//! and observes the sender's XEP-0198 `h`, the room archive and the
//! reflections occupants receive.
//!
//! The contract under test: `h` covers a groupchat stanza only once the room
//! owner has durably accepted it, and a lost reply or a restart recovers that
//! one acceptance instead of producing a second. Remote room acceptance runs
//! through the durable mailbox from protocol epoch 2 on, so the scenarios
//! that test the handled count activate that epoch on the shared database.
//! The scenarios that only test recovery of one acceptance after a lost
//! reply or an origin restart stay below that epoch, where the origin hands
//! the stanza to the owner through the ordered relay. The epoch only ever
//! advances and the shared database reaches epoch 2 as soon as any scenario
//! or suite activates it, so each of those scenarios runs its nodes on a
//! schema of its own, created empty and therefore at epoch 0.
//!
//! The binary needs PostgreSQL and fails without `WADDLE_TEST_POSTGRES_URL`:
//! every scenario is about durable state shared by two processes.

#![cfg(all(feature = "clustering", feature = "test-support"))]

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
use waddle_ws_test_support::{raw_ws, TestServer, WsXmppClient};
use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType};
use waddle_xmpp::stream_management::SM_NS;

const DOMAIN: &str = "localhost";
const SEAM_DIRECTORY_ENV: &str = "WADDLE_TEST_SEAM_REMOTE_ACCEPTANCE_DIRECTORY";
/// How the nodes of a scenario carry remote room acceptance, decided by the
/// protocol epoch of the shared database.
#[derive(Clone, Copy)]
enum Acceptance {
    /// Below the mailbox epoch: the origin relays the stanza and counts it
    /// when its own commit succeeds.
    Relay,
    /// The server's default at the mailbox epoch: the origin counts the
    /// stanza only after reading the owner's decision from the durable
    /// mailbox.
    Mailbox,
}

/// Advance the shared database's ingress protocol epoch to 2, where remote
/// room acceptance uses the mailbox. Each statement is one step of the
/// epoch guard; a database already there is left alone. Running nodes read
/// the epoch at the start of every ingress transaction.
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

/// The shared database's ingress protocol epoch.
async fn protocol_epoch(db: &Database) -> i64 {
    let conn = db.guard().await.expect("epoch guard");
    let mut rows = conn
        .query("SELECT epoch FROM ingress_protocol_epoch WHERE id = 1", ())
        .await
        .expect("read the protocol epoch");
    rows.next()
        .await
        .expect("protocol epoch row")
        .expect("one protocol epoch row")
        .get::<i64>(0)
        .expect("protocol epoch")
}

/// Prefix of the schemas the relay scenarios run on.
const RELAY_SCHEMA_PREFIX: &str = "waddle_test_relay_acceptance_";

/// A URL of the database at `postgres_url` whose connections use a new,
/// empty schema, so the nodes started on it build their schema at protocol
/// epoch 0 whatever epoch the shared schema has reached. Schemas left by
/// earlier relay scenarios are dropped first; the suite's scenarios run one
/// at a time.
async fn relay_epoch_url(postgres_url: &str) -> String {
    let admin = sqlx::PgPool::connect(postgres_url)
        .await
        .expect("connect PostgreSQL admin pool");
    let stale: Vec<String> = sqlx::query_scalar(
        "SELECT nspname::text FROM pg_catalog.pg_namespace WHERE starts_with(nspname, $1)",
    )
    .bind(RELAY_SCHEMA_PREFIX)
    .fetch_all(&admin)
    .await
    .expect("list relay scenario schemas");
    for schema in stale {
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop an earlier relay scenario schema");
    }
    let schema = format!("{RELAY_SCHEMA_PREFIX}{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create the relay scenario schema");
    admin.close().await;
    let mut url = url::Url::parse(postgres_url).expect("parse PostgreSQL URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

const HOLD_BEFORE_COMMIT: &str = "hold-before-commit";
const HOLD_AFTER_COMMIT: &str = "hold-after-commit";
const DROP_REPLY: &str = "drop-reply";

/// Control directory of the owner's acceptance seams.
struct OwnerSeams {
    directory: PathBuf,
}

impl OwnerSeams {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "waddle-remote-acceptance-seams-{}",
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

    /// Wait until the owner process has recorded `event`.
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

/// A bound client that keeps XEP-0198 counters for its own stream.
struct SmClient {
    socket: WsXmppClient,
    /// Stanzas received since `<enabled/>`, the `h` this client reports.
    inbound: u32,
    stream_id: Option<String>,
    /// Why the socket stopped yielding frames, once it has.
    failure: Option<String>,
}

fn is_stanza(element: &Element) -> bool {
    ["message", "presence", "iq"]
        .iter()
        .any(|name| element.is(name, waddle_xmpp::ns::JABBER_CLIENT))
}

impl SmClient {
    fn new(socket: WsXmppClient) -> Self {
        Self {
            socket,
            inbound: 0,
            stream_id: None,
            failure: None,
        }
    }

    async fn send(&mut self, element: &Element) {
        self.socket
            .send(&String::from(element))
            .await
            .expect("client sends");
    }

    /// Next frame within `budget`, or `None` on timeout. Server `<r/>`
    /// requests are answered and not returned.
    async fn next(&mut self, budget: Duration) -> Option<Element> {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let frame = match self.socket.recv_raw_timeout(remaining).await {
                Ok(Some(raw_ws::Message::Text(frame))) => frame.to_string(),
                // The server's RFC 7395 keepalive pings an idle socket. The
                // socket answers it; it is not a frame of the stream.
                Ok(Some(raw_ws::Message::Ping(_) | raw_ws::Message::Pong(_))) => continue,
                Ok(Some(other)) => {
                    self.failure = Some(format!("unexpected message type: {other:?}"));
                    return None;
                }
                Ok(None) => {
                    self.failure = Some("WebSocket stream ended".to_owned());
                    return None;
                }
                Err(error) if error.starts_with("Timeout waiting") => return None,
                Err(error) => {
                    self.failure = Some(error);
                    return None;
                }
            };
            let element: Element = frame.parse().expect("server frame is one XML element");
            if element.is("r", SM_NS) {
                let answer = Element::builder("a", SM_NS)
                    .attr(xml_ncname!("h").to_owned(), self.inbound.to_string())
                    .build();
                self.send(&answer).await;
                continue;
            }
            if self.stream_id.is_some() && is_stanza(&element) {
                self.inbound += 1;
            }
            return Some(element);
        }
    }

    /// Every frame that arrives within `budget`.
    async fn drain(&mut self, budget: Duration) -> Vec<Element> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut frames = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return frames;
            }
            match self.next(remaining).await {
                Some(frame) => frames.push(frame),
                None => return frames,
            }
        }
    }

    /// Frames up to and including the first that satisfies `wanted`.
    async fn until(
        &mut self,
        budget: Duration,
        what: &str,
        wanted: impl Fn(&Element) -> bool,
    ) -> Vec<Element> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut frames = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Some(frame) = self.next(remaining).await else {
                panic!(
                    "no {what} within {budget:?} (socket failure: {:?}); received {frames:?}",
                    self.failure
                );
            };
            let done = wanted(&frame);
            frames.push(frame);
            if done {
                return frames;
            }
        }
    }

    async fn enable_resumable_sm(&mut self) {
        let enable = Element::builder("enable", SM_NS)
            .attr(xml_ncname!("resume").to_owned(), "true")
            .build();
        self.send(&enable).await;
        let frames = self
            .until(Duration::from_secs(10), "<enabled/>", |frame| {
                frame.is("enabled", SM_NS)
            })
            .await;
        let enabled = frames.last().expect("enabled frame");
        self.stream_id = Some(
            enabled
                .attr("id")
                .expect("resumable stream identifier")
                .to_owned(),
        );
    }

    /// Ask the server for its handled count. Frames that arrive before the
    /// answer are returned with it; `None` when no answer arrives in `budget`.
    async fn request_h(&mut self, budget: Duration) -> (Option<u32>, Vec<Element>) {
        self.send(&Element::builder("r", SM_NS).build()).await;
        let deadline = tokio::time::Instant::now() + budget;
        let mut earlier = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.next(remaining).await {
                Some(frame) if frame.is("a", SM_NS) => {
                    let h = frame
                        .attr("h")
                        .and_then(|h| h.parse().ok())
                        .expect("typed handled count");
                    return (Some(h), earlier);
                }
                Some(frame) => earlier.push(frame),
                None => return (None, earlier),
            }
        }
    }
}

/// A groupchat message carrying an XEP-0359 origin-id, so a retransmission is
/// recognisable as the same submission.
fn groupchat(room: &jid::BareJid, id: &str, body: &str, origin: uuid::Uuid) -> Element {
    let mut message = fanout_message(room, id, Some(body));
    message.append_child(
        Element::builder("origin-id", waddle_xmpp::xep::NS_SID)
            .attr(xml_ncname!("id").to_owned(), origin.to_string())
            .build(),
    );
    message
}

fn is_groupchat(frame: &Element, id: &str) -> bool {
    frame.is("message", waddle_xmpp::ns::JABBER_CLIENT)
        && frame.attr("id") == Some(id)
        && frame.attr("type") == Some("groupchat")
}

fn is_error(frame: &Element, id: &str) -> bool {
    frame.is("message", waddle_xmpp::ns::JABBER_CLIENT)
        && frame.attr("id") == Some(id)
        && frame.attr("type") == Some("error")
}

/// Room-assigned XEP-0359 stanza ids on a reflection.
fn room_stanza_ids(frame: &Element, room: &jid::BareJid) -> Vec<String> {
    let room = room.to_string();
    frame
        .children()
        .filter(|child| {
            child.is("stanza-id", waddle_xmpp::xep::NS_SID)
                && child.attr("by") == Some(room.as_str())
        })
        .filter_map(|child| child.attr("id").map(str::to_owned))
        .collect()
}

/// Archive ids of the room's messages with this body.
async fn archived_ids(db: &Database, room: &jid::BareJid, body: &str) -> Vec<String> {
    let conn = db.guard().await.expect("archive guard");
    let mut rows = conn
        .query(
            "SELECT id FROM mam_messages WHERE room_jid = ? AND body = ? ORDER BY archive_seq",
            waddle_server::db_params![room.to_string(), body.to_owned()],
        )
        .await
        .expect("archive query");
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.expect("archive row") {
        ids.push(row.get::<String>(0).expect("archive id"));
    }
    ids
}

/// Two meshed nodes, a room owned by node B with a witness occupant there,
/// and a sender on node A with resumable stream management and `h == 0`.
struct Scenario {
    postgres_url: String,
    db: Database,
    pool: EnrolledPool,
    port_a: u16,
    port_b: u16,
    server_a: Option<TestServer>,
    server_b: Option<TestServer>,
    /// Node ids of the two nodes the scenario started with.
    node_a: String,
    node_b: String,
    seams: OwnerSeams,
    room: jid::BareJid,
    sender: SmClient,
    sender_occupant: jid::FullJid,
    witness: SmClient,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Scenario {
    async fn start() -> Self {
        Self::start_with(Acceptance::Relay).await
    }

    async fn start_with(acceptance: Acceptance) -> Self {
        let shared_url = waddle_ws_test_support::require_test_postgres_url()
            .expect("the remote acceptance suite needs PostgreSQL: set WADDLE_TEST_POSTGRES_URL");
        let serial = cluster_e2e_serial_lock().lock().await;
        let postgres_url = match acceptance {
            Acceptance::Relay => relay_epoch_url(&shared_url).await,
            Acceptance::Mailbox => shared_url,
        };
        let db = open_control_db(&postgres_url).await;
        let pool = generate_pool();
        reset_and_enroll(&db, &pool).await;
        reset_node_lease_tables(&db).await;
        let seams = OwnerSeams::new();
        let port_a = free_tcp_port();
        let port_b = free_tcp_port();
        let (server_a, node_a, _) =
            spawn_cluster_server_with_envs(&postgres_url, &pool.pool_env, port_a, &[port_b], &[])
                .await;
        let (server_b, node_b, _) = spawn_cluster_server_with_envs(
            &postgres_url,
            &pool.pool_env,
            port_b,
            &[port_a],
            &[(SEAM_DIRECTORY_ENV, &seams.directory())],
        )
        .await;
        wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
        wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;
        match acceptance {
            Acceptance::Mailbox => activate_mailbox_epoch(&db).await,
            Acceptance::Relay => assert!(
                protocol_epoch(&db).await < 2,
                "a relay scenario needs its nodes below the mailbox epoch"
            ),
        }

        let room: jid::BareJid = format!("acceptance-{}@muc.{DOMAIN}", uuid::Uuid::new_v4())
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
        let mut sender = WsXmppClient::connect_and_auth(
            &server_a.ws_url(),
            DOMAIN,
            CLUSTER_PEER_USERNAME,
            CLUSTER_PEER_PASSWORD,
            &format!("sender-{}", uuid::Uuid::new_v4()),
        )
        .await
        .expect("sender binds on the origin node");
        join_fanout_room(&mut sender, &room, "sender").await;
        let room_claim = PostgresClaimStore::new(db.clone())
            .current_claim(&Entity::new(EntityType::RoomActor, room.to_string()))
            .await
            .expect("room claim lookup")
            .expect("live room claim");
        assert_eq!(
            room_claim.owner.node_id, node_b,
            "the room must be owned by node B, remote from the sender"
        );

        let mut witness = SmClient::new(witness);
        // The witness sees the sender join; nothing else is pending for it.
        witness.drain(Duration::from_millis(500)).await;
        let mut sender = SmClient::new(sender);
        sender.enable_resumable_sm().await;
        let (h, earlier) = sender.request_h(Duration::from_secs(10)).await;
        assert_eq!(h, Some(0), "fresh stream handled count; frames {earlier:?}");
        let sender_occupant = room.with_resource_str("sender").expect("sender occupant");
        Self {
            postgres_url,
            db,
            pool,
            port_a,
            port_b,
            server_a: Some(server_a),
            server_b: Some(server_b),
            node_a,
            node_b,
            seams,
            room,
            sender,
            sender_occupant,
            witness,
            _serial: serial,
        }
    }
}

impl Scenario {
    /// Send one groupchat message from the sender and return its body, which
    /// is unique and identifies the message in the archive.
    async fn send_groupchat(&mut self, id: &str, origin: uuid::Uuid) -> String {
        let body = format!("{id}-{}", uuid::Uuid::new_v4());
        self.sender
            .send(&groupchat(&self.room, id, &body, origin))
            .await;
        body
    }

    /// Assert that `frames` hold exactly one reflection of `id` from the
    /// sender's occupant JID and that it carries exactly `archive_id`.
    fn assert_one_reflection(&self, who: &str, frames: &[Element], id: &str, archive_id: &str) {
        let copies: Vec<&Element> = frames
            .iter()
            .filter(|frame| is_groupchat(frame, id))
            .collect();
        assert_eq!(copies.len(), 1, "{who} must receive one copy: {frames:?}");
        assert_eq!(
            copies[0].attr("from"),
            Some(self.sender_occupant.to_string().as_str()),
            "{who} copy must come from the sender's occupant JID"
        );
        assert_eq!(
            room_stanza_ids(copies[0], &self.room),
            [archive_id.to_owned()],
            "{who} copy must carry the one archived stanza id"
        );
    }
}

/// What the sender's stream and the room archive show while the owner is
/// stopped before its commit.
struct PausedOwnerCommit {
    /// The sender's handled count, answered while the owner was stopped.
    h: Option<u32>,
    body: String,
}

/// Stop the owner before its commit of one groupchat message and ask the
/// origin for the sender's handled count. On return the owner has not passed
/// its commit hold and the archive has been checked empty.
async fn request_h_while_owner_commit_paused(scenario: &mut Scenario) -> PausedOwnerCommit {
    scenario.seams.arm(HOLD_BEFORE_COMMIT);
    let body = scenario
        .send_groupchat("paused", uuid::Uuid::new_v4())
        .await;
    scenario
        .seams
        .wait_for("held before-commit paused", Duration::from_secs(10))
        .await;
    // The owner abandons a held delivery when its six-second receiver
    // budget ends, which also leaves nothing committed. An answer later than
    // this budget is reported as absent.
    let (h, earlier) = scenario.sender.request_h(Duration::from_secs(7)).await;
    assert!(
        !earlier.iter().any(|frame| is_groupchat(frame, "paused")),
        "no reflection can precede the owner's commit: {earlier:?}"
    );
    assert!(
        !scenario
            .seams
            .events()
            .contains(&"released before-commit paused".to_owned()),
        "the owner must not have passed its commit hold: {:?}",
        scenario.seams.events()
    );
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &body).await,
        Vec::<String>::new(),
        "nothing is archived before the owner commits"
    );
    PausedOwnerCommit { h, body }
}

/// The sender's handled count covers a groupchat stanza only after the room
/// owner has durably accepted it: while the owner is stopped before its
/// commit, `<r/>` is still answered and the answer excludes the stanza. Once
/// the owner commits, the stanza is archived once, reflected, and counted.
#[tokio::test]
async fn h_does_not_advance_while_owner_commit_paused() {
    let mut scenario = Scenario::start_with(Acceptance::Mailbox).await;
    let paused = request_h_while_owner_commit_paused(&mut scenario).await;
    assert_eq!(
        paused.h,
        Some(0),
        "h must not cover a stanza the room owner has not accepted"
    );

    scenario.seams.release(HOLD_BEFORE_COMMIT);
    let frames = scenario
        .sender
        .until(Duration::from_secs(30), "sender reflection", |frame| {
            is_groupchat(frame, "paused")
        })
        .await;
    let archived = archived_ids(&scenario.db, &scenario.room, &paused.body).await;
    assert_eq!(archived.len(), 1, "accepted once: {archived:?}");
    scenario.assert_one_reflection("sender", &frames, "paused", &archived[0]);
    let (h, _) = scenario.sender.request_h(Duration::from_secs(10)).await;
    assert_eq!(h, Some(1), "h covers the stanza once the owner accepted it");
}

/// The first delivery of a groupchat message is lost: the owner is stopped
/// before its commit until its delivery budget has ended, so the attempt the
/// origin's hint started is spent without a commit. The origin relays no
/// payload at the mailbox epoch, so nothing then tells the owner about the
/// message except its own mailbox poll. The owner still accepts it exactly once, from the canonical rows,
/// and only then does the sender's handled count cover the stanza.
#[tokio::test]
async fn lost_delivery_and_hint_are_repaired_by_the_owner_poller() {
    let mut scenario = Scenario::start_with(Acceptance::Mailbox).await;
    scenario.seams.arm(HOLD_BEFORE_COMMIT);
    let body = scenario
        .send_groupchat("lost-hint", uuid::Uuid::new_v4())
        .await;
    scenario
        .seams
        .wait_for("held before-commit lost-hint", Duration::from_secs(10))
        .await;
    // Past the owner's six-second receiver budget the held delivery is
    // abandoned with nothing committed.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let (h, _) = scenario.sender.request_h(Duration::from_secs(10)).await;
    assert_eq!(h, Some(0), "an abandoned delivery is not an acceptance");
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &body).await,
        Vec::<String>::new()
    );

    scenario.seams.release(HOLD_BEFORE_COMMIT);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let archived = loop {
        let archived = archived_ids(&scenario.db, &scenario.room, &body).await;
        if !archived.is_empty() {
            break archived;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the owner's poller must accept the pending entry"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(archived.len(), 1, "accepted once: {archived:?}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (h, _) = scenario.sender.request_h(Duration::from_secs(10)).await;
        if h == Some(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "h covers the stanza once the origin has read the acceptance; got {h:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &body).await,
        archived,
        "later poll passes do not accept it again"
    );
}

/// At the mailbox epoch the origin relays no groupchat payload: the owner
/// reads the stanza from the canonical row its mailbox entry refers to. The
/// `drop-reply` seam is consumed only by an acknowledged groupchat relay
/// reply, so it stays armed and records nothing while the message is
/// accepted once, reflected once to each occupant with the archived stanza
/// id, and counted by the sender's `h`.
#[tokio::test]
async fn mailbox_transport_relays_no_groupchat_payload() {
    let mut scenario = Scenario::start_with(Acceptance::Mailbox).await;
    scenario.seams.arm(DROP_REPLY);
    let body = scenario
        .send_groupchat("mailbox-only", uuid::Uuid::new_v4())
        .await;
    let mut sender_frames = scenario
        .sender
        .until(Duration::from_secs(30), "sender reflection", |frame| {
            is_groupchat(frame, "mailbox-only")
        })
        .await;
    let archived = archived_ids(&scenario.db, &scenario.room, &body).await;
    assert_eq!(archived.len(), 1, "accepted once: {archived:?}");
    // Long enough for a second copy from a retry or a recovery pass to show.
    sender_frames.extend(scenario.sender.drain(Duration::from_secs(5)).await);
    let witness_frames = scenario.witness.drain(Duration::from_secs(1)).await;
    scenario.assert_one_reflection("sender", &sender_frames, "mailbox-only", &archived[0]);
    scenario.assert_one_reflection("witness", &witness_frames, "mailbox-only", &archived[0]);
    let (h, _) = scenario.sender.request_h(Duration::from_secs(10)).await;
    assert_eq!(h, Some(1), "h covers the stanza the owner accepted");
    assert_eq!(
        scenario.seams.events(),
        Vec::<String>::new(),
        "no groupchat relay reply exists to drop at the mailbox epoch"
    );
    scenario.seams.release(DROP_REPLY);
}

/// The owner refuses a groupchat message from a sender who has left the
/// room. No relay reply carries the refusal at the mailbox epoch: the origin
/// reads the owner's decision from the mailbox, answers the sender with the
/// owner's frozen error and only then counts the stanza. Nothing is archived
/// and no occupant receives a copy.
#[tokio::test]
async fn owner_refusal_is_answered_from_the_frozen_error() {
    owner_refusal_is_answered_once(Some(uuid::Uuid::new_v4())).await;
}

/// The same refusal for a stanza without an XEP-0359 origin-id. The owner
/// has no alias to find the origin's canonical message by, so it must record
/// its refusal against the message the entry names: a refusal recorded
/// against a message of the owner's own would leave the entry undecided.
#[tokio::test]
async fn owner_refusal_without_an_origin_id_decides_the_origins_message() {
    owner_refusal_is_answered_once(None).await;
}

async fn owner_refusal_is_answered_once(origin: Option<uuid::Uuid>) {
    let mut scenario = Scenario::start_with(Acceptance::Mailbox).await;
    let leave = Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(
            xml_ncname!("to").to_owned(),
            scenario.sender_occupant.to_string(),
        )
        .attr(xml_ncname!("type").to_owned(), "unavailable")
        .build();
    scenario.sender.send(&leave).await;
    scenario.sender.drain(Duration::from_secs(2)).await;
    scenario.witness.drain(Duration::from_secs(1)).await;
    let (before, _) = scenario.sender.request_h(Duration::from_secs(10)).await;

    let body = match origin {
        Some(origin) => scenario.send_groupchat("refused", origin).await,
        None => {
            let body = format!("refused-{}", uuid::Uuid::new_v4());
            let stanza = fanout_message(&scenario.room, "refused", Some(&body));
            scenario.sender.send(&stanza).await;
            body
        }
    };
    let frames = scenario
        .sender
        .until(Duration::from_secs(30), "owner refusal", |frame| {
            is_error(frame, "refused")
        })
        .await;
    assert_eq!(
        frames
            .iter()
            .filter(|frame| is_error(frame, "refused"))
            .count(),
        1,
        "the refusal is answered once: {frames:?}"
    );
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &body).await,
        Vec::<String>::new(),
        "a refused message is not archived"
    );
    let (disposition, _, _) = newest_mailbox_entry(&scenario.db)
        .await
        .expect("the origin appended an entry");
    assert_eq!(disposition, 4, "the entry records a consumed refusal");
    let (h, later) = scenario.sender.request_h(Duration::from_secs(10)).await;
    assert_eq!(
        h,
        before.map(|before| before + 1),
        "h covers the stanza together with its refusal"
    );
    assert!(
        !later.iter().any(|frame| is_error(frame, "refused")),
        "the refusal is not repeated: {later:?}"
    );
    let witness_frames = scenario.witness.drain(Duration::from_secs(1)).await;
    assert!(
        !witness_frames
            .iter()
            .any(|frame| is_groupchat(frame, "refused")),
        "no occupant receives a refused message: {witness_frames:?}"
    );
}

/// The owner process is killed after it has committed a groupchat message
/// and before it executes any effect. The acceptance is durable, so the
/// origin reads it from the mailbox and the sender's `h` covers the stanza
/// while no owner is running. The replacement owner delivers the frozen
/// copies from canonical rows: the sender receives exactly one reflection
/// carrying the archived stanza id, and the message stays archived once.
#[tokio::test]
async fn owner_sigkill_after_commit_delivers_the_frozen_reflection_once() {
    let mut scenario = Scenario::start_with(Acceptance::Mailbox).await;
    scenario.seams.arm(HOLD_AFTER_COMMIT);
    let body = scenario
        .send_groupchat("after-commit", uuid::Uuid::new_v4())
        .await;
    scenario
        .seams
        .wait_for("held after-commit after-commit", Duration::from_secs(10))
        .await;
    let archived = archived_ids(&scenario.db, &scenario.room, &body).await;
    assert_eq!(archived.len(), 1, "the owner accepted once: {archived:?}");
    // Dropping the handle SIGKILLs the owner before any copy is delivered.
    drop(scenario.server_b.take());
    wait_for_node_lease_lapse(&scenario.db, &scenario.node_b).await;
    scenario.seams.release(HOLD_AFTER_COMMIT);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (h, _) = scenario.sender.request_h(Duration::from_secs(10)).await;
        if h == Some(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "h covers a durably accepted stanza without a running owner; got {h:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let port_b = free_tcp_port();
    let (server_b, _, _) = spawn_cluster_server_with_envs(
        &scenario.postgres_url,
        &scenario.pool.pool_env,
        port_b,
        &[scenario.port_a],
        &[],
    )
    .await;
    wait_for_readiness(&server_b, true, Duration::from_secs(30)).await;
    scenario.server_b = Some(server_b);
    // The ingress maintenance pass repairs unreceipted copies once they are
    // a minute old.
    let mut frames = scenario
        .sender
        .until(Duration::from_secs(180), "recovered reflection", |frame| {
            is_groupchat(frame, "after-commit")
        })
        .await;
    frames.extend(scenario.sender.drain(Duration::from_secs(10)).await);
    scenario.assert_one_reflection("sender", &frames, "after-commit", &archived[0]);
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &body).await,
        archived,
        "recovery delivers the accepted message, it does not accept another"
    );
}

/// The owner commits and executes a groupchat message but its relay reply
/// never reaches the origin. The origin's own handling of the failed ask,
/// and then the client's retransmission of the same stanza, both resolve to
/// the one acceptance: one archived message, one copy per occupant, and the
/// same room stanza id everywhere.
#[tokio::test]
async fn reply_loss_recovers_same_acceptance() {
    let mut scenario = Scenario::start().await;
    let origin = uuid::Uuid::new_v4();
    scenario.seams.arm(DROP_REPLY);
    let body = scenario.send_groupchat("lost", origin).await;
    scenario
        .seams
        .wait_for("dropped reply lost", Duration::from_secs(10))
        .await;
    let archived = archived_ids(&scenario.db, &scenario.room, &body).await;
    assert_eq!(archived.len(), 1, "the owner accepted once: {archived:?}");

    // Long enough for the origin's ask to fail (8 s reply budget) and for
    // anything it does about that to reach either occupant.
    let settle = Duration::from_secs(12);
    let mut sender_frames = scenario.sender.drain(settle).await;
    let mut witness_frames = scenario.witness.drain(Duration::from_secs(1)).await;
    scenario.assert_one_reflection("sender", &sender_frames, "lost", &archived[0]);
    scenario.assert_one_reflection("witness", &witness_frames, "lost", &archived[0]);

    // The client never saw an `<a/>` for the stanza, so it sends it again.
    scenario
        .sender
        .send(&groupchat(&scenario.room, "lost", &body, origin))
        .await;
    sender_frames.extend(scenario.sender.drain(settle).await);
    witness_frames.extend(scenario.witness.drain(Duration::from_secs(1)).await);
    assert!(
        !sender_frames.iter().any(|frame| is_error(frame, "lost")),
        "the retransmission is the accepted message, not an error: {sender_frames:?}"
    );
    scenario.assert_one_reflection("sender", &sender_frames, "lost", &archived[0]);
    scenario.assert_one_reflection("witness", &witness_frames, "lost", &archived[0]);
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &body).await,
        archived,
        "recovery must find the same acceptance, not create another"
    );
    assert_eq!(
        scenario.seams.events(),
        ["dropped reply lost"],
        "exactly one reply was dropped"
    );
}

/// The origin process is killed after the owner has committed a groupchat
/// message and before the owner replies. The acceptance is durable on the
/// owner: the message is archived once and the owner-side occupant receives
/// one copy. The sender's stream-management session is durable too: the
/// restarted origin records its lost owner and resumes it
/// (`2139-durable-sm-session-model.md`, "Resume on a node that does not hold
/// the claim"). Below the mailbox epoch the origin counted the stanza when
/// its own commit succeeded, before handing it to the owner, so the resumed
/// `h` already covers it and the client has nothing to retransmit; the
/// message stays archived once.
#[tokio::test]
async fn origin_sigkill_after_owner_commit() {
    let mut scenario = Scenario::start().await;
    let origin = uuid::Uuid::new_v4();
    scenario.seams.arm(HOLD_AFTER_COMMIT);
    let body = scenario.send_groupchat("origin-kill", origin).await;
    scenario
        .seams
        .wait_for("held after-commit origin-kill", Duration::from_secs(10))
        .await;
    let archived = archived_ids(&scenario.db, &scenario.room, &body).await;
    assert_eq!(
        archived.len(),
        1,
        "the owner's commit archives: {archived:?}"
    );

    // Dropping the handle SIGKILLs the process.
    drop(scenario.server_a.take());
    wait_for_node_lease_lapse(&scenario.db, &scenario.node_a).await;
    scenario.seams.release(HOLD_AFTER_COMMIT);
    scenario
        .seams
        .wait_for("released after-commit origin-kill", Duration::from_secs(10))
        .await;
    let mut witness_frames = scenario.witness.drain(Duration::from_secs(5)).await;
    scenario.assert_one_reflection("witness", &witness_frames, "origin-kill", &archived[0]);
    assert!(
        scenario
            .sender
            .drain(Duration::from_secs(1))
            .await
            .is_empty()
            && scenario.sender.failure.is_some(),
        "the sender's socket died with the origin"
    );

    let port_a = free_tcp_port();
    let (server_a, _, _) = spawn_cluster_server_with_envs(
        &scenario.postgres_url,
        &scenario.pool.pool_env,
        port_a,
        &[scenario.port_b],
        &[],
    )
    .await;
    wait_for_readiness(&server_a, true, Duration::from_secs(30)).await;
    let mut socket = WsXmppClient::connect(&server_a.ws_url())
        .await
        .expect("reconnect to the restarted origin");
    socket
        .authenticate(DOMAIN, CLUSTER_PEER_USERNAME, CLUSTER_PEER_PASSWORD)
        .await
        .expect("reauthenticate");
    let mut reconnected = SmClient::new(socket);
    let resume = Element::builder("resume", SM_NS)
        .attr(
            xml_ncname!("previd").to_owned(),
            scenario.sender.stream_id.clone().expect("stream id"),
        )
        .attr(
            xml_ncname!("h").to_owned(),
            scenario.sender.inbound.to_string(),
        )
        .build();
    reconnected.send(&resume).await;
    let answer = reconnected
        .until(Duration::from_secs(30), "resume answer", |frame| {
            frame.is("resumed", SM_NS) || frame.is("failed", SM_NS)
        })
        .await;
    let resumed = answer
        .last()
        .filter(|frame| frame.is("resumed", SM_NS))
        .unwrap_or_else(|| panic!("the restarted origin resumes the durable session: {answer:?}"));
    assert_eq!(
        resumed.attr("h"),
        Some("1"),
        "the resumed handled count covers the stanza the origin committed"
    );
    reconnected.inbound = scenario.sender.inbound;
    reconnected.stream_id = scenario.sender.stream_id.clone();
    // The resumed stream answers `<r/>` with the same count: nothing more
    // was handled on it.
    let (h, mut sender_frames) = reconnected.request_h(Duration::from_secs(10)).await;
    assert_eq!(h, Some(1), "the resumed stream keeps its handled count");
    sender_frames.extend(reconnected.drain(Duration::from_secs(10)).await);
    witness_frames.extend(scenario.witness.drain(Duration::from_secs(1)).await);
    for copy in sender_frames
        .iter()
        .filter(|frame| is_groupchat(frame, "origin-kill"))
    {
        assert_eq!(
            room_stanza_ids(copy, &scenario.room),
            archived,
            "a reflection after restart is the original acceptance: {sender_frames:?}"
        );
    }
    scenario.assert_one_reflection("witness", &witness_frames, "origin-kill", &archived[0]);
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &body).await,
        archived,
        "the message stays archived exactly once"
    );
    scenario.server_a = Some(server_a);
}

/// What the sender's stream and the room archive show after the owner
/// process died before committing a groupchat message and was replaced.
struct OwnerKilledBeforeCommit {
    h: Option<u32>,
    /// Whether the sender was sent an error for the stanza.
    refused: bool,
    /// Room stanza ids of each reflection of the stanza the sender received.
    reflections: Vec<Vec<String>>,
    /// Read after `h`, so a decision `h` covers is visible here.
    archived: Vec<String>,
}

/// Kill the owner while it is stopped before its commit, start a
/// replacement, and observe the sender's still-live stream on the origin.
async fn observe_owner_killed_before_commit(scenario: &mut Scenario) -> OwnerKilledBeforeCommit {
    scenario.seams.arm(HOLD_BEFORE_COMMIT);
    let body = scenario
        .send_groupchat("owner-kill", uuid::Uuid::new_v4())
        .await;
    scenario
        .seams
        .wait_for("held before-commit owner-kill", Duration::from_secs(10))
        .await;
    // Dropping the handle SIGKILLs the process.
    drop(scenario.server_b.take());
    wait_for_node_lease_lapse(&scenario.db, &scenario.node_b).await;
    scenario.seams.release(HOLD_BEFORE_COMMIT);

    let port_b = free_tcp_port();
    let (server_b, _, _) = spawn_cluster_server_with_envs(
        &scenario.postgres_url,
        &scenario.pool.pool_env,
        port_b,
        &[scenario.port_a],
        &[(SEAM_DIRECTORY_ENV, &scenario.seams.directory())],
    )
    .await;
    wait_for_readiness(&server_b, true, Duration::from_secs(30)).await;
    scenario.server_b = Some(server_b);

    // Long enough for the origin's failed ask (8 s reply budget) and for a
    // retry against the replacement owner to show a result.
    let mut frames = scenario.sender.drain(Duration::from_secs(15)).await;
    let (h, earlier) = scenario.sender.request_h(Duration::from_secs(10)).await;
    frames.extend(earlier);
    assert_eq!(
        scenario.sender.failure, None,
        "the sender's stream on the origin stays up"
    );
    assert_eq!(
        scenario.seams.events(),
        ["held before-commit owner-kill"],
        "the killed owner never passed its commit hold"
    );
    OwnerKilledBeforeCommit {
        h,
        refused: frames.iter().any(|frame| is_error(frame, "owner-kill")),
        reflections: frames
            .iter()
            .filter(|frame| is_groupchat(frame, "owner-kill"))
            .map(|frame| room_stanza_ids(frame, &scenario.room))
            .collect(),
        archived: archived_ids(&scenario.db, &scenario.room, &body).await,
    }
}

/// The owner process is killed before it commits a groupchat message and is
/// replaced. The killed owner decided nothing. The entry stays pending in
/// the mailbox, and the node that takes the room's claim decides it once
/// from canonical rows, if it does so within the observation: the message
/// is archived at most once, a reflection exists only as the copy of that
/// one archived acceptance, an acceptance and a refusal exclude each other,
/// and the sender's handled count covers the stanza only together with a
/// decision. Without one the stanza is still pending and uncounted.
#[tokio::test]
async fn owner_sigkill_before_commit() {
    let mut scenario = Scenario::start_with(Acceptance::Mailbox).await;
    let observed = observe_owner_killed_before_commit(&mut scenario).await;
    assert!(
        observed.archived.len() <= 1,
        "the stanza is accepted at most once: {:?}",
        observed.archived
    );
    assert!(
        observed.reflections.len() <= 1
            && observed
                .reflections
                .iter()
                .all(|ids| *ids == observed.archived),
        "a reflection is the copy of the one archived acceptance: {:?} against {:?}",
        observed.reflections,
        observed.archived
    );
    assert!(
        !(observed.refused && !observed.archived.is_empty()),
        "the successor either accepts or refuses the stanza"
    );
    let decided = observed.refused || !observed.archived.is_empty();
    assert!(
        observed.h == Some(0) || (observed.h == Some(1) && decided),
        "h covers the stanza only together with its owner's decision: h {:?}, refused {}, \
         archived {:?}",
        observed.h,
        observed.refused,
        observed.archived
    );
}

/// Room membership changes between the owner's acceptance of a groupchat
/// message and the delivery of its copies.
///
/// Before the owner's decision the sender's `h` excludes the stanza. The
/// owner then commits it while `witness` and `sender` are the only
/// occupants, and is stopped before it executes effects or replies. From
/// that commit on the acceptance is durable: the origin reads it from the
/// mailbox and `h` covers the stanza although no copy has been delivered. A
/// third occupant, `latecomer`, joins while the owner is still stopped. When
/// the owner continues, the audience is the one frozen at acceptance:
/// `witness` and `sender` each receive one live copy carrying the archived
/// stanza id, `latecomer` receives no live copy (history delivered on join
/// carries XEP-0203 `<delay/>` and is not a live copy), and the archive
/// still holds one message.
#[tokio::test]
async fn membership_change_between_acceptance_and_ack() {
    let mut scenario = Scenario::start_with(Acceptance::Mailbox).await;
    let paused = request_h_while_owner_commit_paused(&mut scenario).await;
    assert_eq!(
        paused.h,
        Some(0),
        "h must not cover the stanza before the owner's decision"
    );

    scenario.seams.arm(HOLD_AFTER_COMMIT);
    scenario.seams.release(HOLD_BEFORE_COMMIT);
    scenario
        .seams
        .wait_for("held after-commit paused", Duration::from_secs(30))
        .await;
    let archived = archived_ids(&scenario.db, &scenario.room, &paused.body).await;
    assert_eq!(archived.len(), 1, "the owner accepted once: {archived:?}");

    // The decision is durable, so the origin may count the stanza while the
    // owner has delivered nothing.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut before_release = Vec::new();
    loop {
        let (h, frames) = scenario.sender.request_h(Duration::from_secs(10)).await;
        before_release.extend(frames);
        if h == Some(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "h covers the stanza once the owner's decision is recorded; got {h:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        !before_release
            .iter()
            .any(|frame| is_groupchat(frame, "paused")),
        "the owner is stopped before it delivers any copy: {before_release:?}"
    );

    let server_a = scenario.server_a.as_ref().expect("origin node");
    // Every node seeds the shared `admin` account with its own generated
    // password when it starts, so the stored credential is the one of the
    // node that started last: the owner.
    let admin_password = scenario
        .server_b
        .as_ref()
        .expect("owner node")
        .fixed_account_password();
    let mut latecomer = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        DOMAIN,
        "admin",
        admin_password,
        &format!("latecomer-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("latecomer binds");
    join_fanout_room(&mut latecomer, &scenario.room, "latecomer").await;
    let mut latecomer = SmClient::new(latecomer);

    scenario.seams.release(HOLD_AFTER_COMMIT);
    let sender_frames = scenario
        .sender
        .until(Duration::from_secs(30), "sender reflection", |frame| {
            is_groupchat(frame, "paused")
        })
        .await;
    scenario.assert_one_reflection("sender", &sender_frames, "paused", &archived[0]);
    let witness_frames = scenario.witness.drain(Duration::from_secs(5)).await;
    scenario.assert_one_reflection("witness", &witness_frames, "paused", &archived[0]);
    let late_frames = latecomer.drain(Duration::from_secs(5)).await;
    assert!(
        !late_frames.iter().any(|frame| {
            is_groupchat(frame, "paused") && frame.get_child("delay", "urn:xmpp:delay").is_none()
        }),
        "an occupant who joined after acceptance is outside the frozen audience: {late_frames:?}"
    );
    let (h, _) = scenario.sender.request_h(Duration::from_secs(10)).await;
    assert_eq!(h, Some(1), "the stanza is counted once");
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &paused.body).await,
        archived,
        "membership changes do not create another acceptance"
    );
}

/// The newest room-acceptance entry: its disposition as stored and the node
/// incarnation its lane records as target. Joins, presence and leaves append
/// entries of other kinds on lanes of their own.
async fn newest_mailbox_entry(db: &Database) -> Option<(i64, String, String)> {
    let conn = db.guard().await.expect("mailbox guard");
    let mut rows = conn
        .query(
            "SELECT e.disposition::bigint, l.target_node_id, l.target_node_epoch \
             FROM ingress_mailbox_entries e \
             JOIN ingress_mailbox_lanes l ON l.lane_id = e.lane_id \
             WHERE e.operation_kind = 1 \
             ORDER BY e.appended_at DESC LIMIT 1",
            (),
        )
        .await
        .expect("mailbox query");
    let row = rows.next().await.expect("mailbox row")?;
    Some((
        row.get::<i64>(0).expect("disposition"),
        row.get::<String>(1).expect("target node"),
        row.get::<String>(2).expect("target incarnation"),
    ))
}

/// The room's owner dies before it decides a groupchat message, and the
/// room's claim then moves to another node incarnation.
///
/// The origin appended the entry on a lane that names the dead owner. The
/// successor's backlog scan does not read that lane until the successor
/// holds the room's claim and has adopted the lane. It then decides the one
/// entry from canonical rows. Whatever it decides, the decision is made
/// once: the archive holds the message exactly when the entry records an
/// acceptance, the lane names the successor, and the sender's `h` covers the
/// stanza only when the origin has read that decision.
#[tokio::test]
async fn owner_transfer_before_commit_is_decided_once_by_the_successor() {
    let mut scenario = Scenario::start_with(Acceptance::Mailbox).await;
    let room_entity = Entity::new(EntityType::RoomActor, scenario.room.to_string());
    let claims = PostgresClaimStore::new(scenario.db.clone());
    let first_owner = claims
        .current_claim(&room_entity)
        .await
        .expect("room claim lookup")
        .expect("live room claim")
        .owner;

    scenario.seams.arm(HOLD_BEFORE_COMMIT);
    let body = scenario
        .send_groupchat("transfer", uuid::Uuid::new_v4())
        .await;
    scenario
        .seams
        .wait_for("held before-commit transfer", Duration::from_secs(10))
        .await;
    let (h, _) = scenario.sender.request_h(Duration::from_secs(7)).await;
    assert_eq!(h, Some(0), "nothing is decided yet");
    let (disposition, target_node, target_incarnation) = newest_mailbox_entry(&scenario.db)
        .await
        .expect("the origin appended an entry");
    assert_eq!(disposition, 0, "the entry awaits its owner");
    assert_eq!(
        (target_node, target_incarnation),
        (first_owner.node_id.clone(), first_owner.node_epoch.clone()),
        "the lane names the owner the origin proved"
    );

    // Dropping the handle SIGKILLs the owner. Its replacement is a new
    // incarnation; rejoining the room there makes it take the room's claim.
    drop(scenario.server_b.take());
    wait_for_node_lease_lapse(&scenario.db, &scenario.node_b).await;
    scenario.seams.release(HOLD_BEFORE_COMMIT);
    let port_b = free_tcp_port();
    let (server_b, _, _) = spawn_cluster_server_with_envs(
        &scenario.postgres_url,
        &scenario.pool.pool_env,
        port_b,
        &[scenario.port_a],
        &[],
    )
    .await;
    wait_for_readiness(&server_b, true, Duration::from_secs(30)).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    // Kept open: an occupant that left would let the room close again.
    let mut rejoined = Vec::new();
    let successor = loop {
        let claim = claims
            .current_claim(&room_entity)
            .await
            .expect("room claim lookup");
        if let Some(claim) = claim.filter(|claim| claim.owner != first_owner) {
            break claim.owner;
        }
        // A join is refused or times out until the dead owner's claim can
        // be taken; each attempt asks the replacement to host the room.
        if let Ok(mut rejoining) = WsXmppClient::connect_and_auth(
            &server_b.ws_url(),
            DOMAIN,
            "admin",
            server_b.fixed_account_password(),
            &format!("witness-{}", uuid::Uuid::new_v4()),
        )
        .await
        {
            let _ = rejoining
                .send(&String::from(&cluster_support::muc_join_presence(
                    &scenario.room.to_string(),
                    "witness",
                )))
                .await;
            rejoined.push(rejoining);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the room's claim must move off the dead owner"
        );
    };
    scenario.server_b = Some(server_b);

    // The successor adopts the lane and decides its head entry.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let (disposition, target_node, target_incarnation) = loop {
        let entry = newest_mailbox_entry(&scenario.db)
            .await
            .expect("the entry outlives the transfer");
        if entry.0 != 0 {
            break entry;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the successor's poller must decide the predecessor's entry"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(
        (target_node, target_incarnation),
        (successor.node_id.clone(), successor.node_epoch.clone()),
        "the lane follows the room's claim"
    );
    let accepted = matches!(disposition, 1 | 3);
    let archived = archived_ids(&scenario.db, &scenario.room, &body).await;
    assert_eq!(
        archived.len(),
        usize::from(accepted),
        "the archive holds the message exactly when the entry records an acceptance"
    );

    // Give the origin's watcher time to read the decision, if it still can.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (h, _) = scenario.sender.request_h(Duration::from_secs(10)).await;
    let (disposition, _, _) = newest_mailbox_entry(&scenario.db)
        .await
        .expect("the entry is kept with its message");
    let consumed = matches!(disposition, 3 | 4);
    assert_eq!(
        h,
        Some(u32::from(consumed)),
        "h covers the stanza only when the origin has read the owner's decision"
    );
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &body).await,
        archived,
        "later poll passes decide nothing again"
    );
    drop(rejoined);
    assert!(
        !scenario
            .seams
            .events()
            .iter()
            .any(|event| event.starts_with("released")),
        "the killed owner never passed its commit hold: {:?}",
        scenario.seams.events()
    );
}
