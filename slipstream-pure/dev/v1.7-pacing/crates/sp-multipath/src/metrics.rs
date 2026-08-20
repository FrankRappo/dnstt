use std::time::Duration;

/// Per-path metrics tracking latency, success/failure counts.
#[derive(Debug)]
pub struct PathMetrics {
    pub success_count: u64,
    pub failure_count: u64,
    /// Exponentially weighted moving average of latency
    ewma_latency: Duration,
    /// Whether we have at least one sample
    has_samples: bool,
}

const EWMA_ALPHA: f64 = 0.3; // Weight for new samples

impl PathMetrics {
    pub fn new() -> Self {
        Self {
            success_count: 0,
            failure_count: 0,
            ewma_latency: Duration::from_millis(100), // initial estimate
            has_samples: false,
        }
    }

    pub fn record_success(&mut self, latency: Duration) {
        self.success_count += 1;
        if self.has_samples {
            let old = self.ewma_latency.as_secs_f64();
            let new = latency.as_secs_f64();
            let avg = old * (1.0 - EWMA_ALPHA) + new * EWMA_ALPHA;
            self.ewma_latency = Duration::from_secs_f64(avg);
        } else {
            self.ewma_latency = latency;
            self.has_samples = true;
        }
    }

    pub fn record_failure(&mut self) {
        self.failure_count += 1;
    }

    pub fn avg_latency(&self) -> Duration {
        self.ewma_latency
    }

    pub fn failure_rate(&self) -> f64 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            0.0
        } else {
            self.failure_count as f64 / total as f64
        }
    }

    pub fn total_requests(&self) -> u64 {
        self.success_count + self.failure_count
    }

    pub fn reset(&mut self) {
        self.success_count = 0;
        self.failure_count = 0;
        self.ewma_latency = Duration::from_millis(100);
        self.has_samples = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_initial() {
        let m = PathMetrics::new();
        assert_eq!(m.failure_rate(), 0.0);
        assert_eq!(m.total_requests(), 0);
    }

    #[test]
    fn test_metrics_success() {
        let mut m = PathMetrics::new();
        m.record_success(Duration::from_millis(50));
        m.record_success(Duration::from_millis(100));
        assert_eq!(m.success_count, 2);
        assert_eq!(m.failure_rate(), 0.0);
        // EWMA should be between 50 and 100
        let avg = m.avg_latency().as_millis();
        assert!(avg > 50 && avg < 100, "avg_latency={}", avg);
    }

    #[test]
    fn test_metrics_failure_rate() {
        let mut m = PathMetrics::new();
        m.record_success(Duration::from_millis(50));
        m.record_failure();
        assert!((m.failure_rate() - 0.5).abs() < 0.01);
    }
}
