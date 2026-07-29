//! Algorithm rotation — runtime profitability-based mining algorithm switching.
//!
//! Evaluates per-algorithm revenue estimates periodically and recommends switching
//! when a significantly more profitable algorithm is available.
//!
//! # Safeguards
//!
//! - **Cooldown**: minimum time between rotations (default: 5 min).
//! - **Threshold**: new algo must exceed current by `improvement_threshold_pct`.
//! - **Staleness**: refuses to rotate when market data is stale (> 5 min).
//! - **GPU availability**: filters out GPU algos when GPU is unavailable.

use std::time::{Duration, Instant};

use mining_telemetry_core::RotationEvent;

use crate::mining_supervisor::MiningAlgo;

// ─── Market snapshot ─────────────────────────────────────────────────────────

/// Live market + hardware state consumed by the rotation evaluator.
#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    pub dnx_price_usd: f64,
    pub qu_price_usd: f64,
    pub dynex_hashrate_mh: f64,
    pub quai_hashrate_mh: f64,
    pub qubic_hashrate_kh: f64,
    pub gpu_available: bool,
    /// Time since last successful price API fetch.
    pub price_age: Duration,
    /// Time since last hashrate telemetry update.
    pub hashrate_age: Duration,
}

impl Default for MarketSnapshot {
    fn default() -> Self {
        Self {
            dnx_price_usd: 0.0,
            qu_price_usd: 0.0,
            dynex_hashrate_mh: 0.0,
            quai_hashrate_mh: 0.0,
            qubic_hashrate_kh: 0.0,
            gpu_available: true,
            price_age: Duration::from_secs(0),
            hashrate_age: Duration::from_secs(0),
        }
    }
}

// ─── Revenue estimation ──────────────────────────────────────────────────────

/// Per-algorithm revenue estimate.
#[derive(Debug, Clone)]
pub struct AlgoRevenue {
    pub algo: MiningAlgo,
    pub revenue_usd_per_hour: f64,
    pub confidence: f64,
    pub available: bool,
}

// Conservative default hashrates for algos that haven't run yet.
const DEFAULT_DYNEX_MH: f64 = 0.8; // RTX 5080 conservative
const DEFAULT_QUAI_MH: f64 = 10.0; // kawpow conservative
const DEFAULT_QUBIC_KH: f64 = 50.0; // CPU conservative

// Revenue model constants (per-unit hashrate reward rates).
const DNX_REWARD_PER_MH_HR: f64 = 0.05; // DNX per MH/s per hour (approximate)
const QUAI_REWARD_PER_MH_HR: f64 = 0.001; // QUAI per MH/s per hour (approximate)
const QUBIC_SOL_RATE: f64 = 100.0; // kH/s per solution (approximate)
const QUBIC_REWARD_PER_SOL: f64 = 0.0001; // QU per solution (approximate)

/// Estimate revenue for a single algorithm.
fn estimate_revenue(algo: MiningAlgo, market: &MarketSnapshot) -> AlgoRevenue {
    match algo {
        MiningAlgo::Dynex => {
            let hashrate = if market.dynex_hashrate_mh > 0.0 {
                market.dynex_hashrate_mh
            } else {
                DEFAULT_DYNEX_MH
            };
            let confidence = compute_confidence(market.dynex_hashrate_mh, market);
            AlgoRevenue {
                algo,
                revenue_usd_per_hour: hashrate * DNX_REWARD_PER_MH_HR * market.dnx_price_usd,
                confidence,
                available: market.gpu_available,
            }
        }
        MiningAlgo::Quai => {
            let hashrate = if market.quai_hashrate_mh > 0.0 {
                market.quai_hashrate_mh
            } else {
                DEFAULT_QUAI_MH
            };
            let confidence = compute_confidence(market.quai_hashrate_mh, market);
            AlgoRevenue {
                algo,
                revenue_usd_per_hour: hashrate * QUAI_REWARD_PER_MH_HR * market.qu_price_usd,
                confidence,
                available: true, // CPU-only, always available
            }
        }
        MiningAlgo::Qubic => {
            let hashrate = if market.qubic_hashrate_kh > 0.0 {
                market.qubic_hashrate_kh
            } else {
                DEFAULT_QUBIC_KH
            };
            let confidence = compute_confidence(market.qubic_hashrate_kh, market);
            let solutions_per_hour = hashrate / QUBIC_SOL_RATE;
            AlgoRevenue {
                algo,
                revenue_usd_per_hour: solutions_per_hour
                    * QUBIC_REWARD_PER_SOL
                    * market.qu_price_usd,
                confidence,
                available: true, // CPU-only, always available
            }
        }
        MiningAlgo::Kaspa => AlgoRevenue {
            algo,
            revenue_usd_per_hour: 0.0,
            confidence: 0.0,
            available: true,
        },
        // Multi-algo variants aren't rotation targets — we rotate between singles.
        MiningAlgo::Both | MiningAlgo::All => AlgoRevenue {
            algo,
            revenue_usd_per_hour: 0.0,
            confidence: 0.0,
            available: false,
        },
    }
}

/// Compute confidence based on live data quality.
fn compute_confidence(hashrate: f64, market: &MarketSnapshot) -> f64 {
    if market.price_age > Duration::from_secs(600) {
        return 0.0; // >10 min stale → zero confidence
    }
    if market.hashrate_age > Duration::from_secs(120) {
        return 0.3; // >2 min hashrate lag → low confidence
    }
    if hashrate > 0.0 {
        0.9 // live hashrate
    } else {
        0.3 // no data, using default
    }
}

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration for the algorithm rotator.
#[derive(Debug, Clone)]
pub struct RotationConfig {
    /// Minimum time between rotations.
    pub cooldown: Duration,
    /// New algo must exceed current revenue by this percentage to trigger switch.
    pub improvement_threshold_pct: f64,
    /// How often to run evaluation.
    pub evaluation_interval: Duration,
}

impl Default for RotationConfig {
    fn default() -> Self {
        Self {
            cooldown: Duration::from_secs(300),           // 5 minutes
            improvement_threshold_pct: 20.0,              // 20%
            evaluation_interval: Duration::from_secs(60), // 1 minute
        }
    }
}

// ─── AlgoRotator ─────────────────────────────────────────────────────────────

/// Evaluates mining algorithms and recommends rotation based on profitability.
pub struct AlgoRotator {
    config: RotationConfig,
    current_algo: MiningAlgo,
    last_rotation: Instant,
    last_evaluation: Instant,
}

impl AlgoRotator {
    pub fn new(initial_algo: MiningAlgo, config: RotationConfig) -> Self {
        Self {
            config,
            current_algo: initial_algo,
            last_rotation: Instant::now(),
            last_evaluation: Instant::now()
                .checked_sub(Duration::from_secs(3600))
                .unwrap_or_else(Instant::now),
        }
    }

    /// For tests: create with a pre-set last_rotation time.
    #[cfg(test)]
    fn new_with_times(
        initial_algo: MiningAlgo,
        config: RotationConfig,
        last_rotation_ago: Duration,
        last_eval_ago: Duration,
    ) -> Self {
        Self {
            config,
            current_algo: initial_algo,
            last_rotation: Instant::now()
                .checked_sub(last_rotation_ago)
                .unwrap_or_else(Instant::now),
            last_evaluation: Instant::now()
                .checked_sub(last_eval_ago)
                .unwrap_or_else(Instant::now),
        }
    }

    /// Evaluate all algorithms and return a recommendation + telemetry event.
    ///
    /// Returns `Some(new_algo)` if a switch is recommended.  Always returns
    /// a `RotationEvent` for telemetry (even when no switch occurs).
    pub fn evaluate(&mut self, market: &MarketSnapshot) -> (Option<MiningAlgo>, RotationEvent) {
        let now = Instant::now();

        // Not yet time for evaluation?
        if now.duration_since(self.last_evaluation) < self.config.evaluation_interval {
            return (None, self.make_event("skipped:interval", None, &[], market));
        }
        self.last_evaluation = now;

        // Stale market data?
        if market.price_age > Duration::from_secs(300) {
            return (None, self.make_event("skipped:stale", None, &[], market));
        }

        // Cooldown not elapsed?
        if now.duration_since(self.last_rotation) < self.config.cooldown {
            return (None, self.make_event("skipped:cooldown", None, &[], market));
        }

        // Evaluate all single-algo candidates.
        let candidates = [MiningAlgo::Dynex, MiningAlgo::Quai, MiningAlgo::Qubic];
        let revenues: Vec<AlgoRevenue> = candidates
            .iter()
            .map(|&a| estimate_revenue(a, market))
            .collect();

        let rev_tuples: Vec<(String, f64, f64)> = revenues
            .iter()
            .map(|r| (r.algo.to_string(), r.revenue_usd_per_hour, r.confidence))
            .collect();

        let current_rev = revenues
            .iter()
            .find(|r| r.algo == self.current_algo)
            .map(|r| r.revenue_usd_per_hour)
            .unwrap_or(0.0);

        // Find best available algo.
        let best = revenues
            .iter()
            .filter(|r| r.available && r.confidence > 0.0)
            .max_by(|a, b| {
                a.revenue_usd_per_hour
                    .partial_cmp(&b.revenue_usd_per_hour)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        let best = match best {
            Some(b) => b,
            None => {
                return (
                    None,
                    self.make_event("evaluated", None, &rev_tuples, market),
                );
            }
        };

        // Check improvement threshold.
        let improvement_pct = if current_rev > 0.0 {
            ((best.revenue_usd_per_hour - current_rev) / current_rev) * 100.0
        } else {
            // Current revenue is zero — any positive revenue is infinite improvement.
            if best.revenue_usd_per_hour > 0.0 {
                f64::INFINITY
            } else {
                0.0
            }
        };

        if best.algo != self.current_algo
            && improvement_pct >= self.config.improvement_threshold_pct
        {
            let new_algo = best.algo;
            return (
                Some(new_algo),
                self.make_event("rotated", Some(new_algo.to_string()), &rev_tuples, market),
            );
        }

        (
            None,
            self.make_event("skipped:threshold", None, &rev_tuples, market),
        )
    }

    /// Record that a rotation was performed.
    pub fn record_rotation(&mut self, algo: MiningAlgo) {
        self.current_algo = algo;
        self.last_rotation = Instant::now();
    }

    pub fn current_algo(&self) -> MiningAlgo {
        self.current_algo
    }

    fn make_event(
        &self,
        kind: &str,
        to_algo: Option<String>,
        revenues: &[(String, f64, f64)],
        market: &MarketSnapshot,
    ) -> RotationEvent {
        RotationEvent {
            kind: kind.to_string(),
            from_algo: Some(self.current_algo.to_string()),
            to_algo,
            revenues: revenues.to_vec(),
            market_age_secs: market.price_age.as_secs_f64(),
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_market() -> MarketSnapshot {
        MarketSnapshot {
            dnx_price_usd: 0.50,
            qu_price_usd: 0.001,
            dynex_hashrate_mh: 1.0,
            quai_hashrate_mh: 15.0,
            qubic_hashrate_kh: 100.0,
            gpu_available: true,
            price_age: Duration::from_secs(10),
            hashrate_age: Duration::from_secs(5),
        }
    }

    #[test]
    fn revenue_estimate_dynex() {
        let market = fresh_market();
        let rev = estimate_revenue(MiningAlgo::Dynex, &market);
        assert!(rev.revenue_usd_per_hour > 0.0);
        assert!(rev.available);
        assert_eq!(rev.confidence, 0.9);
    }

    #[test]
    fn revenue_estimate_quai() {
        let market = fresh_market();
        let rev = estimate_revenue(MiningAlgo::Quai, &market);
        assert!(rev.revenue_usd_per_hour > 0.0);
        assert!(rev.available);
    }

    #[test]
    fn revenue_estimate_qubic() {
        let market = fresh_market();
        let rev = estimate_revenue(MiningAlgo::Qubic, &market);
        assert!(rev.revenue_usd_per_hour > 0.0);
        assert!(rev.available);
    }

    #[test]
    fn gpu_unavailable_filters_dynex() {
        let mut market = fresh_market();
        market.gpu_available = false;
        let rev = estimate_revenue(MiningAlgo::Dynex, &market);
        assert!(!rev.available);
    }

    #[test]
    fn stale_market_blocks_rotation() {
        let mut market = fresh_market();
        market.price_age = Duration::from_secs(400); // >5 min

        let config = RotationConfig {
            evaluation_interval: Duration::from_millis(0),
            cooldown: Duration::from_millis(0),
            ..RotationConfig::default()
        };
        let mut rotator = AlgoRotator::new_with_times(
            MiningAlgo::Dynex,
            config,
            Duration::from_secs(600),
            Duration::from_secs(600),
        );
        let (result, event) = rotator.evaluate(&market);
        assert!(result.is_none());
        assert_eq!(event.kind, "skipped:stale");
    }

    #[test]
    fn cooldown_blocks_rotation() {
        let market = fresh_market();
        let config = RotationConfig {
            cooldown: Duration::from_secs(600), // 10 min cooldown
            evaluation_interval: Duration::from_millis(0),
            ..RotationConfig::default()
        };
        // Last rotation was just now.
        let mut rotator = AlgoRotator::new_with_times(
            MiningAlgo::Dynex,
            config,
            Duration::from_secs(0), // rotated just now
            Duration::from_secs(600),
        );
        let (result, event) = rotator.evaluate(&market);
        assert!(result.is_none());
        assert_eq!(event.kind, "skipped:cooldown");
    }

    #[test]
    fn threshold_blocks_small_improvement() {
        let mut market = fresh_market();
        // Make all algos have similar revenue.
        market.dnx_price_usd = 1.0;
        market.qu_price_usd = 1.0;
        market.dynex_hashrate_mh = 1.0;
        market.quai_hashrate_mh = 0.0;
        market.qubic_hashrate_kh = 0.0;

        let config = RotationConfig {
            improvement_threshold_pct: 100.0, // require 100% improvement
            cooldown: Duration::from_millis(0),
            evaluation_interval: Duration::from_millis(0),
        };
        let mut rotator = AlgoRotator::new_with_times(
            MiningAlgo::Dynex,
            config,
            Duration::from_secs(600),
            Duration::from_secs(600),
        );
        let (result, _event) = rotator.evaluate(&market);
        assert!(result.is_none());
    }

    #[test]
    fn rotation_triggers_on_large_improvement() {
        let mut market = fresh_market();
        // Make Quai massively more profitable.
        market.dnx_price_usd = 0.001; // nearly worthless
        market.qu_price_usd = 100.0; // very valuable
        market.quai_hashrate_mh = 15.0;
        market.dynex_hashrate_mh = 1.0;

        let config = RotationConfig {
            improvement_threshold_pct: 20.0,
            cooldown: Duration::from_millis(0),
            evaluation_interval: Duration::from_millis(0),
        };
        let mut rotator = AlgoRotator::new_with_times(
            MiningAlgo::Dynex,
            config,
            Duration::from_secs(600),
            Duration::from_secs(600),
        );
        let (result, event) = rotator.evaluate(&market);
        assert!(result.is_some());
        assert_eq!(event.kind, "rotated");
        assert_eq!(result.unwrap(), MiningAlgo::Quai);
    }

    #[test]
    fn stale_hashrate_lowers_confidence() {
        let mut market = fresh_market();
        market.hashrate_age = Duration::from_secs(150); // >2 min
        let rev = estimate_revenue(MiningAlgo::Dynex, &market);
        assert_eq!(rev.confidence, 0.3);
    }

    #[test]
    fn very_stale_prices_zero_confidence() {
        let mut market = fresh_market();
        market.price_age = Duration::from_secs(700); // >10 min
        let rev = estimate_revenue(MiningAlgo::Dynex, &market);
        assert_eq!(rev.confidence, 0.0);
    }

    #[test]
    fn record_rotation_updates_state() {
        let config = RotationConfig::default();
        let mut rotator = AlgoRotator::new(MiningAlgo::Dynex, config);
        assert_eq!(rotator.current_algo(), MiningAlgo::Dynex);
        rotator.record_rotation(MiningAlgo::Quai);
        assert_eq!(rotator.current_algo(), MiningAlgo::Quai);
    }

    #[test]
    fn initial_evaluation_returns_none_too_soon() {
        let market = fresh_market();
        let config = RotationConfig {
            evaluation_interval: Duration::from_secs(60),
            ..RotationConfig::default()
        };
        // last_evaluation is set to just now by default in `new()`.
        let mut rotator = AlgoRotator::new(MiningAlgo::Dynex, config);
        // Override last_evaluation to be very recent (0 seconds ago).
        rotator.last_evaluation = Instant::now();
        let (result, event) = rotator.evaluate(&market);
        assert!(result.is_none());
        assert_eq!(event.kind, "skipped:interval");
    }

    #[test]
    fn default_hashrates_used_when_zero() {
        let mut market = fresh_market();
        market.dynex_hashrate_mh = 0.0;
        market.dnx_price_usd = 1.0;
        let rev = estimate_revenue(MiningAlgo::Dynex, &market);
        // Should use DEFAULT_DYNEX_MH (0.8), not 0.
        assert!(rev.revenue_usd_per_hour > 0.0);
    }
}
