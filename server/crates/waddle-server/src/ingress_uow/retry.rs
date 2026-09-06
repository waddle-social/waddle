use std::{fmt, future::Future, time::Duration};

use crate::{db::DatabaseError, ingress_uow::IngressUowError};

/// Sanitized SQLSTATE classification retained after database diagnostics are
/// discarded at the ingress boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbRetryClass {
    SerializationFailure,
    Deadlock,
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
        match error.code().as_deref() {
            Some("40001") => Self::SerializationFailure,
            Some("40P01") => Self::Deadlock,
            _ => Self::NotRetryable,
        }
    }

    fn is_retryable(self) -> bool {
        matches!(self, Self::SerializationFailure | Self::Deadlock)
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

/// Retry a whole ingress transaction after serialization failures or deadlocks.
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

    use super::*;

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
