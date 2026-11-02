//! Kameo actor wrapping a logical database handle.

use crate::db::Value;
use std::time::Instant;

use kameo::message::Context;
use kameo::Actor;

use super::{Database, DatabaseError};

#[derive(Clone, Copy)]
enum AuthTable {
    Users,
    Sessions,
}

impl AuthTable {
    fn name(self) -> &'static str {
        match self {
            Self::Users => "users",
            Self::Sessions => "sessions",
        }
    }
}

fn auth_table_reference(database: &Database, table: AuthTable) -> String {
    match database.attested_pg_writer_capability() {
        Some(capability) => {
            // The schema comes from PhysicalAttestationConfig's validated
            // identifier grammar, and table names are private literals here.
            let schema = &capability.identity().schema.name.0;
            format!("\"{schema}\".\"{}\"", table.name())
        }
        None => table.name().to_owned(),
    }
}

fn mark_actor_result<T>(result: Result<T, DatabaseError>) -> Result<T, DatabaseError> {
    if result.is_err() {
        crate::telemetry::mark_span_error("database actor operation failed");
    }
    result
}

#[derive(Actor)]
pub struct DbActor {
    db: Database,
    last_accessed: Instant,
}

impl DbActor {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            last_accessed: Instant::now(),
        }
    }

    fn touch(&mut self) {
        self.last_accessed = Instant::now();
    }
}

pub struct GetDatabase;

impl kameo::message::Message<GetDatabase> for DbActor {
    type Reply = Database;

    async fn handle(
        &mut self,
        _msg: GetDatabase,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();
        self.db.clone()
    }
}

pub struct DbExecute {
    pub sql: String,
    pub params: Vec<Value>,
}

impl kameo::message::Message<DbExecute> for DbActor {
    type Reply = Result<u64, DatabaseError>;

    async fn handle(
        &mut self,
        msg: DbExecute,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();
        let result = async {
            let conn = self.db.guard().await?;
            conn.execute(&msg.sql, msg.params).await
        }
        .await;
        mark_actor_result(result)
    }
}

pub struct CreateAuthSession {
    pub session_id: String,
    pub user_jid: String,
    pub username: String,
    pub xmpp_localpart: String,
    /// Canonical form of `xmpp_localpart` (`users.localpart_key`).
    pub localpart_key: Option<jid::NodePart>,
    pub token_hash: String,
    pub auth_context_id: uuid::Uuid,
    pub auth_context_version: u64,
    pub principal_auth_epoch: u64,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub last_used_at: String,
}

impl kameo::message::Message<CreateAuthSession> for DbActor {
    type Reply = Result<(), DatabaseError>;

    async fn handle(
        &mut self,
        msg: CreateAuthSession,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();

        let auth_context_version = i64::try_from(msg.auth_context_version)
            .map_err(|_| DatabaseError::QueryFailed("auth context version overflow".to_string()))?;
        let principal_auth_epoch = i64::try_from(msg.principal_auth_epoch)
            .map_err(|_| DatabaseError::QueryFailed("principal auth epoch overflow".to_string()))?;
        let mut tx = self.db.begin().await?;
        let users = auth_table_reference(&self.db, AuthTable::Users);
        tx.execute(
            &format!(
                r#"
            INSERT INTO {users} (jid, username, xmpp_localpart, localpart_key, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT DO NOTHING
            "#
            ),
            crate::db_params![
                &msg.user_jid,
                &msg.username,
                &msg.xmpp_localpart,
                msg.localpart_key.as_ref().map(|key| key.as_str()),
                &msg.created_at,
                &msg.created_at
            ],
        )
        .await?;

        // The column is TEXT on both engines (index-served principal
        // resolution with one shared SQL string); bind the string form.
        let sessions = auth_table_reference(&self.db, AuthTable::Sessions);
        tx.execute(
            &format!(
                r#"
            INSERT INTO {sessions} (
                id, user_jid, token_hash, auth_context_id, auth_context_version,
                principal_auth_epoch, expires_at, created_at, last_used_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#
            ),
            crate::db_params![
                &msg.session_id,
                &msg.user_jid,
                &msg.token_hash,
                msg.auth_context_id.to_string(),
                auth_context_version,
                principal_auth_epoch,
                msg.expires_at.clone(),
                &msg.created_at,
                &msg.last_used_at
            ],
        )
        .await?;

        tx.commit().await?;

        Ok(())
    }
}

pub struct DbQuery {
    pub sql: String,
    pub params: Vec<Value>,
}

pub type RowValues = Vec<Value>;

impl kameo::message::Message<DbQuery> for DbActor {
    type Reply = Result<Vec<RowValues>, DatabaseError>;

    async fn handle(&mut self, msg: DbQuery, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        self.touch();
        let conn = self.db.guard().await?;
        let mut rows = conn.query(&msg.sql, msg.params).await?;
        let col_count = rows.column_count();

        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut values = Vec::with_capacity(col_count);
            for i in 0..col_count {
                values.push(row.value(i)?);
            }
            result.push(values);
        }
        Ok(result)
    }
}

pub struct DbQueryOne {
    pub sql: String,
    pub params: Vec<Value>,
}

impl kameo::message::Message<DbQueryOne> for DbActor {
    type Reply = Result<Option<RowValues>, DatabaseError>;

    async fn handle(
        &mut self,
        msg: DbQueryOne,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();
        let result = async {
            let conn = self.db.guard().await?;
            let mut rows = conn.query(&msg.sql, msg.params).await?;
            let col_count = rows.column_count();

            match rows.next().await? {
                Some(row) => {
                    let mut values = Vec::with_capacity(col_count);
                    for i in 0..col_count {
                        values.push(row.value(i)?);
                    }
                    Ok(Some(values))
                }
                None => Ok(None),
            }
        }
        .await;
        mark_actor_result(result)
    }
}

pub struct DbHealthCheck;

impl kameo::message::Message<DbHealthCheck> for DbActor {
    type Reply = Result<bool, DatabaseError>;

    async fn handle(
        &mut self,
        _msg: DbHealthCheck,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();
        let result = mark_actor_result(self.db.health_check().await);
        if matches!(result, Ok(false)) {
            crate::telemetry::mark_span_error("database health check returned unhealthy");
        }
        result
    }
}

pub struct GetIdleMs;

impl kameo::message::Message<GetIdleMs> for DbActor {
    type Reply = u64;

    async fn handle(
        &mut self,
        _msg: GetIdleMs,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_accessed.elapsed().as_millis() as u64
    }
}
