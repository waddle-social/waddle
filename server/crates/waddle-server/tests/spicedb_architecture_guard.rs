use std::fs;
use std::path::{Path, PathBuf};

fn crate_path(relative: &str) -> PathBuf {
    Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").expect("test runner sets CARGO_MANIFEST_DIR"))
        .join(relative)
}

fn read(relative: &str) -> String {
    let path = crate_path(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn active_server_permissions_stay_actor_only() {
    for relative in [
        "src/server/mod.rs",
        "src/server/lifecycle.rs",
        "src/server/xmpp_state.rs",
    ] {
        let contents = read(relative);
        assert!(
            contents.contains("PermissionActor"),
            "{relative} should continue routing permissions through PermissionActor",
        );
        assert!(
            !contents.contains("PermissionService"),
            "{relative} reintroduced the removed PermissionService wrapper",
        );
    }

    for relative in [
        "src/server/routes/permissions.rs",
        "src/server/routes/channels.rs",
        "src/server/routes/waddles.rs",
    ] {
        assert!(
            !crate_path(relative).exists(),
            "{relative} should stay removed with XMPP-native spaces"
        );
    }
}

#[test]
fn spicedb_runtime_and_chart_stay_free_of_removed_bootstrap_knobs() {
    let forbidden_tokens = [
        "WADDLE_SPICEDB_BOOTSTRAP_SCHEMA",
        "WADDLE_SPICEDB_SCHEMA_VERSION",
        "bootstrapSchema",
        "schemaVersion",
        "bootstrap_schema(",
        "SPICEDB_SCHEMA",
    ];

    for relative in [
        "src/config.rs",
        "src/permissions/mod.rs",
        "src/permissions/spicedb.rs",
        "../../charts/waddle-server/templates/configmap.yaml",
        "../../charts/waddle-server/templates/validations.yaml",
        "../../charts/waddle-server/values.yaml",
    ] {
        let contents = read(relative);
        for forbidden in forbidden_tokens {
            assert!(
                !contents.contains(forbidden),
                "{relative} still contains removed SpiceDB bootstrap token {forbidden:?}",
            );
        }
    }

    let configmap = read("../../charts/waddle-server/templates/configmap.yaml");
    assert!(configmap.contains("WADDLE_SPICEDB_ENDPOINT"));
    assert!(configmap.contains("WADDLE_SPICEDB_INSECURE"));

    let secret = read("../../charts/waddle-server/templates/secret.yaml");
    assert!(secret.contains("WADDLE_SPICEDB_PRESHARED_KEY"));
}

#[test]
fn spicedb_schema_writes_are_decided_by_the_version_marker() {
    let contents = read("src/permissions/spicedb.rs");
    assert!(
        contents.contains("pub async fn ensure_schema(&self, schema: &str)"),
        "SpiceDB backend should own schema reconciliation",
    );
    assert!(
        contents.contains("pub async fn record_schema_version(&self, schema: &str)"),
        "SpiceDB backend should own the schema-version record",
    );
    assert!(
        contents.contains(".write_schema(schema)"),
        "startup must still install or upgrade the built-in schema",
    );
    assert!(
        contents.contains("self.schema_marker_state().await"),
        "schema writes must be decided by the schema-version marker relationship",
    );
    // `ReadSchema` can serve a cached schema after a direct datastore reset,
    // so it may only add a refusal (an unfinished newer upgrade), never
    // decide that a schema write is skipped or made.
    assert_eq!(
        contents.matches(".read_schema(").count(),
        1,
        "schema writes must not be decided by read_schema; SpiceDB can serve cached schema after direct datastore resets",
    );
    let read = contents
        .find(".read_schema(")
        .expect("one read_schema call");
    let reader = contents[..read]
        .rfind("async fn ")
        .map(|start| &contents[start..read])
        .expect("read_schema inside a function");
    assert!(
        reader.starts_with("async fn refuse_unfinished_newer_upgrade("),
        "read_schema may only feed the unfinished-upgrade refusal",
    );
    assert!(
        contents[read..].contains("unfinished_upgrade_refusal(&stored_schema"),
        "the stored schema may only be classified for the unfinished-upgrade refusal",
    );
    assert!(
        !contents.contains("current.trim() == schema.trim()"),
        "schema reconciliation must not skip writes based on read_schema equality; SpiceDB can serve cached schema after direct datastore resets",
    );
}

#[test]
fn startup_records_the_schema_version_after_the_codec_guard() {
    let contents = read("src/server/lifecycle.rs");
    let position = |needle: &str| {
        contents
            .find(needle)
            .unwrap_or_else(|| panic!("lifecycle no longer sends {needle}"))
    };
    let schema = position("crate::permissions::EnsureSchema)");
    let codec = position("crate::permissions::EnsureAuthzCodecIncarnation)");
    let version = position("crate::permissions::RecordAuthzSchemaVersion)");
    assert!(
        schema < codec && codec < version,
        "startup must write the schema, then bind the codec, then record the schema version"
    );
}
