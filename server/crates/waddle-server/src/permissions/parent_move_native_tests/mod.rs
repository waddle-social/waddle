//! Characterizes production inherited graph access during a Space parent move.
//! Every observation is a separate fresh read: there is no common snapshot,
//! admission fence, socket exchange, MUC or MAM policy claim.
use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use jid::{BareJid, FullJid, Jid};
use kameo::actor::{ActorRef, Spawn};
use minidom::Element;
use tokio::time::timeout;
use waddle_native_fixture::fixture::Fixture;
use waddle_xmpp::muc::room_registry_actor::RoomRegistryActor;
use waddle_xmpp::pubsub::{PubSubItem, PubSubStorage};
use waddle_xmpp::xep::xep0317::ServerHats;
use waddle_xmpp::xep::xep0402::{build_bookmark_item, Bookmark};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::pubsub::pubsub::{Item, PubSub, Publish};
use xmpp_parsers::pubsub::{ItemId, NodeName};

use super::{
    CheckPermission, DeleteTuple, ListSubjects, Object, ObjectType, Permission, PermissionActor,
    Relation, Subject, SubjectType, Tuple, WriteTuple,
};
use crate::channel_space_links::{ChannelSpaceLinkStore, DatabaseChannelSpaceLinkStore};
use crate::clustering::{ClusteringHandles, NodeLifecycle};
use crate::config::{LineageConfig, ServerConfig, SpiceDbConfig};
use crate::db::{DatabaseConfig, DatabaseDriver, DatabasePool, MigrationRunner, PoolConfig};
use crate::inbox::DatabaseInboxStorage;
use crate::pubsub::DatabasePubSubStorage;
use crate::server::routes::websocket::test_state::{
    create_test_session, create_test_websocket_state_with_extension_manager,
    dispatch_native_bound_iq, empty_extension_manager, TestStateOverrides,
};
use crate::server::routes::websocket::WebSocketState;
use crate::server::{AppState, AppStateDeps};
use crate::spaces_metadata::DatabaseSpacesMetadataStore;
use crate::storage::LocalStorage;

mod storage;
use storage::{CaptureBarrier, CapturingStorage};
const BUDGET: Duration = Duration::from_secs(15);
const PUBSUB_NS: &str = waddle_xmpp::pubsub::NS_PUBSUB;

struct Harness {
    state: Arc<WebSocketState>,
    database: Arc<DatabasePubSubStorage>,
    links: Arc<DatabaseChannelSpaceLinkStore>,
    observer: ActorRef<PermissionActor>,
    barrier: Arc<CaptureBarrier>,
    nodes: [String; 3],
    channel: String,
    room: BareJid,
    owner: BareJid,
    item: PubSubItem,
}

fn membership(node: &str, user: &str) -> Tuple {
    Tuple::new(
        Object::new(ObjectType::Space, node),
        Relation::new("member"),
        Subject::user(user),
    )
}
async fn write(actor: &ActorRef<PermissionActor>, tuple: Tuple) {
    actor
        .ask(WriteTuple { tuple })
        .await
        .expect("production tuple write ACK");
}
async fn allowed(actor: &ActorRef<PermissionActor>, channel: &str, user: &str) -> bool {
    actor
        .ask(CheckPermission {
            subject: Subject::user(user),
            permission: Permission::Read,
            object: Object::new(ObjectType::Channel, channel),
        })
        .await
        .expect("fresh fully consistent production check")
        .allowed
}
impl Harness {
    async fn new(fixture: &Fixture) -> Self {
        let settings = &fixture.settings;
        settings.validate().expect("private fixture identity");
        let mut uri =
            url::Url::parse("postgresql://fence_app@localhost/fence_app").expect("fixed URI");
        uri.set_password(Some(&settings.app_password))
            .expect("generated password");
        uri.query_pairs_mut()
            .append_pair(
                "host",
                settings.root.join("socket").to_str().expect("socket UTF8"),
            )
            .append_pair("sslmode", "disable");
        let database_url = uri.as_str();
        let pool = Arc::new(
            DatabasePool::try_new(
                DatabaseConfig::new(DatabaseDriver::Postgres, database_url),
                PoolConfig,
            )
            .await
            .expect("real global PostgreSQL"),
        );
        assert_eq!(pool.global().driver(), DatabaseDriver::Postgres);
        MigrationRunner::single()
            .run(pool.global())
            .await
            .expect("production migrations");
        let database = Arc::new(
            DatabasePubSubStorage::open(Some(database_url))
                .await
                .expect("real PostgreSQL PubSub"),
        );
        let links = Arc::new(
            DatabaseChannelSpaceLinkStore::open(Some(database_url))
                .await
                .expect("real PostgreSQL links"),
        );
        assert_eq!(database.database().driver(), DatabaseDriver::Postgres);
        assert_eq!(links.database().driver(), DatabaseDriver::Postgres);
        let mut config = ServerConfig::test_homeserver();
        config.spicedb = Some(SpiceDbConfig {
            endpoint: format!("http://127.0.0.1:{}", settings.grpc_port),
            preshared_key: settings.preshared_key.clone(),
            insecure: true,
        });
        let backend = PermissionActor::from_server_config(&config)
            .await
            .expect("production SpiceDB backend");
        assert_eq!(backend.backend_name(), "spicedb");
        let actor = PermissionActor::spawn(backend);
        let backend = PermissionActor::from_server_config(&config)
            .await
            .expect("independent production SpiceDB backend");
        assert_eq!(backend.backend_name(), "spicedb");
        let observer = PermissionActor::spawn(backend);
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let nodes = [
            format!("old-{tag}"),
            format!("new-{tag}"),
            format!("independent-{tag}"),
        ];
        let channel = format!("moving-{tag}");
        let room: BareJid = format!("{channel}@muc.example.com")
            .parse()
            .expect("managed room");
        let owner: BareJid = "spaces.example.com".parse().expect("service");
        let item = build_bookmark_item(&Bookmark::new(room.clone()).with_name("frozen bookmark"));
        let barrier = Arc::new(CaptureBarrier::new());
        let decorated: Arc<dyn PubSubStorage> = Arc::new(CapturingStorage {
            inner: database.clone(),
            barrier: barrier.clone(),
            owner: owner.clone(),
            node: nodes[0].clone(),
            item_id: room.to_string(),
        });
        let secret = crate::config::test_occupant_id_secret();
        let registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
            "muc.example.com".to_owned(),
            secret.clone(),
        ));
        let app = Arc::new(AppState::new_with_deps(AppStateDeps {
            db_pool: pool.clone(),
            blob_storage: Arc::new(LocalStorage::new(settings.root.join("blobs"))),
            inbox_storage: Arc::new(
                DatabaseInboxStorage::from_database(pool.global().clone())
                    .await
                    .expect("PG inbox"),
            ),
            spaces_metadata_store: Arc::new(
                DatabaseSpacesMetadataStore::open(Some(database_url))
                    .await
                    .expect("PG metadata"),
            ),
            channel_space_link_store: links.clone(),
            pubsub_storage: decorated.clone(),
            room_registry: registry,
            spaces_jid: owner.clone(),
            muc_domain: "muc.example.com".parse().expect("MUC domain"),
            occupant_id_secret: secret,
            server_hats: ServerHats::default(),
            permission_actor: actor,
            server_owner_jids: Arc::from(Vec::<BareJid>::new()),
            node_lifecycle: NodeLifecycle::new(),
            clustering_claims: ClusteringHandles::default(),
            lineage_config: LineageConfig::default(),
            clustering_enabled: false,
        }));
        app.seal_lineage_registry();
        let state = create_test_websocket_state_with_extension_manager(
            empty_extension_manager().await,
            TestStateOverrides {
                db_pool: Some(pool),
                app_state: Some(app),
                database_pubsub_storage: Some(database.clone()),
                share_app_room_registry: true,
                ..Default::default()
            },
        )
        .await;
        assert!(Arc::ptr_eq(
            &state.deps.app_state.pubsub_storage,
            &state.deps.protocol.pubsub_storage
        ));
        let connection = state
            .deps
            .app_state
            .db_pool
            .global()
            .guard()
            .await
            .expect("PG connection");
        connection.execute("INSERT INTO channels (id, name, description, channel_type, position, is_default) VALUES (?, ?, ?, 'text', 0, 0)", crate::db_params![&channel, "Moving", "test-only"]).await.expect("actual channel catalog row");
        drop(connection);
        for node in &nodes {
            database
                .get_or_create_node(&owner, node)
                .await
                .expect("real node");
            write(
                &observer,
                Tuple::new(
                    Object::new(ObjectType::Space, node),
                    Relation::new("owner"),
                    Subject::user("operator@example.com"),
                ),
            )
            .await;
        }
        for (node, user) in [
            (&nodes[0], "old@example.com"),
            (&nodes[1], "new@example.com"),
        ] {
            write(&observer, membership(node, user)).await;
        }
        Self {
            state,
            database,
            links,
            observer,
            barrier,
            nodes,
            channel,
            room,
            owner,
            item,
        }
    }
    async fn publish(&self, node: &str, id: &str) {
        let session = create_test_session(&self.state, "operator").await;
        let bound: FullJid = "operator@example.com/web".parse().expect("bound");
        let payload = PubSub::Publish {
            publish: Publish {
                node: NodeName(node.to_owned()),
                items: vec![Item {
                    id: self.item.id.clone().map(ItemId),
                    publisher: None,
                    payload: self.item.payload.clone(),
                }],
            },
            publish_options: None,
        };
        let iq = Iq::from_set(id, payload)
            .with_from(Jid::from(bound.clone()))
            .with_to(Jid::from(self.owner.clone()));
        let replies = dispatch_native_bound_iq(&self.state, &session, &bound, iq).await;
        assert_eq!(replies.len(), 1, "one production response");
        let reply =
            Iq::try_from(replies[0].parse::<Element>().expect("response XML")).expect("typed IQ");
        assert_eq!(reply.id(), id);
        assert_eq!(reply.to(), Some(&Jid::from(bound)));
        assert_eq!(reply.from(), Some(&Jid::from(self.owner.clone())));
        let Iq::Result {
            payload: Some(payload),
            ..
        } = reply
        else {
            panic!("expected production publish result")
        };
        let publish = payload
            .get_child("publish", PUBSUB_NS)
            .expect("publish result");
        assert_eq!(publish.attr("node"), Some(node));
        assert_eq!(
            publish
                .get_child("item", PUBSUB_NS)
                .expect("result item")
                .attr("id"),
            self.item.id.as_deref()
        );
    }
    async fn observe(&self, parents: &[&str], old_bookmark: bool) {
        let mut actual = self
            .observer
            .ask(ListSubjects {
                object: Object::new(ObjectType::Channel, &self.channel),
                relation: Relation::new("parent"),
            })
            .await
            .expect("parent read");
        actual.sort_by(|a, b| a.id.cmp(&b.id));
        let mut expected: Vec<_> = parents
            .iter()
            .map(|node| Subject::userset(SubjectType::Space, *node, ""))
            .collect();
        expected.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(actual, expected, "exact direct parent closure");
        let mut bookmarked = self
            .database
            .list_node_names_for_item(&self.owner, &self.room.to_string())
            .await
            .expect("complete real bookmark placement");
        bookmarked.sort();
        let mut expected_nodes: Vec<_> = parents.iter().map(|node| (*node).to_owned()).collect();
        expected_nodes.sort();
        assert_eq!(bookmarked, expected_nodes);

        for (index, present) in [
            (0, old_bookmark),
            (1, parents.contains(&self.nodes[1].as_str())),
        ] {
            let rows = self
                .database
                .get_items(
                    &self.owner,
                    &self.nodes[index],
                    Some(1),
                    &[self.room.to_string()],
                )
                .await
                .expect("independent real committed item read");
            assert_eq!(rows.len(), usize::from(present));
            if present {
                assert_eq!(rows[0].to_pubsub_item().payload, self.item.payload);
                assert_eq!(rows[0].id, self.item.id.clone().expect("ID"));
            }
        }
        let link = self
            .links
            .get(&self.room)
            .await
            .expect("real link read")
            .expect("link present");
        assert_eq!(link.channel_jid, self.room);
        let expected_node = if parents.len() == 1 && old_bookmark {
            &self.nodes[0]
        } else {
            &self.nodes[1]
        };
        let expected_jid: BareJid = format!("{expected_node}@{}", self.owner)
            .parse()
            .expect("projected Space JID");
        assert_eq!(link.space_jid, expected_jid);

        assert_eq!(
            link.space_node.as_str(),
            if parents.len() == 1 && old_bookmark {
                &self.nodes[0]
            } else {
                &self.nodes[1]
            }
        );
    }
    async fn stop(&self) {
        for actor in [&self.state.deps.app_state.permission_actor, &self.observer] {
            actor
                .stop_gracefully()
                .await
                .expect("permission actor stop");
            timeout(BUDGET, actor.wait_for_shutdown())
                .await
                .expect("permission actor shutdown");
        }
        self.state
            .deps
            .protocol
            .user_registry
            .stop_gracefully()
            .await
            .expect("user registry stop");
        timeout(
            BUDGET,
            self.state.deps.protocol.user_registry.wait_for_shutdown(),
        )
        .await
        .expect("user registry shutdown");
        self.state
            .deps
            .app_state
            .room_registry
            .stop_gracefully()
            .await
            .expect("registry stop");
        timeout(
            BUDGET,
            self.state.deps.app_state.room_registry.wait_for_shutdown(),
        )
        .await
        .expect("registry shutdown");
    }
}
async fn characterize(revoke_before: bool) {
    let mut fixture = Fixture::new().expect("mandatory pinned native prerequisites");
    fixture
        .start("", super::schema::SPICEDB_SCHEMA)
        .await
        .expect("real PostgreSQL and production SpiceDB schema");
    let versions = fixture.versions().await.expect("pinned binary attestation");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let head_before = waddle_native_fixture::git_probe::head(root)
        .await
        .expect("exact HEAD");
    let status_before = waddle_native_fixture::status_capture::capture(root)
        .await
        .expect("bounded status digest");
    let harness = Harness::new(&fixture).await;
    let case = AssertUnwindSafe(timeout(Duration::from_secs(90), async {
        harness.publish(&harness.nodes[0], "initial-parent").await;
        harness.observe(&[&harness.nodes[0]], true).await;
        assert!(harness
            .observer
            .ask(ListSubjects {
                object: Object::new(ObjectType::Space, &harness.nodes[2]),
                relation: Relation::new("member")
            })
            .await
            .expect("independent pre-state")
            .is_empty());

        assert!(allowed(&harness.observer, &harness.channel, "old@example.com").await);
        assert!(!allowed(&harness.observer, &harness.channel, "new@example.com").await);
        if revoke_before {
            harness
                .observer
                .ask(DeleteTuple {
                    tuple: membership(&harness.nodes[0], "old@example.com"),
                })
                .await
                .expect("revocation ACK before move");
        }
        harness.barrier.armed.store(true, Ordering::SeqCst);
        let mover = harness.publish(&harness.nodes[1], "move-parent");
        let controller = async {
            timeout(BUDGET, harness.barrier.captured.notified())
                .await
                .expect("exact real stale-cleanup read");
            assert_eq!(harness.barrier.count.load(Ordering::SeqCst), 1);
            let frozen = harness
                .barrier
                .frozen
                .lock()
                .expect("captured rows")
                .clone()
                .expect("exact query completed");
            assert_eq!(frozen.len(), 1);
            assert_eq!(frozen[0].id, harness.item.id);
            assert_eq!(frozen[0].payload, harness.item.payload);
            harness
                .observe(&[&harness.nodes[0], &harness.nodes[1]], true)
                .await;
            assert_eq!(
                allowed(&harness.observer, &harness.channel, "old@example.com").await,
                !revoke_before
            );
            assert!(allowed(&harness.observer, &harness.channel, "new@example.com").await);
            if !revoke_before {
                harness
                    .observer
                    .ask(DeleteTuple {
                        tuple: membership(&harness.nodes[0], "old@example.com"),
                    })
                    .await
                    .expect("actual concurrent revocation ACK");
            }
            assert!(harness
                .observer
                .ask(ListSubjects {
                    object: Object::new(ObjectType::Space, &harness.nodes[0]),
                    relation: Relation::new("member")
                })
                .await
                .expect("fresh revoked relation read")
                .is_empty());
            assert!(!allowed(&harness.observer, &harness.channel, "old@example.com").await);
            write(
                &harness.observer,
                membership(&harness.nodes[2], "independent@example.com"),
            )
            .await;
            assert!(
                harness
                    .observer
                    .ask(CheckPermission {
                        subject: Subject::user("independent@example.com"),
                        permission: Permission::View,
                        object: Object::new(ObjectType::Space, &harness.nodes[2])
                    })
                    .await
                    .expect("independent scope fresh check")
                    .allowed
            );
            harness.barrier.release.notify_one();
        };
        tokio::join!(mover, controller);
        assert!(!harness.barrier.armed.load(Ordering::SeqCst));
        harness.observe(&[&harness.nodes[1]], false).await;
        assert!(!allowed(&harness.observer, &harness.channel, "old@example.com").await);
        assert!(allowed(&harness.observer, &harness.channel, "new@example.com").await);

        assert_eq!(
            harness
                .observer
                .ask(ListSubjects {
                    object: Object::new(ObjectType::Space, &harness.nodes[2]),
                    relation: Relation::new("member")
                })
                .await
                .expect("independent final state"),
            vec![Subject::user("independent@example.com")]
        );
        assert!(
            harness
                .observer
                .ask(CheckPermission {
                    subject: Subject::user("independent@example.com"),
                    permission: Permission::View,
                    object: Object::new(ObjectType::Space, &harness.nodes[2])
                })
                .await
                .expect("independent final fresh check")
                .allowed
        );
        for relation in [
            "owner",
            "admin",
            "member",
            "outcast",
            "manager",
            "moderator",
            "writer",
            "viewer",
        ] {
            assert!(harness
                .observer
                .ask(ListSubjects {
                    object: Object::new(ObjectType::Channel, &harness.channel),
                    relation: Relation::new(relation)
                })
                .await
                .expect("no direct channel grant read")
                .is_empty());
        }
    }))
    .catch_unwind()
    .await;
    harness.stop().await;
    drop(harness);
    let cleaned = fixture.shutdown().is_ok();
    let head_after = waddle_native_fixture::git_probe::head(root)
        .await
        .expect("HEAD after cleanup");
    let status_after = waddle_native_fixture::status_capture::capture(root)
        .await
        .expect("status after cleanup");
    assert_eq!(head_before, head_after, "source revision must not change");
    assert_eq!(status_before, status_after, "source status must not change");
    fixture.save("parent-move.json", &serde_json::json!({"case":if revoke_before {"revoke-before"} else {"revoke-during"},"completed":matches!(case,Ok(Ok(()))),"cleanup_verified":cleaned,"common_snapshot":false,"production_authority_selected":false,"git_head":head_before,"source_status":status_before,"formal_clean_tree":status_before.bytes==0,"versions":versions})).expect("secret-scanned evidence");
    eprintln!(
        "parent-move evidence: {}",
        fixture.evidence.join("parent-move.json").display()
    );
    assert!(cleaned, "verified native cleanup required");
    match case {
        Ok(result) => result.expect("bounded joined schedule"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
#[tokio::test]
#[ignore = "requires mandatory pinned native PostgreSQL and SpiceDB runner"]
async fn production_parent_move_revoke_before_cleanup() {
    characterize(true).await;
}
#[tokio::test]
#[ignore = "requires mandatory pinned native PostgreSQL and SpiceDB runner"]
async fn production_parent_move_revoke_during_cleanup() {
    characterize(false).await;
}
