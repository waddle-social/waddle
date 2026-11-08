//! The one reader of the Postgres test-database environment.
//!
//! Postgres-backed tests skip when no test database is configured, so a
//! local run without Postgres still passes. A CI lane that provisions
//! Postgres also sets `WADDLE_TEST_REQUIRE_POSTGRES=1`; there a missing URL
//! is a lane defect, and returning `None` would let an early `return` report
//! a skipped test as a pass. `scripts/check-test-postgres-env.py` rejects
//! reads of either variable outside this module.

use std::env;
use std::ffi::OsString;
use std::fmt;

/// Connection URL of the Postgres database that tests may reset freely.
/// Private so that no caller can read the variable by name and bypass the
/// requirement (or `scripts/check-test-postgres-env.py`).
const TEST_POSTGRES_URL_ENV: &str = "WADDLE_TEST_POSTGRES_URL";

/// Set to `1` by lanes in which every Postgres-backed test must run.
const REQUIRE_TEST_POSTGRES_ENV: &str = "WADDLE_TEST_REQUIRE_POSTGRES";

/// Returns the test database URL, or `None` when the caller should skip.
///
/// # Panics
///
/// Panics, at the caller's location, when `WADDLE_TEST_REQUIRE_POSTGRES` is
/// `1` and `WADDLE_TEST_POSTGRES_URL` is unset or empty, when the URL is not
/// Unicode, or when the requirement flag holds a value other than unset,
/// empty, `0` or `1`.
#[track_caller]
pub fn require_test_postgres_url() -> Option<String> {
    match resolve(
        env::var_os(TEST_POSTGRES_URL_ENV),
        env::var_os(REQUIRE_TEST_POSTGRES_ENV),
    ) {
        Ok(url) => url,
        Err(error) => panic!("{error}"),
    }
}

/// Skips a PostgreSQL proxy witness when its configured target is not local.
///
/// The proxy harness can only connect to a local PostgreSQL target. Optional
/// local runs may skip that witness, but a lane that requires PostgreSQL must
/// fail so it cannot report the witness as passed without running it.
#[track_caller]
pub fn skip_unavailable_test_postgres_proxy_target(reason: impl fmt::Display) {
    let required = match parse_requirement(env::var_os(REQUIRE_TEST_POSTGRES_ENV)) {
        Ok(required) => required,
        Err(error) => panic!("{error}"),
    };
    if let Some(message) = unavailable_proxy_target_message(required, &reason.to_string()) {
        panic!("{message}");
    }
    eprintln!("skipping PostgreSQL proxy witness: {reason}");
}

#[derive(Debug, PartialEq, Eq)]
enum TestPostgresError {
    Missing,
    NonUnicodeUrl,
    InvalidRequirement(OsString),
}

impl fmt::Display for TestPostgresError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(
                formatter,
                "{REQUIRE_TEST_POSTGRES_ENV}=1 but {TEST_POSTGRES_URL_ENV} is unset or empty: \
                 this lane requires every Postgres-backed test to run, not skip"
            ),
            Self::NonUnicodeUrl => {
                write!(formatter, "{TEST_POSTGRES_URL_ENV} is not valid Unicode")
            }
            Self::InvalidRequirement(value) => write!(
                formatter,
                "{REQUIRE_TEST_POSTGRES_ENV} must be unset, empty, 0 or 1; got {value:?}"
            ),
        }
    }
}

fn resolve(
    url: Option<OsString>,
    requirement_value: Option<OsString>,
) -> Result<Option<String>, TestPostgresError> {
    let required = parse_requirement(requirement_value)?;
    match url {
        Some(url) if !url.is_empty() => url
            .into_string()
            .map(Some)
            .map_err(|_| TestPostgresError::NonUnicodeUrl),
        _ if required => Err(TestPostgresError::Missing),
        _ => Ok(None),
    }
}

fn parse_requirement(requirement_value: Option<OsString>) -> Result<bool, TestPostgresError> {
    match requirement_value {
        None => Ok(false),
        Some(value) => match value.to_str() {
            Some("" | "0") => Ok(false),
            Some("1") => Ok(true),
            _ => Err(TestPostgresError::InvalidRequirement(value)),
        },
    }
}

fn unavailable_proxy_target_message(required: bool, reason: &str) -> Option<String> {
    required.then(|| {
        format!(
            "WADDLE_TEST_REQUIRE_POSTGRES=1 but the PostgreSQL proxy witness requires a local target: {reason}"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{resolve, unavailable_proxy_target_message, TestPostgresError};
    use std::ffi::OsString;

    const URL: &str = "postgresql:///waddle_test";

    fn os(value: &str) -> Option<OsString> {
        Some(OsString::from(value))
    }

    #[test]
    fn unset_url_skips_when_postgres_is_not_required() {
        for requirement in [None, os(""), os("0")] {
            assert_eq!(resolve(None, requirement.clone()), Ok(None));
            assert_eq!(resolve(os(""), requirement), Ok(None));
        }
    }

    #[test]
    fn unset_url_fails_when_postgres_is_required() {
        assert_eq!(resolve(None, os("1")), Err(TestPostgresError::Missing));
        assert_eq!(resolve(os(""), os("1")), Err(TestPostgresError::Missing));
    }

    #[test]
    fn configured_url_is_returned_whether_or_not_required() {
        for requirement in [None, os("0"), os("1")] {
            assert_eq!(resolve(os(URL), requirement), Ok(Some(URL.to_owned())));
        }
    }

    #[test]
    fn unrecognised_requirement_fails_instead_of_guessing() {
        assert_eq!(
            resolve(os(URL), os("true")),
            Err(TestPostgresError::InvalidRequirement(OsString::from(
                "true"
            )))
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_url_fails_instead_of_skipping() {
        use std::os::unix::ffi::OsStringExt;

        let url = OsString::from_vec(vec![0x66, 0x80]);
        assert_eq!(
            resolve(Some(url), None),
            Err(TestPostgresError::NonUnicodeUrl)
        );
    }

    #[test]
    fn missing_required_url_message_names_both_variables() {
        let message = TestPostgresError::Missing.to_string();
        assert!(
            message.contains("WADDLE_TEST_REQUIRE_POSTGRES=1"),
            "{message}"
        );
        assert!(message.contains("WADDLE_TEST_POSTGRES_URL"), "{message}");
    }

    #[test]
    fn optional_proxy_witness_can_skip_a_nonlocal_target() {
        assert_eq!(
            unavailable_proxy_target_message(false, "target is not loopback"),
            None
        );
    }

    #[test]
    fn required_proxy_witness_fails_for_a_nonlocal_target() {
        assert_eq!(
            unavailable_proxy_target_message(true, "target is not loopback"),
            Some("WADDLE_TEST_REQUIRE_POSTGRES=1 but the PostgreSQL proxy witness requires a local target: target is not loopback".to_owned())
        );
    }
}
