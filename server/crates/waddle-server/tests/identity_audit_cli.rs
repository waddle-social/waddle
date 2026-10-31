use std::ffi::OsString;
use std::path::Path;
use std::process::Output;

use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::Connection;
use tempfile::TempDir;
use tokio::process::Command;

async fn fixture() -> (TempDir, String) {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("accounts.sqlite");
    let mut database = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .expect("fixture connection");
    for statement in [
        "CREATE TABLE users(jid TEXT PRIMARY KEY, xmpp_localpart TEXT, localpart_key TEXT)",
        "CREATE TABLE native_users(id INTEGER PRIMARY KEY, username TEXT, domain TEXT, jid_key TEXT)",
    ] {
        sqlx::query(statement).execute(&mut database).await.expect("fixture schema");
    }
    database.close().await.expect("close fixture");
    (directory, format!("sqlite://{}", path.display()))
}

fn audit_executable() -> OsString {
    // Runtime paths are remapped when nextest extracts an archive. Cargo 1.94+
    // also supplies its binary path at runtime for ordinary cargo test runs.
    std::env::var_os("NEXTEST_BIN_EXE_waddle_identity_audit")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_waddle-identity-audit"))
        .expect("test runner must provide the runtime identity-audit executable path")
}

fn audit_command(url: &str, report: &Path, extra: &[&str]) -> Command {
    let mut command = Command::new(audit_executable());
    command
        .kill_on_drop(true)
        .env("WADDLE_IDENTITY_AUDIT_DATABASE_URL", url)
        .args([
            "--driver",
            "sqlite",
            "--oidc-home-domain",
            "home.example.test",
            "--report",
        ])
        .arg(report)
        .args(extra);
    command
}

async fn audit(url: &str, report: &Path, extra: &[&str]) -> Output {
    audit_command(url, report, extra)
        .output()
        .await
        .expect("run audit executable")
}

fn public_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[tokio::test]
async fn complete_findings_keep_identifiers_in_private_artifact_only() {
    let (directory, url) = fixture().await;
    let mut database = SqliteConnection::connect(&url)
        .await
        .expect("fixture connection");
    for statement in [
        "INSERT INTO users VALUES('private-alice@home.example.test','private-alice','private-alice')",
        "INSERT INTO native_users VALUES(1,'private-alice','home.example.test','private-alice@home.example.test')",
    ] { sqlx::query(statement).execute(&mut database).await.expect("fixture data"); }
    database.close().await.expect("close fixture");
    let corpus = directory.path().join("frames.json");
    std::fs::write(
        &corpus,
        r#"["<message xmlns='jabber:client' to='fu&#xDF;ball@home.example.test'/>"]"#,
    )
    .expect("corpus");
    let report = directory.path().join("report.json");
    let output = audit(&url, &report, &["--frames", corpus.to_str().expect("path")]).await;
    assert_eq!(output.status.code(), Some(1), "{}", public_output(&output));
    let public = public_output(&output);
    for private in [
        "private-alice",
        "home.example.test",
        "fußball",
        "fussball",
        &url,
    ] {
        assert!(
            !public.contains(private),
            "identifier leaked to public output"
        );
    }
    let artifact: Value =
        serde_json::from_slice(&std::fs::read(&report).expect("private report")).expect("JSON");
    assert_eq!(artifact["status"], "complete_with_findings");
    assert_eq!(artifact["users_snapshot_complete"], true);
    assert_eq!(artifact["native_users_snapshot_complete"], true);
    assert_eq!(artifact["accounts"].as_array().expect("accounts").len(), 2);
    assert_eq!(
        artifact["collisions"].as_array().expect("collisions").len(),
        3
    );
    assert_eq!(
        artifact["frames"][0]["to"]["original_xml_decoded"],
        "fußball@home.example.test"
    );
    assert_eq!(
        artifact["frames"][0]["to"]["current_parser_jid"],
        "fussball@home.example.test"
    );
}

#[tokio::test]
async fn complete_empty_inventory_reports_two_present_sources() {
    let (directory, url) = fixture().await;
    let report = directory.path().join("report.json");
    let output = audit(&url, &report, &[]).await;
    assert_eq!(output.status.code(), Some(0), "{}", public_output(&output));
    let artifact: Value =
        serde_json::from_slice(&std::fs::read(report).expect("report")).expect("JSON");
    assert_eq!(artifact["status"], "complete_no_findings");
    assert_eq!(artifact["accounts"], serde_json::json!([]));
    assert_eq!(artifact["frame_corpus_requested"], false);
    assert_eq!(
        artifact["limitations"]
            .as_array()
            .expect("limitations")
            .len(),
        5
    );
}

#[tokio::test]
async fn incomplete_source_or_frame_corpus_never_publishes_a_report() {
    for failure in ["schema", "limit", "frames"] {
        let (directory, url) = fixture().await;
        let mut database = SqliteConnection::connect(&url)
            .await
            .expect("fixture connection");
        if failure == "schema" {
            sqlx::query("DROP TABLE native_users")
                .execute(&mut database)
                .await
                .expect("fixture mutation");
        } else {
            sqlx::query("INSERT INTO users VALUES('private@home.example.test','private','private'),('other@home.example.test','other','other')")
                .execute(&mut database).await.expect("fixture rows");
        }
        database.close().await.expect("close fixture");
        let report = directory.path().join("report.json");
        let output = match failure {
            "limit" => audit(&url, &report, &["--max-rows", "1"]).await,
            "frames" => {
                audit(
                    &url,
                    &report,
                    &["--frames", "/nonexistent-private-frame-corpus"],
                )
                .await
            }
            _ => audit(&url, &report, &[]).await,
        };
        assert_eq!(output.status.code(), Some(2), "{}", public_output(&output));
        assert!(public_output(&output).contains("incomplete"));
        assert!(!public_output(&output).contains("private"));
        assert!(!report.exists());
    }
}

#[tokio::test]
async fn locked_database_deadline_is_incomplete_without_a_partial_report() {
    let (directory, url) = fixture().await;
    let mut database = SqliteConnection::connect(&url)
        .await
        .expect("fixture connection");
    sqlx::query("BEGIN EXCLUSIVE")
        .execute(&mut database)
        .await
        .expect("owned fixture lock");
    let report = directory.path().join("report.json");
    let output = audit(&url, &report, &["--timeout-seconds", "1"]).await;
    sqlx::query("ROLLBACK")
        .execute(&mut database)
        .await
        .expect("release lock");
    assert_eq!(output.status.code(), Some(2), "{}", public_output(&output));
    assert!(public_output(&output).contains("timeout"));
    assert!(!report.exists());
}

#[tokio::test]
async fn connection_and_argument_failures_do_not_echo_sensitive_values() {
    let directory = tempfile::tempdir().expect("directory");
    let report = directory.path().join("report.json");
    for extra in [&[][..], &["--max-rows", "private-account@example.test"][..]] {
        let output = audit(
            "sqlite://private-secret@example.test/absent",
            &report,
            extra,
        )
        .await;
        assert_eq!(output.status.code(), Some(2));
        let public = public_output(&output);
        assert!(!public.contains("private-"));
        assert!(!public.contains("example.test"));
        assert!(!report.exists());
    }
}

#[tokio::test]
async fn encoded_file_uri_cannot_hide_live_wal_accounts_behind_a_decoy() {
    let (directory, url) = fixture().await;
    let path = directory.path().join("accounts.sqlite");
    let mut writer = SqliteConnection::connect(&url)
        .await
        .expect("WAL fixture writer");
    for statement in [
        "PRAGMA journal_mode=WAL",
        "PRAGMA wal_autocheckpoint=0",
        "PRAGMA wal_checkpoint(TRUNCATE)",
    ] {
        sqlx::query(statement)
            .execute(&mut writer)
            .await
            .expect("checkpoint empty fixture schema");
    }
    for statement in [
        "INSERT INTO users VALUES('wal-private@home.example.test','wal-private','wal-private')",
        "INSERT INTO native_users VALUES(1,'wal-private','home.example.test','wal-private@home.example.test')",
    ] { sqlx::query(statement).execute(&mut writer).await.expect("accounts only in live WAL"); }
    assert!(
        directory
            .path()
            .join("accounts.sqlite-wal")
            .metadata()
            .expect("live WAL")
            .len()
            > 0
    );

    // Control proves the exact dangerous option would hide these account rows.
    let mut immutable = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(format!("file:{}?immutable=true", path.display()))
            .read_only(true),
    )
    .await
    .expect("owned immutable control");
    let hidden: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&mut immutable)
        .await
        .expect("immutable control count");
    assert_eq!(hidden, 0);
    immutable.close().await.expect("close control");

    let report = directory.path().join("normal.json");
    let normal = audit(&url, &report, &[]).await;
    assert_eq!(normal.status.code(), Some(1), "{}", public_output(&normal));
    let artifact: Value =
        serde_json::from_slice(&std::fs::read(report).expect("normal report")).expect("JSON");
    assert_eq!(artifact["accounts"].as_array().expect("accounts").len(), 2);
    assert_eq!(
        artifact["collisions"].as_array().expect("collisions").len(),
        3
    );

    std::fs::write(
        directory.path().join("file:accounts.sqlite?immutable=true"),
        [],
    )
    .expect("literal decoy inspected by previous URL parser");
    for (index, malicious) in [
        "sqlite://file:accounts.sqlite%3Fimmutable=true".to_owned(),
        "sqlite://%66ile:accounts.sqlite%3fimmutable=true".to_owned(),
        "sqlite://file:accounts.sqlite%253Fimmutable=true".to_owned(),
        format!("{url}%3Fimmutable=true"),
        format!("{url}?immutable=true"),
    ]
    .into_iter()
    .enumerate()
    {
        let report = directory.path().join(format!("rejected-{index}.json"));
        let output = audit_command(&malicious, &report, &[])
            .current_dir(directory.path())
            .output()
            .await
            .expect("audit encoded URI");
        assert_eq!(output.status.code(), Some(2), "{}", public_output(&output));
        assert!(!report.exists());
        assert!(!public_output(&output).contains("wal-private"));
    }
    writer.close().await.expect("close WAL fixture writer");
}
