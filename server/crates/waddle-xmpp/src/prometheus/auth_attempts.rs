use std::sync::atomic::{AtomicU64, Ordering};

const AUTH_TERMINAL_ATTEMPTS_METRIC: &str = "waddle_auth_terminal_attempts_total";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthMechanism {
    OAuthBearer,
    ScramSha256,
}

impl AuthMechanism {
    const ALL: [Self; 2] = [Self::OAuthBearer, Self::ScramSha256];

    const fn index(self) -> usize {
        match self {
            Self::OAuthBearer => 0,
            Self::ScramSha256 => 1,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::OAuthBearer => "oauthbearer",
            Self::ScramSha256 => "scram_sha_256",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthTerminalOutcome {
    Success,
    InvalidCredentials,
    Malformed,
    InternalError,
    Cancelled,
}

impl AuthTerminalOutcome {
    const ALL: [Self; 5] = [
        Self::Success,
        Self::InvalidCredentials,
        Self::Malformed,
        Self::InternalError,
        Self::Cancelled,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Success => 0,
            Self::InvalidCredentials => 1,
            Self::Malformed => 2,
            Self::InternalError => 3,
            Self::Cancelled => 4,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::InvalidCredentials => "invalid_credentials",
            Self::Malformed => "malformed",
            Self::InternalError => "internal_error",
            Self::Cancelled => "cancelled",
        }
    }
}

struct AuthAttempts {
    counters: [[AtomicU64; 5]; 2],
}

impl AuthAttempts {
    const fn new() -> Self {
        Self {
            counters: [
                [
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                ],
                [
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                ],
            ],
        }
    }

    fn increment(&self, mechanism: AuthMechanism, outcome: AuthTerminalOutcome) {
        self.counters[mechanism.index()][outcome.index()].fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> [[u64; 5]; 2] {
        std::array::from_fn(|mechanism| {
            std::array::from_fn(|outcome| self.counters[mechanism][outcome].load(Ordering::Relaxed))
        })
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn reset(&self) {
        for outcomes in &self.counters {
            for counter in outcomes {
                counter.store(0, Ordering::Release);
            }
        }
    }
}

static ATTEMPTS: AuthAttempts = AuthAttempts::new();

pub fn increment_auth_terminal_attempt(mechanism: AuthMechanism, outcome: AuthTerminalOutcome) {
    ATTEMPTS.increment(mechanism, outcome);
}

pub(super) fn render(out: &mut String) {
    render_counts(out, &ATTEMPTS.snapshot());
}

fn render_counts(out: &mut String, counts: &[[u64; 5]; 2]) {
    out.push_str("# HELP ");
    out.push_str(AUTH_TERMINAL_ATTEMPTS_METRIC);
    out.push_str(" Completed SASL1 OAUTHBEARER and SCRAM-SHA-256 exchanges by closed mechanism and terminal outcome. Explicit aborts and unfinished exchanges superseded by a new auth request are cancelled; the replacement exchange records its own terminal outcome.\n# TYPE ");
    out.push_str(AUTH_TERMINAL_ATTEMPTS_METRIC);
    out.push_str(" counter\n");
    for mechanism in AuthMechanism::ALL {
        for outcome in AuthTerminalOutcome::ALL {
            out.push_str(AUTH_TERMINAL_ATTEMPTS_METRIC);
            out.push_str("{mechanism=\"");
            out.push_str(mechanism.label());
            out.push_str("\",outcome=\"");
            out.push_str(outcome.label());
            out.push_str("\"} ");
            out.push_str(&counts[mechanism.index()][outcome.index()].to_string());
            out.push('\n');
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub(super) fn reset() {
    ATTEMPTS.reset();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_family_renders_only_closed_privacy_safe_labels() {
        let attempts = AuthAttempts::new();
        attempts.increment(AuthMechanism::OAuthBearer, AuthTerminalOutcome::Success);
        attempts.increment(
            AuthMechanism::ScramSha256,
            AuthTerminalOutcome::InvalidCredentials,
        );
        attempts.increment(AuthMechanism::ScramSha256, AuthTerminalOutcome::Cancelled);

        let mut rendered = String::new();
        render_counts(&mut rendered, &attempts.snapshot());

        assert!(rendered.contains("# TYPE waddle_auth_terminal_attempts_total counter"));
        assert!(rendered.contains(
            "waddle_auth_terminal_attempts_total{mechanism=\"oauthbearer\",outcome=\"success\"} 1"
        ));
        assert!(rendered.contains(
            "waddle_auth_terminal_attempts_total{mechanism=\"scram_sha_256\",outcome=\"invalid_credentials\"} 1"
        ));
        assert!(rendered.contains(
            "waddle_auth_terminal_attempts_total{mechanism=\"scram_sha_256\",outcome=\"cancelled\"} 1"
        ));
        assert!(rendered.contains(
            "waddle_auth_terminal_attempts_total{mechanism=\"oauthbearer\",outcome=\"cancelled\"} 0"
        ));
        for forbidden in ["jid=", "provider=", "user=", "session=", "token="] {
            assert!(!rendered.contains(forbidden), "forbidden label {forbidden}");
        }
    }
}
