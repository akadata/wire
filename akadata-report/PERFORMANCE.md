# Executor performance work — like-for-like comparison (Homer)

Branch: `akadata/executor-performance` (from `50448d9`, the validated
accounting fix). Baseline for comparison is commit `50448d9` itself, built
and benched side-by-side with the improved code — not the older REPORT.md
figures.

## 0. Machine state (both versions; §2 at 57 hugepages, §3 at 324)

- `vm.nr_hugepages` requested `1024`; kernel granted **57** (114 MB,
  `HugePages_Free: 57`, after `drop_caches`; before that only 14 — busy
  workstation, 2 GB contiguous unavailable without reboot).
- CPU governor: `performance` on all 32 CPUs (was already set; verified).
- Method: serial back-to-back runs, identical binaries invocation, same
  `EXECUTOR` honest contract (`attempted == accepted == completed ==
  1_000_000`, else fail-closed). One early parallel run showed contention
  outliers and was discarded; everything below is serial.
- Note: the executor micro-benchmark does not consume hugetlb (pure heap),
  and the pre/post-hugepage baseline means were identical (~15.4 vs ~15.4),
  so hugepages are documented context, not a confound.

## 1. What changed (and why it is faster, not a benchmark trick)

`wire-runtime/src/task.rs`
- `Task<F>` now stores the future **inline** instead of `Pin<Box<F>>`
  (one fewer heap allocation per `spawn` for real futures; ZST benchmark
  futures were already allocation-free, so this is "better", not the
  source of the speedup). Pin-projection via `Pin::new_unchecked` with a
  SAFETY comment: the `Task` always lives in the same `Box<dyn Runnable>`
  (moved as a pointer, pointee address stable), so the future never moves
  while pinned.

`wire-runtime/src/executor.rs`
- New `drain_all() -> usize`: one thread-local guard + one `ring.submit()`
  for the whole drain, tight loop to empty, exact `Ready` count returned.
  The old path ran `run_single_tick()` (≈16× per 1024-task batch), each
  doing a TLS `RefCell` guard pair **and an `io_uring_enter` syscall**
  (`ring.submit()`), then the benchmark summed the per-tick counts.
- `run_until_stalled()` is now that fast drain (all existing callers get it).
- `run_single_tick()` kept with identical signature/semantics (legacy path).
- Wake-during-poll correctness fix (found by the new test, pre-existing):
  a future that calls `wake_by_ref()` inside its own `poll` used to have
  the wake silently dropped (`runnable` is taken during poll, so
  `enqueue_task` saw `None` and parked the task forever). New
  `is_polling` / `woke_while_polling` flags re-queue instead of parking.
  Both drain loops share one `poll_task_slot()` helper.
- 3 new unit tests: exact-count drain, Pending+wake completion, and
  over-capacity spawn rejection (`MAX_TASKS` still 4096, still enforced).

`wire-echo/src/bin/{v6,v7}_benchmark.rs`
- Drain helpers now call `drain_all()` once per batch with the same
  exact-count check (`Some(n)` iff `n == expected`, else invalid →
  zeroed metrics + non-zero exit). Same contract, same BATCH=1024,
  same 1M/1M/1M requirement. No CLI added.

Why ~1.7x and not ~5%: the old drain issued **15,632**
`io_uring_enter` syscalls per 1M tasks (16 per batch); the new one issues
**977** (1 per batch). On this kernel (heavy IBRS/IBPB mitigations)
each enter costs on the order of a microsecond — that syscall collapse
accounts for essentially the whole ~28 ms saved per 1M tasks. The
benchmark measures the same real work (spawn + poll + completion of 1M
real futures); the removed work was pure overhead syscalls, so the gain
is genuine executor improvement, not measurement change.

## 2. Like-for-like results @57 hugepages (10 runs each, all 1M/1M/1M valid)

Normal release:

| series   | Mops/s min | max   | mean  | median | sd   | ns/task mean |
|----------|------------|-------|-------|--------|------|--------------|
| base v6  | 14.73      | 15.80 | 15.40 | 15.48  | 0.34 | 64.95        |
| new v6   | 24.84      | 27.68 | 26.03 | 25.76  | 0.94 | 38.47        |
| base v7  | 15.11      | 16.90 | 15.91 | 15.86  | 0.60 | 62.94        |
| new v7   | 25.67      | 28.41 | 27.41 | 27.88  | 1.01 | 36.54        |

`target-cpu=native`:

| series   | Mops/s min | max   | mean  | median | sd   | ns/task mean |
|----------|------------|-------|-------|--------|------|--------------|
| base v6  | 14.70      | 15.98 | 15.68 | 15.85  | 0.40 | 63.83        |
| new v6   | 23.06      | 28.10 | 26.01 | 25.69  | 1.48 | 38.58        |
| base v7  | 12.67      | 16.71 | 15.39 | 15.68  | 1.02 | 65.32        |
| new v7   | 24.24      | 28.42 | 26.37 | 25.56  | 1.53 | 38.05        |

Speedup (mean Mops/s, new/base): **v6 normal 1.69x, v7 normal 1.72x,
v6 native 1.66x, v7 native 1.71x.**

Raw per-run tables: `perf_like_for_like_normal.csv`,
`perf_like_for_like_native.csv`. Sample full outputs in `samples/`.

## 3. Retest at 324 hugepages on a loaded Homer (2026-10-13 ~22:00 BST)

Homer is not a quiet machine, and the numbers below were earned on a loaded
one — that is stated as context, not as an excuse (every baseline-vs-new
pair ran back-to-back under the same load, so the comparison stays fair).

- Box: 125 GiB RAM, `free` showed ~108–110 GiB used, ~8–10 GiB free,
  ~14–17 GiB available; 239 GiB swap (17 GiB used). Uptime 7 days, 29 users,
  load typically ~5–7 — with spikes: mid-retest the 1-minute average hit
  **38** (`cintpos -slow` at ~100% CPU, a `mame marble` arcade session, the
  Egg qemu VM at ~112% CPU / ~63 GiB resident, several `opencode --auto`
  sessions, Firefox). The spike polluted one native set (bimodal 13–24
  Mops); that set was discarded, the load was left to decay to ~4.5, and
  the affected sets were re-run serially. Discarded data is disclosed here
  precisely so nobody suspects cherry-picking: the re-runs reproduce the
  same ~1.7x gap.
- Hugepages after 2 VMs were shut down: requested 2048 → granted 292;
  `drop_caches` + requested 4096 (4x suggested) → granted **324** (648 MB),
  `HugePages_Free: 324` throughout. That is the fragmentation ceiling with
  a 63 GiB VM pinned for 7 days; chasing more would mean reclaim pressure
  against Egg, so 324 is where it stands. Available memory never dropped
  below 14 GiB; Egg/qemu untouched throughout.
- Finding: hugepages do not move the executor benchmark (pure heap,
  consumes no hugetlb). Baseline means at 57 vs 324 hugepages: v6 normal
  15.40 → 15.04, v7 normal 15.91 → 15.53, v6 native 15.68 → 15.64,
  v7 native 15.39 → 15.67 — all within run-to-run noise. The 324-page
  matrix below exists so the comparison with the improved code is
  like-for-like at the higher hugepage count, not because hugepages help here.

Normal release @324 hugepages (10 runs each, all 1M/1M/1M valid):

| series   | Mops/s min | max   | mean  | median | sd   | ns/task mean |
|----------|------------|-------|-------|--------|------|--------------|
| base v6  | 14.31      | 15.62 | 15.04 | 15.02  | 0.37 | 66.52        |
| new v6   | 23.05      | 27.82 | 25.90 | 25.79  | 1.21 | 38.70        |
| base v7  | 14.92      | 16.14 | 15.53 | 15.63  | 0.42 | 64.44        |
| new v7   | 21.96      | 28.53 | 26.62 | 26.55  | 1.87 | 37.77        |

Native @324 hugepages:

| series   | Mops/s min | max   | mean  | median | sd   | ns/task mean |
|----------|------------|-------|-------|--------|------|--------------|
| base v6  | 14.92      | 16.31 | 15.64 | 15.78  | 0.43 | 63.98        |
| new v6   | 24.38      | 27.90 | 25.75 | 25.38  | 1.02 | 38.89        |
| base v7  | 14.83      | 16.02 | 15.67 | 15.86  | 0.41 | 63.88        |
| new v7   | 25.70      | 28.41 | 27.32 | 27.99  | 1.14 | 36.67        |

Speedup (mean Mops/s, new/base) @324: **v6 normal 1.72x, v7 normal
1.71x, v6 native 1.65x, v7 native 1.74x** — same ~1.7x as the 57-page
matrix (1.66–1.72x). Raw tables: `perf_h324_normal.csv`,
`perf_h324_native.csv`.

- Stack-overflow tally, updated: the intermittent `thread 'main' has
  overflowed its stack` abort in `v7_benchmark` (before any benchmark
  section, right after the probing banner) has now been seen **3 times in
  ~60 v7 runs**: baseline-native, baseline-normal, and improved-native —
  i.e. both code versions and both build modes. The executor rewrite is
  therefore ruled out as the cause; it is a pre-existing v7-harness flake
  (suspected oversized `main` frame under inlining; exact frame TBD,
  follow-up out of scope). Each crashed run was replaced by a documented
  clean rerun; every tabulated run is a valid 1M/1M/1M pass. `v6_benchmark`
  never crashed in 80+ runs.

## 4. Validation

- `cargo check --workspace`: pass. `cargo test --workspace`: pass
  (incl. 3 new `wire-runtime` tests; 0 failures anywhere).
- `git diff --check`: clean.
- Hugepages/governor verified before, during, and after each matrix
  (57 free for the first matrix, 324 free for the retest;
  `performance` throughout).
