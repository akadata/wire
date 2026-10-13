# Executor performance work — like-for-like comparison (Homer)

Branch: `akadata/executor-performance` (from `50448d9`, the validated
accounting fix). Baseline for comparison is commit `50448d9` itself, built
and benched side-by-side with the improved code — not the older REPORT.md
figures.

## 0. Machine state during ALL runs below (both versions)

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

## 2. Like-for-like results (10 runs each, all 1M/1M/1M valid)

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

## 3. Pre-existing issue found (baseline, not introduced here)

- Baseline `v7_benchmark` built with `target-cpu=native` aborted once with
  `thread 'main' has overflowed its stack` right after the probing banner
  (1 of 13 runs; 3 immediate reruns passed; a documented rerun completed
  the 10-set). Never seen in normal-release builds (0/40+) or improved
  native v7 (0/10) — but with n=1 vs n=10 that difference is not
  statistically significant, so no causal claim is made either way.
  Suspect: native inlining inflating `main`'s frame in harness code
  (histogram/disruptor paths are heap-based; exact frame TBD).
  Follow-up (bigger main stack / frame analysis) is out of scope for the
  executor work; recorded here so it is not mistaken for a regression.
  The crashed run was replaced by a clean rerun; every tabulated run is a
  valid 1M/1M/1M pass.

## 4. Validation

- `cargo check --workspace`: pass. `cargo test --workspace`: pass
  (incl. 3 new `wire-runtime` tests; 0 failures anywhere).
- `git diff --check`: clean.
- Hugepages/governor verified before and after the matrix (57 free,
  `performance` throughout).
