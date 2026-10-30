use super::node::{clustering_scope_token, prepare_node_lease};
use super::{claims, CancellationToken, Database};
use crate::db::{DatabaseConfig, DatabaseDriver};
use waddle_xmpp::ownership::NodeIdentity;

// Regression coverage for the cancellation-scope invariant: a
// clustering self-fence must not take down the whole process, but
// process shutdown must still take down clustering.
#[test]
fn clustering_scope_cancels_independently_of_parent() {
    let parent = CancellationToken::new();
    let child = clustering_scope_token(&parent);

    child.cancel();

    assert!(child.is_cancelled());
    assert!(!parent.is_cancelled());
}

#[test]
fn parent_cancellation_still_tears_down_clustering_scope() {
    let parent = CancellationToken::new();
    let child = clustering_scope_token(&parent);

    parent.cancel();

    assert!(child.is_cancelled());
}

async fn isolated_test_control_db(
    name: &str,
    schema: &str,
) -> anyhow::Result<Option<(Database, sqlx::PgPool)>> {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return Ok(None);
    };
    let admin = sqlx::PgPool::connect(&url)
        .await
        .map_err(|error| anyhow::anyhow!("connect postgres admin pool: {error}"))?;
    let create_schema = format!("CREATE SCHEMA {schema}");
    sqlx::query(&create_schema)
        .execute(&admin)
        .await
        .map_err(|error| anyhow::anyhow!("create isolated schema {schema}: {error}"))?;
    let scoped_url = postgres_url_with_search_path(&url, schema);
    let db = match Database::from_config(
        name,
        &DatabaseConfig::new(DatabaseDriver::Postgres, scoped_url)
            .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    {
        Ok(db) => db,
        Err(error) => {
            drop_isolated_schema(&admin, schema).await?;
            return Err(anyhow::anyhow!("open isolated test postgres: {error}"));
        }
    };
    Ok(Some((db, admin)))
}

#[tokio::test]
async fn prepare_node_lease_provisions_schema_before_registration() -> anyhow::Result<()> {
    let schema = unique_postgres_schema_name("clustering_startup");
    let Some((db, admin)) =
        isolated_test_control_db("clustering-startup-schema-test", &schema).await?
    else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
                 (clustering startup claim/node-lease schema regression)"
        );
        return Ok(());
    };

    let result = async {
        let store = claims::PostgresClaimStore::new(db.clone());
        let identity = NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        prepare_node_lease(
            &store,
            &identity,
            Some("test-template".to_string()),
            Some("test-peer".to_string()),
        )
        .await
        .map_err(|error| anyhow::anyhow!("prepare node lease: {error}"))?;

        let conn = db
            .guard()
            .await
            .map_err(|error| anyhow::anyhow!("guard: {error}"))?;
        let mut rows = conn
            .query(
                "SELECT pod_template_hash, peer_id, expired \
                     FROM clustering_nodes \
                     WHERE node_id = ?",
                crate::db_params![identity.node_id.as_str()],
            )
            .await
            .map_err(|error| anyhow::anyhow!("query node row: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| anyhow::anyhow!("node row query: {error}"))?
            .ok_or_else(|| anyhow::anyhow!("node row missing"))?;
        let pod_template_hash = row
            .get::<Option<String>>(0)
            .map_err(|error| anyhow::anyhow!("decode pod_template_hash: {error}"))?;
        if pod_template_hash.as_deref() != Some("test-template") {
            anyhow::bail!(
                "unexpected pod_template_hash: {:?}",
                pod_template_hash.as_deref()
            );
        }
        let peer_id = row
            .get::<Option<String>>(1)
            .map_err(|error| anyhow::anyhow!("decode peer_id: {error}"))?;
        if peer_id.as_deref() != Some("test-peer") {
            anyhow::bail!("unexpected peer_id: {:?}", peer_id.as_deref());
        }
        let expired = row
            .get::<bool>(2)
            .map_err(|error| anyhow::anyhow!("decode expired: {error}"))?;
        if expired {
            anyhow::bail!("fresh node lease row must start non-expired");
        }
        Ok(())
    }
    .await;
    drop(db);

    let cleanup = drop_isolated_schema(&admin, &schema).await;
    result?;
    cleanup?;
    Ok(())
}

async fn drop_isolated_schema(admin: &sqlx::PgPool, schema: &str) -> anyhow::Result<()> {
    let drop_schema = format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
    sqlx::query(&drop_schema)
        .execute(admin)
        .await
        .map_err(|error| anyhow::anyhow!("drop isolated schema {schema}: {error}"))?;
    Ok(())
}

fn unique_postgres_schema_name(prefix: &str) -> String {
    format!("waddle_test_{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres url");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}
