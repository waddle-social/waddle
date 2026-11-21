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
    Deletion,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL and SpiceDB"]
async fn live_spicedb_cluster_delivers_protected_space_deletion_over_websocket() {
    exercise(Exercise::Deletion).await;
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

    if matches!(exercise, Exercise::Deletion) {
        use waddle_ws_test_support::iq::send_stanza;
        use waddle_ws_test_support::stanza::{get, is_error};
        use waddle_xmpp::pubsub::NS_PUBSUB_OWNER;
        use xmpp_parsers::stanza_error::DefinedCondition;

        let delete_request = |id| {
            set(
                id,
                service.as_str(),
                Element::builder("pubsub", NS_PUBSUB_OWNER)
                    .append(
                        Element::builder("delete", NS_PUBSUB_OWNER)
                            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                            .build(),
                    )
                    .build(),
            )
        };
        let mut second_resource = WsXmppClient::connect_and_auth(
            &a.ws_url(),
            DOMAIN,
            "alice",
            "alice-password",
            "space-second-resource",
        )
        .await
        .expect("subscriber's other resource on node A");
        let mut negative_resource = WsXmppClient::connect_and_auth(
            &b.ws_url(),
            DOMAIN,
            "alice",
            "alice-password",
            "space-negative-resource",
        )
        .await
        .expect("subscriber's negative-priority resource");
        let available = |priority| {
            Element::builder("presence", "jabber:client")
                .append(
                    Element::builder("priority", "jabber:client")
                        .append(priority)
                        .build(),
                )
                .build()
        };
        for (client, priority) in [
            (&mut subscriber, "5"),
            (&mut second_resource, "1"),
            (&mut negative_resource, "-1"),
        ] {
            send_stanza(client, available(priority)).await;
            let barrier = round_trip(client, delete_request("member-delete-barrier")).await;
            assert!(
                is_error(&barrier, DefinedCondition::Forbidden),
                "{barrier:?}"
            );
        }
        let denied = round_trip(&mut subscriber, delete_request("member-delete")).await;
        assert!(is_error(&denied, DefinedCondition::Forbidden), "{denied:?}");
        let accepted = round_trip(&mut owner, delete_request("owner-delete")).await;
        assert!(
            is_result(&accepted) && result_payload(&accepted).is_none(),
            "{accepted:?}"
        );
        let deletion_for_node = |wire: &str| {
            wire.parse::<Element>().is_ok_and(|message| {
                message
                    .get_child("event", NS_PUBSUB_EVENT)
                    .and_then(|event| event.get_child("delete", NS_PUBSUB_EVENT))
                    .is_some_and(|delete| delete.attr("node") == Some(node))
            })
        };
        let wire = subscriber
            .recv_matching_within(Duration::from_secs(20), deletion_for_node)
            .await
            .expect("node B delivers node A's deletion to the subscribed socket");
        let second_wire = second_resource
            .recv_matching_within(Duration::from_secs(20), deletion_for_node)
            .await
            .expect("node A delivers the same notice to the other eligible resource");
        let notice: Element = wire.parse().expect("deletion notice");
        assert_eq!(
            notice,
            second_wire
                .parse::<Element>()
                .expect("second deletion notice")
        );
        let id = notice.attr("id").expect("stable deletion notice ID");
        let (operation, _) = id.split_once('-').expect("deletion notice ID");
        let operation = uuid::Uuid::parse_str(operation).expect("deletion operation ID");
        assert_eq!(
            waddle_xmpp::stream_management::space_deletion_notice_id(
                operation,
                &"alice@localhost".parse().expect("subscriber JID"),
            ),
            id,
        );
        assert!(
            waddle_xmpp_core::pubsub::stanzas::matches_pubsub_delete_event(
                &notice,
                service.as_str(),
                "alice@localhost",
                node,
                id,
            )
        );

        let readback = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&prepared.owner_url)
            .await
            .expect("connect custody readback");
        let operation_id = GrantOperationId::from_uuid(operation).to_storage();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let state = sqlx::query_scalar::<_, String>(
                    "SELECT j.state FROM host_grant_space_deletion_jobs j
                      JOIN host_grant_space_deletions d USING (lifecycle_id)
                      WHERE d.operation_id = $1 AND j.lifecycle_id = $2
                        AND j.subject_jid = 'alice@localhost' AND j.delivery_kind = 'local'",
                )
                .bind(&operation_id)
                .bind(lifecycle.to_storage())
                .fetch_optional(&readback)
                .await
                .expect("read deletion job");
                match state.as_deref() {
                    Some("custody") => break,
                    Some("ambiguous" | "refused") => panic!("deletion job settled as {state:?}"),
                    _ => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        })
        .await
        .expect("socket receipt settles the exact deletion job");
        readback.close().await;
        assert!(
            negative_resource
                .recv_matching_within(Duration::from_millis(500), deletion_for_node)
                .await
                .is_err(),
            "negative-priority resource received a deletion notice"
        );
        second_resource
            .close()
            .await
            .expect("close second resource");
        negative_resource
            .close()
            .await
            .expect("close negative-priority resource");

        let items = get(
            "deleted-space-items",
            service.as_str(),
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                        .build(),
                )
                .build(),
        );
        let gone = round_trip(&mut subscriber, items).await;
        assert!(is_error(&gone, DefinedCondition::Forbidden), "{gone:?}");
    }

    subscriber.close().await.expect("close subscriber");
    owner.close().await.expect("close owner");
    drop((a, b));
    prepared.cleanup().await;
}
