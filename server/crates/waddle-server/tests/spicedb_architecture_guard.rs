use std::{
    fs,
    path::{Path, PathBuf},
};

fn crate_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read(relative: &str) -> String {
    let path = crate_path(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn active_server_permissions_stay_actor_only() {
    for relative in ["src/server/mod.rs", "src/server/xmpp_state.rs"] {
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
