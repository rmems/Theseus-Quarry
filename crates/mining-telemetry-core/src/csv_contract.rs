//! One-way copy of the frozen hardware-telemetry CSV contract (RM-629).
//!
//! Authoritative source: `rmems/vahtisiru` `docs/telemetry_csv.md` and
//! `src/telemetry_csv.rs` (audited at commit `4feb196b`, vahtisiru crate
//! 0.2.0 / `vahtisiru-telemetry-csv` 0.1.0). There is **no** Cargo dependency
//! on vahtisiru in either direction; this module pins the same header and
//! reader semantics so Quarry can prove conformance offline.
//!
//! Theseus-Quarry remains a **JSONL-only producer** (issue #24 acceptance
//! criteria). This module does not write files and is not wired into the
//! collector. It exists so that:
//!
//! 1. the frozen header is pinned by a golden test in this repo, and
//! 2. any future, separately approved CSV projection has a fail-loud row
//!    builder ([`ContractRow::from_samples`]) that refuses missing,
//!    non-finite, unit-ambiguous, or stale inputs instead of zero-filling.
//!
//! The reader half ([`parse_csv`]) deliberately mirrors vahtisiru's
//! *permissive* consumer semantics (malformed rows are skipped and counted).
//! The producer half is *strict*: a producer must never emit a row the
//! consumer would skip.

use std::fmt;

/// Frozen CSV header. Byte-for-byte match after trim; extra columns fail.
pub const HEADER: &str = "timestamp_ms,gpu_temp_c,gpu_power_w,cpu_tctl_c,cpu_package_power_w";

/// Number of comma-separated fields in a valid data row.
pub const FIELD_COUNT: usize = 5;

/// Contract version this copy was audited against (vahtisiru git commit).
pub const AUDITED_CONTRACT_COMMIT: &str = "4feb196b53509461ceb9b1e7f9a6dd35e67db280";

/// Plausible temperature window (°C). Values outside it are almost always a
/// unit error (millidegrees from sysfs, Kelvin) rather than physiology.
/// Note: Fahrenheit inside this window cannot be distinguished from Celsius;
/// that ambiguity is resolved only at the source (see the audit doc).
pub const TEMP_C_RANGE: (f64, f64) = (-40.0, 150.0);

/// Plausible power window (W). Rejects milliwatts / microwatts passed as W.
pub const POWER_W_RANGE: (f64, f64) = (0.0, 2000.0);

/// Smallest accepted `timestamp_ms` (2001-09-09T01:46:40Z). Anything smaller
/// is almost certainly Unix *seconds* mislabeled as milliseconds.
pub const MIN_TIMESTAMP_MS: u64 = 1_000_000_000_000;

/// One contract row (field order == [`HEADER`] order).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContractRow {
    pub timestamp_ms: u64,
    pub gpu_temp_c: f32,
    pub gpu_power_w: f32,
    pub cpu_tctl_c: f32,
    pub cpu_package_power_w: f32,
}

/// A single timestamped reading from one Quarry source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// Unix-epoch milliseconds when the source was sampled.
    pub timestamp_ms: u64,
    pub value: f64,
}

/// Inputs for one joined row. Each column comes from a different Quarry
/// payload (`gpu_sched`, `host_hw` hwmon, `host_hw` RAPL), so each carries
/// its own sample time and is checked for staleness against the row time.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RowSamples {
    pub gpu_temp_c: Option<Reading>,
    pub gpu_power_w: Option<Reading>,
    pub cpu_tctl_c: Option<Reading>,
    pub cpu_package_power_w: Option<Reading>,
}

/// Why a producer refused to emit a row. Every variant names the column.
#[derive(Debug, Clone, PartialEq)]
pub enum RowError {
    /// Required sensor absent. The contract has no `None`; never zero-fill.
    Missing { column: &'static str },
    /// `NaN` / `inf` / `-inf`, or a value that overflows `f32`.
    NonFinite { column: &'static str },
    /// Outside the plausible physical window: likely a unit error.
    OutOfRange {
        column: &'static str,
        value: f64,
        min: f64,
        max: f64,
    },
    /// `timestamp_ms` looks like seconds (or is zero).
    TimestampUnit { timestamp_ms: u64 },
    /// A source reading is older/newer than the row time by more than the
    /// allowed skew: the join would mix different moments of physiology.
    Stale {
        column: &'static str,
        skew_ms: u64,
        max_skew_ms: u64,
    },
}

impl fmt::Display for RowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { column } => write!(f, "telemetry CSV column '{column}' is missing"),
            Self::NonFinite { column } => {
                write!(f, "telemetry CSV column '{column}' is not a finite f32")
            }
            Self::OutOfRange {
                column,
                value,
                min,
                max,
            } => write!(
                f,
                "telemetry CSV column '{column}' = {value} outside plausible [{min}, {max}] (unit error?)"
            ),
            Self::TimestampUnit { timestamp_ms } => write!(
                f,
                "telemetry CSV timestamp_ms = {timestamp_ms} below {MIN_TIMESTAMP_MS} (seconds mislabeled as ms?)"
            ),
            Self::Stale {
                column,
                skew_ms,
                max_skew_ms,
            } => write!(
                f,
                "telemetry CSV column '{column}' sample is {skew_ms} ms from row time (max {max_skew_ms} ms)"
            ),
        }
    }
}

impl std::error::Error for RowError {}

fn checked(
    column: &'static str,
    reading: Option<Reading>,
    row_ts: u64,
    max_skew_ms: u64,
    range: (f64, f64),
) -> Result<f32, RowError> {
    let r = reading.ok_or(RowError::Missing { column })?;
    if !r.value.is_finite() {
        return Err(RowError::NonFinite { column });
    }
    let skew_ms = r.timestamp_ms.abs_diff(row_ts);
    if skew_ms > max_skew_ms {
        return Err(RowError::Stale {
            column,
            skew_ms,
            max_skew_ms,
        });
    }
    let (min, max) = range;
    if r.value < min || r.value > max {
        return Err(RowError::OutOfRange {
            column,
            value: r.value,
            min,
            max,
        });
    }
    let v = r.value as f32;
    if !v.is_finite() {
        return Err(RowError::NonFinite { column });
    }
    Ok(v)
}

impl ContractRow {
    /// Strict producer-side constructor. Fails loud; never zero-fills.
    pub fn from_samples(
        timestamp_ms: u64,
        samples: RowSamples,
        max_skew_ms: u64,
    ) -> Result<Self, RowError> {
        if timestamp_ms < MIN_TIMESTAMP_MS {
            return Err(RowError::TimestampUnit { timestamp_ms });
        }
        Ok(Self {
            timestamp_ms,
            gpu_temp_c: checked(
                "gpu_temp_c",
                samples.gpu_temp_c,
                timestamp_ms,
                max_skew_ms,
                TEMP_C_RANGE,
            )?,
            gpu_power_w: checked(
                "gpu_power_w",
                samples.gpu_power_w,
                timestamp_ms,
                max_skew_ms,
                POWER_W_RANGE,
            )?,
            cpu_tctl_c: checked(
                "cpu_tctl_c",
                samples.cpu_tctl_c,
                timestamp_ms,
                max_skew_ms,
                TEMP_C_RANGE,
            )?,
            cpu_package_power_w: checked(
                "cpu_package_power_w",
                samples.cpu_package_power_w,
                timestamp_ms,
                max_skew_ms,
                POWER_W_RANGE,
            )?,
        })
    }

    /// One CSV data line (no trailing newline), same formatting as vahtisiru.
    pub fn to_csv_line(self) -> String {
        format!(
            "{},{},{},{},{}",
            self.timestamp_ms,
            self.gpu_temp_c,
            self.gpu_power_w,
            self.cpu_tctl_c,
            self.cpu_package_power_w
        )
    }
}

/// Serialize rows with the canonical header (in-memory only).
pub fn format_csv(rows: &[ContractRow]) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    for row in rows {
        out.push_str(&row.to_csv_line());
        out.push('\n');
    }
    out
}

/// Header-level failure (data-row problems are skipped, not errors).
#[derive(Debug, Clone, PartialEq)]
pub enum CsvError {
    Empty,
    HeaderMismatch { actual: String },
}

impl fmt::Display for CsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "telemetry CSV is empty"),
            Self::HeaderMismatch { actual } => write!(
                f,
                "telemetry CSV header mismatch: expected '{HEADER}', got '{actual}'"
            ),
        }
    }
}

impl std::error::Error for CsvError {}

/// Rows accepted plus malformed data lines skipped (vahtisiru `LoadResult`).
#[derive(Debug, Clone, PartialEq)]
pub struct LoadResult {
    pub rows: Vec<ContractRow>,
    pub skipped: usize,
}

impl LoadResult {
    pub fn has_usable_rows(&self) -> bool {
        !self.rows.is_empty()
    }
}

fn parse_finite_f32(s: &str) -> Option<f32> {
    s.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// Consumer-side reader with vahtisiru `parse_csv` semantics: trimmed header
/// must equal [`HEADER`]; blank lines ignored; malformed rows skipped and
/// counted; individual fields are not trimmed.
pub fn parse_csv(contents: &str) -> Result<LoadResult, CsvError> {
    let mut lines = contents.lines();
    let header = lines.next().ok_or(CsvError::Empty)?.trim();
    if header != HEADER {
        return Err(CsvError::HeaderMismatch {
            actual: header.to_string(),
        });
    }
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for raw in lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        let parsed = (f.len() == FIELD_COUNT)
            .then(|| {
                Some(ContractRow {
                    timestamp_ms: f[0].parse::<u64>().ok()?,
                    gpu_temp_c: parse_finite_f32(f[1])?,
                    gpu_power_w: parse_finite_f32(f[2])?,
                    cpu_tctl_c: parse_finite_f32(f[3])?,
                    cpu_package_power_w: parse_finite_f32(f[4])?,
                })
            })
            .flatten();
        match parsed {
            Some(row) => rows.push(row),
            None => skipped += 1,
        }
    }
    Ok(LoadResult { rows, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: u64 = 1_760_000_000_000;

    fn r(value: f64) -> Option<Reading> {
        Some(Reading {
            timestamp_ms: TS,
            value,
        })
    }

    fn good() -> RowSamples {
        RowSamples {
            gpu_temp_c: r(61.0),
            gpu_power_w: r(252.5),
            cpu_tctl_c: r(70.5),
            cpu_package_power_w: r(121.5),
        }
    }

    #[test]
    fn golden_header_is_frozen_contract() {
        assert_eq!(
            HEADER,
            "timestamp_ms,gpu_temp_c,gpu_power_w,cpu_tctl_c,cpu_package_power_w"
        );
        assert_eq!(HEADER.split(',').count(), FIELD_COUNT);
    }

    #[test]
    fn strict_row_round_trips_through_reader() {
        let row = ContractRow::from_samples(TS, good(), 1_000).unwrap();
        let text = format_csv(&[row]);
        assert!(text.starts_with(&format!("{HEADER}\n")));
        let back = parse_csv(&text).unwrap();
        assert_eq!(back.skipped, 0);
        assert_eq!(back.rows, vec![row]);
    }

    #[test]
    fn missing_sensor_fails_loud_not_zero_filled() {
        for col in [
            "gpu_temp_c",
            "gpu_power_w",
            "cpu_tctl_c",
            "cpu_package_power_w",
        ] {
            let mut s = good();
            match col {
                "gpu_temp_c" => s.gpu_temp_c = None,
                "gpu_power_w" => s.gpu_power_w = None,
                "cpu_tctl_c" => s.cpu_tctl_c = None,
                _ => s.cpu_package_power_w = None,
            }
            assert_eq!(
                ContractRow::from_samples(TS, s, 1_000),
                Err(RowError::Missing { column: col })
            );
        }
    }

    #[test]
    fn non_finite_fails_loud() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut s = good();
            s.gpu_power_w = r(bad);
            assert_eq!(
                ContractRow::from_samples(TS, s, 1_000),
                Err(RowError::NonFinite {
                    column: "gpu_power_w"
                })
            );
        }
    }

    #[test]
    fn unit_errors_fail_loud() {
        // Raw k10temp millidegrees passed through unconverted.
        let mut s = good();
        s.cpu_tctl_c = r(70_500.0);
        assert!(matches!(
            ContractRow::from_samples(TS, s, 1_000),
            Err(RowError::OutOfRange {
                column: "cpu_tctl_c",
                ..
            })
        ));
        // Raw NVML milliwatts passed through unconverted.
        let mut s = good();
        s.gpu_power_w = r(252_500.0);
        assert!(matches!(
            ContractRow::from_samples(TS, s, 1_000),
            Err(RowError::OutOfRange {
                column: "gpu_power_w",
                ..
            })
        ));
        // Negative power is not physiology.
        let mut s = good();
        s.cpu_package_power_w = r(-1.0);
        assert!(matches!(
            ContractRow::from_samples(TS, s, 1_000),
            Err(RowError::OutOfRange { .. })
        ));
        // Unix seconds mislabeled as milliseconds.
        assert_eq!(
            ContractRow::from_samples(1_760_000_000, good(), 1_000),
            Err(RowError::TimestampUnit {
                timestamp_ms: 1_760_000_000
            })
        );
    }

    #[test]
    fn stale_join_fails_loud() {
        let mut s = good();
        s.gpu_temp_c = Some(Reading {
            timestamp_ms: TS - 30_000,
            value: 61.0,
        });
        assert_eq!(
            ContractRow::from_samples(TS, s, 5_000),
            Err(RowError::Stale {
                column: "gpu_temp_c",
                skew_ms: 30_000,
                max_skew_ms: 5_000
            })
        );
    }
}
