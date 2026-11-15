//! The disposable SpiceDB the live host-grant suites empty before each case.
//!
//! Emptying a store deletes every projection marker and witness in it,
//! position witnesses included, which is what tells a production deployment
//! that its database was restored. A suite therefore empties only a store it
//! marked itself: after its first case it writes a test-store marker beside
//! the witnesses, and it refuses to empty a store that holds a position
//! witness without that marker. Each suite run is meant to have a SpiceDB
//! endpoint of its own; the marker is the guard against one that is not
//! disposable after all.

use futures::StreamExt;
use prescience::{
    Client, Consistency, ObjectReference, Relationship, RelationshipFilter, RelationshipUpdate,
    SubjectReference,
};
use waddle_server::permissions::host_grant::{
    HOST_GRANT_PROJECTION_DEFINITION, HOST_GRANT_PROJECTION_MARKER_RELATION,
};
use waddle_server::permissions::{
    guarded_resource_definitions, AUTHZ_CODEC_MARKER_DEFINITION, AUTHZ_SCHEMA_MARKER_DEFINITION,
};

/// The resource of the test-store marker: no lifecycle identity and no
/// witness prefix, so the restore fence ignores it.
const TEST_STORE_RESOURCE: &str = "disposable_test_store";
const TEST_STORE_STAMP: &str = "host_grant_live_suites";

/// The endpoint and key of the disposable SpiceDB, refusing to proceed unless
/// the operator has declared the store disposable: these tests delete every
/// relationship of the guarded definitions and both markers.
pub fn disposable_spicedb_env() -> (String, String) {
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

/// Empty the store, markers and witnesses included, after checking that it
/// holds no position witness a test suite did not write. A definition the
/// stored schema lacks makes its delete fail; that is the state wanted.
pub async fn empty_disposable_store(raw: &Client) {
    let mut position_witness = false;
    let mut marked = false;
    if let Ok(mut stream) = raw
        .read_relationships(RelationshipFilter::new(HOST_GRANT_PROJECTION_DEFINITION))
        .consistency(Consistency::FullyConsistent)
        .send()
        .await
    {
        while let Some(Ok(item)) = stream.next().await {
            let resource = item.relationship.resource.object_id().to_owned();
            position_witness |= resource.starts_with("position_");
            marked |= resource == TEST_STORE_RESOURCE;
        }
    }
    assert!(
        marked || !position_witness,
        "refusing to empty a SpiceDB that holds a host-grant position witness no test suite \
         marked: point WADDLE_TEST_SPICEDB_ENDPOINT at a disposable SpiceDB of this run"
    );
    for definition in guarded_resource_definitions().chain([
        AUTHZ_CODEC_MARKER_DEFINITION,
        AUTHZ_SCHEMA_MARKER_DEFINITION,
    ]) {
        let _ = raw
            .delete_relationships(RelationshipFilter::new(definition))
            .await;
    }
}

/// Mark the store as one a test suite empties. It is written once the
/// store's codec is marked, since the codec guard refuses a store that
/// holds relationships without its marker.
pub async fn mark_test_store(raw: &Client) {
    let marker = Relationship::new(
        ObjectReference::new(HOST_GRANT_PROJECTION_DEFINITION, TEST_STORE_RESOURCE)
            .expect("test-store resource"),
        HOST_GRANT_PROJECTION_MARKER_RELATION,
        SubjectReference::new(
            ObjectReference::new(HOST_GRANT_PROJECTION_DEFINITION, TEST_STORE_STAMP)
                .expect("test-store stamp"),
            None::<String>,
        )
        .expect("test-store subject"),
    );
    raw.write_relationships(vec![RelationshipUpdate::touch(marker)])
        .await
        .expect("mark the test store");
}
