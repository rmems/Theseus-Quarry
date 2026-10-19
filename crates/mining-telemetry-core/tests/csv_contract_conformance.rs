//! Conformance of `mining_telemetry_core::csv_contract` against the vahtisiru
//! frozen CSV fixtures (RM-629 / issue #24).
//!
//! Fixtures are vendored byte-for-byte from `rmems/vahtisiru`
//! `tests/fixtures/telemetry_csv/` at commit `4feb196b`; the expected results
//! below are copied from that directory's README fixture table.

use mining_telemetry_core::csv_contract::{
    AUDITED_CONTRACT_COMMIT, CsvError, HEADER, LoadResult, parse_csv,
};

fn fixture(name: &str) -> &'static str {
    match name {
        "canonical.csv" => include_str!("fixtures/telemetry_csv/canonical.csv"),
        "canonical_crlf.csv" => include_str!("fixtures/telemetry_csv/canonical_crlf.csv"),
        "empty.csv" => include_str!("fixtures/telemetry_csv/empty.csv"),
        "header_extra_column.csv" => {
            include_str!("fixtures/telemetry_csv/header_extra_column.csv")
        }
        "header_mismatch.csv" => include_str!("fixtures/telemetry_csv/header_mismatch.csv"),
        "header_only.csv" => include_str!("fixtures/telemetry_csv/header_only.csv"),
        "malformed_rows.csv" => include_str!("fixtures/telemetry_csv/malformed_rows.csv"),
        "no_trailing_newline.csv" => {
            include_str!("fixtures/telemetry_csv/no_trailing_newline.csv")
        }
        "whitespace_lines.csv" => include_str!("fixtures/telemetry_csv/whitespace_lines.csv"),
        other => panic!("unknown fixture {other}"),
    }
}

fn ok(name: &str) -> LoadResult {
    parse_csv(fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn audited_commit_is_pinned() {
    assert_eq!(AUDITED_CONTRACT_COMMIT.len(), 40);
}

#[test]
fn golden_header_bytes() {
    // Golden: the first line of the canonical fixture IS the contract.
    let first = fixture("canonical.csv").lines().next().unwrap();
    assert_eq!(first, HEADER);
    assert_eq!(
        HEADER.as_bytes(),
        b"timestamp_ms,gpu_temp_c,gpu_power_w,cpu_tctl_c,cpu_package_power_w"
    );
}

#[test]
fn canonical_variants_parse() {
    for name in [
        "canonical.csv",
        "canonical_crlf.csv",
        "no_trailing_newline.csv",
    ] {
        let r = ok(name);
        assert_eq!(r.rows.len(), 2, "{name}");
        assert_eq!(r.skipped, 0, "{name}");
        assert!(r.has_usable_rows(), "{name}");
        assert_eq!(r.rows[0].timestamp_ms, 1000, "{name}");
        assert_eq!(r.rows[1].gpu_power_w, 252.5, "{name}");
    }
}

#[test]
fn extra_column_is_header_mismatch_not_superset() {
    assert!(matches!(
        parse_csv(fixture("header_extra_column.csv")),
        Err(CsvError::HeaderMismatch { .. })
    ));
    assert!(matches!(
        parse_csv(fixture("header_mismatch.csv")),
        Err(CsvError::HeaderMismatch { .. })
    ));
}

#[test]
fn empty_is_error_not_zero_rows() {
    assert_eq!(parse_csv(fixture("empty.csv")), Err(CsvError::Empty));
}

#[test]
fn header_only_has_no_usable_rows() {
    let r = ok("header_only.csv");
    assert_eq!(r.skipped, 0);
    assert!(!r.has_usable_rows());
}

#[test]
fn malformed_rows_skipped_and_counted() {
    let r = ok("malformed_rows.csv");
    let ts: Vec<u64> = r.rows.iter().map(|row| row.timestamp_ms).collect();
    assert_eq!(ts, vec![1000, 4000, 7000]);
    assert_eq!(r.skipped, 7);
}

#[test]
fn whitespace_lines_ignored_not_skipped() {
    let r = ok("whitespace_lines.csv");
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.skipped, 0);
}
