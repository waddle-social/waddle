//! XEP-0045 §7.14: retained remote departures converge after a live foreign
//! UserActor claim is released. Uses the parent suite's native cluster harness.

use super::*;
use waddle_xmpp::ownership::{ClaimStore, ExactReleaseOutcome};

fn is_departure(frame: &str, occupant: &jid::FullJid) -> bool {
    frame.parse::<minidom::Element>().is_ok_and(|element| {
        element.is("presence", waddle_xmpp::ns::JABBER_CLIENT)
            && element.attr("from") == Some(occupant.to_string().as_str())
            && element.attr("type") == Some("unavailable")
    })
}

#[tokio::test]
async fn foreign_user_claim_defers_remote_departure_until_reconciliation_after_release() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping XEP-0045 remote cleanup retry: WADDLE_TEST_POSTGRES_URL not set");
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

    let room: jid::BareJid = format!("cleanup-retry-{}@muc.localhost", uuid::Uuid::new_v4())
        .parse()
        .expect("unique room JID");
    let occupant = room.with_resource_str("departing").expect("occupant JID");
    let mut observer = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        "localhost",
        "admin",
        server_b.fixed_account_password(),
        &format!("cleanup-observer-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("observer binds on room owner A");
    join_fanout_room(&mut observer, &room, "observer").await;
    let mut departing = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        "localhost",
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        &format!("cleanup-departing-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("departing client binds on B");
    join_fanout_room(&mut departing, &room, "departing").await;
    let departing_jid = departing
        .full_jid
        .as_ref()
        .expect("bound departing JID")
        .parse::<jid::FullJid>()
        .expect("typed departing JID");
    let entity = Entity::new(EntityType::UserActor, departing_jid.to_bare().to_string());
    let claims = PostgresClaimStore::new(db.clone());
    let original = claims
        .current_claim(&entity)
        .await
        .expect("original user claim lookup")
        .expect("original user claim");
    let room_claim = claims
        .current_claim(&Entity::new(EntityType::RoomActor, room.to_string()))
        .await
        .expect("room claim lookup")
        .expect("room claim");
    assert_eq!(original.owner.node_id, node_b);
    assert_eq!(room_claim.owner.node_id, node_a);
    assert!(original.owner_lease_fresh && room_claim.owner_lease_fresh);

    // Establish the real foreign-holder condition before disconnect. The
    // exact release/acquire fixture advances ownership through the production
    // claim store; B's old sender authority can no longer authorize a relay.
    assert_eq!(
        claims
            .release_exact(&entity, &original.owner, original.claim_epoch)
            .await
            .expect("release original claim"),
        ExactReleaseOutcome::Released
    );
    let held_epoch = claims
        .acquire(&entity, &room_claim.owner)
        .await
        .expect("A holds the foreign user claim");
    assert!(
        !claims
            .fence(&entity, &original.owner, original.claim_epoch)
            .await
            .expect("check old sender fence"),
        "B must lack valid user authority before its disconnect cleanup"
    );
    // No XEP-0198 enable: this terminal disconnect owes a departure, rather
    // than preserving an intentionally resumable occupancy.
    departing.close().await.expect("departing client closes");

    // Cover at least one production 30-second reconciliation scan while
    // the foreign claim stays live. A departure here would be premature.
    match observer
        .recv_matching_within(Duration::from_secs(35), |frame| {
            is_departure(frame, &occupant)
        })
        .await
    {
        Err(error) if error.starts_with("Timeout waiting") => {}
        result => panic!("departure must remain deferred while A holds the user claim: {result:?}"),
    }
    let held = claims
        .current_claim(&entity)
        .await
        .expect("held claim lookup")
        .expect("foreign claim remains held");
    assert_eq!(held.owner, room_claim.owner);
    assert_eq!(held.claim_epoch, held_epoch);
    assert!(held.owner_lease_fresh, "deferral must involve a live owner");
    assert_eq!(
        claims
            .release_exact(&entity, &held.owner, held.claim_epoch)
            .await
            .expect("release live foreign claim"),
        ExactReleaseOutcome::Released
    );

    // Nothing reconnects or sends another presence. Only retained membership
    // reconciliation on B can now originate this generation's remote leave.
    let departure = observer
        .recv_matching_within(Duration::from_secs(120), |frame| {
            is_departure(frame, &occupant)
        })
        .await
        .expect("reconciliation delivers remote unavailable after foreign claim release");
    let presence: minidom::Element = departure.parse().expect("typed unavailable presence");
    let item = presence
        .get_child("x", xmpp_parsers::ns::MUC_USER)
        .and_then(|extension| extension.get_child("item", xmpp_parsers::ns::MUC_USER))
        .expect("XEP-0045 departure item");
    assert_eq!(item.attr("role"), Some("none"));
    observer.close().await.expect("observer closes");
}
