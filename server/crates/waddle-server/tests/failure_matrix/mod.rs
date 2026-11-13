//! Rows of the #2046 mandatory failure matrix that need two real server
//! processes and had no native multi-node test.

use std::time::{Duration, Instant};

use minidom::Element;
use waddle_ws_test_support::WsXmppClient;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

use super::cluster_support::{
    activate_mailbox_epoch, cluster_e2e_serial_lock, free_tcp_port, generate_pool, open_control_db,
    reset_and_enroll, reset_node_lease_tables, spawn_cluster_server, wait_for_readiness,
    CLUSTER_PEER_PASSWORD, CLUSTER_PEER_USERNAME,
};

const DOMAIN: &str = "localhost";

fn chat(to: &str, id: &str, body: &str) -> String {
    let mut message = Message::new(Some(to.parse().expect("recipient")));
    message.id = Some(Id(id.to_owned()));
    message.type_ = MessageType::Chat;
    message.bodies.insert(Lang::new(), body.to_owned());
    String::from(&Element::from(message))
}

/// What one contested socket has shown so far.
#[derive(Default)]
struct Observed {
    frames: Vec<String>,
    ended: bool,
}

impl Observed {
    /// The stream was closed for the conflict, or the transport ended.
    fn retired(&self) -> bool {
        self.ended || self.frames.iter().any(|frame| frame.contains("conflict"))
    }

    fn copies_of(&self, body: &str) -> usize {
        self.frames
            .iter()
            .filter(|frame| frame.contains(body))
            .count()
    }
}

/// Read whatever `client` has within `budget`, without failing on silence.
async fn observe(client: &mut WsXmppClient, observed: &mut Observed, budget: Duration) {
    let deadline = Instant::now() + budget;
    while !observed.ended {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match client.recv_timeout(remaining).await {
            Ok(frame) => observed.frames.push(frame),
            Err(error) if error.contains("Timeout") => break,
            Err(_) => observed.ended = true,
        }
    }
}

/// Failure matrix, row 1: the same full JID binds concurrently on two
/// nodes. One binding remains. The other socket is closed, and a message
/// addressed to the full JID afterwards reaches the remaining binding once
/// and the closed one never.
#[tokio::test]
async fn same_full_jid_bound_concurrently_on_two_nodes_leaves_one_binding() {
    let Some(postgres_url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;
    let db = open_control_db(&postgres_url).await;
    let pool = generate_pool();
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;

    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (server_a, _node_a, _peer_a) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
    let (server_b, _node_b, _peer_b) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;
    wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
    wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;
    // The message from the node that lost the race reaches the remaining
    // binding on the other node as a bound direct request, which the
    // mailbox carries from protocol epoch 2
    // (`docs/planning/2173-direct-lanes.md`, "Bound requests"). The nodes
    // build the schema the epoch row lives in.
    activate_mailbox_epoch(&db).await;

    // Both sockets authenticate first, so the two binds are the only work
    // left when they race.
    let resource = format!("matrix-{}", uuid::Uuid::new_v4().simple());
    let mut on_a = WsXmppClient::connect(&server_a.ws_url())
        .await
        .expect("socket on node A");
    on_a.authenticate(DOMAIN, CLUSTER_PEER_USERNAME, CLUSTER_PEER_PASSWORD)
        .await
        .expect("authenticate on node A");
    let mut on_b = WsXmppClient::connect(&server_b.ws_url())
        .await
        .expect("socket on node B");
    on_b.authenticate(DOMAIN, CLUSTER_PEER_USERNAME, CLUSTER_PEER_PASSWORD)
        .await
        .expect("authenticate on node B");
    let (bound_a, bound_b) = tokio::join!(on_a.bind(&resource), on_b.bind(&resource));
    assert!(
        bound_a.is_ok() || bound_b.is_ok(),
        "one of two concurrent binds must succeed: {bound_a:?}, {bound_b:?}"
    );
    let full_jid = format!("{CLUSTER_PEER_USERNAME}@{DOMAIN}/{resource}");

    // Wait until one socket has been retired: its bind was refused, its
    // stream was closed with a conflict, or its transport ended.
    let mut seen_a = Observed {
        ended: bound_a.is_err(),
        ..Observed::default()
    };
    let mut seen_b = Observed {
        ended: bound_b.is_err(),
        ..Observed::default()
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !(seen_a.retired() || seen_b.retired()) {
        assert!(
            Instant::now() < deadline,
            "both sockets still hold the full JID: A {:?}, B {:?}",
            seen_a.frames,
            seen_b.frames
        );
        observe(&mut on_a, &mut seen_a, Duration::from_millis(250)).await;
        observe(&mut on_b, &mut seen_b, Duration::from_millis(250)).await;
    }
    assert!(
        !(seen_a.retired() && seen_b.retired()),
        "one binding must remain: A {:?}, B {:?}",
        seen_a.frames,
        seen_b.frames
    );

    // A message to the full JID, sent from the node that lost and then from
    // the node that kept the binding, reaches the remaining socket once each
    // time and the retired socket never.
    let mut sender_a = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        DOMAIN,
        "admin",
        server_b.fixed_account_password(),
        "matrix-sender-a",
    )
    .await
    .expect("sender on node A");
    let mut sender_b = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        DOMAIN,
        "admin",
        server_b.fixed_account_password(),
        "matrix-sender-b",
    )
    .await
    .expect("sender on node B");
    let bodies = [
        format!("matrix-from-a-{}", uuid::Uuid::new_v4().simple()),
        format!("matrix-from-b-{}", uuid::Uuid::new_v4().simple()),
    ];
    sender_a
        .send(&chat(&full_jid, "matrix-a", &bodies[0]))
        .await
        .expect("send from node A");
    sender_b
        .send(&chat(&full_jid, "matrix-b", &bodies[1]))
        .await
        .expect("send from node B");
    let settle = Instant::now() + Duration::from_secs(15);
    loop {
        observe(&mut on_a, &mut seen_a, Duration::from_millis(250)).await;
        observe(&mut on_b, &mut seen_b, Duration::from_millis(250)).await;
        let delivered = bodies
            .iter()
            .all(|body| seen_a.copies_of(body) + seen_b.copies_of(body) >= 1);
        if delivered || Instant::now() >= settle {
            break;
        }
    }
    // Late duplicates, if any, arrive within this window.
    observe(&mut on_a, &mut seen_a, Duration::from_secs(2)).await;
    observe(&mut on_b, &mut seen_b, Duration::from_secs(2)).await;

    let (kept, retired, kept_name) = if seen_a.retired() {
        (&seen_b, &seen_a, "B")
    } else {
        (&seen_a, &seen_b, "A")
    };
    assert!(
        !kept.retired(),
        "the remaining binding on node {kept_name} was retired afterwards: {:?}",
        kept.frames
    );
    for body in &bodies {
        assert_eq!(
            kept.copies_of(body),
            1,
            "the remaining binding on node {kept_name} must receive {body} once: {:?}",
            kept.frames
        );
        assert_eq!(
            retired.copies_of(body),
            0,
            "the retired socket must not receive {body}: {:?}",
            retired.frames
        );
    }
}
