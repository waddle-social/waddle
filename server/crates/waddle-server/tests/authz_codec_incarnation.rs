//! The startup guard that binds a SpiceDB store to the `v2_` ID codec.
//!
//! The offline tests pin the marker, its classification and the set of
//! definitions the guard requires to be empty. The `live_spicedb_` tests run
//! the production backend against a disposable SpiceDB: an empty store is
//! marked, a store holding legacy relationships or another codec's marker is
//! refused, a marked store is accepted, and racing replicas write one marker.
#![cfg(feature = "test-support")]

use futures::StreamExt;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use prescience::{
    Client, Consistency, ObjectReference, Relationship, RelationshipFilter, RelationshipUpdate,
    SubjectReference,
};
use tokio::sync::Mutex;
use waddle_server::config::SpiceDbConfig;
use waddle_server::permissions::{
    classify_codec_markers, decode_object, guarded_resource_definitions, AuthzIncarnationError,
    AuthzUserJid, CodecMarkerRow, CodecMarkerState, EnsureAuthzCodecIncarnation, EnsureSchema,
    Object, ObjectType, PermissionActor, PermissionError, Relation, Subject, SubjectType, Tuple,
    WriteTuple, AUTHZ_CODEC_MARKER_DEFINITION, AUTHZ_CODEC_MARKER_ID,
    AUTHZ_CODEC_MARKER_READ_LIMIT, AUTHZ_CODEC_MARKER_RELATION, AUTHZ_CODEC_VERSION,
    AUTHZ_ID_WIRE_PREFIX,
};
use waddle_server::ServerConfig;

fn marker_row(resource_id: &str, relation: &str, subject_id: &str) -> CodecMarkerRow {
    CodecMarkerRow {
        resource_id: resource_id.to_string(),
        relation: relation.to_string(),
        subject_type: AUTHZ_CODEC_MARKER_DEFINITION.to_string(),
        subject_id: subject_id.to_string(),
        subject_relation: None,
    }
}

#[test]
fn marker_names_the_codec_version_of_the_wire_prefix() {
    assert_eq!(format!("{AUTHZ_CODEC_VERSION}_"), AUTHZ_ID_WIRE_PREFIX);
    assert_eq!(
        CodecMarkerRow::current(),
        marker_row(
            AUTHZ_CODEC_MARKER_ID,
            AUTHZ_CODEC_MARKER_RELATION,
            AUTHZ_CODEC_VERSION
        )
    );
    assert_eq!(AUTHZ_CODEC_MARKER_DEFINITION, "waddle_authz_codec");
    assert_eq!(AUTHZ_CODEC_MARKER_ID, "incarnation");
    assert_eq!(AUTHZ_CODEC_MARKER_RELATION, "codec");
    assert_eq!(AUTHZ_CODEC_VERSION, "v2");
}

#[test]
fn only_the_exact_current_marker_is_accepted() {
    let current = CodecMarkerRow::current();
    assert_eq!(classify_codec_markers(&[]), CodecMarkerState::Absent);
    assert_eq!(
        classify_codec_markers(std::slice::from_ref(&current)),
        CodecMarkerState::Current
    );

    let mut with_subject_relation = current.clone();
    with_subject_relation.subject_relation = Some(AUTHZ_CODEC_MARKER_RELATION.to_string());
    let mut other_subject_type = current.clone();
    other_subject_type.subject_type = "user".to_string();
    for rows in [
        vec![marker_row(
            AUTHZ_CODEC_MARKER_ID,
            AUTHZ_CODEC_MARKER_RELATION,
            "v3",
        )],
        vec![marker_row(
            AUTHZ_CODEC_MARKER_ID,
            AUTHZ_CODEC_MARKER_RELATION,
            "v1",
        )],
        vec![marker_row("other", AUTHZ_CODEC_MARKER_RELATION, "v2")],
        vec![with_subject_relation],
        vec![other_subject_type],
        vec![current.clone(), current.clone()],
        vec![
            current.clone(),
            marker_row(AUTHZ_CODEC_MARKER_ID, AUTHZ_CODEC_MARKER_RELATION, "v3"),
        ],
    ] {
        assert_eq!(
            classify_codec_markers(&rows),
            CodecMarkerState::Incompatible,
            "{rows:?}"
        );
    }
}

#[test]
fn guard_requires_every_waddle_definition_to_be_empty() {
    let guarded: Vec<&str> = guarded_resource_definitions().collect();
    assert!(!guarded.contains(&AUTHZ_CODEC_MARKER_DEFINITION));
    for object_type in [
        ObjectType::Server,
        ObjectType::Space,
        ObjectType::Channel,
        ObjectType::Message,
        ObjectType::Dm,
        ObjectType::Role,
    ] {
        let name = object_type.to_string();
        assert!(guarded.contains(&name.as_str()), "{name} is not guarded");
    }
    for subject_type in [
        SubjectType::User,
        SubjectType::Space,
        SubjectType::Channel,
        SubjectType::Role,
    ] {
        let name = subject_type.to_string();
        assert!(guarded.contains(&name.as_str()), "{name} is not guarded");
    }
    let mut unique = guarded.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), guarded.len(), "{guarded:?}");
}

#[test]
fn legacy_ids_can_equal_current_wire_ids() {
    // Why an unmarked store cannot be read: the legacy substitution left IDs
    // without `@` or `.` unchanged, so a legacy channel named `v2_c2t5`
    // decodes as the current encoding of `sky`.
    assert_eq!(
        decode_object(ObjectType::Channel, "v2_c2t5").expect("valid v2 wire ID"),
        Object::new(ObjectType::Channel, "sky")
    );
}

#[test]
fn refusals_name_at_most_a_definition() {
    let error = AuthzIncarnationError::UnmarkedStoreNotEmpty {
        resource_type: "channel",
    };
    let message = PermissionError::from(error).to_string();
    assert!(message.contains("`channel`"), "{message}");
    for error in [
        AuthzIncarnationError::IncompatibleMarker,
        AuthzIncarnationError::Contended,
    ] {
        let message = PermissionError::from(error).to_string();
        assert!(!message.contains('@'), "{message}");
    }
}

#[test]
fn account_subjects_require_parser_canonical_parts() {
    let subject =
        AuthzUserJid::from_account_parts("alice", "example.org").expect("canonical account");
    assert_eq!(
        subject,
        AuthzUserJid::parse_exact("alice@example.org").expect("exact JID")
    );
    for (username, domain) in [
        ("Alice", "example.org"),
        ("alice", "Example.org"),
        ("ALICE", "EXAMPLE.ORG"),
    ] {
        assert_eq!(
            AuthzUserJid::from_account_parts(username, domain),
            Err(waddle_server::permissions::AuthzIdError::UserJidChangedByParser),
            "{username:?} {domain:?}"
        );
    }
    for (username, domain) in [("", "example.org"), ("a@b", "example.org"), ("a", "")] {
        assert!(
            AuthzUserJid::from_account_parts(username, domain).is_err(),
            "{username:?} {domain:?}"
        );
    }
    let long = "a".repeat(700);
    assert!(AuthzUserJid::from_account_parts(&long, "example.org").is_err());
}

// Live tests share one SpiceDB and each replaces its whole content, so they
// run one at a time: this lock serializes them within the binary, and the
// `spicedb-live` nextest group serializes them across binaries.
static LIVE_SPICEDB: Mutex<()> = Mutex::const_new(());

struct LiveSpiceDb {
    config: ServerConfig,
    client: Client,
}

impl LiveSpiceDb {
    async fn connect() -> Self {
        let endpoint =
            std::env::var("WADDLE_TEST_SPICEDB_ENDPOINT").expect("WADDLE_TEST_SPICEDB_ENDPOINT");
        let preshared_key = std::env::var("WADDLE_TEST_SPICEDB_PRESHARED_KEY")
            .expect("WADDLE_TEST_SPICEDB_PRESHARED_KEY");
        let client = Client::builder(&endpoint, &preshared_key)
            .insecure(true)
            .build()
            .await
            .expect("raw SpiceDB client");
        let mut config = ServerConfig::test_homeserver();
        config.spicedb = Some(SpiceDbConfig {
            endpoint,
            preshared_key,
            insecure: true,
        });
        let live = Self { config, client };
        // Write the built-in schema through the production path, then empty
        // every definition it declares.
        live.actor()
            .await
            .ask(EnsureSchema)
            .await
            .expect("built-in schema");
        for definition in
            guarded_resource_definitions().chain(std::iter::once(AUTHZ_CODEC_MARKER_DEFINITION))
        {
            live.client
                .delete_relationships(RelationshipFilter::new(definition))
                .await
                .expect("empty definition");
        }
        live
    }

    /// A fresh production backend, as one replica would build at startup.
    async fn actor(&self) -> ActorRef<PermissionActor> {
        let backend = PermissionActor::from_server_config(&self.config)
            .await
            .expect("production SpiceDB backend");
        assert_eq!(backend.backend_name(), "spicedb");
        PermissionActor::spawn(backend)
    }

    async fn marker_rows(&self) -> Vec<CodecMarkerRow> {
        let mut stream = self
            .client
            .read_relationships(RelationshipFilter::new(AUTHZ_CODEC_MARKER_DEFINITION))
            .consistency(Consistency::FullyConsistent)
            .limit(AUTHZ_CODEC_MARKER_READ_LIMIT)
            .send()
            .await
            .expect("read markers");
        let mut rows = Vec::new();
        while let Some(item) = stream.next().await {
            let relationship = item.expect("marker row").relationship;
            rows.push(CodecMarkerRow {
                resource_id: relationship.resource.object_id().to_string(),
                relation: relationship.relation,
                subject_type: relationship.subject.object().object_type().to_string(),
                subject_id: relationship.subject.object().object_id().to_string(),
                subject_relation: relationship
                    .subject
                    .optional_relation()
                    .map(ToString::to_string),
            });
        }
        rows
    }

    /// Write a relationship verbatim, bypassing the codec.
    async fn write_raw(&self, resource: (&str, &str), relation: &str, subject: (&str, &str)) {
        let relationship = Relationship::new(
            ObjectReference::new(resource.0, resource.1).expect("resource"),
            relation,
            SubjectReference::new(
                ObjectReference::new(subject.0, subject.1).expect("subject"),
                None::<String>,
            )
            .expect("subject reference"),
        );
        self.client
            .write_relationships(vec![RelationshipUpdate::touch(relationship)])
            .await
            .expect("raw write");
    }
}

async fn guard(actor: &ActorRef<PermissionActor>) -> Result<(), AuthzIncarnationError> {
    match actor.ask(EnsureAuthzCodecIncarnation).await {
        Ok(()) => Ok(()),
        Err(SendError::HandlerError(PermissionError::AuthzIncarnation(error))) => Err(error),
        Err(other) => panic!("guard failed outside the incarnation check: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_marks_an_empty_store() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    assert!(live.marker_rows().await.is_empty());

    guard(&live.actor().await).await.expect("empty store");
    assert_eq!(live.marker_rows().await, vec![CodecMarkerRow::current()]);
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_accepts_a_marked_store_with_relationships() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    let first = live.actor().await;
    guard(&first).await.expect("empty store");
    first
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "general"),
                Relation::new("member"),
                Subject::user("alice@example.org"),
            ),
        })
        .await
        .expect("grant after marking");

    // A restarted replica finds the marker and serves the existing grants.
    guard(&live.actor().await).await.expect("marked store");
    assert_eq!(live.marker_rows().await, vec![CodecMarkerRow::current()]);
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_refuses_an_unmarked_store_with_legacy_rows() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    // A ban written by the replaced encoding. Its channel ID also reads as
    // the current encoding of `sky`.
    live.write_raw(
        ("channel", "v2_c2t5"),
        "outcast",
        ("user", "alice|example=com"),
    )
    .await;

    assert_eq!(
        guard(&live.actor().await).await,
        Err(AuthzIncarnationError::UnmarkedStoreNotEmpty {
            resource_type: "channel"
        })
    );
    assert!(live.marker_rows().await.is_empty(), "store was marked");
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_refuses_another_codec_version() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    live.write_raw(
        (AUTHZ_CODEC_MARKER_DEFINITION, AUTHZ_CODEC_MARKER_ID),
        AUTHZ_CODEC_MARKER_RELATION,
        (AUTHZ_CODEC_MARKER_DEFINITION, "v3"),
    )
    .await;

    assert_eq!(
        guard(&live.actor().await).await,
        Err(AuthzIncarnationError::IncompatibleMarker)
    );
    assert_eq!(
        live.marker_rows().await,
        vec![marker_row(
            AUTHZ_CODEC_MARKER_ID,
            AUTHZ_CODEC_MARKER_RELATION,
            "v3"
        )]
    );
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_racing_replicas_write_one_marker() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    let replicas = [live.actor().await, live.actor().await, live.actor().await];

    let results = futures::future::join_all(replicas.iter().map(guard)).await;
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert_eq!(live.marker_rows().await, vec![CodecMarkerRow::current()]);
}
