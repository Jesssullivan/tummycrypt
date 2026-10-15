//! Exponential backoff for work that needs the storage backend.
//!
//! When the backend is unreachable, retrying on every timer tick only burns
//! CPU, floods the log and keeps a dead endpoint busy. [`BackendBackoff`]
//! spaces retries out (base, 2x base, 4x base, … up to a cap) and resets on
//! the first success. It is plain data over `Instant`, so the policy is unit
//! tested without a runtime.
//!
//! Only backend failures back off ([`CycleFailure::Backend`]). A cycle that
//! fails while the backend answers (a local refusal, a name collision) is
//! retried at the normal interval, and a passing health probe while backing
//! off ends the streak at once, so recovery never waits out a long delay.

use std::time::{Duration, Instant};

/// Why a periodic cycle failed, for the purpose of backing off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleFailure {
    /// The backend did not answer a health probe: back off.
    Backend,
    /// The backend answered, so the failure is local. Backing off cannot help
    /// and would only delay the retry once the operator fixes the cause.
    Local,
}

/// Retry spacing for backend-dependent periodic work.
#[derive(Debug, Clone)]
pub struct BackendBackoff {
    base: Duration,
    cap: Duration,
    consecutive_failures: u32,
    next_attempt: Option<Instant>,
}

impl BackendBackoff {
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base: base.max(Duration::from_secs(1)),
            cap: cap.max(base),
            consecutive_failures: 0,
            next_attempt: None,
        }
    }

    /// Whether an attempt may run at `now`.
    pub fn ready(&self, now: Instant) -> bool {
        self.next_attempt.is_none_or(|next| now >= next)
    }

    /// Record a failed attempt at `now`; returns the delay before the next.
    pub fn record_failure(&mut self, now: Instant) -> Duration {
        let doublings = self.consecutive_failures.min(31);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let delay = self
            .base
            .checked_mul(1u32 << doublings)
            .map_or(self.cap, |delay| delay.min(self.cap));
        self.next_attempt = Some(now + delay);
        delay
    }

    /// Record a success; returns `true` when this ends a failure streak.
    pub fn record_success(&mut self) -> bool {
        let recovered = self.consecutive_failures > 0;
        self.consecutive_failures = 0;
        self.next_attempt = None;
        recovered
    }

    /// Record a failed cycle. A backend failure backs off and returns the
    /// delay; a local failure ends any streak and returns `None`.
    pub fn record_cycle_failure(&mut self, now: Instant, kind: CycleFailure) -> Option<Duration> {
        match kind {
            CycleFailure::Backend => Some(self.record_failure(now)),
            CycleFailure::Local => {
                self.record_success();
                None
            }
        }
    }

    /// Time left before the next attempt at `now`, or `None` when ready.
    pub fn retry_in(&self, now: Instant) -> Option<Duration> {
        self.next_attempt
            .and_then(|next| next.checked_duration_since(now))
            .filter(|left| !left.is_zero())
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_double_the_delay_up_to_the_cap() {
        let mut backoff = BackendBackoff::new(Duration::from_secs(30), Duration::from_secs(600));
        let t0 = Instant::now();
        assert!(backoff.ready(t0));
        let delays: Vec<u64> = (0..7)
            .map(|_| backoff.record_failure(t0).as_secs())
            .collect();
        assert_eq!(delays, vec![30, 60, 120, 240, 480, 600, 600]);
        assert_eq!(backoff.consecutive_failures(), 7);
    }

    #[test]
    fn attempts_are_suppressed_until_the_delay_elapses() {
        let mut backoff = BackendBackoff::new(Duration::from_secs(30), Duration::from_secs(600));
        let t0 = Instant::now();
        backoff.record_failure(t0);
        assert!(!backoff.ready(t0 + Duration::from_secs(29)));
        assert!(backoff.ready(t0 + Duration::from_secs(30)));
    }

    #[test]
    fn success_resets_and_reports_recovery_once() {
        let mut backoff = BackendBackoff::new(Duration::from_secs(30), Duration::from_secs(600));
        let t0 = Instant::now();
        assert!(!backoff.record_success(), "no streak to end yet");
        backoff.record_failure(t0);
        backoff.record_failure(t0);
        assert!(backoff.record_success());
        assert!(backoff.ready(t0));
        assert_eq!(backoff.consecutive_failures(), 0);
        assert_eq!(backoff.record_failure(t0), Duration::from_secs(30));
    }

    #[test]
    fn local_failures_never_back_off_and_end_a_backend_streak() {
        let mut backoff = BackendBackoff::new(Duration::from_secs(60), Duration::from_secs(3600));
        let t0 = Instant::now();
        for _ in 0..5 {
            assert_eq!(backoff.record_cycle_failure(t0, CycleFailure::Local), None);
            assert!(
                backoff.ready(t0),
                "a local failure must not delay the next cycle"
            );
        }
        assert_eq!(
            backoff.record_cycle_failure(t0, CycleFailure::Backend),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            backoff.record_cycle_failure(t0, CycleFailure::Backend),
            Some(Duration::from_secs(120))
        );
        assert!(!backoff.ready(t0 + Duration::from_secs(60)));
        // The backend answers but the cycle fails locally: retry next tick.
        assert_eq!(backoff.record_cycle_failure(t0, CycleFailure::Local), None);
        assert!(backoff.ready(t0));
        assert_eq!(backoff.consecutive_failures(), 0);
    }

    #[test]
    fn retry_in_reports_the_remaining_delay() {
        let mut backoff = BackendBackoff::new(Duration::from_secs(60), Duration::from_secs(3600));
        let t0 = Instant::now();
        assert_eq!(backoff.retry_in(t0), None);
        backoff.record_failure(t0);
        assert_eq!(
            backoff.retry_in(t0 + Duration::from_secs(20)),
            Some(Duration::from_secs(40))
        );
        assert_eq!(backoff.retry_in(t0 + Duration::from_secs(60)), None);
    }

    #[test]
    fn a_day_of_outage_at_a_one_minute_tick_attempts_rarely() {
        // The reconcile loop ticks every minute; with the backend down for a
        // day it must not attempt every tick.
        let mut backoff = BackendBackoff::new(Duration::from_secs(60), Duration::from_secs(3600));
        let t0 = Instant::now();
        let mut attempts = 0;
        for minute in 0..(24 * 60) {
            let now = t0 + Duration::from_secs(minute * 60);
            if backoff.ready(now) {
                attempts += 1;
                backoff.record_failure(now);
            }
        }
        assert!(attempts <= 30, "{attempts} attempts in a day");
    }
}
