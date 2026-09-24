use std::{fmt, future::Future, time::Duration};

use crate::{db::DatabaseError, ingress_uow::IngressUowError};

/// Sanitized driver error classification retained after database diagnostics are
/// discarded at the ingress boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbRetryClass {
    SerializationFailure,
    Deadlock,
    SqliteContention,
    NotRetryable,
}

impl DbRetryClass {
    pub fn from_database_error(error: &DatabaseError) -> Self {
        let DatabaseError::Internal(error) = error else {
            return Self::NotRetryable;
        };
        Self::from_sqlx_error(error)
    }

    pub(crate) fn from_sqlx_error(error: &sqlx::Error) -> Self {
        let sqlx::Error::Database(error) = error else {
            return Self::NotRetryable;
        };
        if error
            .try_downcast_ref::<sqlx::sqlite::SqliteError>()
            .is_some()
        {
            // SQLite extended result codes retain their primary code in the
            // low byte. Never interpret another driver's numeric SQLSTATEs
            // as SQLite result codes.
            return match error.code().and_then(|code| code.parse::<u32>().ok()) {
                Some(code) if matches!(code & 0xff, 5 | 6) => Self::SqliteContention,
                _ => Self::NotRetryable,
            };
        }
        match error.code().as_deref() {
            Some("40001") => Self::SerializationFailure,
            Some("40P01") => Self::Deadlock,
            _ => Self::NotRetryable,
        }
    }

    fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::SerializationFailure | Self::Deadlock | Self::SqliteContention
        )
    }
}

/// The last retryable failure after all transaction attempts have been used.
#[derive(Debug)]
pub struct RetryExhausted<E> {
    pub attempts: usize,
    pub last_error: E,
}

impl<E: fmt::Display> fmt::Display for RetryExhausted<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ingress transaction retries exhausted after {} attempts",
            self.attempts
        )
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for RetryExhausted<E> {}

/// Retry a whole ingress transaction after serialization failures, deadlocks,
/// or SQLite BUSY/LOCKED failures.
///
/// `operation` must open a new transaction every time it is invoked; the
/// helper deliberately knows nothing about transactions so stale locks and
/// aborted PostgreSQL transaction state cannot cross attempts.
pub async fn run_with_retry<T, F, Fut>(
    attempts: usize,
    mut operation: F,
) -> Result<T, RetryExhausted<IngressUowError>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, IngressUowError>>,
{
    assert!(attempts > 0, "retry attempts must be non-zero");
    let mut delay = Duration::from_millis(2);
    for attempt in 1..=attempts {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) if error.retry_class().is_retryable() && attempt < attempts => {
                let ceiling = delay.saturating_mul(3).min(Duration::from_millis(50));
                let ceiling_ms = u64::try_from(ceiling.as_millis()).unwrap_or(50);
                let delay_ms = rand::random_range(2..=ceiling_ms.max(2));
                delay = Duration::from_millis(delay_ms);
                tokio::time::sleep(delay).await;
            }
            Err(last_error) => {
                return Err(RetryExhausted {
                    attempts: attempt,
                    last_error,
                });
            }
        }
    }
    unreachable!("non-zero attempts return from the loop")
}

/// Recognize only driver timeout evidence, before diagnostics are discarded.
pub(crate) fn is_database_timeout(error: &DatabaseError) -> bool {
    match error {
        DatabaseError::Internal(sqlx::Error::PoolTimedOut) => true,
        DatabaseError::Internal(sqlx::Error::Database(error)) => {
            matches!(error.code().as_deref(), Some("57014" | "55P03"))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqlx::{Connection, Executor};

    use super::*;

    #[tokio::test]
    async fn classifies_actual_sqlite_busy_and_extended_busy_snapshot() {
        let file = tempfile::NamedTempFile::new().expect("SQLite fixture");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(file.path())
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(Duration::ZERO);
        let mut owner = sqlx::SqliteConnection::connect_with(&options)
            .await
            .expect("owner connection");
        owner
            .execute("CREATE TABLE retry_value (value INTEGER); INSERT INTO retry_value VALUES (1)")
            .await
            .expect("fixture table");
        let mut contender = sqlx::SqliteConnection::connect_with(&options)
            .await
            .expect("contender connection");
        owner.execute("BEGIN IMMEDIATE").await.expect("write lock");
        let busy = contender
            .execute("BEGIN IMMEDIATE")
            .await
            .expect_err("writer busy");
        assert_sqlite_retry_class(&busy, 5);
        owner.execute("ROLLBACK").await.expect("release writer");

        contender
            .execute("BEGIN; SELECT value FROM retry_value")
            .await
            .expect("read snapshot");
        owner
            .execute("UPDATE retry_value SET value = 2")
            .await
            .expect("advance snapshot");
        let stale = contender
            .execute("UPDATE retry_value SET value = 3")
            .await
            .expect_err("stale snapshot");
        assert_sqlite_retry_class(&stale, 517);
        contender
            .execute("ROLLBACK")
            .await
            .expect("release snapshot");
    }

    #[tokio::test]
    async fn classifies_actual_sqlite_shared_cache_deadlock() {
        let file = tempfile::NamedTempFile::new().expect("SQLite fixture");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(file.path())
            .shared_cache(true)
            .busy_timeout(Duration::ZERO);
        let mut left = sqlx::SqliteConnection::connect_with(&options)
            .await
            .expect("left connection");
        left.execute(
            "CREATE TABLE retry_value (value INTEGER); INSERT INTO retry_value VALUES (1)",
        )
        .await
        .expect("fixture table");
        let mut right = sqlx::SqliteConnection::connect_with(&options)
            .await
            .expect("right connection");
        for connection in [&mut left, &mut right] {
            connection
                .execute("BEGIN; SELECT value FROM retry_value")
                .await
                .expect("shared read lock");
        }
        // Each transaction holds the read lock the other needs to upgrade.
        // The driver must report LOCKED for one, whose rollback releases the other.
        let (left, right) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                upgrade_and_rollback(&mut left),
                upgrade_and_rollback(&mut right)
            )
        })
        .await
        .expect("driver detects the deadlock");
        let errors: Vec<_> = [left, right].into_iter().filter_map(Result::err).collect();
        assert_eq!(errors.len(), 1);
        assert_sqlite_retry_class(&errors[0], 6);
    }

    async fn upgrade_and_rollback(
        connection: &mut sqlx::SqliteConnection,
    ) -> Result<(), sqlx::Error> {
        let result = connection
            .execute("UPDATE retry_value SET value = 2")
            .await
            .map(|_| ());
        connection
            .execute("ROLLBACK")
            .await
            .expect("release deadlock participant");
        result
    }

    fn assert_sqlite_retry_class(error: &sqlx::Error, expected_code: u32) {
        let code: u32 = error
            .as_database_error()
            .and_then(|error| error.code())
            .expect("SQLite code")
            .parse()
            .expect("numeric SQLite code");
        assert_eq!(code, expected_code);
        assert_eq!(
            DbRetryClass::from_sqlx_error(error),
            DbRetryClass::SqliteContention
        );
    }

    #[tokio::test]
    async fn sqlite_constraints_remain_non_retryable() {
        let mut connection = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .expect("SQLite connection");
        connection
            .execute("CREATE TABLE checked_value (value INTEGER CHECK (value > 0))")
            .await
            .expect("constraint fixture");
        let error = connection
            .execute("INSERT INTO checked_value VALUES (0)")
            .await
            .expect_err("check violation");
        assert_eq!(
            DbRetryClass::from_sqlx_error(&error),
            DbRetryClass::NotRetryable
        );
    }

    #[derive(Debug, thiserror::Error)]
    #[error("SQLSTATE fixture")]
    struct SqlStateError(&'static str);

    impl sqlx::error::DatabaseError for SqlStateError {
        fn message(&self) -> &str {
            "SQLSTATE fixture"
        }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(self.0.into())
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    #[test]
    fn numeric_sqlstates_are_not_sqlite_result_codes() {
        for (code, expected) in [
            ("40001", DbRetryClass::SerializationFailure),
            ("40P01", DbRetryClass::Deadlock),
            // Character-not-in-repertoire's numeric value has low byte 5.
            ("22021", DbRetryClass::NotRetryable),
            ("5", DbRetryClass::NotRetryable),
            ("6", DbRetryClass::NotRetryable),
        ] {
            let error = sqlx::Error::Database(Box::new(SqlStateError(code)));
            assert_eq!(DbRetryClass::from_sqlx_error(&error), expected);
        }
    }

    #[tokio::test]
    async fn retries_only_sanitized_retryable_database_classes() {
        let attempts = AtomicUsize::new(0);
        let result = run_with_retry(5, || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    Err(IngressUowError::Database {
                        retry_class: DbRetryClass::SerializationFailure,
                    })
                } else {
                    Ok(42_u8)
                }
            }
        })
        .await;
        assert_eq!(result.expect("retryable operation succeeds"), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stops_at_the_first_non_retryable_failure() {
        let attempts = AtomicUsize::new(0);
        let result = run_with_retry(5, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(IngressUowError::Database {
                    retry_class: DbRetryClass::NotRetryable,
                })
            }
        })
        .await;
        assert!(matches!(
            result,
            Err(RetryExhausted {
                attempts: 1,
                last_error: IngressUowError::Database {
                    retry_class: DbRetryClass::NotRetryable
                }
            })
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn execution_deadline_cancels_sqlite_retry_backoff() {
        let attempts = AtomicUsize::new(0);
        let result = tokio::time::timeout(
            Duration::from_millis(1),
            run_with_retry(5, || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async {
                    Err::<(), _>(IngressUowError::Database {
                        retry_class: DbRetryClass::SqliteContention,
                    })
                }
            }),
        )
        .await;
        assert!(result.is_err(), "the existing execution deadline must win");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reports_typed_exhaustion_after_all_five_retryable_attempts() {
        let attempts = AtomicUsize::new(0);
        let result = run_with_retry(5, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(IngressUowError::Database {
                    retry_class: DbRetryClass::SerializationFailure,
                })
            }
        })
        .await;
        assert!(matches!(
            result,
            Err(RetryExhausted {
                attempts: 5,
                last_error: IngressUowError::Database {
                    retry_class: DbRetryClass::SerializationFailure
                }
            })
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 5);
    }
}
