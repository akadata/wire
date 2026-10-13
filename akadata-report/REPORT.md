# AKADATA executor benchmark validation — Homer report

Branch: `akadata/validate-and-correct-errors` (fork `akadata/wire`)
Base commit at time of work: `0e40e851da04ad11b3cf7ee373a0ab73743a188b`
`wire-runtime` untouched. Only the two benchmark harnesses were corrected.
No new benchmark CLI was added; the normal output now carries a
machine-readable `EXECUTOR ...` line that audit scripts parse.

## 1. Issues found

### 1.1 v6 `benchmark_runtime_executor` — inflated numerator (the 4096 bug)
File: `wire-echo/src/bin/v6_benchmark.rs` (before fix, lines ~196-208)

- `wire-runtime/src/executor.rs:8` sets `MAX_TASKS = 4096`.
- `LocalExecutor::spawn()` returns `Option<usize>`: `None` when the
  `free_slots` pool is exhausted.
- The old v6 code looped `1_000_000` times calling
  `executor.spawn(async {})` and **ignored the return value**, then called
  `run_until_stalled()` once and computed
  `Mops = 1_000_000 / elapsed`.
- At most `4096` spawns can succeed before the queue is full, so
  `~995,904` spawns fail silently. The numerator (`1,000,000`) counts work
  that was never accepted, let alone polled to `Ready`. That is the ~244x
  inflation behind the old `202.15 Mops/s / 4.95 ns` claim.
- `completed` was never observed at all: `run_until_stalled()` returns
  `()`, and the return of `run_single_tick()` (which does report per-tick
  completions) was discarded.

### 1.2 v7 `benchmark_honest_executor` — half-fixed
File: `wire-echo/src/bin/v7_benchmark.rs` (before fix, lines ~163-185)

- Counted `accepted` (`spawn().is_some()`), which is better than v6, but:
  - workload was `200_000`, not the required `1,000_000`;
  - denominator was `accepted`, not `completed` — completion was inferred,
    never measured (`run_until_stalled()` returns `void`);
  - drain guard `if accepted % 1024 == 0` fires on `accepted == 0` and
    couples draining to a counter that stalls if spawns fail;
  - pass criterion (`ns < 200`) is arbitrary; no
    `attempted == accepted == completed` fail-closed check.

### 1.3 Where the 4096 limit bites
- `executor.rs:33-37`: `slots` and `free_slots` are sized `MAX_TASKS`.
- `executor.rs:56-67`: `spawn()` pops `free_slots`; `None` = full.
- `executor.rs:78-114`: `run_single_tick()` has a budget of 64 polls per
  call and returns `completed_count`; `run_until_stalled()` loops it but
  discards the count.
- Any harness that spawns more than `4096` futures without draining in
  between **will** overflow. The only honest patterns are batch + drain,
  with `BATCH << MAX_TASKS`.

## 2. Corrections made (what changed, what did not)

- **No `wire-runtime` change**, per approved choice. The existing
  `run_single_tick() -> usize` already exposes completions; the harness
  just has to sum it.
- **v6 fixed in place** (`wire-echo/src/bin/v6_benchmark.rs`): the old
  inflated implementation was removed, not preserved under another name.
- **v7 corrected to the same contract** (`wire-echo/src/bin/v7_benchmark.rs`).
- Both harnesses now implement one algorithm:
  - `TOTAL = 1_000_000`, `BATCH = 1024` (`1024 < 4096`, final partial
    batch of `1_000_000 % 1024 = 576` handled explicitly).
  - `attempted` = requested workload (constant `TOTAL`).
  - `accepted` = count of `spawn()` returning `Some`.
  - `completed` = sum of `run_single_tick()` returns, drained per batch
    via `drain_batch(expected)` / `drain_executor_batch(expected)` which
    polls until exactly `expected` completions are seen and returns `None`
    on stall (`run_single_tick() == 0` early).
  - `elapsed` spans spawn + poll + completion.
  - `Mops/s = completed / elapsed`; `ns/task = elapsed_ns / completed`.
  - Valid run requires `attempted == accepted == completed == 1_000_000`.
    Any `spawn() -> None`, any stall, or any count mismatch marks the run
    invalid: metrics are forced to `0.0`, the dashboard row shows `[FAIL]`,
    an `[FAIL] accounting mismatch` message is printed, and the binary
    exits non-zero (`std::process::exit(1)`). No flattering number is
    ever printed for invalid counts.
  - Machine-readable line on every run (parsed by the audit, no CLI flag):
    `EXECUTOR attempted=.. accepted=.. completed=.. elapsed_s=.. mops=.. ns_per_task=.. valid=..`
- v6's unconditional `All Tier-1 ... verified and exceeded` finale is now
  gated behind the accounting check; v7's `[FAIL]` path now exits non-zero.

## 3. Homer hardware / software identity

- hostname: `homer`
- uname: `Linux homer 7.2.2-arch1-1 #1 SMP PREEMPT_DYNAMIC Fri, 28 Aug 2026 16:09:39 +0000 x86_64 GNU/Linux`
- CPU: `13th Gen Intel(R) Core(TM) i9-13900K`, 32 CPUs (24 cores + HT),
  1 socket, L1d 896 KiB / L1i 1.3 MiB / L2 32 MiB / L3 36 MiB, 1 NUMA node.
  CPU flags include `avx2` but **not** `avx512f` (expected on Raptor Lake),
  so the v6 AVX-512 rows report N/A and the AVX2 path is active.
- `rustc 1.99.0 (b940084d7 2026-09-28)`, host `x86_64-unknown-linux-gnu`,
  LLVM 23.1.1; `cargo 1.99.0`.
- Builds:
  - normal: `cargo build --release -p wire-echo --bin v6_benchmark --bin v7_benchmark`
  - native: `RUSTFLAGS="-C target-cpu=native" cargo build --release -p wire-echo --bin v6_benchmark --bin v7_benchmark`

## 4. Measured Homer figures (corrected accounting)

Every run below completed exactly `attempted=accepted=completed=1000000`
with `valid=true` and exit code 0. Full per-run table:
`executor_runs_normal.csv`, `executor_runs_native.csv`.
Sample full outputs: `samples/`.

### 4.1 Normal release (`cargo build --release`), 10 runs each

v6 executor (`v6_benchmark`, TOTAL=1M, BATCH=1024):

| run | elapsed_s | Mops/s | ns/task | counts |
|-----|-----------|--------|---------|--------|
| 1 | 0.067462 | 14.8232 | 67.46 | 1M/1M/1M |
| 2 | 0.066603 | 15.0144 | 66.60 | 1M/1M/1M |
| 3 | 0.064105 | 15.5993 | 64.11 | 1M/1M/1M |
| 4 | 0.064572 | 15.4867 | 64.57 | 1M/1M/1M |
| 5 | 0.067397 | 14.8374 | 67.40 | 1M/1M/1M |
| 6 | 0.064369 | 15.5354 | 64.37 | 1M/1M/1M |
| 7 | 0.070356 | 14.2134 | 70.36 | 1M/1M/1M |
| 8 | 0.067135 | 14.8953 | 67.14 | 1M/1M/1M |
| 9 | 0.064048 | 15.6133 | 64.05 | 1M/1M/1M |
| 10 | 0.064403 | 15.5273 | 64.40 | 1M/1M/1M |

- Mops/s: min 14.2134, max 15.6133, mean 15.1546, median 15.2506, sd (pop) 0.4456
- ns/task: min 64.05, max 70.36, mean 66.05, median 65.59, sd (pop) 1.98

v7 executor (`v7_benchmark`, same contract), normal release:

| run | elapsed_s | Mops/s | ns/task | counts |
|-----|-----------|--------|---------|--------|
| 1 | 0.063690 | 15.7010 | 63.69 | 1M/1M/1M |
| 2 | 0.063927 | 15.6429 | 63.93 | 1M/1M/1M |
| 3 | 0.064137 | 15.5916 | 64.14 | 1M/1M/1M |
| 4 | 0.065090 | 15.3633 | 65.09 | 1M/1M/1M |
| 5 | 0.066017 | 15.1477 | 66.02 | 1M/1M/1M |
| 6 | 0.067001 | 14.9252 | 67.00 | 1M/1M/1M |
| 7 | 0.063096 | 15.8488 | 63.10 | 1M/1M/1M |
| 8 | 0.064350 | 15.5400 | 64.35 | 1M/1M/1M |
| 9 | 0.063328 | 15.7907 | 63.33 | 1M/1M/1M |
| 10 | 0.063389 | 15.7757 | 63.39 | 1M/1M/1M |

- Mops/s: min 14.9252, max 15.8488, mean 15.5327, median 15.6172, sd (pop) 0.2856
- ns/task: min 63.10, max 67.00, mean 64.40, median 64.04, sd (pop) 1.20

### 4.2 `target-cpu=native`, 10 runs each

v6 executor, native:

| run | elapsed_s | Mops/s | ns/task | counts |
|-----|-----------|--------|---------|--------|
| 1 | 0.066445 | 15.0501 | 66.44 | 1M/1M/1M |
| 2 | 0.072204 | 13.8497 | 72.20 | 1M/1M/1M |
| 3 | 0.062338 | 16.0416 | 62.34 | 1M/1M/1M |
| 4 | 0.062476 | 16.0062 | 62.48 | 1M/1M/1M |
| 5 | 0.062154 | 16.0892 | 62.15 | 1M/1M/1M |
| 6 | 0.062377 | 16.0315 | 62.38 | 1M/1M/1M |
| 7 | 0.060931 | 16.4121 | 60.93 | 1M/1M/1M |
| 8 | 0.062920 | 15.8933 | 62.92 | 1M/1M/1M |
| 9 | 0.063308 | 15.7957 | 63.31 | 1M/1M/1M |
| 10 | 0.061789 | 16.1841 | 61.79 | 1M/1M/1M |

- Mops/s: min 13.8497, max 16.4121, mean 15.7354, median 16.0189, sd (pop) 0.7137
- ns/task: min 60.93, max 72.20, mean 63.69, median 62.43, sd (pop) 3.15

v7 executor, native:

| run | elapsed_s | Mops/s | ns/task | counts |
|-----|-----------|--------|---------|--------|
| 1 | 0.062534 | 15.9912 | 62.53 | 1M/1M/1M |
| 2 | 0.066521 | 15.0328 | 66.52 | 1M/1M/1M |
| 3 | 0.062849 | 15.9113 | 62.85 | 1M/1M/1M |
| 4 | 0.066840 | 14.9611 | 66.84 | 1M/1M/1M |
| 5 | 0.062391 | 16.0281 | 62.39 | 1M/1M/1M |
| 6 | 0.063260 | 15.8078 | 63.26 | 1M/1M/1M |
| 7 | 0.066432 | 15.0529 | 66.43 | 1M/1M/1M |
| 8 | 0.066582 | 15.0191 | 66.58 | 1M/1M/1M |
| 9 | 0.062106 | 16.1016 | 62.11 | 1M/1M/1M |
| 10 | 0.062033 | 16.1204 | 62.03 | 1M/1M/1M |

- Mops/s: min 14.9611, max 16.1204, mean 15.6026, median 15.8596, sd (pop) 0.4864
- ns/task: min 62.03, max 66.84, mean 64.15, median 63.06, sd (pop) 2.02

## 5. Comparison with the stated `5.77 Mops/s / 173.43 ns`

- Homer's corrected single-thread `async {}` spawn+poll+complete figures are
  **~15-16 Mops/s (~61-70 ns/task)**, i.e. roughly **2.6-2.8x the throughput**
  (about one-third the latency) of the stated `5.77 Mops/s / 173.43 ns`.
- That stated figure was reportedly obtained by draining in batches so all
  1M tasks complete; on Homer the same honest contract runs faster. The
  difference is hardware/toolchain-specific (i9-13900K, rustc 1.99,
  release vs. the original reporter's machine) — it is **not** a claim that
  Homer is "faster" in general, only what this workload measures here.
- The old `202.15 Mops/s / 4.95 ns` figure is not comparable: it divided by
  work never accepted (see 1.1). The corrected Homer numbers replace it;
  no `202 Mops/s` claim survives honest accounting.

## 6. Validation

- `cargo check --workspace`: pass, exit 0.
- `cargo test --workspace`: pass, exit 0 (all suites ok; e.g. 11 + 3 + 3 + 1
  passed across crates, 0 failed). No unrelated failures found, so nothing
  was fixed opportunistically.
- `git diff --check`: clean.
- `git status`: only `wire-echo/src/bin/v6_benchmark.rs`,
  `wire-echo/src/bin/v7_benchmark.rs`, plus this `akadata-report/` folder.

## 7. What this report does not claim

- The executor micro-benchmark measures `async {}` spawn + poll + completion
  on one `LocalExecutor`. It does not measure QUIC, NVMe, SmartNIC, or
  full-stack ingestion; those harness sections were left as-is and their
  labels should still be read with the scepticism in the original review
  (dummy auth tags, hard-coded NIC metadata, possible SQPOLL fallback).
- These are single-thread, single-executor, best-case `Ready`-immediate
  futures. Real workloads with `Pending`, wakeups, and I/O will differ.
