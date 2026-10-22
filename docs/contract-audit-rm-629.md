# Contract audit: Theseus-Quarry vs vahtisiru frozen telemetry CSV (RM-629)

Issue: [#24](https://github.com/rmems/Theseus-Quarry/issues/24) · Linear RM-629 / RM-1393 · Audited 2026-10-19

## Contract version audited

| Item | Value |
| --- | --- |
| Repository | `rmems/vahtisiru` @ `4feb196b53509461ceb9b1e7f9a6dd35e67db280` (main, 2026-10-19) |
| Crate | `vahtisiru` 0.2.0 (`telemetry_csv` module); standalone `vahtisiru-telemetry-csv` 0.1.0 |
| Spec | `docs/telemetry_csv.md` (frozen) |
| Reference impl | `src/telemetry_csv.rs` |
| Fixtures | `tests/fixtures/telemetry_csv/*.csv` (vendored here byte-for-byte) |
| Frozen header | `timestamp_ms,gpu_temp_c,gpu_power_w,cpu_tctl_c,cpu_package_power_w` (`docs/telemetry_csv.md:21`, `src/telemetry_csv.rs:33`) |

The contract has no version field. The commit hash above is the version, and it is pinned in code as `csv_contract::AUDITED_CONTRACT_COMMIT` (`crates/mining-telemetry-core/src/csv_contract.rs:32`).

## Scope and framing

Theseus-Quarry is a **JSONL-only producer** (schema v1). It has no CSV exporter. Issue #24's acceptance criteria say it stays that way unless a CSV export is separately approved. This audit therefore answers a narrower question: *if* a projection were approved, could each contract column be filled from the signals Quarry already records, honestly and without zero-fill? And does the JSONL corpus already contain fabricated or unit-ambiguous vitals that would contaminate that projection?

Mining is one physiology source. The five contract columns are host vitals (GPU and CPU temperature and power) that every workload produces. Miner vitals (hashrate, shares, fan) have **no contract column**. They are covered under [Out-of-contract vitals](#out-of-contract-vitals).

No Cargo dependency on vahtisiru was added. `crates/mining-telemetry-core/src/csv_contract.rs` is a one-way copy of the header and reader semantics, matching how vahtisiru itself copies them from corinth-canal.

## Column map

Line references on the Quarry side are post-fix (this branch). The *Defect* column cites pre-fix `main` @ `3c98cc6`.

| # | Contract column (vahtisiru) | Contract parse / unit | Quarry source field | Quarry unit and conversion | Status |
| --- | --- | --- | --- | --- | --- |
| 1 | `timestamp_ms` (`docs/telemetry_csv.md:35`, `src/telemetry_csv.rs:42`, parsed `:196`) | `u64` Unix-epoch **ms** | `TelemetryEnvelope.timestamp` (`crates/mining-telemetry-core/src/schema.rs:30`), set `Utc::now()` at envelope build (`schema.rs:121`) | RFC 3339 `DateTime<Utc>` string. Projection uses `timestamp.timestamp_millis()` (ns precision truncated to ms) | **Mismatch (representation).** Convertible offline. JSONL keeps RFC 3339 by design. The projection guard `ContractRow::from_samples` rejects values `< 1e12` as seconds-mislabeled-as-ms (`csv_contract.rs:177`). It is envelope-build time, not sensor-acquisition time; for sysfs and NVML the gap is sub-ms (see U-5) |
| 2 | `gpu_temp_c` (`docs/telemetry_csv.md:36`, `src/telemetry_csv.rs:44`) | finite `f32`, die temp °C | `TelemetryPayload::GpuSched.gpu_temp_c` (`schema.rs:98`), from NVML `TemperatureSensor::Gpu` (`crates/telemetry-collector/src/gpu_scheduler.rs:117`) | NVML whole °C → `f32`. Now `Option<f32>`, omitted when unread, non-finite dropped (`gpu_scheduler.rs` `make_event`) | **Match** after fix D-1/D-2. Before the fix, a failed NVML read was written as `0.0` °C (`gpu_scheduler.rs:106` @main) and NVML-absent hosts got a fabricated `50.0` °C (`gpu_scheduler.rs:273` @main). Live plausibility on the RTX 5080 is unverified (U-1) |
| 3 | `gpu_power_w` (`docs/telemetry_csv.md:37`, `src/telemetry_csv.rs:46`) | finite `f32`, board power W | `TelemetryPayload::GpuSched.power_w` (`schema.rs:101`), from NVML `power_usage()` (`gpu_scheduler.rs:120`) | NVML **mW** → W via `nvml_milliwatts_to_watts` (`gpu_scheduler.rs:31`, tested) | **Mismatch (name only):** `power_w` vs `gpu_power_w`. vahtisiru's live relay uses the same `power_w` name (`docs/telemetry_csv.md:37`), so the rename belongs in the projection, not in a schema-v1 JSONL break. Unit matches. Before fix D-1/D-2, failed reads were `0.0` W and NVML-absent hosts got a fabricated `100.0` W (`gpu_scheduler.rs:107,274` @main). Whether NVML's reading is instantaneous or averaged on Blackwell is unverified (U-1) |
| 4 | `cpu_tctl_c` (`docs/telemetry_csv.md:38`, `src/telemetry_csv.rs:48`) | finite `f32`, CPU Tctl °C | `TelemetryPayload::HostHw.cpu_tctl_c` (`schema.rs:80`), from k10temp hwmon (`crates/telemetry-collector/src/sources/hwmon.rs:68`) | sysfs integer **millidegrees** ÷ 1000 (`hwmon.rs:40` `parse_millidegrees`), `f64` → `f32` at projection | **Match** after fix D-3/D-4. Before the fix, the channel was chosen by index (`temp1_input`, `hwmon.rs:38` @main) and any float was accepted and divided again (`hwmon.rs:27-30` @main). The channel is now resolved by the `Tctl` label, with a plausibility window. The label map on the live CPU is unverified (U-3) |
| 5 | `cpu_package_power_w` (`docs/telemetry_csv.md:39`, `src/telemetry_csv.rs:50`) | finite `f32`, CPU package W | `TelemetryPayload::HostHw.cpu_package_power_w` (`schema.rs:86`), from powercap RAPL (`crates/telemetry-collector/src/sources/rapl.rs:23`) | Δ`energy_uj` (**µJ**) ÷ 1e6 ÷ Δt s (`rapl.rs:36` `power_from_delta`) | **Match** after fix D-5/D-6. Before the fix, the first sample after start was written as `0.0` W (`rapl.rs:43` @main), and counter wrap used `u64::MAX` instead of `max_energy_range_uj`, producing a huge bogus value on wrap (`rapl.rs:36` @main). The zone, name and permissions on the AMD host are unverified (U-4) |

### Counts

| Status | Count | Columns |
| --- | --- | --- |
| Match | 3 | `gpu_temp_c`, `cpu_tctl_c`, `cpu_package_power_w` |
| Mismatch (non-blocking, mapped in a projection) | 2 | `timestamp_ms` (RFC 3339 → ms), `gpu_power_w` (name `power_w`) |
| Missing (no Quarry source) | 0 | — |
| Unverifiable without the live host | 4 of 5 for live plausibility (U-1, U-3, U-4); every column for join cadence (U-5) | see below |

At the **artifact** level the CSV projection itself is **missing by design**. There is no exporter, and the five columns live in three different JSONL stems: `gpu_sched_telemetry`, `hwmon_telemetry` and `rapl_telemetry`. They are recorded at different cadences and are never joined into one row. That gap is intentional per #24 and is not "fixed" here.

### Header and reader semantics

| Contract rule (vahtisiru) | Quarry copy | Status |
| --- | --- | --- |
| Exact trimmed header, extra column = mismatch (`docs/telemetry_csv.md:24-29`, `src/telemetry_csv.rs:172-183`) | `csv_contract::HEADER` + `parse_csv` (`csv_contract.rs:26`, `:278`). Golden test `golden_header_bytes` | Match |
| Empty input is an error (`src/telemetry_csv.rs:174`) | `CsvError::Empty` | Match (fixture `empty.csv`) |
| Whitespace lines ignored, not skipped (`docs/telemetry_csv.md:53`) | `parse_csv` | Match (fixture `whitespace_lines.csv`) |
| Short / extra / non-numeric / NaN / ±inf / non-`u64` timestamp rows skipped and counted (`src/telemetry_csv.rs:187-223`) | `parse_csv` | Match (fixture `malformed_rows.csv`: 3 rows, 7 skipped) |
| CRLF and missing final newline accepted | `str::lines` + trim | Match (fixtures) |
| Fields required; missing sensor = malformed, not `None` (`docs/telemetry_csv.md:41-43`) | Producer side is **stricter**: `ContractRow::from_samples` returns `RowError::Missing` rather than emitting a row the reader would skip | Match (producer is stricter, which is intentional) |

`row_for_tick` (replay) is consumer-only and is not copied.

## Defects fixed on this branch (offline-safe)

Each fix is covered by negative tests that fail loud. None of them changes the safety decision ladder's outcome for any input that was possible before.

| ID | Defect (pre-fix @ `3c98cc6`) | Fix | Tests |
| --- | --- | --- | --- |
| D-1 | NVML temp/power read failure became `0` (`gpu_scheduler.rs:106-107`), so `0 °C` / `0 W` entered the corpus as real vitals | `GpuSnapshot` / `GpuSchedulerEvent` / `GpuSched` payload fields are now `Option<f32>`, omitted from JSONL when unread. Old JSONL lines still deserialize | `unread_sensors_are_not_fabricated_in_events` |
| D-2 | `FallbackProvider` (NVML absent) fabricated `50 °C` / `100 W` (`gpu_scheduler.rs:269-276`), and the "provider returned None" path wrote `0.0`/`0.0` (`:209-214`) | Both paths now report `None` | same as D-1 |
| D-2b | NaN GPU temperature would serialize as JSON `null` and compare false everywhere | Non-finite values are filtered before serialization. The thermal rungs require a finite reading | `non_finite_temperature_never_counts_as_cool_reading`, `thermal_ladder_still_fires_on_real_reading` |
| D-3 | hwmon parsed millidegrees as `f64`, so an already-converted `61.5` became `0.0615 °C`. No plausibility check (`hwmon.rs:26-31`) | Integer-only parse plus a −40…150 °C window | `millidegrees_convert_to_celsius`, `non_integer_or_garbage_fails_loud`, `implausible_values_rejected` |
| D-4 | Tctl/CCD channels picked by fixed index (`temp1/3/4_input`, `hwmon.rs:38-40`). The index→label map varies across k10temp families | Resolve by `temp*_label` (`Tctl`, `Tccd1`, `Tccd2`); the legacy index is used only when the chip publishes no labels at all, otherwise a missing label leaves the channel absent | `label_lookup_beats_index`, `labeled_chip_never_falls_back_to_legacy_index` |
| D-5 | RAPL first sample (and `dt == 0`) was emitted as `0.0 W` (`rapl.rs:43-45`) | No record until a real delta exists. `dt ≤ 0` is dropped | `first_sample_emits_nothing_not_zero`, `zero_dt_dropped` |
| D-6 | RAPL wrap computed against `u64::MAX` (`rapl.rs:36`). A real wrap at `max_energy_range_uj` produced an enormous power value, and a counter reset was indistinguishable from a wrap | Unwrap with `max_energy_range_uj`. A backwards counter with no known range is treated as a reset and dropped. Values > 2 kW are dropped. The zone `name` must start with `package` | `wrap_uses_max_energy_range_not_u64_max`, `backwards_counter_without_range_is_reset_dropped`, `implausible_power_dropped`, `microjoules_per_second_to_watts` |
| D-7 | Contract header not pinned anywhere in Quarry | `csv_contract` module with golden header test, vendored vahtisiru fixtures and a conformance test. The strict producer row builder rejects missing, NaN/inf, out-of-range (mW-as-W, m°C-as-°C, negative power), seconds-as-ms and stale-join inputs | `crates/mining-telemetry-core/tests/csv_contract_conformance.rs` (8), `csv_contract::tests` (6) |

JSONL compatibility: the `gpu_sched` payload keeps `schema_version = 1`. `gpu_temp_c` / `power_w` are now *absent* rather than *zero* when unread. A consumer that treated them as always present will now fail on a missing key, which is the intended fail-loud behavior. Historical lines with numeric values still parse.

## Unverified pending the live host (feeds #15)

These were **not guessed**. Each one needs a capture on ShipOfTheseus (RTX 5080, Ryzen) before a projection can be trusted.

| ID | Column(s) | Question | How to confirm |
| --- | --- | --- | --- |
| U-1 | `gpu_temp_c`, `gpu_power_w` | Does NVML `TemperatureSensor::Gpu` report the die/edge sensor the contract means on Blackwell? Is `power_usage()` instantaneous or averaged board power on the RTX 5080? Do the values agree with `nvidia-smi --query-gpu=temperature.gpu,power.draw`? | Run the collector alongside `nvidia-smi -lms 1000` under idle, mining and path-tracing loads, then diff |
| U-2 | `gpu_temp_c`, `gpu_power_w` | The scheduler reads NVML device index 0 only. Is index 0 the RTX 5080 on this host (no iGPU or second NVIDIA device ahead of it)? | `nvidia-smi -L`; check the UUID against the physical card |
| U-3 | `cpu_tctl_c` | Which `temp*_label` values does k10temp expose on this CPU? Does the SKU apply a Tctl offset (Tctl ≠ Tdie), which matters for physiology even though the contract names Tctl? | `grep . /sys/class/hwmon/hwmon*/{name,temp*_label}`; compare with `sensors` |
| U-4 | `cpu_package_power_w` | Is `/sys/class/powercap/intel-rapl:0` present on the AMD host, with `name` = `package-0`? Is `max_energy_range_uj` readable? Is `energy_uj` readable by the collector's user? Since CVE-2020-8694 it is often root-only (0400), which would make RAPL silently absent for a non-root collector | `ls -l /sys/class/powercap/intel-rapl:0/`; run the collector as its service user |
| U-5 | all (join) | The five columns arrive on different cadences: `gpu_sched` is emitted only on transition or a **30 s heartbeat** (`gpu_scheduler.rs:259`), while `hwmon`/`rapl` follow `TELEMETRY_INTERVAL` (default 5 s). A strict row join needs a `max_skew_ms` budget, and with the current heartbeat most rows would be correctly refused as `Stale`. Choosing the budget (or adding a per-tick GPU sample) is a product decision that needs real cadence data | Capture one hour of JSONL and measure per-stem inter-arrival times |
| U-6 | safety, not contract | An unread or non-finite GPU temperature skips the thermal rungs, so mining stays allowed. This **preserves** prior behavior (0 °C / fabricated 50 °C also allowed mining). Whether unknown GPU temperature should fail **closed** is a deterministic-safety decision for the owner. It was not changed here | Owner decision; if adopted, add a `PauseReason::SensorUnavailable` rung |

## Out-of-contract vitals

These are recorded in JSONL but have **no column** in the frozen CSV. They are noted here so nobody assumes otherwise:

- **Hashrate** (`MinerPerf.hashrate` + native `hashrate_unit`). BzMiner, XMRig and SRBMiner are documented as H/s. OneZeroMiner is labeled H/s conservatively and is unconfirmed (#15).
- **Accepted / rejected shares** (`MinerPerf.shares_*`) are cumulative counters. Counter resets on miner restart are not yet detected in JSONL; that belongs in downstream ETL (spikenaut-telemetry-etl#27). Stale shares are not separated from rejected.
- **Fan speed** is not collected by any source.
- **VRAM** (`GpuSched.vram_*_mb`) is MiB (÷ 1024²) despite the `_mb` suffix.

Adding any of these to the CSV would be a contract change in vahtisiru, not a Quarry change.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p mining-telemetry-core --test csv_contract_conformance
```
