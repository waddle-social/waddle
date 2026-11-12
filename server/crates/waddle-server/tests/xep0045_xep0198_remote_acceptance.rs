//! XEP-0045 / XEP-0198: when a remotely owned room accepts a groupchat
//! message relative to the sender's handled count (#1948).
//!
//! Two real `waddle-server` processes share one PostgreSQL control plane.
//! The sender binds on node A with stream management enabled; the room is
//! owned by node B. A groupchat message from the sender is committed on A,
//! relayed to B, committed again by B as the room owner, and answered with
//! the sender's reflection. The suite stops B at fixed points of that
//! exchange through the file-armed seams in `clustering/acceptance_seams.rs`
//! and observes the sender's XEP-0198 `h`, the room archive and the
//! reflections occupants receive.
//!
//! The contract under test: `h` covers a groupchat stanza only once the room
//! owner has durably accepted it, and a lost reply or a restart recovers that
//! one acceptance instead of producing a second. The server meets the
//! handled-count part of the contract when remote room acceptance runs
//! through the durable mailbox, so those tests start their nodes with that
//! selector. Each has a `*_characterizes_*` twin that pins what the default
//! relay selector does instead. A contract test the server does not meet
//! under either selector is `#[ignore]`d.
//!
//! The binary needs PostgreSQL and fails without `WADDLE_TEST_POSTGRES_URL`:
//! every scenario is about durable state shared by two processes.

#![cfg(all(feature = "clustering", feature = "test-support"))]

pub mod cluster_support;

use std::path::PathBuf;
use std::time::Duration;

use cluster_support::{
    cluster_e2e_serial_lock, fanout_message, free_tcp_port, generate_pool, join_fanout_room,
    open_control_db, reset_and_enroll, reset_node_lease_tables, spawn_cluster_server,
    spawn_cluster_server_with_envs, wait_for_readiness, EnrolledPool, CLUSTER_PEER_PASSWORD,
    CLUSTER_PEER_USERNAME,
};
use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_server::clustering::claims::PostgresClaimStore;
use waddle_server::db::Database;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType};
use waddle_xmpp::stream_management::SM_NS;

const DOMAIN: &str = "localhost";
const SEAM_DIRECTORY_ENV: &str = "WADDLE_TEST_SEAM_REMOTE_ACCEPTANCE_DIRECTORY";
/// Selects how both nodes carry remote room acceptance.
const ACCEPTANCE_ENV: &str = "WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE";

/// How the nodes of a scenario carry remote room acceptance.
#[derive(Clone, Copy)]
enum Acceptance {
    /// The server's default: the origin counts the stanza when its own
    /// commit succeeds.
    Relay,
    /// The origin counts the stanza only after reading the owner's decision
    /// from the durable mailbox.
    Mailbox,
}

impl Acceptance {
    fn value(self) -> &'static str {
        match self {
            Self::Relay => "relay",
            Self::Mailbox => "mailbox",
        }
    }
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
            let frame = match self.socket.recv_timeout(remaining).await {
                Ok(frame) => frame,
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
                panic!("no {what} within {budget:?}; received {frames:?}");
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
    seams: OwnerSeams,
    room: jid::BareJid,
    sender: SmClient,
    sender_occupant: jid::FullJid,
    witness: SmClient,
    acceptance: Acceptance,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Scenario {
    async fn start() -> Self {
        Self::start_with(Acceptance::Relay).await
    }

    async fn start_with(acceptance: Acceptance) -> Self {
        let postgres_url = waddle_ws_test_support::require_test_postgres_url()
            .expect("the remote acceptance suite needs PostgreSQL: set WADDLE_TEST_POSTGRES_URL");
        let serial = cluster_e2e_serial_lock().lock().await;
        let db = open_control_db(&postgres_url).await;
        let pool = generate_pool();
        reset_and_enroll(&db, &pool).await;
        reset_node_lease_tables(&db).await;
        let seams = OwnerSeams::new();
        let port_a = free_tcp_port();
        let port_b = free_tcp_port();
        let (server_a, _node_a, _) = spawn_cluster_server_with_envs(
            &postgres_url,
            &pool.pool_env,
            port_a,
            &[port_b],
            &[(ACCEPTANCE_ENV, acceptance.value())],
        )
        .await;
        let (server_b, node_b, _) = spawn_cluster_server_with_envs(
            &postgres_url,
            &pool.pool_env,
            port_b,
            &[port_a],
            &[
                (SEAM_DIRECTORY_ENV, &seams.directory()),
                (ACCEPTANCE_ENV, acceptance.value()),
            ],
        )
        .await;
        wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
        wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;

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
            seams,
            room,
            sender,
            sender_occupant,
            witness,
            acceptance,
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

/// Characterizes the relay selector, which
/// `h_does_not_advance_while_owner_commit_paused` shows the mailbox selector
/// does not share: the origin answers `<r/>` with a handled count that
/// already includes the groupchat stanza while the room owner is stopped
/// before its commit and the room archive is empty. It goes when the relay
/// selector does.
#[tokio::test]
async fn h_while_owner_commit_paused_characterizes_current_early_ack() {
    let mut scenario = Scenario::start().await;
    let paused = request_h_while_owner_commit_paused(&mut scenario).await;
    assert_eq!(
        paused.h,
        Some(1),
        "the origin counts the stanza before the owner has accepted it"
    );
    scenario.seams.release(HOLD_BEFORE_COMMIT);
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
/// owner alone: the message is archived once and the owner-side occupant
/// receives one copy. The restarted origin has no stream to resume, so the
/// client binds again on a fresh stream whose `h` starts at zero, and its
/// retransmission of the unacknowledged stanza is counted without a second
/// archived message.
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
    let (server_a, _, _) = spawn_cluster_server(
        &scenario.postgres_url,
        &scenario.pool.pool_env,
        port_a,
        &[scenario.port_b],
    )
    .await;
    wait_for_readiness(&server_a, true, Duration::from_secs(30)).await;
    let resource = scenario
        .sender
        .socket
        .full_jid
        .as_deref()
        .and_then(|jid| jid.parse::<jid::FullJid>().ok())
        .expect("sender full JID")
        .resource()
        .to_string();
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
    assert!(
        answer.last().is_some_and(|frame| frame.is("failed", SM_NS)),
        "a SIGKILLed origin leaves no stream to resume: {answer:?}"
    );
    reconnected
        .socket
        .bind(&resource)
        .await
        .expect("bind again after the failed resume");
    reconnected.enable_resumable_sm().await;
    let (h, _) = reconnected.request_h(Duration::from_secs(10)).await;
    assert_eq!(
        h,
        Some(0),
        "a fresh stream starts its handled count at zero"
    );

    reconnected
        .send(&groupchat(&scenario.room, "origin-kill", &body, origin))
        .await;
    let (h, mut sender_frames) = reconnected.request_h(Duration::from_secs(20)).await;
    assert_eq!(h, Some(1), "the retransmitted stanza is handled once");
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
    scenario.seams.release(HOLD_BEFORE_COMMIT);

    let port_b = free_tcp_port();
    let (server_b, _, _) = spawn_cluster_server_with_envs(
        &scenario.postgres_url,
        &scenario.pool.pool_env,
        port_b,
        &[scenario.port_a],
        &[
            (SEAM_DIRECTORY_ENV, &scenario.seams.directory()),
            (ACCEPTANCE_ENV, scenario.acceptance.value()),
        ],
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
    assert!(
        !frames.iter().any(|frame| is_groupchat(frame, "owner-kill")),
        "no reflection without an archived acceptance: {frames:?}"
    );
    assert_eq!(
        scenario.seams.events(),
        ["held before-commit owner-kill"],
        "the killed owner never passed its commit hold"
    );
    OwnerKilledBeforeCommit {
        h,
        refused: frames.iter().any(|frame| is_error(frame, "owner-kill")),
        archived: archived_ids(&scenario.db, &scenario.room, &body).await,
    }
}

/// The owner process is killed before it commits a groupchat message and is
/// replaced. Nothing was accepted, so nothing is archived, and the sender's
/// handled count may cover the stanza only if the sender was also sent the
/// error that refuses it; otherwise the stanza is still pending.
#[tokio::test]
async fn owner_sigkill_before_commit() {
    let mut scenario = Scenario::start_with(Acceptance::Mailbox).await;
    let observed = observe_owner_killed_before_commit(&mut scenario).await;
    assert_eq!(
        observed.archived,
        Vec::<String>::new(),
        "an owner killed before its commit accepted nothing"
    );
    assert_eq!(
        observed.h,
        Some(u32::from(observed.refused)),
        "h covers the stanza only together with its refusal"
    );
}

/// Characterizes the relay selector, which `owner_sigkill_before_commit`
/// shows the mailbox selector does not share: after the owner dies before its
/// commit, the origin reports the stanza as handled, the room archive never
/// receives it, and the sender is sent no error. It goes when the relay
/// selector does.
#[tokio::test]
async fn owner_sigkill_before_commit_characterizes_current_silent_loss() {
    let mut scenario = Scenario::start().await;
    let observed = observe_owner_killed_before_commit(&mut scenario).await;
    assert_eq!(observed.archived, Vec::<String>::new());
    assert_eq!(
        observed.h,
        Some(1),
        "the origin counted the stanza the owner never accepted"
    );
    assert!(!observed.refused, "the sender is told nothing");
}

/// Room membership changes between the owner's acceptance of a groupchat
/// message and the origin's acknowledgement of it.
///
/// The owner commits the message while `witness` and `sender` are the only
/// occupants, and is stopped before it executes effects or replies. A third
/// occupant, `latecomer`, then joins. When the owner continues, the audience
/// is the one frozen at acceptance: `witness` and `sender` each receive one
/// live copy carrying the archived stanza id, `latecomer` receives no live
/// copy (history delivered on join carries XEP-0203 `<delay/>` and is not a
/// live copy), the archive still holds one message, and only then does the
/// sender's `h` cover the stanza.
#[tokio::test]
#[ignore = "requires remote child settlement"]
async fn membership_change_between_acceptance_and_ack() {
    let mut scenario = Scenario::start().await;
    scenario.seams.arm(HOLD_AFTER_COMMIT);
    let body = scenario
        .send_groupchat("membership", uuid::Uuid::new_v4())
        .await;
    scenario
        .seams
        .wait_for("held after-commit membership", Duration::from_secs(10))
        .await;
    let archived = archived_ids(&scenario.db, &scenario.room, &body).await;
    assert_eq!(archived.len(), 1, "the owner accepted once: {archived:?}");
    let (h, _) = scenario.sender.request_h(Duration::from_secs(7)).await;
    assert_eq!(h, Some(0), "the origin has not learned of the acceptance");

    let server_a = scenario.server_a.as_ref().expect("origin node");
    let mut latecomer = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        DOMAIN,
        "admin",
        server_a.fixed_account_password(),
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
            is_groupchat(frame, "membership")
        })
        .await;
    scenario.assert_one_reflection("sender", &sender_frames, "membership", &archived[0]);
    let witness_frames = scenario.witness.drain(Duration::from_secs(5)).await;
    scenario.assert_one_reflection("witness", &witness_frames, "membership", &archived[0]);
    let late_frames = latecomer.drain(Duration::from_secs(5)).await;
    assert!(
        !late_frames.iter().any(|frame| {
            is_groupchat(frame, "membership")
                && frame.get_child("delay", "urn:xmpp:delay").is_none()
        }),
        "an occupant who joined after acceptance is outside the frozen audience: {late_frames:?}"
    );
    let (h, _) = scenario.sender.request_h(Duration::from_secs(10)).await;
    assert_eq!(h, Some(1), "h covers the stanza after owner acceptance");
    assert_eq!(
        archived_ids(&scenario.db, &scenario.room, &body).await,
        archived,
        "membership changes do not create another acceptance"
    );
}
