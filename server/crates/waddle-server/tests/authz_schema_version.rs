//! The startup fence that binds a SpiceDB store to one version of the
//! built-in schema.
//!
//! The offline tests pin the marker, its classification, the schema text of
//! the current version, the text written while an older store is upgraded,
//! and the scan of a stored schema for version relations newer than the
//! marker. The `live_spicedb_` tests run the production backend against a
//! disposable SpiceDB: an unversioned store is installed and recorded, a
//! restart at the same version does not rewrite the schema, a newer or
//! unrecognised marker is refused without overwriting the schema, an upgrade
//! a later release left unfinished is refused without overwriting its schema,
//! SpiceDB itself refuses schema writes that would drop the marker, a store
//! the codec guard refuses keeps its schema and gains no version marker, and
//! racing replicas record one marker and all start. A store recorded at
//! version 1 is upgraded and recorded at this version, also by racing
//! replicas, and refuses a version 1 replica from the moment this release
//! has written its schema.
#![cfg(feature = "test-support")]

use futures::StreamExt;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use prescience::{
    Client, Consistency, ObjectReference, Relationship, RelationshipFilter, RelationshipUpdate,
    SubjectReference,
};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use waddle_server::config::SpiceDbConfig;
use waddle_server::permissions::host_grant::{
    HOST_GRANT_PROJECTION_DEFINITION, HOST_GRANT_PROJECTION_MARKER_RELATION,
};
use waddle_server::permissions::{
    authz_schema_retaining_version, authz_schema_version_relation, builtin_authz_schema,
    classify_schema_markers, declared_authz_schema_versions, guarded_resource_definitions,
    parse_authz_schema_version_relation, unfinished_upgrade_refusal, AuthzIncarnationError,
    AuthzSchemaVersionError, EnsureAuthzCodecIncarnation, EnsureSchema, PermissionActor,
    PermissionError, RecordAuthzSchemaVersion, SchemaMarkerRow, SchemaMarkerState,
    AUTHZ_CODEC_MARKER_DEFINITION, AUTHZ_CODEC_MARKER_ID, AUTHZ_CODEC_MARKER_RELATION,
    AUTHZ_CODEC_VERSION, AUTHZ_SCHEMA_MARKER_DEFINITION, AUTHZ_SCHEMA_MARKER_ID,
    AUTHZ_SCHEMA_MARKER_READ_LIMIT, AUTHZ_SCHEMA_VERSION, AUTHZ_SCHEMA_VERSION_RELATION_PREFIX,
};
use waddle_server::ServerConfig;

/// SHA-256 of the built-in schema text of [`AUTHZ_SCHEMA_VERSION`].
const BUILTIN_SCHEMA_SHA256: &str =
    "172ba6f744eb8163b8b6ed52ace7bf5d3cb6b30ecd2475eff4a4f9a4d63c77db";

/// SHA-256 of the built-in schema text of version 1, as the release that
/// introduced schema versioning pinned it.
const VERSION_1_SCHEMA_SHA256: &str =
    "ddc3ba07454b2100cff42101596c432d079b37311f60212b2d4c856dc1eae299";

/// The declaration of a marker relation in the built-in schema.
fn declaration(version: u32) -> String {
    format!(
        "relation {}: {AUTHZ_SCHEMA_MARKER_DEFINITION}",
        authz_schema_version_relation(version)
    )
}

/// The trimmed lines of one definition of `schema`.
fn definition_lines<'a>(schema: &'a str, name: &str) -> Vec<&'a str> {
    let header = format!("definition {name} {{");
    let mut lines = schema.lines().map(str::trim);
    assert!(
        lines.any(|line| line == header),
        "schema has no {name} definition"
    );
    lines.take_while(|line| *line != "}").collect()
}

/// `schema` without the schema-version marker definition: the schema of a
/// release built before schema versioning.
fn schema_before_versioning(schema: &str) -> String {
    let header = format!("definition {AUTHZ_SCHEMA_MARKER_DEFINITION} {{");
    let mut text = String::new();
    let mut inside = false;
    for line in schema.split_inclusive('\n') {
        if line.trim() == header {
            inside = true;
        } else if inside {
            inside = line.trim() != "}";
        } else {
            text.push_str(line);
        }
    }
    assert!(!text.contains(AUTHZ_SCHEMA_MARKER_DEFINITION));
    text
}

/// `schema` without the codec marker and schema-version marker definitions:
/// the schema of a release built before the ID codec guard.
fn schema_before_codec_guard(schema: &str) -> String {
    let header = format!("definition {AUTHZ_CODEC_MARKER_DEFINITION} {{");
    let mut text = String::new();
    let mut inside = false;
    for line in schema_before_versioning(schema).split_inclusive('\n') {
        if line.trim() == header {
            inside = true;
        } else if inside {
            inside = line.trim() != "}";
        } else {
            text.push_str(line);
        }
    }
    assert!(!text.contains(AUTHZ_CODEC_MARKER_DEFINITION));
    text
}

/// The built-in schema as a later release, recording `version`, would write it.
fn schema_of_later_release(version: u32) -> String {
    let schema = builtin_authz_schema();
    assert_eq!(
        schema.matches(&declaration(AUTHZ_SCHEMA_VERSION)).count(),
        1
    );
    schema.replace(&declaration(AUTHZ_SCHEMA_VERSION), &declaration(version))
}

/// The built-in schema of version 1: this build's schema without the
/// host-grant projection marker definition, which version 2 added, and with
/// the version 1 marker relation.
fn schema_of_version_1() -> String {
    let projection = format!(
        "definition {HOST_GRANT_PROJECTION_DEFINITION} {{\n  relation \
         {HOST_GRANT_PROJECTION_MARKER_RELATION}: {HOST_GRANT_PROJECTION_DEFINITION}\n}}\n\n"
    );
    let schema = builtin_authz_schema();
    assert_eq!(schema.matches(&projection).count(), 1);
    assert_eq!(
        schema.matches(&declaration(AUTHZ_SCHEMA_VERSION)).count(),
        1
    );
    schema
        .replacen(&projection, "", 1)
        .replace(&declaration(AUTHZ_SCHEMA_VERSION), &declaration(1))
}

fn row(resource_id: &str, relation: &str, subject_id: &str) -> SchemaMarkerRow {
    SchemaMarkerRow {
        resource_id: resource_id.to_string(),
        relation: relation.to_string(),
        subject_type: AUTHZ_SCHEMA_MARKER_DEFINITION.to_string(),
        subject_id: subject_id.to_string(),
        subject_relation: None,
    }
}

#[test]
fn marker_records_the_version_in_its_relation() {
    assert_eq!(AUTHZ_SCHEMA_MARKER_DEFINITION, "waddle_authz_schema");
    assert_eq!(AUTHZ_SCHEMA_MARKER_ID, "installed");
    assert_eq!(AUTHZ_SCHEMA_VERSION_RELATION_PREFIX, "version_");
    assert_eq!(AUTHZ_SCHEMA_VERSION, 2);
    assert_eq!(authz_schema_version_relation(1), "version_1");
    assert_eq!(authz_schema_version_relation(12), "version_12");
    assert_eq!(
        SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION),
        row(AUTHZ_SCHEMA_MARKER_ID, "version_2", AUTHZ_SCHEMA_MARKER_ID)
    );
}

#[test]
fn only_canonical_version_relations_parse() {
    assert_eq!(parse_authz_schema_version_relation("version_1"), Some(1));
    assert_eq!(parse_authz_schema_version_relation("version_10"), Some(10));
    assert_eq!(
        parse_authz_schema_version_relation("version_4294967295"),
        Some(u32::MAX)
    );
    for relation in [
        "version_0",
        "version_01",
        "version_+1",
        "version_-1",
        "version_",
        "version_1a",
        "version_4294967296",
        "v1",
        "codec",
        "Version_1",
    ] {
        assert_eq!(
            parse_authz_schema_version_relation(relation),
            None,
            "{relation}"
        );
    }
}

#[test]
fn markers_classify_relative_to_the_supported_version() {
    let one = SchemaMarkerRow::for_version(1);
    let three = SchemaMarkerRow::for_version(3);
    assert_eq!(classify_schema_markers(&[], 2), SchemaMarkerState::Absent);
    assert_eq!(
        classify_schema_markers(std::slice::from_ref(&one), 1),
        SchemaMarkerState::Current
    );
    assert_eq!(
        classify_schema_markers(std::slice::from_ref(&one), 2),
        SchemaMarkerState::Older(1)
    );
    assert_eq!(
        classify_schema_markers(std::slice::from_ref(&three), 2),
        SchemaMarkerState::Newer(3)
    );

    let mut with_subject_relation = one.clone();
    with_subject_relation.subject_relation = Some("version_1".to_string());
    let mut other_subject_type = one.clone();
    other_subject_type.subject_type = "user".to_string();
    for rows in [
        vec![row("other", "version_1", AUTHZ_SCHEMA_MARKER_ID)],
        vec![row(AUTHZ_SCHEMA_MARKER_ID, "version_1", "other")],
        vec![row(
            AUTHZ_SCHEMA_MARKER_ID,
            "version_01",
            AUTHZ_SCHEMA_MARKER_ID,
        )],
        vec![row(
            AUTHZ_SCHEMA_MARKER_ID,
            "version_0",
            AUTHZ_SCHEMA_MARKER_ID,
        )],
        vec![row(AUTHZ_SCHEMA_MARKER_ID, "codec", AUTHZ_SCHEMA_MARKER_ID)],
        vec![with_subject_relation],
        vec![other_subject_type],
        vec![one.clone(), one.clone()],
        vec![one.clone(), three.clone()],
    ] {
        assert_eq!(
            classify_schema_markers(&rows, 1),
            SchemaMarkerState::Unrecognised,
            "{rows:?}"
        );
    }
}

#[test]
fn only_newer_and_unrecognised_markers_are_refused() {
    assert_eq!(
        SchemaMarkerState::Newer(3).refusal(2),
        Some(AuthzSchemaVersionError::Newer {
            stored: 3,
            supported: 2
        })
    );
    assert_eq!(
        SchemaMarkerState::Unrecognised.refusal(2),
        Some(AuthzSchemaVersionError::UnrecognisedMarker)
    );
    for state in [
        SchemaMarkerState::Absent,
        SchemaMarkerState::Current,
        SchemaMarkerState::Older(1),
    ] {
        assert_eq!(state.refusal(2), None, "{state:?}");
    }
}

#[test]
fn builtin_schema_declares_only_its_own_version() {
    let schema = builtin_authz_schema();
    assert_eq!(
        definition_lines(schema, AUTHZ_SCHEMA_MARKER_DEFINITION),
        vec![declaration(AUTHZ_SCHEMA_VERSION).as_str()]
    );
    assert_eq!(
        schema.matches(AUTHZ_SCHEMA_VERSION_RELATION_PREFIX).count(),
        1,
        "only the marker definition may declare a version relation"
    );
}

#[test]
fn builtin_schema_text_is_pinned_to_its_version() {
    // Startup leaves a store recorded at this version untouched, so a schema
    // change that keeps the version number never reaches existing stores.
    // When this fails: bump AUTHZ_SCHEMA_VERSION, rename the marker relation
    // in the built-in schema, and update both expectations here.
    let digest = hex::encode(Sha256::digest(builtin_authz_schema().as_bytes()));
    assert_eq!(
        (AUTHZ_SCHEMA_VERSION, digest.as_str()),
        (2, BUILTIN_SCHEMA_SHA256),
        "the built-in schema changed without a new schema version"
    );
}

#[test]
fn version_1_schema_is_reconstructed_exactly() {
    // The live upgrade tests install this text as the release before this
    // one stored it, so it must be that release's schema byte for byte.
    let version_1 = schema_of_version_1();
    assert_eq!(
        hex::encode(Sha256::digest(version_1.as_bytes())),
        VERSION_1_SCHEMA_SHA256
    );
    assert_eq!(declared_authz_schema_versions(&version_1), vec![1]);
    assert!(!version_1.contains(HOST_GRANT_PROJECTION_DEFINITION));
}

#[test]
fn version_2_adds_only_the_host_grant_projection_marker_definition() {
    assert_eq!(
        definition_lines(builtin_authz_schema(), HOST_GRANT_PROJECTION_DEFINITION),
        vec![format!(
            "relation {HOST_GRANT_PROJECTION_MARKER_RELATION}: {HOST_GRANT_PROJECTION_DEFINITION}"
        )
        .as_str()]
    );
    // A version 1 replica reads a store recorded at version 2 as newer, and
    // a store whose schema already declares version 2 as an unfinished
    // upgrade; either way it refuses.
    assert_eq!(
        classify_schema_markers(&[SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)], 1),
        SchemaMarkerState::Newer(AUTHZ_SCHEMA_VERSION)
    );
    let upgrading = authz_schema_retaining_version(builtin_authz_schema(), AUTHZ_SCHEMA_VERSION, 1)
        .expect("upgrade text");
    assert_eq!(
        unfinished_upgrade_refusal(&upgrading, 1, 1),
        Some(AuthzSchemaVersionError::UnfinishedNewerUpgrade {
            declared: AUTHZ_SCHEMA_VERSION,
            recorded: 1,
            supported: 1,
        })
    );
}

#[test]
fn upgrade_schema_keeps_the_older_marker_relation_beside_the_current_one() {
    let schema = builtin_authz_schema();
    let older = AUTHZ_SCHEMA_VERSION + 7;
    let upgrade =
        authz_schema_retaining_version(schema, AUTHZ_SCHEMA_VERSION, older).expect("upgrade text");
    assert_eq!(
        definition_lines(&upgrade, AUTHZ_SCHEMA_MARKER_DEFINITION),
        vec![
            declaration(older).as_str(),
            declaration(AUTHZ_SCHEMA_VERSION).as_str()
        ]
    );
    // Everything else is the built-in schema, unchanged.
    let retained_line = format!("  {}\n", declaration(older));
    assert_eq!(upgrade.replacen(&retained_line, "", 1), schema);

    assert_eq!(
        authz_schema_retaining_version(schema, AUTHZ_SCHEMA_VERSION, AUTHZ_SCHEMA_VERSION),
        None
    );
    assert_eq!(
        authz_schema_retaining_version(&schema_before_versioning(schema), AUTHZ_SCHEMA_VERSION, 9),
        None
    );
    let doubled = format!("{schema}{schema}");
    assert_eq!(
        authz_schema_retaining_version(&doubled, AUTHZ_SCHEMA_VERSION, 9),
        None
    );
}

#[test]
fn stored_schema_declares_the_marker_relations_of_its_marker_definition() {
    let schema = builtin_authz_schema();
    assert_eq!(
        declared_authz_schema_versions(schema),
        vec![AUTHZ_SCHEMA_VERSION]
    );
    let upgrade = authz_schema_retaining_version(
        &schema_of_later_release(AUTHZ_SCHEMA_VERSION + 1),
        AUTHZ_SCHEMA_VERSION + 1,
        AUTHZ_SCHEMA_VERSION,
    )
    .expect("upgrade text");
    assert_eq!(
        declared_authz_schema_versions(&upgrade),
        vec![AUTHZ_SCHEMA_VERSION, AUTHZ_SCHEMA_VERSION + 1]
    );
    assert!(declared_authz_schema_versions(&schema_before_versioning(schema)).is_empty());
    assert!(declared_authz_schema_versions("").is_empty());

    // The stored schema comes back from `ReadSchema` in SpiceDB's own layout,
    // so the scan depends on tokens only, not on lines or indentation.
    let compact = "definition user {}\ndefinition waddle_authz_schema {\n\trelation \
                   version_3: waddle_authz_schema\n\trelation version_12:waddle_authz_schema }";
    assert_eq!(declared_authz_schema_versions(compact), vec![3, 12]);
    let one_line = "definition waddle_authz_schema{relation version_2:waddle_authz_schema}";
    assert_eq!(declared_authz_schema_versions(one_line), vec![2]);

    // Only relations of the marker definition count, and only canonical
    // version spellings; comments and other definitions are ignored.
    let noisy = "/** relation version_90: waddle_authz_schema */\n\
                 definition channel {\n  relation version_91: user\n}\n\
                 definition waddle_authz_schema {\n\
                 // relation version_92: waddle_authz_schema\n\
                 /* relation version_93: waddle_authz_schema */\n\
                 relation version_01: waddle_authz_schema\n\
                 relation spare: waddle_authz_schema\n\
                 relation version_4: waddle_authz_schema\n\
                 permission version_94 = version_4\n\
                 }\n\
                 definition other_waddle_authz_schema {\n  relation version_95: user\n}";
    assert_eq!(declared_authz_schema_versions(noisy), vec![4]);
    // An unterminated comment hides the rest of the text.
    assert!(declared_authz_schema_versions(
        "/* definition waddle_authz_schema { relation version_5: waddle_authz_schema }"
    )
    .is_empty());
}

#[test]
fn only_a_declared_version_newer_than_this_server_is_an_unfinished_upgrade() {
    let supported = 2;
    let declaring = |versions: &[u32]| {
        let relations: String = versions
            .iter()
            .map(|version| format!("  {}\n", declaration(*version)))
            .collect();
        format!("definition {AUTHZ_SCHEMA_MARKER_DEFINITION} {{\n{relations}}}\n")
    };

    // A later release wrote its upgrade schema over a store recorded at this
    // version, or at an older one, and stopped before recording its own.
    assert_eq!(
        unfinished_upgrade_refusal(&declaring(&[2, 3]), 2, supported),
        Some(AuthzSchemaVersionError::UnfinishedNewerUpgrade {
            declared: 3,
            recorded: 2,
            supported
        })
    );
    assert_eq!(
        unfinished_upgrade_refusal(&declaring(&[1, 5]), 1, supported),
        Some(AuthzSchemaVersionError::UnfinishedNewerUpgrade {
            declared: 5,
            recorded: 1,
            supported
        })
    );
    // This server's own schema, its own upgrade schema, an older schema, a
    // stale relation from an interrupted earlier upgrade, or no marker
    // definition at all are not.
    for (schema, recorded) in [
        (declaring(&[2]), 2),
        (declaring(&[1, 2]), 1),
        (declaring(&[1]), 1),
        (declaring(&[1, 2]), 2),
        (String::new(), 2),
        (schema_before_versioning(builtin_authz_schema()), 2),
    ] {
        assert_eq!(
            unfinished_upgrade_refusal(&schema, recorded, supported),
            None,
            "{schema}"
        );
    }
    assert_eq!(
        unfinished_upgrade_refusal(
            builtin_authz_schema(),
            AUTHZ_SCHEMA_VERSION,
            AUTHZ_SCHEMA_VERSION
        ),
        None
    );
}

#[test]
fn codec_guard_ignores_the_schema_version_marker() {
    let guarded: Vec<&str> = guarded_resource_definitions().collect();
    assert!(!guarded.contains(&AUTHZ_SCHEMA_MARKER_DEFINITION));
    assert!(!guarded.contains(&AUTHZ_CODEC_MARKER_DEFINITION));
    assert!(guarded.contains(&"channel"));
}

#[test]
fn refusals_name_versions_only() {
    for error in [
        AuthzSchemaVersionError::Newer {
            stored: 3,
            supported: 2,
        },
        AuthzSchemaVersionError::UnrecognisedMarker,
        AuthzSchemaVersionError::Contended,
        AuthzSchemaVersionError::UnfinishedNewerUpgrade {
            declared: 3,
            recorded: 2,
            supported: 2,
        },
        AuthzSchemaVersionError::BuiltinSchemaUnversioned,
    ] {
        let message = PermissionError::from(error).to_string();
        assert!(
            message.starts_with("Authorization store refused: "),
            "{message}"
        );
        assert!(!message.contains('@'), "{message}");
    }
    let newer = AuthzSchemaVersionError::Newer {
        stored: 3,
        supported: 2,
    }
    .to_string();
    assert!(newer.contains("version 3"), "{newer}");
    assert!(newer.contains("version 2"), "{newer}");
    assert!(newer.contains("docs/operations/authz-schema-versions.md"));
    assert!(AuthzSchemaVersionError::UnrecognisedMarker
        .to_string()
        .contains("docs/operations/authz-schema-versions.md"));
    let unfinished = AuthzSchemaVersionError::UnfinishedNewerUpgrade {
        declared: 3,
        recorded: 1,
        supported: 2,
    }
    .to_string();
    for expected in [
        "declares version 3",
        "version 2 of this server",
        "records version 1",
        "docs/operations/authz-schema-versions.md",
    ] {
        assert!(unfinished.contains(expected), "{unfinished}");
    }
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
    /// An empty, unversioned store holding this build's schema.
    async fn connect() -> Self {
        let (endpoint, preshared_key) = disposable_spicedb_env();
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
        // Drop the markers and relationships an earlier test left, so that
        // the schema is written rather than refused or left as it is. A
        // definition the stored schema lacks, or every definition on a fresh
        // SpiceDB, makes its delete fail; that is the state wanted.
        for definition in guarded_resource_definitions().chain([
            AUTHZ_CODEC_MARKER_DEFINITION,
            AUTHZ_SCHEMA_MARKER_DEFINITION,
        ]) {
            let _ = live
                .client
                .delete_relationships(RelationshipFilter::new(definition))
                .await;
        }
        schema_outcome(live.actor().await.ask(EnsureSchema).await).expect("built-in schema");
        for definition in guarded_resource_definitions().chain([
            AUTHZ_CODEC_MARKER_DEFINITION,
            AUTHZ_SCHEMA_MARKER_DEFINITION,
        ]) {
            live.client
                .delete_relationships(RelationshipFilter::new(definition))
                .await
                .expect("empty definition");
        }
        assert!(live.marker_rows().await.is_empty());
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

    async fn marker_rows(&self) -> Vec<SchemaMarkerRow> {
        let mut stream = self
            .client
            .read_relationships(RelationshipFilter::new(AUTHZ_SCHEMA_MARKER_DEFINITION))
            .consistency(Consistency::FullyConsistent)
            .limit(AUTHZ_SCHEMA_MARKER_READ_LIMIT)
            .send()
            .await
            .expect("read schema markers");
        let mut rows = Vec::new();
        while let Some(item) = stream.next().await {
            let relationship = item.expect("schema marker row").relationship;
            rows.push(SchemaMarkerRow {
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

    async fn stored_schema(&self) -> String {
        self.client.read_schema().await.expect("read schema").0
    }

    /// Write a relationship verbatim, bypassing the codec.
    async fn write_raw(&self, resource: (&str, &str), relation: &str, subject: (&str, &str)) {
        self.client
            .write_relationships(vec![RelationshipUpdate::touch(raw_relationship(
                resource, relation, subject,
            ))])
            .await
            .expect("raw write");
    }

    /// Make the store what the release before this one left: its schema,
    /// the codec marker and the version 1 marker.
    async fn install_version_1(&self) {
        self.client
            .write_schema(schema_of_version_1())
            .await
            .expect("version 1 schema");
        self.write_raw(
            (AUTHZ_CODEC_MARKER_DEFINITION, AUTHZ_CODEC_MARKER_ID),
            AUTHZ_CODEC_MARKER_RELATION,
            (AUTHZ_CODEC_MARKER_DEFINITION, AUTHZ_CODEC_VERSION),
        )
        .await;
        self.client
            .write_relationships(vec![RelationshipUpdate::create(marker(1))])
            .await
            .expect("version 1 marker");
        assert_eq!(
            self.marker_rows().await,
            vec![SchemaMarkerRow::for_version(1)]
        );
        assert_eq!(
            declared_authz_schema_versions(&self.stored_schema().await),
            vec![1]
        );
    }

    /// Step 1 of a later release's upgrade from this version to `version`:
    /// write its schema with this version's marker relation kept beside its
    /// own.
    async fn begin_upgrade_as_later_release(&self, version: u32) {
        let upgrade = authz_schema_retaining_version(
            &schema_of_later_release(version),
            version,
            AUTHZ_SCHEMA_VERSION,
        )
        .expect("later release upgrade schema");
        self.client
            .write_schema(upgrade)
            .await
            .expect("later release upgrade schema");
    }

    /// Record `version` as a later release would: write its upgrade schema,
    /// replace the marker, then write its own schema.
    async fn record_as_later_release(&self, version: u32) {
        self.begin_upgrade_as_later_release(version).await;
        self.client
            .write_relationships(vec![
                RelationshipUpdate::delete(marker(AUTHZ_SCHEMA_VERSION)),
                RelationshipUpdate::create(marker(version)),
            ])
            .await
            .expect("later release marker");
        self.client
            .write_schema(schema_of_later_release(version))
            .await
            .expect("later release schema");
    }
}

fn raw_relationship(resource: (&str, &str), relation: &str, subject: (&str, &str)) -> Relationship {
    Relationship::new(
        ObjectReference::new(resource.0, resource.1).expect("resource"),
        relation,
        SubjectReference::new(
            ObjectReference::new(subject.0, subject.1).expect("subject"),
            None::<String>,
        )
        .expect("subject reference"),
    )
}

fn marker(version: u32) -> Relationship {
    raw_relationship(
        (AUTHZ_SCHEMA_MARKER_DEFINITION, AUTHZ_SCHEMA_MARKER_ID),
        &authz_schema_version_relation(version),
        (AUTHZ_SCHEMA_MARKER_DEFINITION, AUTHZ_SCHEMA_MARKER_ID),
    )
}

fn schema_outcome<M>(
    result: Result<(), SendError<M, PermissionError>>,
) -> Result<(), AuthzSchemaVersionError> {
    match result {
        Ok(()) => Ok(()),
        Err(SendError::HandlerError(PermissionError::AuthzSchemaVersion(error))) => Err(error),
        Err(other) => panic!("startup step failed outside the schema-version check: {other:?}"),
    }
}

/// The three startup steps of the server, in order.
async fn start(replica: &ActorRef<PermissionActor>) -> Result<(), AuthzSchemaVersionError> {
    schema_outcome(replica.ask(EnsureSchema).await)?;
    replica
        .ask(EnsureAuthzCodecIncarnation)
        .await
        .expect("codec guard accepts the store");
    schema_outcome(replica.ask(RecordAuthzSchemaVersion).await)
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_records_the_version_of_an_unversioned_store() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;

    start(&live.actor().await).await.expect("unversioned store");
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_restart_at_the_same_version_leaves_the_schema() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    start(&live.actor().await).await.expect("unversioned store");

    // A distinguishable schema that keeps this version's marker valid and
    // declares no other version: the built-in schema with one more, empty,
    // relation in the marker definition.
    let spare = format!("relation spare_marker: {AUTHZ_SCHEMA_MARKER_DEFINITION}");
    let distinguishable = builtin_authz_schema().replacen(
        &declaration(AUTHZ_SCHEMA_VERSION),
        &format!("{}\n  {spare}", declaration(AUTHZ_SCHEMA_VERSION)),
        1,
    );
    assert_ne!(distinguishable, builtin_authz_schema());
    live.client
        .write_schema(distinguishable)
        .await
        .expect("distinguishable schema");

    start(&live.actor().await).await.expect("recorded store");
    assert!(
        live.stored_schema().await.contains(&spare),
        "a replica at the recorded version rewrote the schema"
    );
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_refuses_a_newer_version_without_overwriting_it() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    start(&live.actor().await).await.expect("unversioned store");
    let newer = AUTHZ_SCHEMA_VERSION + 1;
    live.record_as_later_release(newer).await;

    // A replica of this release refuses at both steps and writes nothing.
    let replica = live.actor().await;
    let refusal = AuthzSchemaVersionError::Newer {
        stored: newer,
        supported: AUTHZ_SCHEMA_VERSION,
    };
    assert_eq!(
        schema_outcome(replica.ask(EnsureSchema).await),
        Err(refusal)
    );
    assert_eq!(
        schema_outcome(replica.ask(RecordAuthzSchemaVersion).await),
        Err(refusal)
    );
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(newer)]
    );
    let stored = live.stored_schema().await;
    assert!(stored.contains(&declaration(newer)), "{stored}");
    assert!(
        !stored.contains(&declaration(AUTHZ_SCHEMA_VERSION)),
        "{stored}"
    );

    // SpiceDB itself refuses this release's schema while the newer marker
    // exists, so a binary that skipped the check could not revert it either.
    assert!(live
        .client
        .write_schema(builtin_authz_schema())
        .await
        .is_err());
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(newer)]
    );
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_refuses_schema_writes_of_releases_before_versioning() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    start(&live.actor().await).await.expect("unversioned store");

    // A release built before schema versioning writes its schema at every
    // start. It lacks the marker definition, which now holds the marker.
    assert!(live
        .client
        .write_schema(schema_before_versioning(builtin_authz_schema()))
        .await
        .is_err());
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );
    start(&live.actor().await)
        .await
        .expect("store is still at this version");
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_refuses_an_unrecognised_marker() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    live.write_raw(
        (AUTHZ_SCHEMA_MARKER_DEFINITION, "other"),
        &authz_schema_version_relation(AUTHZ_SCHEMA_VERSION),
        (AUTHZ_SCHEMA_MARKER_DEFINITION, AUTHZ_SCHEMA_MARKER_ID),
    )
    .await;

    let replica = live.actor().await;
    assert_eq!(
        schema_outcome(replica.ask(EnsureSchema).await),
        Err(AuthzSchemaVersionError::UnrecognisedMarker)
    );
    assert_eq!(
        schema_outcome(replica.ask(RecordAuthzSchemaVersion).await),
        Err(AuthzSchemaVersionError::UnrecognisedMarker)
    );
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow {
            resource_id: "other".to_string(),
            ..SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)
        }]
    );
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_codec_refusal_leaves_the_legacy_schema_untouched() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    // A store of a release before the ID codec guard: its schema lacks both
    // marker definitions and it holds a ban in the replaced ID encoding.
    let legacy_schema = schema_before_codec_guard(builtin_authz_schema());
    live.client
        .write_schema(legacy_schema.clone())
        .await
        .expect("legacy schema");
    live.write_raw(
        ("channel", "v2_c2t5"),
        "outcast",
        ("user", "alice|example=com"),
    )
    .await;
    let legacy_text = live.stored_schema().await;

    // Startup stops before writing the schema: replicas of the legacy release
    // still running keep their own policy, and its schema write at their
    // next start does not flap with this release's.
    let replica = live.actor().await;
    match replica.ask(EnsureSchema).await {
        Err(SendError::HandlerError(PermissionError::AuthzIncarnation(
            AuthzIncarnationError::UnmarkedStoreNotEmpty {
                resource_type: "channel",
            },
        ))) => {}
        other => panic!("a populated legacy store should be refused before its schema: {other:?}"),
    }
    let stored = live.stored_schema().await;
    assert_eq!(
        stored, legacy_text,
        "the refused store's schema was rewritten"
    );
    assert!(!stored.contains(AUTHZ_SCHEMA_MARKER_DEFINITION), "{stored}");
    assert!(!stored.contains(AUTHZ_CODEC_MARKER_DEFINITION), "{stored}");
    assert!(declared_authz_schema_versions(&stored).is_empty());

    // The legacy release can still write its schema.
    live.client
        .write_schema(legacy_schema)
        .await
        .expect("schema of a release before the codec guard");
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_refuses_an_unfinished_newer_upgrade_without_overwriting_it() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    start(&live.actor().await).await.expect("unversioned store");
    let newer = AUTHZ_SCHEMA_VERSION + 1;
    // A later release wrote its schema in step 1 of its upgrade and stopped
    // before recording its version: the marker still records this version.
    live.begin_upgrade_as_later_release(newer).await;
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );

    // A replica of this release refuses at both steps instead of serving
    // under the later schema, and writes nothing.
    let replica = live.actor().await;
    let refusal = AuthzSchemaVersionError::UnfinishedNewerUpgrade {
        declared: newer,
        recorded: AUTHZ_SCHEMA_VERSION,
        supported: AUTHZ_SCHEMA_VERSION,
    };
    assert_eq!(
        schema_outcome(replica.ask(EnsureSchema).await),
        Err(refusal)
    );
    assert_eq!(
        schema_outcome(replica.ask(RecordAuthzSchemaVersion).await),
        Err(refusal)
    );
    let stored = live.stored_schema().await;
    assert_eq!(
        declared_authz_schema_versions(&stored),
        vec![AUTHZ_SCHEMA_VERSION, newer],
        "{stored}"
    );
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );

    // Once the later release finishes the upgrade, the refusal is the
    // ordinary one for a newer version.
    live.client
        .write_relationships(vec![
            RelationshipUpdate::delete(marker(AUTHZ_SCHEMA_VERSION)),
            RelationshipUpdate::create(marker(newer)),
        ])
        .await
        .expect("later release marker");
    live.client
        .write_schema(schema_of_later_release(newer))
        .await
        .expect("later release schema");
    assert_eq!(
        schema_outcome(live.actor().await.ask(EnsureSchema).await),
        Err(AuthzSchemaVersionError::Newer {
            stored: newer,
            supported: AUTHZ_SCHEMA_VERSION,
        })
    );
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_racing_replicas_record_one_version() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    let mut replicas = Vec::new();
    for _ in 0..8 {
        replicas.push(live.actor().await);
    }

    let results = futures::future::join_all(replicas.iter().map(start)).await;
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );
    start(&live.actor().await).await.expect("recorded store");
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_upgrades_a_version_1_store() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    live.install_version_1().await;

    start(&live.actor().await).await.expect("version 1 store");
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );
    let stored = live.stored_schema().await;
    assert_eq!(
        declared_authz_schema_versions(&stored),
        vec![AUTHZ_SCHEMA_VERSION],
        "the upgrade left the version 1 relation declared"
    );
    assert!(
        stored.contains(&format!("definition {HOST_GRANT_PROJECTION_DEFINITION}")),
        "the upgraded schema lacks the host-grant projection marker definition"
    );

    // A version 1 replica reads the store as newer than itself, and SpiceDB
    // refuses its schema, which would drop the relation that holds the
    // marker.
    assert_eq!(
        classify_schema_markers(&live.marker_rows().await, 1).refusal(1),
        Some(AuthzSchemaVersionError::Newer {
            stored: AUTHZ_SCHEMA_VERSION,
            supported: 1,
        })
    );
    assert!(live
        .client
        .write_schema(schema_of_version_1())
        .await
        .is_err());
    assert_eq!(
        declared_authz_schema_versions(&live.stored_schema().await),
        vec![AUTHZ_SCHEMA_VERSION]
    );

    // A restart of this release finds its own version and writes nothing.
    start(&live.actor().await).await.expect("recorded store");
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_version_1_replica_refuses_once_this_release_wrote_its_schema() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    live.install_version_1().await;

    // Step 1 of this release's upgrade: its schema is stored with the
    // version 1 relation kept, and the marker still records version 1.
    schema_outcome(live.actor().await.ask(EnsureSchema).await).expect("upgrade schema");
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(1)]
    );
    let stored = live.stored_schema().await;
    assert_eq!(
        declared_authz_schema_versions(&stored),
        vec![1, AUTHZ_SCHEMA_VERSION]
    );

    // A version 1 replica that starts now reads its own marker and the newer
    // version in the stored schema, and refuses.
    assert_eq!(
        classify_schema_markers(&live.marker_rows().await, 1),
        SchemaMarkerState::Current
    );
    assert_eq!(
        unfinished_upgrade_refusal(&stored, 1, 1),
        Some(AuthzSchemaVersionError::UnfinishedNewerUpgrade {
            declared: AUTHZ_SCHEMA_VERSION,
            recorded: 1,
            supported: 1,
        })
    );

    // This release finishes the upgrade on its next start.
    start(&live.actor().await)
        .await
        .expect("unfinished upgrade");
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );
    assert_eq!(
        declared_authz_schema_versions(&live.stored_schema().await),
        vec![AUTHZ_SCHEMA_VERSION]
    );
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_racing_replicas_upgrade_a_version_1_store_once() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = LiveSpiceDb::connect().await;
    live.install_version_1().await;
    let mut replicas = Vec::new();
    for _ in 0..8 {
        replicas.push(live.actor().await);
    }

    let results = futures::future::join_all(replicas.iter().map(start)).await;
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );
    // A replica that read the version 1 marker before another replaced it
    // may write the upgrade schema last, which leaves the emptied version 1
    // relation declared until the next upgrade. No newer version appears.
    let declared = declared_authz_schema_versions(&live.stored_schema().await);
    assert!(declared.contains(&AUTHZ_SCHEMA_VERSION), "{declared:?}");
    assert!(
        declared
            .iter()
            .all(|version| *version <= AUTHZ_SCHEMA_VERSION),
        "{declared:?}"
    );
    start(&live.actor().await).await.expect("recorded store");
    assert_eq!(
        live.marker_rows().await,
        vec![SchemaMarkerRow::for_version(AUTHZ_SCHEMA_VERSION)]
    );
}

/// The endpoint and key of the disposable SpiceDB, refusing to proceed unless
/// the operator has declared the store disposable: these tests delete every
/// relationship of the guarded definitions and both markers.
fn disposable_spicedb_env() -> (String, String) {
    assert_eq!(
        std::env::var("WADDLE_TEST_SPICEDB_DISPOSABLE").as_deref(),
        Ok("1"),
        "WADDLE_TEST_SPICEDB_DISPOSABLE=1 is required: the live SpiceDB tests delete every \
         relationship of the guarded definitions and both markers, so they only run against a \
         store that is safe to wipe"
    );
    let endpoint =
        std::env::var("WADDLE_TEST_SPICEDB_ENDPOINT").expect("WADDLE_TEST_SPICEDB_ENDPOINT");
    let preshared_key = std::env::var("WADDLE_TEST_SPICEDB_PRESHARED_KEY")
        .expect("WADDLE_TEST_SPICEDB_PRESHARED_KEY");
    (endpoint, preshared_key)
}
