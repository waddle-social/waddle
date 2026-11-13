//! XEP-0198 resume across nodes with the durable session authority
//! selected (`WADDLE_XMPP_DURABLE_SM=durable`).
//!
//! Two real `waddle-server` processes share one PostgreSQL database and are
//! meshed over the swarm. A session is enabled on node A and its resume
//! token is presented on node B: while its connection on A is live, after
//! A detached it, and after A was killed with no orphan reaper to reclaim
//! its claims. The same resource is also bound afresh on node B while A
//! holds its session, live or detached. The tests observe the wire, the
//! authority's session row and the session's claim row.
//!
//! Requires `WADDLE_TEST_POSTGRES_URL`; skipped otherwise.

#![cfg(feature = "clustering")]

pub mod cluster_support;

use std::time::Duration;

use cluster_support::{
    cluster_e2e_serial_lock, free_tcp_port, generate_pool, open_control_db, reset_and_enroll,
    reset_node_lease_tables, spawn_cluster_server_with_envs, wait_for_cross_node_registration,
    wait_for_node_lease_lapse, wait_for_readiness, CLUSTER_PEER_PASSWORD, CLUSTER_PEER_USERNAME,
};
use minidom::Element;
use waddle_server::clustering::claims::{NodeLeaseStore, PostgresClaimStore};
use waddle_server::db::Database;
use waddle_server::sm_authority::{
    AdmissionAuthority, AdmissionError, AdmissionFrame, AdmissionRejection, AdmissionRequest,
    DetachSnapshot, DurableSessionRecord, DurableSmStore, OwnerError, OwnerRejection,
};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::ownership::{
    verify_resume_identity, ClaimEpoch, ClaimStore, Entity, EntityType, NodeIdentity, SmSessionKey,
};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::durable::{DurableSmState, ExposureBounds, SmOperationId};
use waddle_xmpp::stream_management::persistence::{
    PersistedSession, SmClaimFence, SmReplayPayload,
};
use waddle_xmpp::stream_management::{SmFrameProvenance, SM_NS};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Lang, Message};

const DOMAIN: &str = "localhost";
/// The fixed account every spawned server seeds. It owns the session.
const OWNER: &str = "admin";
const DURABLE: (&str, &str) = ("WADDLE_XMPP_DURABLE_SM", "durable");
/// Keeps the orphan reaper, and with it the stale-node watchdog, from
/// running during a test: a dead node's claims stay where they are.
const NO_REAPER: (&str, &str) = ("WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS", "3600000");
const STANZAS_NS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

/// An owned attribute name for the element builder.
macro_rules! nc {
    ($name:literal) => {
        minidom::rxml::xml_ncname!($name).to_owned()
    };
}

struct Cluster {
    db: Database,
    node_a: TestServer,
    node_a_id: String,
    node_b: TestServer,
    node_b_id: String,
    /// Password of the fixed account, as seeded by the node started last.
    owner_password: String,
}

/// Two meshed durable-selector nodes on a freshly reset control plane, or
/// `None` when no test PostgreSQL is configured.
async fn cluster(extra_envs: &[(&str, &str)]) -> Option<Cluster> {
    let postgres_url = waddle_ws_test_support::require_test_postgres_url()?;
    let db = open_control_db(&postgres_url).await;
    let pool = generate_pool();
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;

    let mut envs = vec![DURABLE];
    envs.extend_from_slice(extra_envs);
    let (port_a, port_b) = (free_tcp_port(), free_tcp_port());
    let (node_a, node_a_id, _peer_a) =
        spawn_cluster_server_with_envs(&postgres_url, &pool.pool_env, port_a, &[port_b], &envs)
            .await;
    let (node_b, node_b_id, _peer_b) =
        spawn_cluster_server_with_envs(&postgres_url, &pool.pool_env, port_b, &[port_a], &envs)
            .await;
    wait_for_cross_node_registration(&node_a, &node_b).await;
    let owner_password = node_b.fixed_account_password().to_owned();
    Some(Cluster {
        db,
        node_a,
        node_a_id,
        node_b,
        node_b_id,
        owner_password,
    })
}

async fn send_element(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize");
    client
        .send(&String::from_utf8(bytes).expect("UTF-8"))
        .await
        .expect("send");
}

fn sm(name: &str) -> minidom::ElementBuilder {
    Element::builder(name, SM_NS)
}

fn resume(previd: &str, h: u32) -> Element {
    sm("resume")
        .attr(nc!("previd"), previd)
        .attr(nc!("h"), h.to_string())
        .build()
}

fn chat(to: &str, id: &str) -> Element {
    Element::builder("message", "jabber:client")
        .attr(nc!("to"), to)
        .attr(nc!("type"), "chat")
        .attr(nc!("id"), id)
        .append(
            Element::builder("body", "jabber:client")
                .append("held")
                .build(),
        )
        .build()
}

fn is_counted_stanza(element: &Element) -> bool {
    matches!(element.name(), "message" | "presence" | "iq") && element.ns() == "jabber:client"
}

/// The first `<resumed/>` or `<failed/>` the server sends, within `wait`.
async fn recv_resume_reply(client: &mut WsXmppClient, wait: Duration) -> Result<Element, String> {
    let frame = client
        .recv_matching_within(wait, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.is("resumed", SM_NS) || element.is("failed", SM_NS))
        })
        .await?;
    frame
        .parse()
        .map_err(|error| format!("server XML: {error}"))
}

async fn owner_connection(server: &TestServer, password: &str) -> WsXmppClient {
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("connect");
    client
        .authenticate(DOMAIN, OWNER, password)
        .await
        .expect("authenticate the session's account");
    client
}

/// The owner account bound to `resource` on `server` with resumable stream
/// management enabled. Returns the connection and its resume token.
async fn enabled_session(
    server: &TestServer,
    password: &str,
    resource: &str,
) -> (WsXmppClient, String) {
    let mut client = owner_connection(server, password).await;
    client.bind(resource).await.expect("bind");
    send_element(
        &mut client,
        sm("enable").attr(nc!("resume"), "true").build(),
    )
    .await;
    let frame = client
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.is("enabled", SM_NS))
        })
        .await
        .expect("<enabled/>");
    let enabled: Element = frame.parse().expect("server XML");
    let token = enabled.attr("id").expect("resume token").to_owned();
    (client, token)
}

/// A second account bound on `server`, to send the owner chat messages.
async fn sender(server: &TestServer) -> WsXmppClient {
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        &format!("sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("sender binds")
}

/// Read frames until every chat message in `ids` has arrived. Returns how
/// many counted stanzas (XEP-0198 `h` units) were read.
async fn recv_chats(client: &mut WsXmppClient, ids: &[&str]) -> u32 {
    let mut counted = 0;
    let mut missing: Vec<&str> = ids.to_vec();
    while !missing.is_empty() {
        let frame = client
            .recv_timeout(Duration::from_secs(15))
            .await
            .unwrap_or_else(|error| panic!("no frame while waiting for {missing:?}: {error}"));
        let Ok(element) = frame.parse::<Element>() else {
            continue;
        };
        if is_counted_stanza(&element) {
            counted += 1;
        }
        if element.name() == "message" {
            missing.retain(|id| element.attr("id") != Some(*id));
        }
    }
    counted
}

/// The ids of the chat messages that arrive within `wait`, in arrival
/// order. A closed transport ends the read.
async fn chat_ids_within(client: &mut WsXmppClient, wait: Duration) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut ids = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return ids;
        }
        let Ok(frame) = client.recv_timeout(remaining).await else {
            return ids;
        };
        let Ok(element) = frame.parse::<Element>() else {
            continue;
        };
        if element.is("message", "jabber:client") && element.attr("type") == Some("chat") {
            if let Some(id) = element.attr("id") {
                ids.push(id.to_owned());
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionRow {
    session_id: String,
    state: i64,
    generation: i64,
    owner_node_id: Option<String>,
    outbound_appended: i64,
}

const STATE_LIVE: i64 = 1;
const STATE_DETACHED: i64 = 2;

async fn session_row(db: &Database, full_jid: &str) -> Option<SessionRow> {
    let conn = db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT session_id, CAST(state AS BIGINT), generation, owner_node_id, \
             outbound_appended FROM sm_durable_sessions WHERE full_jid = ?",
            waddle_server::db_params![full_jid.to_owned()],
        )
        .await
        .expect("read durable session");
    let row = rows.next().await.expect("durable session row")?;
    Some(SessionRow {
        session_id: row.get(0).expect("session_id"),
        state: row.get(1).expect("state"),
        generation: row.get(2).expect("generation"),
        owner_node_id: row.get(3).expect("owner_node_id"),
        outbound_appended: row.get(4).expect("outbound_appended"),
    })
}

async fn wait_for_session<F: Fn(&SessionRow) -> bool>(
    db: &Database,
    full_jid: &str,
    accept: F,
) -> SessionRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let row = session_row(db, full_jid).await;
        if let Some(row) = row.clone().filter(|row| accept(row)) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "durable session of {full_jid} did not reach the expected state: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn claim_owner(db: &Database, session_id: &str) -> Option<String> {
    let conn = db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT node_id FROM clustering_claims WHERE entity = ?",
            waddle_server::db_params![format!("sm_session:{session_id}")],
        )
        .await
        .expect("read session claim");
    let row = rows.next().await.expect("session claim row")?;
    Some(row.get(0).expect("node_id"))
}

async fn retained_frames(db: &Database, session_id: &str) -> i64 {
    let conn = db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sm_unacked WHERE stream_id = ?",
            waddle_server::db_params![session_id.to_owned()],
        )
        .await
        .expect("count retained frames");
    rows.next()
        .await
        .expect("count row")
        .expect("one row")
        .get(0)
        .expect("count")
}

fn owner_jid(resource: &str) -> String {
    format!("{OWNER}@{DOMAIN}/{resource}")
}

fn unique_resource(label: &str) -> String {
    format!("{label}-{}", uuid::Uuid::new_v4())
}

/// The owner's session on node A holding the chat messages `ids`
/// unacknowledged, with its connection still open. Returns the connection,
/// the resume token and how many counted stanzas the connection carried.
async fn live_session_holding(
    cluster: &Cluster,
    resource: &str,
    ids: &[&str],
) -> (WsXmppClient, String, u32) {
    let (mut owner, token) =
        enabled_session(&cluster.node_a, &cluster.owner_password, resource).await;
    let mut sender = sender(&cluster.node_a).await;
    for id in ids {
        send_element(&mut sender, chat(&owner_jid(resource), id)).await;
    }
    let written = recv_chats(&mut owner, ids).await;
    sender.close().await.expect("close sender");
    (owner, token, written)
}

/// A session resumed on node B while its connection on node A is live: A's
/// connection is closed with a conflict, the claim and the session move to
/// B, the stanzas the client had not handled are replayed once and in
/// order, and live output continues on B.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_live_on_another_node_resumes_and_replays_once_in_order() {
    let _serial = cluster_e2e_serial_lock().lock().await;
    let Some(cluster) = cluster(&[]).await else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let resource = unique_resource("live");
    let jid = owner_jid(&resource);
    let (mut on_a, token, written) =
        live_session_holding(&cluster, &resource, &["m1", "m2", "m3"]).await;
    let before = wait_for_session(&cluster.db, &jid, |row| row.state == STATE_LIVE).await;
    assert_eq!(
        before.owner_node_id.as_deref(),
        Some(cluster.node_a_id.as_str())
    );
    assert_eq!(
        claim_owner(&cluster.db, &before.session_id)
            .await
            .as_deref(),
        Some(cluster.node_a_id.as_str())
    );

    // The client handled every stanza but the last two.
    let mut on_b = owner_connection(&cluster.node_b, &cluster.owner_password).await;
    send_element(&mut on_b, resume(&token, written - 2)).await;
    let reply = recv_resume_reply(&mut on_b, Duration::from_secs(30))
        .await
        .expect("resume reply from node B");
    assert!(reply.is("resumed", SM_NS), "resume on node B: {reply:?}");
    assert_eq!(reply.attr("previd"), Some(token.as_str()));

    let closed = on_a
        .recv_matching_within(Duration::from_secs(15), |frame| {
            frame.contains("conflict") || frame.contains("<close")
        })
        .await;
    assert!(
        closed.is_ok(),
        "node A's connection is closed with a conflict: {closed:?}"
    );

    let after = wait_for_session(&cluster.db, &jid, |row| {
        row.state == STATE_LIVE && row.generation == 2
    })
    .await;
    assert_eq!(
        after.session_id, before.session_id,
        "the same session resumed"
    );
    assert_eq!(
        after.owner_node_id.as_deref(),
        Some(cluster.node_b_id.as_str())
    );
    assert_eq!(
        claim_owner(&cluster.db, &after.session_id).await.as_deref(),
        Some(cluster.node_b_id.as_str()),
        "the session claim moved to the resuming node"
    );

    // Live output follows the replay: a message sent now arrives after it.
    let mut sender = sender(&cluster.node_b).await;
    send_element(&mut sender, chat(&jid, "m4")).await;
    assert_eq!(
        chat_ids_within(&mut on_b, Duration::from_secs(5)).await,
        ["m2", "m3", "m4"],
        "the unhandled stanzas are replayed once, in order, ahead of live output"
    );
}

/// What node A would present if it wrote to the session after losing it:
/// the owner fence and claim the session's row recorded for it.
struct FormerOwner {
    record: DurableSessionRecord,
    claim: SmClaimFence,
}

async fn authority(db: &Database) -> DurableSmStore {
    DurableSmStore::open(
        db.clone(),
        ExposureBounds::new(64, 1 << 20).expect("bounds"),
        true,
    )
    .await
    .expect("open the durable authority")
}

async fn former_owner(store: &DurableSmStore, session: SmSessionKey) -> FormerOwner {
    let record = store
        .session(session)
        .await
        .expect("read session")
        .expect("session row");
    let recorded = record
        .claim
        .clone()
        .expect("a clustered session records its claim");
    let claim = SmClaimFence::new(
        NodeIdentity::new(recorded.node_id, recorded.node_epoch),
        ClaimEpoch(recorded.claim_epoch),
    );
    FormerOwner { record, claim }
}

fn late_frame(to: &str) -> AdmissionFrame {
    let mut message = Message::new(Some(to.parse().expect("recipient JID")));
    message.bodies.insert(Lang::new(), "late".to_owned());
    AdmissionFrame {
        operation: SmOperationId::mint(),
        payload: SmReplayPayload::from(Box::new(Stanza::Message(message))),
        original_receipt_at: chrono::Utc::now(),
        ingress_receipts: Vec::new(),
        provenance: SmFrameProvenance::NonIngress,
    }
}

fn late_snapshot(record: &DurableSessionRecord) -> DetachSnapshot {
    DetachSnapshot {
        resource: PersistedSession {
            stream_id: SmSessionId::new(record.session.as_uuid().to_string()),
            user_id: record.full_jid.to_bare().to_string(),
            jid: record.full_jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: None,
            detached_at: chrono::Utc::now(),
            max_resume_duration: Duration::ZERO,
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        },
        inbound_handled: 0,
    }
}

/// A session node A detached is resumed on node B without asking A, and
/// everything A could still write under the fence and claim it held is
/// refused without changing the session.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_detached_on_another_node_resumes_and_its_former_owner_is_fenced() {
    let _serial = cluster_e2e_serial_lock().lock().await;
    let Some(cluster) = cluster(&[]).await else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let resource = unique_resource("detached");
    let jid = owner_jid(&resource);
    let (on_a, token, written) = live_session_holding(&cluster, &resource, &["m1", "m2"]).await;
    drop(on_a);
    let detached = wait_for_session(&cluster.db, &jid, |row| row.state == STATE_DETACHED).await;
    assert_eq!(
        claim_owner(&cluster.db, &detached.session_id)
            .await
            .as_deref(),
        Some(cluster.node_a_id.as_str()),
        "a detached session keeps its claim on the node that detached it"
    );
    let store = authority(&cluster.db).await;
    let session = SmSessionKey::from_uuid(detached.session_id.parse().expect("session key"));
    let former = former_owner(&store, session).await;

    let mut on_b = owner_connection(&cluster.node_b, &cluster.owner_password).await;
    send_element(&mut on_b, resume(&token, written - 1)).await;
    let reply = recv_resume_reply(&mut on_b, Duration::from_secs(30))
        .await
        .expect("resume reply from node B");
    assert!(reply.is("resumed", SM_NS), "resume on node B: {reply:?}");
    assert_eq!(
        chat_ids_within(&mut on_b, Duration::from_secs(3)).await,
        ["m2"],
        "exactly the unhandled stanza is replayed"
    );
    let resumed = wait_for_session(&cluster.db, &jid, |row| {
        row.state == STATE_LIVE && row.generation == 2
    })
    .await;
    assert_eq!(
        resumed.owner_node_id.as_deref(),
        Some(cluster.node_b_id.as_str())
    );
    assert_eq!(
        claim_owner(&cluster.db, &resumed.session_id)
            .await
            .as_deref(),
        Some(cluster.node_b_id.as_str())
    );
    let frames_before = retained_frames(&cluster.db, &resumed.session_id).await;

    // Node A's late writes: an admission as the owner it was, an admission
    // on the detached authority of its generation, a detach and a close.
    let former_fence = former.record.fence();
    let admitted = store
        .admit(
            AdmissionRequest {
                authority: AdmissionAuthority::Owner {
                    fence: &former_fence,
                    principal: &former.record.principal,
                },
                claim: Some(&former.claim),
                now: chrono::Utc::now(),
            },
            &late_frame(&jid),
        )
        .await;
    assert!(
        matches!(
            admitted,
            Err(AdmissionError::Rejected(
                AdmissionRejection::ClaimFenceMissing
            ))
        ),
        "a late owner admission is refused: {admitted:?}"
    );
    let admitted_detached = store
        .admit(
            AdmissionRequest {
                authority: AdmissionAuthority::Detached {
                    session,
                    generation: former.record.generation,
                },
                claim: Some(&former.claim),
                now: chrono::Utc::now(),
            },
            &late_frame(&jid),
        )
        .await;
    assert!(
        matches!(
            admitted_detached,
            Err(AdmissionError::Rejected(
                AdmissionRejection::ClaimFenceMissing
            ))
        ),
        "a late detached admission is refused: {admitted_detached:?}"
    );
    let detached_late = store
        .detach(
            &former_fence,
            Some(&former.claim),
            late_snapshot(&former.record),
            chrono::Utc::now(),
        )
        .await;
    assert!(
        matches!(
            detached_late,
            Err(OwnerError::Rejected(OwnerRejection::ClaimFenceMissing))
        ),
        "a late detach is refused: {detached_late:?}"
    );
    let closed_late = store
        .close(&former_fence, Some(&former.claim), chrono::Utc::now())
        .await;
    assert!(
        matches!(
            closed_late,
            Err(OwnerError::Rejected(OwnerRejection::ClaimFenceMissing))
        ),
        "a late close is refused: {closed_late:?}"
    );

    assert_eq!(
        session_row(&cluster.db, &jid).await,
        Some(resumed.clone()),
        "the refused writes changed nothing in the session"
    );
    assert_eq!(
        retained_frames(&cluster.db, &resumed.session_id).await,
        frames_before
    );
    let current = store
        .session(session)
        .await
        .expect("read session")
        .expect("session row");
    assert_eq!(current.state, DurableSmState::Live);

    // The session still serves its owner on node B.
    let mut sender = sender(&cluster.node_b).await;
    send_element(&mut sender, chat(&jid, "m3")).await;
    assert_eq!(
        chat_ids_within(&mut on_b, Duration::from_secs(5)).await,
        ["m3"]
    );
}

/// A connection that is live on a node whose session claim was taken
/// without it being asked: its next write is refused for the missing claim,
/// the connection is closed as superseded, and the session it no longer
/// owns is left exactly as it was for the node that holds the claim.
#[tokio::test(flavor = "multi_thread")]
async fn a_connection_that_lost_the_session_claim_retires_and_leaves_the_session() {
    let _serial = cluster_e2e_serial_lock().lock().await;
    let Some(cluster) = cluster(&[NO_REAPER]).await else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let resource = unique_resource("fenced");
    let jid = owner_jid(&resource);
    let (mut on_a, _token, written) = live_session_holding(&cluster, &resource, &["m1"]).await;
    let before = wait_for_session(&cluster.db, &jid, |row| row.state == STATE_LIVE).await;
    let frames_before = retained_frames(&cluster.db, &before.session_id).await;

    // A third node takes the claim by the swap a resume uses.
    let claims = PostgresClaimStore::new(cluster.db.clone());
    let taker = NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    claims
        .register(&taker, None)
        .await
        .expect("register the taking node");
    let entity = Entity::new(EntityType::SmSession, before.session_id.clone());
    let held = claims
        .current_claim(&entity)
        .await
        .expect("read the claim")
        .expect("node A holds the claim");
    let account: jid::BareJid = format!("{OWNER}@{DOMAIN}").parse().expect("bare JID");
    let proof = verify_resume_identity(&account, &account).expect("same account");
    claims
        .steal_for_resume(&entity, held.claim_epoch, proof, &taker)
        .await
        .expect("the claim moves");

    // Node A's next write for the session: settling the client's
    // acknowledgement. No stanza is addressed to the session, so nothing is
    // left for a later delivery attempt.
    send_element(
        &mut on_a,
        sm("a").attr(nc!("h"), written.to_string()).build(),
    )
    .await;
    let closed = on_a
        .recv_matching_within(Duration::from_secs(15), |frame| frame.contains("conflict"))
        .await;
    assert!(
        closed.is_ok(),
        "the connection is closed as superseded, as a displaced binding is: {closed:?}"
    );
    assert_eq!(
        claim_owner(&cluster.db, &before.session_id)
            .await
            .as_deref(),
        Some(taker.node_id.as_str())
    );
    // Give the retired connection's teardown time to run.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        session_row(&cluster.db, &jid).await,
        Some(before.clone()),
        "the retired connection changed nothing in the session"
    );
    assert_eq!(
        retained_frames(&cluster.db, &before.session_id).await,
        frames_before
    );
}

/// Resume attempts on node B, each on a new connection, until node B
/// answers one while holding the session's claim. Returns that connection
/// and its reply. An attempt refused because the claim is still held
/// elsewhere, or cut short while the survivor fences itself, is repeated.
async fn resume_until_claimed_by(
    db: &Database,
    node_b: &TestServer,
    node_b_id: &str,
    owner_password: &str,
    session_id: &str,
    request: &Element,
) -> (WsXmppClient, Element) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let attempt = async {
            let mut client = WsXmppClient::connect(&node_b.ws_url()).await?;
            client.authenticate(DOMAIN, OWNER, owner_password).await?;
            send_element(&mut client, request.clone()).await;
            let reply = recv_resume_reply(&mut client, Duration::from_secs(20)).await?;
            Ok::<_, String>((client, reply))
        };
        let refusal = match attempt.await {
            Ok((client, reply)) => {
                if claim_owner(db, session_id).await.as_deref() == Some(node_b_id) {
                    return (client, reply);
                }
                format!("{reply:?}")
            }
            Err(error) => error,
        };
        assert!(
            tokio::time::Instant::now() < deadline,
            "the surviving node never took the session claim: {refusal}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Node A is killed while it owns a live session and no orphan reaper runs.
///
/// While A's node lease has not lapsed, a resume on node B is refused and
/// nothing moves. Once it has lapsed, a resume on node B takes the claim
/// itself, records A's owner as lost and resumes the session there.
///
/// A killed owner stored no resource snapshot. The occupancy generation the
/// resource was bound under is stored with the session from enable onward,
/// so the resuming connection publishes the resource under it: the resume
/// succeeds, the stanza the client had not handled is replayed once, ahead
/// of live output, and the session is live under the surviving node.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_of_a_killed_node_is_taken_by_the_survivor_without_the_reaper() {
    let _serial = cluster_e2e_serial_lock().lock().await;
    let Some(cluster) = cluster(&[NO_REAPER]).await else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let resource = unique_resource("killed");
    let jid = owner_jid(&resource);
    let (on_a, token, written) = live_session_holding(&cluster, &resource, &["m1", "m2"]).await;
    let before = wait_for_session(&cluster.db, &jid, |row| row.state == STATE_LIVE).await;
    let Cluster {
        db,
        node_a,
        node_a_id,
        node_b,
        node_b_id,
        owner_password,
    } = cluster;
    drop(node_a);
    drop(on_a);
    let request = resume(&token, written - 1);

    // A holder that does not answer keeps its claim while its lease is not
    // known to have lapsed. The survivor may be fencing itself in this
    // window, so the attempt may also end without any reply.
    let early = async {
        let mut client = WsXmppClient::connect(&node_b.ws_url()).await?;
        client.authenticate(DOMAIN, OWNER, &owner_password).await?;
        send_element(&mut client, request.clone()).await;
        recv_resume_reply(&mut client, Duration::from_secs(10)).await
    };
    if let Ok(reply) = early.await {
        if claim_owner(&db, &before.session_id).await.as_deref() == Some(node_a_id.as_str()) {
            assert!(reply.is("failed", SM_NS), "{reply:?}");
            assert!(
                reply.get_child("resource-constraint", STANZAS_NS).is_some(),
                "an owner that does not answer keeps the session: {reply:?}"
            );
            assert_eq!(
                session_row(&db, &jid).await,
                Some(before.clone()),
                "a refused resume leaves the session as it was"
            );
        }
    }

    wait_for_node_lease_lapse(&db, &node_a_id).await;
    wait_for_readiness(&node_b, true, Duration::from_secs(30)).await;
    let (mut on_b, reply) = resume_until_claimed_by(
        &db,
        &node_b,
        &node_b_id,
        &owner_password,
        &before.session_id,
        &request,
    )
    .await;
    assert!(reply.is("resumed", SM_NS), "resume on node B: {reply:?}");
    assert_eq!(reply.attr("previd"), Some(token.as_str()));
    let after = wait_for_session(&db, &jid, |row| {
        row.state == STATE_LIVE && row.generation == before.generation + 1
    })
    .await;
    assert_eq!(
        after.session_id, before.session_id,
        "the same session resumed"
    );
    assert_eq!(
        after.owner_node_id.as_deref(),
        Some(node_b_id.as_str()),
        "the session is live under the surviving node"
    );

    // Live output follows the replay: a message sent now arrives after it.
    let mut sender = sender(&node_b).await;
    send_element(&mut sender, chat(&jid, "m3")).await;
    assert_eq!(
        chat_ids_within(&mut on_b, Duration::from_secs(5)).await,
        ["m2", "m3"],
        "the unhandled stanza is replayed once, ahead of live output"
    );
}

/// Two resumes of one detached session race, one on each node. Whatever
/// order they commit in, one connection ends up owning the session: it
/// alone receives live output, the session's row and claim name its node,
/// and it saw the unhandled stanza exactly once.
#[tokio::test(flavor = "multi_thread")]
async fn racing_resumes_on_two_nodes_leave_exactly_one_owner() {
    let _serial = cluster_e2e_serial_lock().lock().await;
    let Some(cluster) = cluster(&[]).await else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let resource = unique_resource("race");
    let jid = owner_jid(&resource);
    let (on_a, token, written) = live_session_holding(&cluster, &resource, &["m1"]).await;
    drop(on_a);
    let detached = wait_for_session(&cluster.db, &jid, |row| row.state == STATE_DETACHED).await;

    let mut racer_a = owner_connection(&cluster.node_a, &cluster.owner_password).await;
    let mut racer_b = owner_connection(&cluster.node_b, &cluster.owner_password).await;
    let request = resume(&token, written - 1);
    tokio::join!(
        send_element(&mut racer_a, request.clone()),
        send_element(&mut racer_b, request),
    );
    let (reply_a, reply_b) = tokio::join!(
        recv_resume_reply(&mut racer_a, Duration::from_secs(30)),
        recv_resume_reply(&mut racer_b, Duration::from_secs(30)),
    );
    let resumed = |reply: &Result<Element, String>| {
        reply
            .as_ref()
            .is_ok_and(|element| element.is("resumed", SM_NS))
    };
    assert!(
        resumed(&reply_a) || resumed(&reply_b),
        "one of the racing resumes succeeds: {reply_a:?} / {reply_b:?}"
    );

    // Let a superseded connection be closed before the owner is read.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let settled = wait_for_session(&cluster.db, &jid, |row| row.state == STATE_LIVE).await;
    assert_eq!(settled.session_id, detached.session_id);
    assert!(settled.generation > detached.generation);
    let owner_node = settled.owner_node_id.clone().expect("owner node");
    assert_eq!(
        claim_owner(&cluster.db, &settled.session_id).await,
        Some(owner_node.clone()),
        "the claim and the session name the same node"
    );

    let mut sender = sender(&cluster.node_a).await;
    send_element(&mut sender, chat(&jid, "after")).await;
    let (seen_a, seen_b) = tokio::join!(
        chat_ids_within(&mut racer_a, Duration::from_secs(5)),
        chat_ids_within(&mut racer_b, Duration::from_secs(5)),
    );
    let (winner, loser, winner_node) = if seen_a.iter().any(|id| id == "after") {
        (&seen_a, &seen_b, &cluster.node_a_id)
    } else {
        (&seen_b, &seen_a, &cluster.node_b_id)
    };
    assert_eq!(
        winner.iter().filter(|id| *id == "after").count(),
        1,
        "the owning connection receives live output once: {seen_a:?} / {seen_b:?}"
    );
    assert!(
        !loser.iter().any(|id| id == "after"),
        "only one connection owns the session: {seen_a:?} / {seen_b:?}"
    );
    assert_eq!(&owner_node, winner_node);
    assert_eq!(
        winner.iter().filter(|id| *id == "m1").count(),
        1,
        "the owner saw the unhandled stanza exactly once: {winner:?}"
    );
    assert_eq!(
        winner.first().map(String::as_str),
        Some("m1"),
        "the replay precedes live output: {winner:?}"
    );
}

const STATE_TERMINAL: i64 = 4;
const TERMINAL_REPLACED: i64 = 3;

/// The state and terminal reason of the session, once it is terminal.
async fn wait_for_terminal(db: &Database, session_id: &str) -> (i64, Option<i64>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT CAST(state AS BIGINT), CAST(terminal_reason AS BIGINT) \
                 FROM sm_durable_sessions WHERE session_id = ?",
                waddle_server::db_params![session_id.to_owned()],
            )
            .await
            .expect("read durable session");
        let row = rows.next().await.expect("durable session row");
        let observed: Option<(i64, Option<i64>)> =
            row.map(|row| (row.get(0).expect("state"), row.get(1).expect("reason")));
        drop(rows);
        drop(conn);
        if let Some(observed) = observed.filter(|(state, _)| *state == STATE_TERMINAL) {
            return observed;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "durable session {session_id} did not end: {observed:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn initial_presence() -> Element {
    Element::builder("presence", "jabber:client").build()
}

/// The owner's resource is bound afresh on node B while its durable session
/// is held by node A, live or detached. Node B takes the session's claim
/// and ends the session as replaced; a live connection on node A is closed
/// with a conflict. The frames the session retained reach the new binding
/// through offline delivery, each exactly once, and the old resume token
/// names no session.
async fn a_fresh_bind_on_node_b_replaces_a_session_held_by_node_a(live: bool) {
    let _serial = cluster_e2e_serial_lock().lock().await;
    let Some(cluster) = cluster(&[]).await else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let resource = unique_resource(if live {
        "replace-live"
    } else {
        "replace-detached"
    });
    let jid = owner_jid(&resource);
    let (on_a, token, _written) = live_session_holding(&cluster, &resource, &["m1", "m2"]).await;
    let before = wait_for_session(&cluster.db, &jid, |row| row.state == STATE_LIVE).await;
    let on_a = if live {
        Some(on_a)
    } else {
        drop(on_a);
        wait_for_session(&cluster.db, &jid, |row| row.state == STATE_DETACHED).await;
        None
    };

    let mut on_b = owner_connection(&cluster.node_b, &cluster.owner_password).await;
    assert_eq!(on_b.bind(&resource).await.expect("bind on node B"), jid);
    if let Some(mut on_a) = on_a {
        let closed = on_a
            .recv_matching_within(Duration::from_secs(15), |frame| frame.contains("conflict"))
            .await;
        assert!(
            closed.is_ok(),
            "node A's connection is closed with a conflict: {closed:?}"
        );
    }
    assert_eq!(
        wait_for_terminal(&cluster.db, &before.session_id).await,
        (STATE_TERMINAL, Some(TERMINAL_REPLACED)),
        "the session is ended as replaced"
    );

    send_element(&mut on_b, initial_presence()).await;
    assert_eq!(
        chat_ids_within(&mut on_b, Duration::from_secs(5)).await,
        ["m1", "m2"],
        "the replaced session's frames arrive once each, in order"
    );

    let mut late = owner_connection(&cluster.node_a, &cluster.owner_password).await;
    send_element(&mut late, resume(&token, 0)).await;
    let reply = recv_resume_reply(&mut late, Duration::from_secs(30))
        .await
        .expect("resume reply");
    assert!(reply.is("failed", SM_NS), "{reply:?}");
    assert!(
        reply.get_child("item-not-found", STANZAS_NS).is_some(),
        "a replaced session cannot be resumed: {reply:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_bind_on_another_node_replaces_a_session_live_there() {
    a_fresh_bind_on_node_b_replaces_a_session_held_by_node_a(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_bind_on_another_node_replaces_a_session_detached_there() {
    a_fresh_bind_on_node_b_replaces_a_session_held_by_node_a(false).await;
}
