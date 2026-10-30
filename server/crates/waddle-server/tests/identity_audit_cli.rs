use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::Connection;
use tempfile::TempDir;

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

fn audit(url: &str, report: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_waddle-identity-audit"))
        .env("WADDLE_IDENTITY_AUDIT_DATABASE_URL", url)
        .args([
            "--driver",
            "sqlite",
            "--oidc-home-domain",
            "home.example.test",
            "--report",
        ])
        .arg(report)
        .args(extra)
        .output()
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
    let output = audit(&url, &report, &["--frames", corpus.to_str().expect("path")]);
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
    let output = audit(&url, &report, &[]);
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
        4
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
            "limit" => audit(&url, &report, &["--max-rows", "1"]),
            "frames" => audit(
                &url,
                &report,
                &["--frames", "/nonexistent-private-frame-corpus"],
            ),
            _ => audit(&url, &report, &[]),
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
    let output = audit(&url, &report, &["--timeout-seconds", "1"]);
    sqlx::query("ROLLBACK")
        .execute(&mut database)
        .await
        .expect("release lock");
    assert_eq!(output.status.code(), Some(2), "{}", public_output(&output));
    assert!(public_output(&output).contains("timeout"));
    assert!(!report.exists());
}

#[test]
fn connection_and_argument_failures_do_not_echo_sensitive_values() {
    let directory = tempfile::tempdir().expect("directory");
    let report = directory.path().join("report.json");
    for extra in [&[][..], &["--max-rows", "private-account@example.test"][..]] {
        let output = audit(
            "sqlite://private-secret@example.test/absent",
            &report,
            extra,
        );
        assert_eq!(output.status.code(), Some(2));
        let public = public_output(&output);
        assert!(!public.contains("private-"));
        assert!(!public.contains("example.test"));
        assert!(!report.exists());
    }
}
