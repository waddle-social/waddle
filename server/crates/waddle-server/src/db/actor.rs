//! Kameo actor wrapping a logical database handle.

use std::time::Instant;

use kameo::message::Context;
use kameo::Actor;

use super::{Database, DatabaseError};

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
        let conn = self.db.guard().await?;
        conn.execute(&msg.sql, msg.params).await
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
        self.db.health_check().await
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
