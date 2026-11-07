//! Bounds for read-only PostgreSQL attachment and lineage checks.

use sqlx::PgConnection;

use super::{DatabaseError, Transaction};

struct TimeoutSetting {
    name: &'static str,
    show: &'static str,
    cap_millis: u64,
    set_local: &'static str,
    set_session: &'static str,
}

const TIMEOUTS: [TimeoutSetting; 2] = [
    TimeoutSetting {
        name: "lock_timeout",
        show: "SHOW lock_timeout",
        cap_millis: 5_000,
        set_local: "SET LOCAL lock_timeout = '5s'",
        set_session: "SET lock_timeout = '5s'",
    },
    TimeoutSetting {
        name: "statement_timeout",
        show: "SHOW statement_timeout",
        cap_millis: 10_000,
        set_local: "SET LOCAL statement_timeout = '10s'",
        set_session: "SET statement_timeout = '10s'",
    },
];

const IDLE_TRANSACTION_TIMEOUT: TimeoutSetting = TimeoutSetting {
    name: "idle_in_transaction_session_timeout",
    show: "SHOW idle_in_transaction_session_timeout",
    cap_millis: 30_000,
    set_local: "SET LOCAL idle_in_transaction_session_timeout = '30s'",
    set_session: "SET idle_in_transaction_session_timeout = '30s'",
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadOnlyTimeoutError {
    #[error("could not verify PostgreSQL {setting}")]
    InvalidSetting { setting: &'static str },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// SHOW and SET LOCAL do not establish a repeatable-read snapshot. Install
/// conservative bounds before the attestor reads identity without loosening a
/// tighter role or session setting. Outer checkout and network deadlines apply
/// separately.
pub(crate) async fn bound_postgres_waits(
    tx: &mut Transaction<'_>,
) -> Result<(), ReadOnlyTimeoutError> {
    for setting in &TIMEOUTS {
        bound_postgres_local_setting(tx, setting).await?;
    }
    Ok(())
}

/// Bound how long PostgreSQL retains an idle transaction if a client stops
/// receiving replies, without relaxing an existing tighter session setting.
pub(crate) async fn bound_postgres_idle_transaction(
    tx: &mut Transaction<'_>,
) -> Result<(), ReadOnlyTimeoutError> {
    bound_postgres_local_setting(tx, &IDLE_TRANSACTION_TIMEOUT).await
}

async fn bound_postgres_local_setting(
    tx: &mut Transaction<'_>,
    setting: &TimeoutSetting,
) -> Result<(), ReadOnlyTimeoutError> {
    let mut rows = tx.query(setting.show, ()).await?;
    let value: String = rows
        .next()
        .await?
        .ok_or(ReadOnlyTimeoutError::InvalidSetting {
            setting: setting.name,
        })?
        .get(0)?;
    drop(rows);
    if requires_cap(setting, &value)? {
        tx.execute(setting.set_local, ()).await?;
    }
    Ok(())
}

/// Saved session values for a physical checkout hook. The hook runs before a
/// transaction can install LOCAL limits; restoring these values before lending
/// preserves the borrower's existing session policy.
pub(crate) struct SessionTimeouts(Vec<(&'static str, String)>);

pub(crate) async fn bound_postgres_session_waits(
    connection: &mut PgConnection,
) -> Result<SessionTimeouts, ReadOnlyTimeoutError> {
    let mut restore = Vec::new();
    for setting in &TIMEOUTS {
        let value: String = sqlx::query_scalar(setting.show)
            .fetch_one(&mut *connection)
            .await
            .map_err(DatabaseError::from)?;
        if requires_cap(setting, &value)? {
            sqlx::query(setting.set_session)
                .execute(&mut *connection)
                .await
                .map_err(DatabaseError::from)?;
            restore.push((setting.name, value));
        }
    }
    Ok(SessionTimeouts(restore))
}

impl SessionTimeouts {
    pub(crate) async fn restore(
        self,
        connection: &mut PgConnection,
    ) -> Result<(), ReadOnlyTimeoutError> {
        for (setting, value) in self.0 {
            sqlx::query("SELECT pg_catalog.set_config($1, $2, false)")
                .bind(setting)
                .bind(value)
                .execute(&mut *connection)
                .await
                .map_err(DatabaseError::from)?;
        }
        Ok(())
    }
}

fn requires_cap(setting: &TimeoutSetting, value: &str) -> Result<bool, ReadOnlyTimeoutError> {
    let millis = timeout_millis(value).ok_or(ReadOnlyTimeoutError::InvalidSetting {
        setting: setting.name,
    })?;
    Ok(millis == 0 || millis > setting.cap_millis)
}

pub(crate) fn timeout_millis(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Ok(millis) = value.parse::<u64>() {
        return Some(millis);
    }
    for (suffix, multiplier) in [
        ("ms", 1),
        ("min", 60_000),
        ("s", 1_000),
        ("h", 3_600_000),
        ("d", 86_400_000),
    ] {
        if let Some(number) = value.strip_suffix(suffix) {
            return number.trim().parse::<u64>().ok()?.checked_mul(multiplier);
        }
    }
    None
}
