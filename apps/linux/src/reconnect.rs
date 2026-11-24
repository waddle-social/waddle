//! When to connect, and when to try again.
//!
//! ADR-0019 names a shared `ConnectionController` in `waddle-client-session`;
//! it does not exist yet, so this is the Linux-local one, with the policy
//! the Apple client uses: jittered exponential backoff, a 15 second connect
//! budget, and an immediate retry when the window is focused again or the
//! network returns. When the shared controller lands this module is
//! replaced by it; nothing outside `ui/connection.rs` knows its shape.
//!
//! It is a state machine and nothing else. The window feeds it events and
//! carries out the [`Action`]s it returns (connect, arm a timer); timers
//! carry a token, and a token the controller no longer holds is ignored, so
//! a timer that fires late cannot start a second attempt.

use std::time::Duration;

/// How long a connect attempt may take to reach `Connected` (as Apple).
pub const CONNECT_BUDGET: Duration = Duration::from_secs(15);
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// Delay before retry number `attempt` (from 0): uniform between the base
/// and min(cap, base * 2^attempt). `unit_random` is a sample in `[0, 1]`
/// from the caller, so the curve is testable.
pub fn backoff(attempt: u32, unit_random: f64) -> Duration {
    let ceiling = (BACKOFF_BASE.as_secs_f64()
        * 2f64.powi(i32::try_from(attempt.min(30)).unwrap_or(30)))
    .min(BACKOFF_CAP.as_secs_f64());
    let base = BACKOFF_BASE.as_secs_f64();
    let unit = if unit_random.is_finite() {
        unit_random.clamp(0.0, 1.0)
    } else {
        0.0
    };
    Duration::from_secs_f64(base + (ceiling - base).max(0.0) * unit)
}

/// What the account's connection is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not started, or signed out.
    Stopped,
    /// An attempt is running. `attempt` counts failures since the last
    /// success, so the first attempt is 0.
    Connecting {
        attempt: u32,
    },
    Online,
    /// The last attempt failed; the next one starts when the timer fires.
    Waiting {
        attempt: u32,
        delay: Duration,
    },
    /// There is no network; nothing is tried until it returns.
    NoNetwork {
        attempt: u32,
    },
}

/// What the window does for the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start a connect attempt.
    Connect,
    /// Call [`Controller::timer`] with `token` after `after`.
    Arm { token: u64, after: Duration },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Timer {
    Budget(u64),
    Retry(u64),
}

#[derive(Debug)]
pub struct Controller {
    state: State,
    network: bool,
    timer: Option<Timer>,
    tokens: u64,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            state: State::Stopped,
            network: true,
            timer: None,
            tokens: 0,
        }
    }
}

impl Controller {
    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_online(&self) -> bool {
        self.state == State::Online
    }

    /// Begin: the first connect attempt.
    pub fn start(&mut self) -> Vec<Action> {
        self.attempt(0)
    }

    /// Sign-out: nothing more is tried.
    pub fn stop(&mut self) {
        self.state = State::Stopped;
        self.timer = None;
    }

    /// The session says it is connected. Accepted in any state: an attempt
    /// that outlived its budget can still succeed, and then it is the
    /// connection.
    pub fn connected(&mut self) {
        if self.state == State::Stopped {
            return;
        }
        self.state = State::Online;
        self.timer = None;
    }

    /// The session lost its connection, or an attempt failed outright.
    /// `random` is a sample in `[0, 1]` for the backoff jitter.
    pub fn disconnected(&mut self, random: f64) -> Vec<Action> {
        let attempt = match self.state {
            State::Stopped => return Vec::new(),
            State::Online => 0,
            State::Connecting { attempt } => attempt.saturating_add(1),
            // Already waiting; a second report of the same failure.
            State::Waiting { .. } | State::NoNetwork { .. } => return Vec::new(),
        };
        self.fail(attempt, random)
    }

    /// An armed timer fired.
    pub fn timer(&mut self, token: u64, random: f64) -> Vec<Action> {
        let fired = match self.timer {
            Some(Timer::Budget(held) | Timer::Retry(held)) if held == token => self.timer,
            _ => return Vec::new(),
        };
        self.timer = None;
        match (fired, self.state) {
            // The attempt neither connected nor reported anything.
            (Some(Timer::Budget(_)), State::Connecting { attempt }) => {
                self.fail(attempt.saturating_add(1), random)
            }
            (Some(Timer::Retry(_)), State::Waiting { attempt, .. }) => {
                if self.network {
                    self.attempt(attempt)
                } else {
                    self.go_offline(attempt)
                }
            }
            // The monitor can be wrong (a portal that reports no network,
            // a captive network), so offline is still probed, slowly.
            (Some(Timer::Retry(_)), State::NoNetwork { attempt }) => self.attempt(attempt),
            _ => Vec::new(),
        }
    }

    /// The network came or went (`gio::NetworkMonitor`).
    pub fn network_changed(&mut self, available: bool) -> Vec<Action> {
        if self.network == available {
            return Vec::new();
        }
        self.network = available;
        match (available, self.state) {
            // Back: try now, from the start of the curve.
            (true, State::NoNetwork { .. } | State::Waiting { .. }) => self.attempt(0),
            (false, State::Waiting { attempt, .. } | State::Connecting { attempt }) => {
                self.go_offline(attempt)
            }
            // Online stays online until the connection itself reports a
            // loss; the socket may well survive a network change.
            _ => Vec::new(),
        }
    }

    /// The window was focused, or the user asked to reconnect: do not wait
    /// out a backoff the user is looking at.
    pub fn resume(&mut self) -> Vec<Action> {
        match self.state {
            State::Waiting { .. } | State::NoNetwork { .. } => self.attempt(0),
            _ => Vec::new(),
        }
    }

    fn attempt(&mut self, attempt: u32) -> Vec<Action> {
        self.state = State::Connecting { attempt };
        let token = self.next_token();
        self.timer = Some(Timer::Budget(token));
        vec![
            Action::Connect,
            Action::Arm {
                token,
                after: CONNECT_BUDGET,
            },
        ]
    }

    fn fail(&mut self, attempt: u32, random: f64) -> Vec<Action> {
        if !self.network {
            return self.go_offline(attempt);
        }
        let delay = backoff(attempt.saturating_sub(1), random);
        self.state = State::Waiting { attempt, delay };
        let token = self.next_token();
        self.timer = Some(Timer::Retry(token));
        vec![Action::Arm {
            token,
            after: delay,
        }]
    }

    fn go_offline(&mut self, attempt: u32) -> Vec<Action> {
        self.state = State::NoNetwork { attempt };
        let token = self.next_token();
        self.timer = Some(Timer::Retry(token));
        vec![Action::Arm {
            token,
            after: BACKOFF_CAP,
        }]
    }

    fn next_token(&mut self) -> u64 {
        self.tokens = self.tokens.wrapping_add(1);
        self.tokens
    }

    /// What to tell the user, if anything. The first connect is quiet.
    pub fn banner(&self) -> Option<&'static str> {
        match self.state {
            State::Stopped | State::Online => None,
            State::Connecting { attempt: 0 } => None,
            State::Connecting { .. } | State::Waiting { .. } => {
                Some("Connection lost. Reconnecting…")
            }
            State::NoNetwork { .. } => Some("You are offline. Waiting for the network…"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(actions: &[Action]) -> u64 {
        actions
            .iter()
            .find_map(|action| match action {
                Action::Arm { token, .. } => Some(*token),
                Action::Connect => None,
            })
            .expect("an armed timer")
    }

    fn connects(actions: &[Action]) -> bool {
        actions.contains(&Action::Connect)
    }

    #[test]
    fn backoff_is_jittered_exponential_between_one_and_sixty_seconds() {
        assert_eq!(backoff(0, 0.9), Duration::from_secs(1));
        assert_eq!(backoff(3, 0.0), Duration::from_secs(1));
        assert_eq!(backoff(3, 1.0), Duration::from_secs(8));
        assert_eq!(backoff(40, 1.0), Duration::from_secs(60));
        assert_eq!(backoff(4, 0.5), Duration::from_secs_f64(1.0 + 15.0 * 0.5));
        // Never out of range, whatever the random source returns.
        assert_eq!(backoff(5, f64::NAN), Duration::from_secs(1));
        assert_eq!(backoff(5, 7.0), Duration::from_secs(32));
        assert_eq!(backoff(5, -1.0), Duration::from_secs(1));
    }

    #[test]
    fn the_first_attempt_has_a_fifteen_second_budget() {
        let mut controller = Controller::default();
        let actions = controller.start();
        assert!(connects(&actions));
        assert!(actions.contains(&Action::Arm {
            token: 1,
            after: Duration::from_secs(15)
        }));
        assert_eq!(controller.state(), State::Connecting { attempt: 0 });
        assert_eq!(controller.banner(), None);
        controller.connected();
        assert!(controller.is_online());
    }

    #[test]
    fn a_budget_that_runs_out_backs_off_then_retries_with_a_longer_wait() {
        let mut controller = Controller::default();
        let budget = token(&controller.start());
        let waiting = controller.timer(budget, 1.0);
        assert_eq!(
            controller.state(),
            State::Waiting {
                attempt: 1,
                delay: Duration::from_secs(1)
            }
        );
        assert_eq!(controller.banner(), Some("Connection lost. Reconnecting…"));
        let retry = token(&waiting);
        let again = controller.timer(retry, 1.0);
        assert!(connects(&again));
        assert_eq!(controller.state(), State::Connecting { attempt: 1 });

        // The second failure waits longer, up to the doubled ceiling.
        let budget = token(&again);
        controller.timer(budget, 1.0);
        assert_eq!(
            controller.state(),
            State::Waiting {
                attempt: 2,
                delay: Duration::from_secs(2)
            }
        );
    }

    #[test]
    fn a_success_resets_the_curve() {
        let mut controller = Controller::default();
        let budget = token(&controller.start());
        controller.timer(budget, 0.0);
        controller.connected();
        let actions = controller.disconnected(1.0);
        // A drop after success starts again from the shortest wait.
        assert_eq!(
            controller.state(),
            State::Waiting {
                attempt: 0,
                delay: Duration::from_secs(1)
            }
        );
        assert!(!connects(&actions));
    }

    #[test]
    fn a_late_or_stale_timer_does_nothing() {
        let mut controller = Controller::default();
        let budget = token(&controller.start());
        controller.connected();
        // The budget fires after the connection: ignored.
        assert!(controller.timer(budget, 0.0).is_empty());
        assert!(controller.is_online());
        // Replaced by a newer attempt: the old token is dead.
        let down = controller.disconnected(0.0);
        let old_retry = token(&down);
        controller.resume();
        assert!(controller.timer(old_retry, 0.0).is_empty());
        assert!(matches!(controller.state(), State::Connecting { .. }));
    }

    #[test]
    fn an_attempt_that_outlived_its_budget_can_still_connect() {
        let mut controller = Controller::default();
        let budget = token(&controller.start());
        controller.timer(budget, 0.0);
        controller.connected();
        assert!(controller.is_online());
    }

    #[test]
    fn losing_the_network_stops_the_retries_and_its_return_connects_at_once() {
        let mut controller = Controller::default();
        controller.start();
        controller.connected();
        controller.disconnected(1.0);
        let offline = controller.network_changed(false);
        assert!(!connects(&offline), "nothing is tried while offline");
        assert_eq!(controller.state(), State::NoNetwork { attempt: 0 });
        assert_eq!(
            controller.banner(),
            Some("You are offline. Waiting for the network…")
        );
        // A disconnect report while offline starts nothing.
        assert!(controller.disconnected(0.0).is_empty());
        let back = controller.network_changed(true);
        assert!(connects(&back));
        assert_eq!(controller.state(), State::Connecting { attempt: 0 });
    }

    #[test]
    fn a_retry_that_fires_without_a_network_waits_for_it() {
        let mut controller = Controller::default();
        let budget = token(&controller.start());
        let waiting = controller.timer(budget, 0.0);
        let retry = token(&waiting);
        // The monitor said offline, then the timer fired before anything
        // else could cancel it.
        controller.network = false;
        let probe = controller.timer(retry, 0.0);
        assert!(!connects(&probe));
        assert_eq!(controller.state(), State::NoNetwork { attempt: 1 });
    }

    #[test]
    fn offline_is_probed_slowly_in_case_the_monitor_is_wrong() {
        let mut controller = Controller::default();
        controller.network_changed(false);
        let budget = token(&controller.start());
        // The first attempt fails; with no network the next try is the
        // slow probe.
        let probe = controller.timer(budget, 0.0);
        assert_eq!(
            probe,
            [Action::Arm {
                token: token(&probe),
                after: Duration::from_secs(60)
            }]
        );
        let tried = controller.timer(token(&probe), 0.0);
        assert!(connects(&tried));
        assert!(matches!(controller.state(), State::Connecting { .. }));
        // It failed again: back to waiting for the network.
        let again = controller.timer(token(&tried), 0.0);
        assert!(!connects(&again));
        assert!(matches!(controller.state(), State::NoNetwork { .. }));
    }

    #[test]
    fn focus_tries_even_when_the_monitor_says_offline() {
        let mut controller = Controller::default();
        controller.network_changed(false);
        let budget = token(&controller.start());
        controller.timer(budget, 0.0);
        assert!(connects(&controller.resume()));
    }

    #[test]
    fn a_drop_with_no_network_does_not_back_off() {
        let mut controller = Controller::default();
        controller.start();
        controller.connected();
        controller.network_changed(false);
        // The socket can outlive the network, so still online...
        assert!(controller.is_online());
        // ...until it reports the loss.
        let lost = controller.disconnected(0.5);
        assert!(!connects(&lost));
        assert_eq!(controller.state(), State::NoNetwork { attempt: 0 });
    }

    #[test]
    fn focus_skips_the_wait_and_changes_nothing_otherwise() {
        let mut controller = Controller::default();
        assert!(controller.resume().is_empty());
        let budget = token(&controller.start());
        assert!(controller.resume().is_empty(), "already connecting");
        controller.timer(budget, 1.0);
        let actions = controller.resume();
        assert!(connects(&actions));
        controller.connected();
        assert!(controller.resume().is_empty(), "already online");
    }

    #[test]
    fn a_stopped_controller_ignores_everything() {
        let mut controller = Controller::default();
        controller.start();
        controller.stop();
        controller.connected();
        assert_eq!(controller.state(), State::Stopped);
        assert!(controller.disconnected(0.0).is_empty());
        assert!(controller.network_changed(false).is_empty());
        assert!(controller.network_changed(true).is_empty());
        assert!(controller.resume().is_empty());
        assert_eq!(controller.banner(), None);
    }

    #[test]
    fn repeated_failure_reports_while_waiting_do_not_stack_timers() {
        let mut controller = Controller::default();
        controller.start();
        controller.connected();
        assert!(!controller.disconnected(0.0).is_empty());
        assert!(controller.disconnected(0.0).is_empty());
    }
}
