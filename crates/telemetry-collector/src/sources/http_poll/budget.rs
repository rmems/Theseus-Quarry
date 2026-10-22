use super::{CLIENT_REQUEST_TIMEOUT, PROBE_INTERVAL_SLACK};
use std::time::Duration;

/// Remaining wall time for sequential probes in one poll tick.
#[derive(Clone, Copy, Debug)]
pub struct ProbeBudget {
    deadline: tokio::time::Instant,
}

impl ProbeBudget {
    pub fn from_poll_interval_secs(interval_secs: u64) -> Self {
        let total = Duration::from_secs(interval_secs).saturating_sub(PROBE_INTERVAL_SLACK);
        Self {
            deadline: tokio::time::Instant::now() + total,
        }
    }

    /// Timeout for the next HTTP call, or `None` when the tick budget is exhausted.
    pub fn request_timeout(&self) -> Option<Duration> {
        let remaining = self
            .deadline
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        Some(remaining.min(CLIENT_REQUEST_TIMEOUT))
    }
}
