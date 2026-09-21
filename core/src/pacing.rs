//! How a sync driver paces itself: exponential backoff after a fatal error
//! and a remote poll interval that is quick after activity and slow when
//! idle. Pure `Duration` arithmetic, shared by `daemon.rs` and the browser
//! driver (through `core-wasm`), so both back off and poll the same way.

use std::time::Duration;

/// The wait after `failures` consecutive failures: `base * 2^failures`, capped.
pub fn backoff_wait(base: Duration, max: Duration, failures: u32) -> Duration {
    base.checked_mul(2_u32.saturating_pow(failures))
        .unwrap_or(max)
        .min(max)
}

/// Exponential backoff for fatal errors: `base * 2^n`, capped.
#[derive(Debug)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    failures: u32,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Backoff {
            base,
            max,
            failures: 0,
        }
    }

    /// The wait after one more failure.
    pub fn next_wait(&mut self) -> Duration {
        let wait = backoff_wait(self.base, self.max, self.failures);
        self.failures = self.failures.saturating_add(1);
        wait
    }

    pub fn reset(&mut self) {
        self.failures = 0;
    }
}

/// The remote poll cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollPacing {
    /// Poll interval while there was activity within `active_window`.
    pub active: Duration,
    /// Poll interval when idle.
    pub idle: Duration,
    pub active_window: Duration,
}

impl PollPacing {
    /// The interval given how long ago the last activity was.
    pub fn interval(&self, since_activity: Duration) -> Duration {
        if since_activity < self.active_window {
            self.active
        } else {
            self.idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Backoff, PollPacing};
    use std::time::Duration;

    #[test]
    fn backoff_doubles_and_caps() {
        let mut backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(300));
        let waits: Vec<u64> = (0..8).map(|_| backoff.next_wait().as_secs()).collect();
        assert_eq!(waits, vec![5, 10, 20, 40, 80, 160, 300, 300]);
        backoff.reset();
        assert_eq!(backoff.next_wait().as_secs(), 5);
    }

    #[test]
    fn the_poll_interval_slows_down_when_idle() {
        let pacing = PollPacing {
            active: Duration::from_secs(5),
            idle: Duration::from_secs(60),
            active_window: Duration::from_secs(60),
        };
        assert_eq!(pacing.interval(Duration::from_secs(30)), pacing.active);
        assert_eq!(pacing.interval(Duration::from_secs(61)), pacing.idle);
    }
}
