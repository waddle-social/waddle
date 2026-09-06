use super::*;
use crate::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use kameo::actor::Spawn;

struct Fixture {
    actor: ActorRef<DbActor>,
    postgres: Option<(sqlx::PgPool, String)>,
}

impl Fixture {
    async fn open(driver: DatabaseDriver) -> Option<Self> {
        let (database, postgres) = match driver {
            DatabaseDriver::Sqlite => (
                Database::in_memory("muc-invite-ledger")
                    .await
                    .expect("open SQLite ledger"),
                None,
            ),
            DatabaseDriver::Postgres => {
                let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
                    eprintln!("skipping PostgreSQL invitation ledger test: WADDLE_TEST_POSTGRES_URL not set");
                    return None;
                };
                let admin = sqlx::PgPool::connect(&database_url)
                    .await
                    .expect("connect PostgreSQL fixture admin");
                let schema = format!("muc_invites_{}", uuid::Uuid::new_v4().simple());
                sqlx::query(&format!("CREATE SCHEMA {schema}"))
                    .execute(&admin)
                    .await
                    .expect("create isolated ledger schema");
                let mut url = url::Url::parse(&database_url).expect("PostgreSQL URL");
                let retained: Vec<_> = url
                    .query_pairs()
                    .filter(|(key, _)| key != "options")
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
                url.query_pairs_mut()
                    .clear()
                    .extend_pairs(retained)
                    .append_pair("options", &format!("-c search_path={schema}"));
                let database = Database::from_config(
                    "muc-invite-ledger",
                    &DatabaseConfig::new(driver, url.to_string()),
                )
                .await
                .expect("open isolated PostgreSQL ledger");
                (database, Some((admin, schema)))
            }
        };
        MigrationRunner::single()
            .run(&database)
            .await
            .expect("migrate invitation ledger");
        Some(Self {
            actor: DbActor::spawn(DbActor::new(database)),
            postgres,
        })
    }

    async fn close(self) {
        self.actor.kill();
        if let Some((admin, schema)) = self.postgres {
            sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
                .execute(&admin)
                .await
                .expect("drop isolated ledger schema");
        }
    }
}

async fn recorded_timestamp(
    actor: &ActorRef<DbActor>,
    invite: &OutstandingInvite,
) -> chrono::DateTime<chrono::Utc> {
    let rows = actor
        .ask(DbQuery {
            sql: "SELECT created_at FROM muc_pending_invites WHERE room_jid = ? AND invitee_jid = ? AND inviter_jid = ?".to_owned(),
            params: vec![invite.room.to_string().into(), invite.invitee.to_string().into(), invite.inviter.to_string().into()],
        })
        .await
        .expect("read persisted timestamp");
    assert_eq!(rows.len(), 1);
    let value = row_value(&rows[0], 0)
        .expect("timestamp column")
        .as_string()
        .expect("timestamp text");
    chrono::DateTime::parse_from_rfc3339(&value)
        .expect("typed stored timestamp")
        .with_timezone(&chrono::Utc)
}

async fn assert_frozen_timestamp_dedup_and_claim(driver: DatabaseDriver) {
    let Some(fixture) = Fixture::open(driver).await else {
        return;
    };
    let actor = &fixture.actor;
    let invite = OutstandingInvite {
        room: "frozen-ledger@muc.example.com".parse().expect("room"),
        invitee: "bob@example.com".parse().expect("invitee"),
        inviter: "alice@example.com".parse().expect("inviter"),
    };
    let frozen = chrono::Utc::now() - chrono::Duration::days(2);
    assert_eq!(
        record_invite_at(actor.clone(), &invite, frozen)
            .await
            .expect("record frozen invitation"),
        RecordOutcome::New { created_at: frozen }
    );
    assert_eq!(recorded_timestamp(actor, &invite).await, frozen);
    assert_eq!(
        record_invite_at(actor.clone(), &invite, frozen + chrono::Duration::hours(1))
            .await
            .expect("deduplicate invitation"),
        RecordOutcome::AlreadyOutstanding
    );
    assert_eq!(
        recorded_timestamp(actor, &invite).await,
        frozen,
        "a duplicate must retain the authoritative original timestamp"
    );
    assert_eq!(
        list_invites(actor.clone(), &invite.room, &invite.invitee)
            .await
            .expect("list invitation"),
        vec![invite.clone()]
    );
    let wrong_inviter = OutstandingInvite {
        inviter: "mallory@example.com".parse().expect("other inviter"),
        ..invite.clone()
    };
    assert!(!claim_invite(actor.clone(), &wrong_inviter)
        .await
        .expect("reject wrong invitation identity"));
    assert!(claim_invite(actor.clone(), &invite)
        .await
        .expect("first decline claims invitation"));
    assert!(!claim_invite(actor.clone(), &invite)
        .await
        .expect("second decline cannot claim again"));
    assert!(list_invites(actor.clone(), &invite.room, &invite.invitee)
        .await
        .expect("list claimed invitation")
        .is_empty());

    let expired = chrono::Utc::now() - INVITE_TTL - chrono::Duration::days(1);
    record_invite_at(actor.clone(), &invite, expired)
        .await
        .expect("seed expired invitation");
    assert_eq!(
        record_invite_at(actor.clone(), &invite, frozen)
            .await
            .expect("refresh expired invitation"),
        RecordOutcome::New { created_at: frozen }
    );
    assert_eq!(recorded_timestamp(actor, &invite).await, frozen);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_record_invite_at_preserves_frozen_timestamp_dedup_and_claim() {
    assert_frozen_timestamp_dedup_and_claim(DatabaseDriver::Sqlite).await;
}

#[tokio::test]
async fn postgres_record_invite_at_preserves_frozen_timestamp_dedup_and_claim() {
    assert_frozen_timestamp_dedup_and_claim(DatabaseDriver::Postgres).await;
}
