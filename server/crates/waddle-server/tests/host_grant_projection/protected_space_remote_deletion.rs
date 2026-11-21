use std::sync::Arc;
use std::time::Duration;

use minidom::{rxml::xml_ncname, Element};
use tokio_util::sync::CancellationToken;
use waddle_server::permissions::host_grant::{BindSpace, GrantOperationId, Reconciliation};
use waddle_server::s2s::route::RouterLimits;
use waddle_server::test_support::{
    host_spaces_for_remote_principals, spawn_remote_space_deletion_worker,
};
use waddle_ws_test_support::{
    iq::round_trip,
    stanza::{is_result, pubsub_owner_affiliations, pubsub_subscribe, set},
};
use waddle_xmpp::pubsub::{
    NodeConfig, PubSubNodeType, NS_PUBSUB, NS_PUBSUB_EVENT, NS_PUBSUB_OWNER,
};
use waddle_xmpp_core::pubsub::{stanzas::matches_pubsub_delete_event, PubSubNodeType};

use super::{bare, served, subject, Live, PermissionActor, LIVE_SPICEDB, OWNER};
use crate::{
    host_grant_revocation_support::Replica,
    s2s_inbound_muc_support::{error_condition, jid, Hosted, Wiring, QUIET},
};

const SERVICE: &str = "spaces.example.com";
const GUEST: &str = "guest@home-b.test";
const STRANGER: &str = "mallory@home-b.test";

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and SpiceDB"]
async fn live_spicedb_protected_space_deletion_reaches_the_remote_subscription_over_tls() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_remote_space_deletion").await;
    let fixture = &live.fixture;
    let lifecycle = fixture.scope.lifecycle;
    let node = fixture.scope.space_node.as_str();
    let service = bare(SERVICE);
    let authority = live.replica().await;
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("initial projection"),
        Reconciliation::Activated(_)
    ));
    let wiring = Wiring::new(|spec| spec.local.push(SERVICE)).await;
    let backend = PermissionActor::from_server_config(&live.config)
        .await
        .expect("SpiceDB permission backend");
    let replica = Replica::start_running_with_backend(
        fixture.ingress.db.database_url(),
        fixture.ingress.deployment_uuid(),
        &wiring.config,
        backend,
    )
    .await;
    replica.account("owner").await;
    let storage = &replica.state.deps.protocol.pubsub_storage;
    storage
        .get_or_create_node(&service, node)
        .await
        .expect("materialize Space");
    storage
        .update_node_config(
            &service,
            node,
            &NodeConfig {
                node_type: Some(PubSubNodeType::Space),
                ..NodeConfig::spaces_private()
            },
        )
        .await
        .expect("enrolled Space configuration");
    fixture
        .store
        .bind_space(
            &BindSpace {
                operation: GrantOperationId::generate(),
                lifecycle,
                expected_generation: fixture.generation().await,
                owner: subject(OWNER),
                federated: true,
            },
            &served(),
        )
        .await
        .expect("bind enrolled Space");
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("binding projection"),
        Reconciliation::Activated(_)
    ));

    let hosted = Hosted::serve(wiring, Arc::clone(&replica.state), RouterLimits::default()).await;
    host_spaces_for_remote_principals(&replica.state, &hosted.router);
    let mut owner = replica.client("owner", "desk").await;
    let granted = round_trip(
        &mut owner,
        set(
            "grant-remote-space-membership",
            SERVICE,
            pubsub_owner_affiliations(node, &[(GUEST, "member")]),
        ),
    )
    .await;
    assert!(is_result(&granted), "{granted:?}");

    for (id, recipient) in [("guest-subscribe", GUEST), ("stranger-subscribe", STRANGER)] {
        let mut request = set(id, SERVICE, pubsub_subscribe(node, recipient));
        if let xmpp_parsers::iq::Iq::Set { from, .. } = &mut request {
            *from = Some(jid(&format!("{recipient}/phone")));
        }
        hosted.peer_sends_to(SERVICE, request).await;
        let reply = hosted.answered(id).await;
        if recipient == GUEST {
            assert_eq!(reply.attr("type"), Some("result"), "{reply:?}");
            let subscription = reply
                .get_child("pubsub", NS_PUBSUB)
                .and_then(|pubsub| pubsub.get_child("subscription", NS_PUBSUB))
                .expect("subscription result");
            assert_eq!(subscription.attr("jid"), Some(GUEST));
            assert_eq!(subscription.attr("node"), Some(node));
            assert_eq!(subscription.attr("subscription"), Some("subscribed"));
        } else {
            assert_eq!(error_condition(&reply).as_deref(), Some("item-not-found"));
        }
    }

    let stop = CancellationToken::new();
    let stop_on_exit = stop.clone().drop_guard();
    assert!(replica.state.deps.app_state.node_lifecycle.is_ready());
    spawn_remote_space_deletion_worker(&replica.state, hosted.router.clone(), stop);
    let before = hosted.received();
    let deleted = round_trip(
        &mut owner,
        set(
            "delete-enrolled-space",
            SERVICE,
            Element::builder("pubsub", NS_PUBSUB_OWNER)
                .append(
                    Element::builder("delete", NS_PUBSUB_OWNER)
                        .attr(xml_ncname!("node").to_owned(), node)
                        .build(),
                )
                .build(),
        ),
    )
    .await;
    assert!(is_result(&deleted), "{deleted:?}");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(fixture.ingress.db.database_url())
        .await
        .expect("durable readback pool");
    let operation: String = sqlx::query_scalar(
        "SELECT operation_id FROM host_grant_space_deletions WHERE lifecycle_id = $1",
    )
    .bind(lifecycle.to_storage())
    .fetch_one(&pool)
    .await
    .expect("recorded deletion operation");
    let event_id = waddle_xmpp::stream_management::space_deletion_notice_id(
        uuid::Uuid::parse_str(&operation).expect("canonical operation UUID"),
        &GUEST.parse().expect("guest bare JID"),
    );
    let is_node_deletion = |message: &Element| {
        message
            .get_child("event", NS_PUBSUB_EVENT)
            .and_then(|event| event.get_child("delete", NS_PUBSUB_EVENT))
            .is_some_and(|delete| delete.attr("node") == Some(node))
    };
    let notice = hosted
        .at_peer_after(before, is_node_deletion)
        .await
        .expect("the admitted peer receives the XEP-0060 deletion notice");
    assert!(
        matches_pubsub_delete_event(&notice, SERVICE, GUEST, node, &event_id),
        "{notice:?}"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let jobs: Vec<(String, String)> = sqlx::query_as(
                "SELECT subject_jid, state FROM host_grant_space_deletion_jobs
                 WHERE lifecycle_id = $1 AND delivery_kind = 'remote' ORDER BY subject_jid",
            )
            .bind(lifecycle.to_storage())
            .fetch_all(&pool)
            .await
            .expect("read exact recipient and attempt outcome");
            assert_eq!(jobs.len(), 1, "{jobs:?}");
            assert_eq!(jobs[0].0, GUEST);
            match jobs[0].1.as_str() {
                "custody" => return,
                "pending" | "leased" | "staging" => {}
                state => panic!("remote attempt settled as {state}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("TLS write settles the exact durable attempt to custody");
    tokio::time::sleep(QUIET).await;
    assert_eq!(hosted.count_at_peer(is_node_deletion), 1);
    assert_eq!(hosted.captured.with_id(&event_id), [notice]);

    drop(stop_on_exit);
    owner.close().await.expect("close owner connection");
    pool.close().await;
    drop((hosted, replica, authority));
    live.close().await;
}
