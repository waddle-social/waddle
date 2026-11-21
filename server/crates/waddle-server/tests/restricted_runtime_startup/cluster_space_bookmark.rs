use std::time::Duration;

use minidom::Element;
use waddle_ws_test_support::{require_test_postgres_url, TestServer, WsXmppClient};

use super::{
    attach_clustered_pool, keypair_pool, owner_database, provision_clustered, Prepared, DOMAIN,
};

#[path = "cluster_space_resume.rs"]
mod cluster_space_resume;
#[path = "../cluster_support/mod.rs"]
#[allow(dead_code)]
mod cluster_support;
#[path = "../host_grant_support/live_spicedb.rs"]
mod live_spicedb;

#[derive(Clone, Copy)]
enum Exercise {
    BookmarkEvent,
    Resume,
}

/// Two real, attested cluster processes share a protected Space, but the
/// publisher and subscriber each authenticate to a different WebSocket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL and SpiceDB"]
async fn live_spicedb_cluster_delivers_protected_space_bookmark_over_websocket() {
    exercise(Exercise::BookmarkEvent).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL and SpiceDB"]
async fn live_spicedb_cluster_resumes_protected_space_reply_over_websocket() {
    exercise(Exercise::Resume).await;
}

async fn exercise(exercise: Exercise) {
    use kameo::actor::Spawn;
    use waddle_server::config::SpiceDbConfig;
    use waddle_server::permissions::host_grant::{
        BindSpace, BootstrapGrant, EnrollScope, GrantOperationId, GrantSubject, HostGrantAuthority,
        HostGrantStore, IssuancePolicy, ManagedRoom, Reconciliation, RoomAffiliation, ScopeKey,
        ScopeLifecycleId,
    };
    use waddle_server::permissions::{
        EnsureAuthzCodecIncarnation, EnsureSchema, PermissionActor, RecordAuthzSchemaVersion,
    };
    use waddle_server::pubsub::DatabasePubSubStorage;
    use waddle_ws_test_support::{
        iq::round_trip,
        stanza::{is_result, pubsub_publish, pubsub_subscribe, result_payload, set},
    };
    use waddle_xmpp::identity::CanonicalDomain;
    use waddle_xmpp::pubsub::{NodeConfig, PubSubStorage, NS_PUBSUB, NS_PUBSUB_EVENT};
    use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};

    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let (endpoint, key) = live_spicedb::disposable_spicedb_env();
    let client = prescience::Client::builder(&endpoint, &key)
        .insecure(true)
        .build()
        .await
        .expect("connect disposable SpiceDB");
    live_spicedb::empty_disposable_store(&client).await;

    let keypairs = keypair_pool(2);
    let prepared =
        Prepared::from_provisioned(provision_clustered(&url, &keypairs).await, &url).await;
    let db = owner_database(&prepared.owner_url).await;
    let store = HostGrantStore::open(db.clone()).expect("owner grant store");
    let mut config = waddle_server::ServerConfig::test_homeserver();
    config.spicedb = Some(SpiceDbConfig {
        endpoint: endpoint.clone(),
        preshared_key: key.clone(),
        insecure: true,
    });
    let permissions = PermissionActor::spawn(
        PermissionActor::from_server_config(&config)
            .await
            .expect("live SpiceDB backend")
            .with_host_grant_store(store.clone()),
    );
    permissions.ask(EnsureSchema).await.expect("SpiceDB schema");
    permissions
        .ask(EnsureAuthzCodecIncarnation)
        .await
        .expect("codec incarnation");
    permissions
        .ask(RecordAuthzSchemaVersion)
        .await
        .expect("authz schema version");
    live_spicedb::mark_test_store(&client).await;

    let domain = |input| CanonicalDomain::parse_exact(input).expect("canonical fixture domain");
    let policy = IssuancePolicy::new(
        [domain(DOMAIN)],
        [domain("muc.localhost"), domain("spaces.localhost")],
        std::iter::empty(),
    )
    .expect("fixture issuance policy");
    let subject = |address: &str| {
        GrantSubject::new(address.parse().expect("account JID")).expect("canonical subject")
    };
    let room: jid::BareJid = "cluster-bookmark@muc.localhost".parse().expect("room JID");
    let service: jid::BareJid = "spaces.localhost".parse().expect("spaces service");
    let node = "cluster-bookmarks";
    let lifecycle = ScopeLifecycleId::generate();
    let runtime = attach_clustered_pool(&prepared).await;
    let pubsub = DatabasePubSubStorage::attach_existing(runtime.global().clone())
        .await
        .expect("attached PubSub");
    pubsub
        .get_or_create_node(&service, node)
        .await
        .expect("materialize Space");
    pubsub
        .update_node_config(&service, node, &NodeConfig::spaces_private())
        .await
        .expect("private Space");
    let authority = HostGrantAuthority::new(store.clone(), permissions, policy.clone());
    let enrolled = authority
        .enroll_scope(&EnrollScope {
            operation: GrantOperationId::generate(),
            lifecycle,
            key: ScopeKey::new(service.clone(), node).expect("Space key"),
            rooms: vec![ManagedRoom::new(room.clone()).expect("managed room")],
            bootstrap: vec![
                BootstrapGrant {
                    room: room.clone(),
                    subject: subject("admin@localhost"),
                    affiliation: RoomAffiliation::Owner,
                },
                BootstrapGrant {
                    room: room.clone(),
                    subject: subject("alice@localhost"),
                    affiliation: RoomAffiliation::Member,
                },
            ],
        })
        .await
        .expect("enroll canonical grants");
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("project room grants"),
        Reconciliation::Activated(_)
    ));
    store
        .bind_space(
            &BindSpace {
                operation: GrantOperationId::generate(),
                lifecycle,
                expected_generation: enrolled.record().generation,
                owner: subject("admin@localhost"),
                federated: true,
            },
            policy.served_account_domains(),
        )
        .await
        .expect("bind canonical Space owner");
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("project Space owner"),
        Reconciliation::Activated(_)
    ));
    drop((authority, store, pubsub, db, runtime));

    let listener_a = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve node A address");
    let listener_b = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve node B address");
    let port_a = listener_a.local_addr().expect("node A address").port();
    let port_b = listener_b.local_addr().expect("node B address").port();
    drop((listener_a, listener_b));
    let env = |port, peer_port| {
        let mut env = prepared.runtime_env();
        env.extend([
            ("WADDLE_CLUSTERING_ENABLED", "true".to_owned()),
            (
                "WADDLE_CLUSTERING_LISTEN_ADDRS",
                format!("/ip4/127.0.0.1/tcp/{port}"),
            ),
            (
                "WADDLE_CLUSTERING_BOOTSTRAP_PEERS",
                format!("localhost:{peer_port}"),
            ),
            ("WADDLE_CLUSTERING_KEYPAIR_POOL", keypairs.seeds.join(",")),
            ("WADDLE_CLUSTERING_DIAL_INTERVAL_MS", "1000".to_owned()),
            ("WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS", "1000".to_owned()),
            ("WADDLE_SPICEDB_ENDPOINT", endpoint.clone()),
            ("WADDLE_SPICEDB_PRESHARED_KEY", key.clone()),
            ("WADDLE_SPICEDB_INSECURE", "true".to_owned()),
        ]);
        if matches!(exercise, Exercise::Resume) {
            env.push(("WADDLE_XMPP_DURABLE_SM", "durable".to_owned()));
        }
        env
    };
    let spawn = |env: Vec<(&'static str, String)>| {
        tokio::task::spawn_blocking(move || {
            let env: Vec<_> = env
                .iter()
                .map(|(key, value)| (*key, value.as_str()))
                .collect();
            TestServer::start_with_extra_envs(
                &[
                    ("alice", "alice-password"),
                    (
                        cluster_support::CLUSTER_PEER_USERNAME,
                        cluster_support::CLUSTER_PEER_PASSWORD,
                    ),
                ],
                &env,
            )
        })
    };
    let a = spawn(env(port_a, port_b))
        .await
        .expect("start runtime node A");
    let b = spawn(env(port_b, port_a))
        .await
        .expect("start runtime node B");
    assert_ne!(a.ws_url(), b.ws_url());
    cluster_support::wait_for_readiness(&a, true, Duration::from_secs(15)).await;
    cluster_support::wait_for_readiness(&b, true, Duration::from_secs(15)).await;
    cluster_support::wait_for_cross_node_stanza(
        &a,
        &b,
        ("admin", b.fixed_account_password()),
        (
            cluster_support::CLUSTER_PEER_USERNAME,
            cluster_support::CLUSTER_PEER_PASSWORD,
        ),
    )
    .await;

    let mut owner = WsXmppClient::connect_and_auth(
        &a.ws_url(),
        DOMAIN,
        "admin",
        b.fixed_account_password(),
        "space-owner",
    )
    .await
    .expect("owner on node A");
    let mut subscriber = WsXmppClient::connect_and_auth(
        &b.ws_url(),
        DOMAIN,
        "alice",
        "alice-password",
        "space-subscriber",
    )
    .await
    .expect("subscriber on node B");
    let affiliations = waddle_ws_test_support::stanza::pubsub_owner_affiliations(
        node,
        &[("alice@localhost", "member")],
    );
    let granted = round_trip(
        &mut owner,
        set("space-grant", service.as_str(), affiliations),
    )
    .await;
    assert!(is_result(&granted), "canonical membership: {granted:?}");
    if matches!(exercise, Exercise::Resume) {
        cluster_space_resume::assert_protected_resume(
            &prepared.owner_url,
            [&a, &b],
            &mut owner,
            subscriber,
            &service,
            node,
            lifecycle,
        )
        .await;
        owner.close().await.expect("close owner");
        drop((a, b));
        prepared.cleanup().await;
        return;
    }

    let subscribed = round_trip(
        &mut subscriber,
        set(
            "space-subscribe",
            service.as_str(),
            pubsub_subscribe(node, "alice@localhost"),
        ),
    )
    .await;
    assert!(
        result_payload(&subscribed)
            .and_then(|pubsub| pubsub.get_child("subscription", NS_PUBSUB))
            .is_some_and(|subscription| subscription.attr("subscription") == Some("subscribed")),
        "{subscribed:?}"
    );

    let bookmark =
        build_bookmark_element(&Bookmark::new(room.clone()).with_name("Cross-node bookmark"));
    let published = round_trip(
        &mut owner,
        set(
            "space-publish",
            service.as_str(),
            pubsub_publish(node, room.as_str(), bookmark.clone()),
        ),
    )
    .await;
    assert!(is_result(&published), "{published:?}");
    let expected_recipient = subscriber.full_jid.clone().expect("bound subscriber");
    subscriber
        .recv_matching_within(Duration::from_secs(20), |wire| {
            wire.parse::<Element>().is_ok_and(|message| {
                message.attr("from") == Some(service.as_str())
                    && message.attr("to") == Some(expected_recipient.as_str())
                    && message
                        .get_child("event", NS_PUBSUB_EVENT)
                        .and_then(|event| event.get_child("items", NS_PUBSUB_EVENT))
                        .filter(|items| items.attr("node") == Some(node))
                        .and_then(|items| items.get_child("item", NS_PUBSUB_EVENT))
                        .is_some_and(|item| {
                            item.attr("id") == Some(room.as_str())
                                && item.children().next() == Some(&bookmark)
                        })
            })
        })
        .await
        .expect("node B delivers node A's protected bookmark to the subscribed socket");

    subscriber.close().await.expect("close subscriber");
    owner.close().await.expect("close owner");
    drop((a, b));
    prepared.cleanup().await;
}
