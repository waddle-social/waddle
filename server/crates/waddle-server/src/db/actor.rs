//! Kameo actor wrapping a Database connection.
//!
//! The `DbActor` owns a `Database` and processes all operations sequentially,
//! eliminating the need for external Mutex locking. This is the foundation
//! of the actor-model migration described in issue #42 Phase 1.

use std::time::Instant;

use kameo::message::Context;
use kameo::Actor;

use super::{Database, DatabaseError};

/// Actor that owns a `Database` and handles queries sequentially.
///
/// Because Kameo processes messages one at a time, the actor holds a single
/// `Database` with no external synchronisation required.
#[derive(Actor)]
pub struct DbActor {
    db: Database,
    last_accessed: Instant,
}

impl DbActor {
    /// Create a new `DbActor` wrapping the given database.
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

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Request the inner `Database` handle (backward-compat for callers that still
/// use `guard()`).
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

/// Execute a SQL statement and return the number of affected rows.
pub struct DbExecute {
    pub sql: String,
    pub params: Vec<libsql::Value>,
}

impl kameo::message::Message<DbExecute> for DbActor {
    type Reply = Result<u64, DatabaseError>;

    async fn handle(
        &mut self,
        msg: DbExecute,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();
        let conn = self.db.guard().await?;
        let rows = conn.execute(&msg.sql, msg.params).await?;
        Ok(rows)
    }
}

/// Execute a SQL query and return all result rows.
pub struct DbQuery {
    pub sql: String,
    pub params: Vec<libsql::Value>,
}

/// A single row of query results, represented as a vector of `libsql::Value`.
pub type RowValues = Vec<libsql::Value>;

impl kameo::message::Message<DbQuery> for DbActor {
    type Reply = Result<Vec<RowValues>, DatabaseError>;

    async fn handle(&mut self, msg: DbQuery, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        self.touch();
        let conn = self.db.guard().await?;
        let mut rows = conn.query(&msg.sql, msg.params).await?;
        let col_count = rows.column_count();

        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut values = Vec::with_capacity(col_count as usize);
            for i in 0..col_count {
                values.push(row.get_value(i)?);
            }
            result.push(values);
        }
        Ok(result)
    }
}

/// Execute a SQL query and return at most one row.
pub struct DbQueryOne {
    pub sql: String,
    pub params: Vec<libsql::Value>,
}

impl kameo::message::Message<DbQueryOne> for DbActor {
    type Reply = Result<Option<RowValues>, DatabaseError>;

    async fn handle(
        &mut self,
        msg: DbQueryOne,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();
        let conn = self.db.guard().await?;
        let mut rows = conn.query(&msg.sql, msg.params).await?;
        let col_count = rows.column_count();

        match rows.next().await? {
            Some(row) => {
                let mut values = Vec::with_capacity(col_count as usize);
                for i in 0..col_count {
                    values.push(row.get_value(i)?);
                }
                Ok(Some(values))
            }
            None => Ok(None),
        }
    }
}

/// Check if the database is healthy.
pub struct DbHealthCheck;

impl kameo::message::Message<DbHealthCheck> for DbActor {
    type Reply = Result<bool, DatabaseError>;

    async fn handle(
        &mut self,
        _msg: DbHealthCheck,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();
        self.db.health_check().await
    }
}

/// Return milliseconds since the actor last handled a message.
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

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::ActorRef;

    async fn spawn_test_actor() -> ActorRef<DbActor> {
        let db = Database::in_memory("test-db-actor").await.expect("db");
        // Run migrations so tables exist
        let runner = super::super::MigrationRunner::global();
        runner.run(&db).await.expect("migrations");
        kameo::spawn(DbActor::new(db))
    }

    #[tokio::test]
    async fn test_get_database() {
        let actor = spawn_test_actor().await;
        let db: Database = actor.ask(GetDatabase).await.expect("ask");
        assert_eq!(db.name(), "test-db-actor");
    }

    #[tokio::test]
    async fn test_execute_and_query() {
        let actor = spawn_test_actor().await;

        // Create a table
        actor
            .ask(DbExecute {
                sql: "CREATE TABLE actor_test (id INTEGER PRIMARY KEY, val TEXT)".to_string(),
                params: vec![],
            })
            .await
            .expect("create table");

        // Insert a row
        let affected = actor
            .ask(DbExecute {
                sql: "INSERT INTO actor_test (val) VALUES (?)".to_string(),
                params: vec![libsql::Value::from("hello")],
            })
            .await
            .expect("insert");
        assert_eq!(affected, 1);

        // Query
        let rows = actor
            .ask(DbQuery {
                sql: "SELECT val FROM actor_test".to_string(),
                params: vec![],
            })
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn test_query_one() {
        let actor = spawn_test_actor().await;

        let row = actor
            .ask(DbQueryOne {
                sql: "SELECT 42".to_string(),
                params: vec![],
            })
            .await
            .expect("query one");
        assert!(row.is_some());
    }

    #[tokio::test]
    async fn test_health_check() {
        let actor = spawn_test_actor().await;
        let healthy = actor.ask(DbHealthCheck).await.expect("health check");
        assert!(healthy);
    }

    #[tokio::test]
    async fn test_idle_ms() {
        let actor = spawn_test_actor().await;
        let idle = actor.ask(GetIdleMs).await.expect("ask");
        // Should be very small since we just created it
        assert!(idle < 1000);
    }
}
