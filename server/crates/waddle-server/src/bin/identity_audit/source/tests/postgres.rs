use std::process::{Child, Command, Stdio};
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::{Connection, Row};
use tempfile::TempDir;

use crate::model::AuditError;
use crate::source::{postgres_snapshot, read_postgres};

struct OwnedPostgres {
    child: Child,
    directory: TempDir,
}

impl Drop for OwnedPostgres {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // TempDir is dropped only after the exact child we spawned has exited.
    }
}

impl OwnedPostgres {
    fn start() -> Self {
        let directory = tempfile::tempdir().expect("private fixture directory");
        let data = directory.path().join("data");
        let socket = directory.path().join("socket");
        std::fs::create_dir(&socket).expect("private socket directory");
        let init = Command::new("initdb")
            .arg("-D")
            .arg(&data)
            .args(["--auth=trust", "--no-locale", "--username=audit_fixture"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("native initdb must be on PATH for this explicitly selected test");
        assert!(init.success(), "initdb must succeed in the owned TempDir");
        let child = Command::new("postgres")
            .arg("-D")
            .arg(data)
            .arg("-k")
            .arg(socket)
            .args(["-h", "", "-p", "5432", "-F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start owned postgres");
        Self { child, directory }
    }

    fn options(&self) -> PgConnectOptions {
        PgConnectOptions::new()
            .host(
                self.directory
                    .path()
                    .join("socket")
                    .to_str()
                    .expect("UTF-8 temporary path"),
            )
            .port(5432)
            .username("audit_fixture")
            .database("postgres")
    }

    async fn connect(&mut self) -> PgConnection {
        for _ in 0..100 {
            if let Ok(connection) = PgConnection::connect_with(&self.options()).await {
                return connection;
            }
            assert!(
                self.child.try_wait().expect("child status").is_none(),
                "owned postgres exited before readiness"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("owned native postgres did not become ready");
    }
}

/// This test owns its entire cluster. It never reads an external database URL.
#[tokio::test]
#[ignore = "requires native initdb/postgres on PATH; select explicitly with --ignored"]
async fn native_postgres_snapshot_read_only_and_non_bypass_rls() {
    let mut fixture = OwnedPostgres::start();
    let mut reader = fixture.connect().await;
    let mut writer = fixture.connect().await;
    for sql in [
        "CREATE TABLE public.users(jid TEXT PRIMARY KEY, xmpp_localpart TEXT, localpart_key TEXT)",
        "CREATE TABLE public.native_users(id BIGINT PRIMARY KEY, username TEXT, domain TEXT, jid_key TEXT)",
        "INSERT INTO public.users VALUES('alice@home.example.test','alice','alice')",
        "INSERT INTO public.native_users VALUES(1,'alice','home.example.test','alice@home.example.test')",
    ] { sqlx::query(sql).execute(&mut writer).await.expect("owned fixture schema/data"); }
    let rows = read_postgres(&mut reader, "home.example.test", 10)
        .await
        .expect("both sources");
    assert_eq!(rows.len(), 2);

    assert!(matches!(
        read_postgres(&mut reader, "home.example.test", 1).await,
        Err(AuditError::Limit)
    ));
    sqlx::query("CREATE TABLE public.native_child() INHERITS(public.native_users)")
        .execute(&mut writer)
        .await
        .expect("owned inherited fixture");
    assert!(matches!(
        read_postgres(&mut reader, "home.example.test", 10).await,
        Err(AuditError::Schema)
    ));
    sqlx::query("DROP TABLE public.native_child")
        .execute(&mut writer)
        .await
        .expect("remove owned inherited fixture");

    let mut snapshot = postgres_snapshot(&mut reader)
        .await
        .expect("actual audit snapshot setup");
    sqlx::query("SET lock_timeout = '100ms'")
        .execute(&mut writer)
        .await
        .expect("fixture DDL deadline");
    let blocked_ddl =
        sqlx::query("ALTER TABLE public.native_users RENAME TO replaced_native_users")
            .execute(&mut writer)
            .await
            .expect_err("audit relation lock must pin its name");
    assert_eq!(
        blocked_ddl
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("55P03")
    );
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM public.native_users")
        .fetch_one(&mut *snapshot)
        .await
        .expect("first snapshot read");
    sqlx::query("INSERT INTO public.native_users VALUES(2,'bob','home.example.test','bob@home.example.test')").execute(&mut writer).await.expect("concurrent fixture writer");
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM public.native_users")
        .fetch_one(&mut *snapshot)
        .await
        .expect("same snapshot read");
    let settings = sqlx::query("SELECT current_setting('transaction_isolation') AS isolation, current_setting('transaction_read_only') AS read_only, current_setting('row_security') AS row_security")
        .fetch_one(&mut *snapshot).await.expect("settings");
    assert_eq!(before, after);
    assert_eq!(settings.get::<&str, _>("isolation"), "repeatable read");
    assert_eq!(settings.get::<&str, _>("read_only"), "on");
    assert_eq!(settings.get::<&str, _>("row_security"), "off");
    let rejected = sqlx::query("DELETE FROM public.users")
        .execute(&mut *snapshot)
        .await
        .expect_err("read-only transaction must reject writes");
    assert_eq!(
        rejected
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("25006")
    );
    snapshot
        .rollback()
        .await
        .expect("rollback read-only transaction");

    for sql in [
        "ALTER TABLE public.users ENABLE ROW LEVEL SECURITY",
        "CREATE POLICY hidden_accounts ON public.users USING (false)",
        "CREATE ROLE audit_reader NOLOGIN NOSUPERUSER NOBYPASSRLS",
        "GRANT USAGE ON SCHEMA public TO audit_reader",
        "GRANT SELECT ON public.users, public.native_users TO audit_reader",
    ] {
        sqlx::query(sql)
            .execute(&mut writer)
            .await
            .expect("owned fixture RLS");
    }
    sqlx::query("SET ROLE audit_reader")
        .execute(&mut reader)
        .await
        .expect("non-bypass reader");
    let hidden: i64 = sqlx::query_scalar("SELECT count(*) FROM public.users")
        .fetch_one(&mut reader)
        .await
        .expect("ordinary filtered read");
    assert_eq!(
        hidden, 0,
        "control proves this role would silently omit accounts"
    );
    assert!(
        matches!(
            read_postgres(&mut reader, "home.example.test", 10).await,
            Err(AuditError::Read)
        ),
        "RLS must fail closed, not return only native rows"
    );
    reader.close().await.expect("close reader");
    writer.close().await.expect("close writer");
}
