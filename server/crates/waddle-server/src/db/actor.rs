//! Kameo actor wrapping a logical database handle.

use std::time::Instant;

use kameo::message::Context;
use kameo::Actor;
use sqlx::query;

use super::{Database, DatabaseError};

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
    pub params: Vec<crate::db::Value>,
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
    pub token_hash: String,
    pub auth_context_id: Option<uuid::Uuid>,
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

        match &self.db.backend {
            super::DatabaseBackend::Sqlite(pool) => {
                let mut tx = pool.begin().await?;

                query(
                    r#"
                    INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at)
                    VALUES (?, ?, ?, ?, ?)
                    ON CONFLICT DO NOTHING
                    "#,
                )
                .bind(&msg.user_jid)
                .bind(&msg.username)
                .bind(&msg.xmpp_localpart)
                .bind(msg.created_at.clone())
                .bind(msg.created_at.clone())
                .execute(&mut *tx)
                .await?;

                query(
                    r#"
                    INSERT INTO sessions (
                        id, user_jid, token_hash, auth_context_id, auth_context_version,
                        principal_auth_epoch, expires_at, created_at, last_used_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                )
                .bind(&msg.session_id)
                .bind(&msg.user_jid)
                .bind(&msg.token_hash)
                .bind(msg.auth_context_id.map(|id| id.to_string()))
                .bind(i64::try_from(msg.auth_context_version).map_err(|_| {
                    DatabaseError::QueryFailed("auth context version overflow".to_string())
                })?)
                .bind(i64::try_from(msg.principal_auth_epoch).map_err(|_| {
                    DatabaseError::QueryFailed("principal auth epoch overflow".to_string())
                })?)
                .bind(&msg.expires_at)
                .bind(&msg.created_at)
                .bind(&msg.last_used_at)
                .execute(&mut *tx)
                .await?;

                tx.commit().await?;
            }
            super::DatabaseBackend::Postgres(pool) => {
                let mut tx = pool.begin().await?;

                query(
                    r#"
                    INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at)
                    VALUES ($1, $2, $3, $4, $5)
                    ON CONFLICT DO NOTHING
                    "#,
                )
                .bind(&msg.user_jid)
                .bind(&msg.username)
                .bind(&msg.xmpp_localpart)
                .bind(msg.created_at.clone())
                .bind(msg.created_at.clone())
                .execute(&mut *tx)
                .await?;

                query(
                    r#"
                    INSERT INTO sessions (
                        id, user_jid, token_hash, auth_context_id, auth_context_version,
                        principal_auth_epoch, expires_at, created_at, last_used_at
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    "#,
                )
                .bind(&msg.session_id)
                .bind(&msg.user_jid)
                .bind(&msg.token_hash)
                .bind(msg.auth_context_id)
                .bind(i64::try_from(msg.auth_context_version).map_err(|_| {
                    DatabaseError::QueryFailed("auth context version overflow".to_string())
                })?)
                .bind(i64::try_from(msg.principal_auth_epoch).map_err(|_| {
                    DatabaseError::QueryFailed("principal auth epoch overflow".to_string())
                })?)
                .bind(&msg.expires_at)
                .bind(&msg.created_at)
                .bind(&msg.last_used_at)
                .execute(&mut *tx)
                .await?;

                tx.commit().await?;
            }
        }

        Ok(())
    }
}

pub struct DbQuery {
    pub sql: String,
    pub params: Vec<crate::db::Value>,
}

pub type RowValues = Vec<crate::db::Value>;

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
                values.push(row.get_value(i)?);
            }
            result.push(values);
        }
        Ok(result)
    }
}

pub struct DbQueryOne {
    pub sql: String,
    pub params: Vec<crate::db::Value>,
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
                        values.push(row.get_value(i)?);
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
