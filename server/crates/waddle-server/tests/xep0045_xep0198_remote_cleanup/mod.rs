//! XEP-0045 §7.14 and XEP-0198: terminal sessions leave a foreign RoomActor
//! even while a sibling keeps their UserActor claim alive on the other node.

use super::*;
use minidom::rxml::xml_ncname;
use waddle_xmpp::ownership::ClaimStore;
use waddle_xmpp::stream_management::SM_NS;

#[derive(Clone, Copy)]
enum SessionEnd {
    Disconnect,
    SmExpiry,
}

fn is_departure(frame: &str, occupant: &jid::FullJid) -> bool {
    frame.parse::<minidom::Element>().is_ok_and(|element| {
        element.is("presence", waddle_xmpp::ns::JABBER_CLIENT)
            && element.attr("from") == Some(occupant.to_string().as_str())
            && element.attr("type") == Some("unavailable")
    })
}

async fn terminal_session_leaves_remote_room(end: SessionEnd) {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping XEP-0045/0198 remote cleanup: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;
    let db = open_control_db(&postgres_url).await;
    let pool = generate_pool();
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;
    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (server_a, node_a, _) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
    let (server_b, node_b, _) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;
    wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
    wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;

    let room: jid::BareJid = format!("terminal-cleanup-{}@muc.localhost", uuid::Uuid::new_v4())
        .parse()
        .expect("unique room JID");
    let occupant = room.with_resource_str("departing").expect("occupant JID");
    let mut observer = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        "localhost",
        "admin",
        server_b.fixed_account_password(),
        "cleanup-observer",
    )
    .await
    .expect("observer binds on A");
    join_fanout_room(&mut observer, &room, "observer").await;
    // Binding the sibling first establishes genuine foreign UserActor
    // ownership; no fixture moves claims behind either server's back.
    let mut sibling = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        "localhost",
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        "cleanup-live-sibling",
    )
    .await
    .expect("sibling binds on A and owns UserActor");
    join_fanout_room(&mut sibling, &room, "sibling").await;
    let mut departing = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        "localhost",
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        "cleanup-departing",
    )
    .await
    .expect("departing socket binds on B through A's UserActor");
    join_fanout_room(&mut departing, &room, "departing").await;
    let departing_jid: jid::FullJid = departing
        .full_jid
        .as_ref()
        .expect("bound departing JID")
        .parse()
        .expect("typed departing JID");
    let entity = Entity::new(EntityType::UserActor, departing_jid.to_bare().to_string());
    let claims = PostgresClaimStore::new(db.clone());
    let user_claim = claims
        .current_claim(&entity)
        .await
        .expect("user claim lookup")
        .expect("live user claim");
    let room_claim = claims
        .current_claim(&Entity::new(EntityType::RoomActor, room.to_string()))
        .await
        .expect("room claim lookup")
        .expect("live room claim");
    assert_eq!(user_claim.owner.node_id, node_a);
    assert_eq!(room_claim.owner.node_id, node_a);
    assert_ne!(room_claim.owner.node_id, node_b, "RoomActor must be remote");
    assert!(user_claim.owner_lease_fresh && room_claim.owner_lease_fresh);

    match end {
        SessionEnd::Disconnect => departing.close().await.expect("terminal close"),
        SessionEnd::SmExpiry => {
            let enable = minidom::Element::builder("enable", SM_NS)
                .attr(xml_ncname!("resume").to_owned(), "true")
                .attr(xml_ncname!("max").to_owned(), "10")
                .build();
            departing
                .send(&String::from(&enable))
                .await
                .expect("enable resumable stream");
            let enabled = departing
                .recv_matching(|frame| {
                    frame
                        .parse::<minidom::Element>()
                        .is_ok_and(|element| element.is("enabled", SM_NS))
                })
                .await
                .expect("SM enabled");
            let enabled: minidom::Element = enabled.parse().expect("typed SM enabled");
            assert_eq!(enabled.attr("max"), Some("10"));
            assert_eq!(enabled.attr("resume"), Some("true"));
            let stream_id = enabled.attr("id").expect("resumable stream identifier");
            // Dropping the transport preserves occupancy until the negotiated
            // resume deadline, unlike a graceful XMPP close.
            drop(departing);
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let conn = db.guard().await.expect("detached SM guard");
                    let mut rows = conn
                        .query(
                            "SELECT detached_at_ms IS NOT NULL AND occupancy_session IS NOT NULL \
                             FROM sm_sessions WHERE stream_id = ?",
                            waddle_server::db_params![stream_id.to_owned()],
                        )
                        .await
                        .expect("detached SM query");
                    if let Some(row) = rows.next().await.expect("detached SM row") {
                        if row.get::<bool>(0).expect("detached generation is durable") {
                            break;
                        }
                    }
                    drop(rows);
                    drop(conn);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .expect("transport loss persists a detached SM session with its occupancy generation");
            match observer
                .recv_matching_within(Duration::from_secs(1), |frame| {
                    is_departure(frame, &occupant)
                })
                .await
            {
                Err(error) if error.starts_with("Timeout waiting") => {}
                result => panic!("resumable occupancy left before SM expiry: {result:?}"),
            }
        }
    }

    // The production SM janitor ticks once per minute. No fixture drains it
    // and neither the sibling nor the departing resource reconnects.
    let departure = observer
        .recv_matching_within(Duration::from_secs(120), |frame| {
            is_departure(frame, &occupant)
        })
        .await
        .expect("terminal session leaves the remote RoomActor");
    let departure: minidom::Element = departure.parse().expect("typed departure");
    let item = departure
        .get_child("x", xmpp_parsers::ns::MUC_USER)
        .and_then(|extension| extension.get_child("item", xmpp_parsers::ns::MUC_USER))
        .expect("XEP-0045 departure item");
    assert_eq!(item.attr("role"), Some("none"));
    let retained_claim = claims
        .current_claim(&entity)
        .await
        .expect("retained user claim lookup")
        .expect("sibling keeps user claim alive");
    assert_eq!(retained_claim.owner, user_claim.owner);
    assert_eq!(retained_claim.claim_epoch, user_claim.claim_epoch);
    assert!(retained_claim.owner_lease_fresh);
    // A fresh join obtains the actual RoomActor roster, independently of
    // the unavailable broadcast. The old resource must be gone while its
    // same-account sibling remains an occupant.
    let mut witness = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        "localhost",
        "admin",
        server_b.fixed_account_password(),
        "cleanup-witness",
    )
    .await
    .expect("fresh roster witness binds on A");
    witness
        .send(&String::from(&muc_join_presence(
            &room.to_string(),
            "witness",
        )))
        .await
        .expect("fresh witness joins room");
    let roster = tokio::time::timeout(
        Duration::from_secs(15),
        witness.recv_until(|frame| frame.contains("<subject")),
    )
    .await
    .expect("bounded witness roster")
    .expect("witness receives complete room roster");
    let roster_occupants: Vec<jid::FullJid> = roster
        .iter()
        .filter_map(|frame| frame.parse::<minidom::Element>().ok())
        .filter(|element| {
            element.is("presence", waddle_xmpp::ns::JABBER_CLIENT) && element.attr("type").is_none()
        })
        .filter_map(|element| element.attr("from").and_then(|jid| jid.parse().ok()))
        .collect();
    assert!(
        !roster_occupants.contains(&occupant),
        "departing occupant must be absent from RoomActor roster: {roster:?}"
    );
    assert!(
        roster_occupants.contains(&room.with_resource_str("sibling").expect("sibling occupant")),
        "live sibling must remain in RoomActor roster: {roster:?}"
    );
    let sibling_ping = xmpp_parsers::iq::Iq::Get {
        from: None,
        to: None,
        id: "cleanup-sibling-still-live".to_string(),
        payload: xmpp_parsers::ping::Ping.into(),
    };
    sibling
        .send(&stanza_xml(Stanza::Iq(Box::new(sibling_ping))))
        .await
        .expect("sibling sends ping after cleanup");
    let pong = sibling
        .recv_matching(|frame| frame_has_attr(frame, "id", "cleanup-sibling-still-live"))
        .await
        .expect("sibling still receives replies");
    assert!(frame_has_attr(&pong, "type", "result"), "{pong}");
    witness.close().await.expect("witness closes");
    sibling.close().await.expect("sibling closes");
    observer.close().await.expect("observer closes");
}

#[tokio::test]
async fn disconnect_removes_remote_occupant_while_claim_owner_sibling_is_live() {
    terminal_session_leaves_remote_room(SessionEnd::Disconnect).await;
}

#[tokio::test]
async fn sm_expiry_removes_remote_occupant_while_claim_owner_sibling_is_live() {
    terminal_session_leaves_remote_room(SessionEnd::SmExpiry).await;
}
