use std::time::{Duration, Instant};

/// Adaptive rate pacer for a single resolver path.
///
/// When failures occur (rate limiting, timeouts), increases backoff.
/// On success, gradually reduces backoff.
pub struct AdaptivePacer {
    /// Minimum interval between sends
    min_interval: Duration,
    /// Current backoff duration
    backoff: Duration,
    /// When the next send is allowed
    next_send_at: Instant,
    /// Consecutive failure count
    consecutive_failures: u32,
    /// Max backoff cap
    max_backoff: Duration,
}

impl AdaptivePacer {
    pub fn new() -> Self {
        Self {
            min_interval: Duration::from_millis(10),
            backoff: Duration::ZERO,
            next_send_at: Instant::now(),
            consecutive_failures: 0,
            max_backoff: Duration::from_secs(30),
        }
    }

    /// Check if we're allowed to send now.
    pub fn can_send(&self) -> bool {
        Instant::now() >= self.next_send_at
    }

    /// Call after a successful exchange.
    pub fn on_success(&mut self) {
        self.consecutive_failures = 0;
        // Halve the backoff
        self.backoff = self.backoff / 2;
        if self.backoff < self.min_interval {
            self.backoff = Duration::ZERO;
        }
        self.next_send_at = Instant::now() + self.backoff;
    }

    /// Call after a failed exchange (timeout, rate-limit, error).
    pub fn on_failure(&mut self) {
        self.consecutive_failures += 1;
        // Exponential backoff: min_interval * 2^failures
        let factor = 2u64.saturating_pow(self.consecutive_failures.min(10));
        self.backoff = self.min_interval * factor as u32;
        if self.backoff > self.max_backoff {
            self.backoff = self.max_backoff;
        }
        self.next_send_at = Instant::now() + self.backoff;
    }

    /// Reset to initial state.
    pub fn reset(&mut self) {
        self.backoff = Duration::ZERO;
        self.next_send_at = Instant::now();
        self.consecutive_failures = 0;
    }

    /// Current backoff duration.
    pub fn current_backoff(&self) -> Duration {
        self.backoff
    }
}

impl std::fmt::Debug for AdaptivePacer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdaptivePacer")
            .field("backoff", &self.backoff)
            .field("consecutive_failures", &self.consecutive_failures)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pacer_initial() {
        let p = AdaptivePacer::new();
        assert!(p.can_send());
        assert_eq!(p.current_backoff(), Duration::ZERO);
    }

    #[test]
    fn test_pacer_backoff_on_failure() {
        let mut p = AdaptivePacer::new();
        p.on_failure();
        assert!(p.current_backoff() > Duration::ZERO);
        let first = p.current_backoff();
        p.on_failure();
        assert!(p.current_backoff() > first);
    }

    #[test]
    fn test_pacer_recovery_on_success() {
        let mut p = AdaptivePacer::new();
        p.on_failure();
        p.on_failure();
        p.on_failure();
        let peak = p.current_backoff();
        p.on_success();
        assert!(p.current_backoff() < peak);
    }

    #[test]
    fn test_pacer_max_cap() {
        let mut p = AdaptivePacer::new();
        for _ in 0..20 {
            p.on_failure();
        }
        assert!(p.current_backoff() <= Duration::from_secs(30));
    }
}
