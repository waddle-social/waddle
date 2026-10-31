use futures::stream;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection, Row};
use tempfile::TempDir;

use super::{collect_rows, read, Driver, StoredRow};
use crate::model::{AccountSource, AuditError};

async fn fixture() -> (TempDir, String) {
    let directory = tempfile::tempdir().expect("fixture dir");
    let path = directory.path().join("accounts.sqlite");
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("fixture connection");
    for sql in [
        "CREATE TABLE users(jid TEXT PRIMARY KEY, xmpp_localpart TEXT, localpart_key TEXT)",
        "CREATE TABLE native_users(id INTEGER PRIMARY KEY, username TEXT, domain TEXT, jid_key TEXT)",
        "INSERT INTO users VALUES('alice@home.example.test','alice','alice')",
        "INSERT INTO native_users VALUES(1,'alice','home.example.test','alice@home.example.test')",
    ] { sqlx::query(sql).execute(&mut connection).await.expect("fixture statement"); }
    connection.close().await.expect("close fixture");
    let url = format!("sqlite://{}", path.display());
    (directory, url)
}

#[tokio::test]
async fn sqlite_reads_both_sources_without_changing_the_file() {
    let (directory, url) = fixture().await;
    let path = directory.path().join("accounts.sqlite");
    let before = std::fs::read(&path).expect("before");
    let rows = read(Driver::Sqlite, &url, "home.example.test", 10)
        .await
        .expect("audit");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.source == AccountSource::Oidc));
    assert!(rows.iter().any(|row| row.source == AccountSource::Native));
    assert_eq!(std::fs::read(path).expect("after"), before);
}

#[tokio::test]
async fn missing_database_is_not_created_and_row_limit_is_incomplete() {
    let (directory, url) = fixture().await;
    let missing = directory.path().join("absent.sqlite");
    assert!(matches!(
        read(
            Driver::Sqlite,
            &format!("sqlite://{}", missing.display()),
            "home.example.test",
            10
        )
        .await,
        Err(AuditError::Connection)
    ));
    assert!(!missing.exists());
    assert!(matches!(
        read(Driver::Sqlite, &url, "home.example.test", 1).await,
        Err(AuditError::Limit)
    ));
}

#[tokio::test]
async fn missing_columns_and_views_cannot_produce_empty_success() {
    for replacement in ["CREATE TABLE native_users(id INTEGER PRIMARY KEY)", "CREATE VIEW native_users AS SELECT 1 AS id, 'a' AS username, 'home.example.test' AS domain, NULL AS jid_key", "CREATE VIRTUAL TABLE native_users USING fts5(id,username,domain,jid_key)"] {
        let (_directory, url) = fixture().await;
        let mut connection = SqliteConnection::connect(&url).await.expect("fixture connection");
        sqlx::query("DROP TABLE native_users").execute(&mut connection).await.expect("drop fixture");
        sqlx::query(replacement).execute(&mut connection).await.expect("replacement");
        assert!(matches!(read(Driver::Sqlite, &url, "home.example.test", 10).await, Err(AuditError::Schema)));
    }
}

#[tokio::test]
async fn missing_source_null_identity_and_oversize_input_fail_closed() {
    for mutation in [
        "DROP TABLE native_users",
        "UPDATE users SET xmpp_localpart=NULL",
        "UPDATE users SET xmpp_localpart=printf('%05000d',1)",
    ] {
        let (_directory, url) = fixture().await;
        let mut connection = SqliteConnection::connect(&url)
            .await
            .expect("fixture connection");
        sqlx::query(mutation)
            .execute(&mut connection)
            .await
            .expect("fixture mutation");
        assert!(read(Driver::Sqlite, &url, "home.example.test", 10)
            .await
            .is_err());
    }
}

#[tokio::test]
async fn stream_error_discards_partial_snapshot() {
    let first = StoredRow {
        source: "oidc".to_owned(),
        valid_types: true,
        native_id: None,
        raw_jid: Some("a@home.example.test".to_owned()),
        localpart: Some("a".to_owned()),
        domain: None,
        stored_key: Some("a".to_owned()),
    };
    let rows = stream::iter([Ok(first), Err(AuditError::Read)]);
    assert!(matches!(
        collect_rows(rows, "home.example.test", 10).await,
        Err(AuditError::Read)
    ));
}

#[tokio::test]
async fn sqlite_audit_connection_rejects_writes() {
    let (_directory, url) = fixture().await;
    let options: SqliteConnectOptions = url.parse().expect("options");
    let mut connection =
        SqliteConnection::connect_with(&options.read_only(true).create_if_missing(false))
            .await
            .expect("read only");
    assert!(sqlx::query("DELETE FROM main.users")
        .execute(&mut connection)
        .await
        .is_err());
    let count = sqlx::query("SELECT count(*) AS count FROM main.users")
        .fetch_one(&mut connection)
        .await
        .expect("read");
    assert_eq!(count.get::<i64, _>("count"), 1);
}

#[tokio::test]
async fn sqlite_embedded_nul_is_not_truncated_into_a_clean_identity() {
    let (_directory, url) = fixture().await;
    let mut connection = SqliteConnection::connect(&url)
        .await
        .expect("fixture connection");
    sqlx::query("UPDATE users SET xmpp_localpart=?")
        .bind("alice\0hidden")
        .execute(&mut connection)
        .await
        .expect("fixture value");
    let rows = read(Driver::Sqlite, &url, "home.example.test", 10)
        .await
        .expect("read original bytes");
    assert_eq!(rows[0].localpart.as_str(), "alice\0hidden");
    let (observations, _) = crate::analysis::accounts(rows);
    assert!(!observations[0].findings.is_empty());
}

#[tokio::test]
async fn retained_byte_limit_rejects_a_partial_inventory() {
    let rows = stream::iter((0..5000).map(|_| {
        Ok(StoredRow {
            source: "oidc".to_owned(),
            valid_types: true,
            native_id: None,
            raw_jid: Some("a".repeat(4096)),
            localpart: Some("a".to_owned()),
            domain: None,
            stored_key: Some("a".to_owned()),
        })
    }));
    assert!(matches!(
        collect_rows(rows, "home.example.test", 10_000).await,
        Err(AuditError::Limit)
    ));
}

mod postgres;

#[tokio::test]
async fn sqlite_uri_options_cannot_bypass_snapshot_locking() {
    let (_directory, url) = fixture().await;
    for option in ["?immutable=true", "?vfs=unix-none"] {
        assert!(matches!(
            read(
                Driver::Sqlite,
                &format!("{url}{option}"),
                "home.example.test",
                10
            )
            .await,
            Err(AuditError::Configuration)
        ));
    }
}

#[tokio::test]
async fn sqlite_invalid_text_bytes_fail_closed() {
    let (_directory, url) = fixture().await;
    let mut connection = SqliteConnection::connect(&url)
        .await
        .expect("fixture connection");
    sqlx::query("UPDATE users SET xmpp_localpart=CAST(x'80' AS TEXT)")
        .execute(&mut connection)
        .await
        .expect("fixture bytes");
    assert!(matches!(
        read(Driver::Sqlite, &url, "home.example.test", 10).await,
        Err(AuditError::Read)
    ));
}

#[tokio::test]
async fn sqlite_plain_path_selection_rejects_replacement_and_decoded_forms() {
    let (directory, url) = fixture().await;
    let selected = super::SqliteSource::inspect(&url).expect("plain existing path");
    let original = directory.path().join("accounts.sqlite");
    let moved = directory.path().join("moved.sqlite");
    std::fs::rename(&original, &moved).expect("replace owned fixture path");
    std::fs::copy(&moved, &original).expect("new inode at same path");
    assert!(matches!(
        selected.connect().await,
        Err(AuditError::Connection)
    ));
    for invalid in [
        "sqlite://file:accounts.sqlite%3Fimmutable=true",
        "sqlite://%66ile:accounts.sqlite%3fimmutable=true",
        "sqlite://file:accounts.sqlite%253Fimmutable=true",
        "sqlite://relative.sqlite",
        "file:/absolute/accounts.sqlite",
    ] {
        assert!(matches!(
            super::SqliteSource::inspect(invalid),
            Err(AuditError::Configuration)
        ));
    }
}
