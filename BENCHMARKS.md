# Wire Executor Benchmarks: What Was Wrong, What We Fixed, What It Measures Now

Branch: `akadata/executor-performance` (fork `akadata/wire`).
Raw per-run data: `akadata-report/` (`REPORT.md`, `PERFORMANCE.md`,
`perf_*.csv`, `samples/`). Upstream README claims are left untouched above;
everything below is AKADATA-measured on Homer.

**TL;DR:** the old executor benchmark divided 1,000,000 by elapsed time while
the queue only holds 4,096 — counting ~244x phantom work. We fixed the
accounting (all runs must prove `attempted == accepted == completed ==
1,000,000` or exit non-zero), then made the executor genuinely faster
(~1.7x: ~15.5 → ~26.5 Mops/s single-threaded) by collapsing 15,632
`io_uring_enter` syscalls per 1M tasks down to 977, plus a real
wake-during-poll correctness fix. All of it measured on a busy shared
workstation, baseline and improved run back-to-back under the same load.

---

## 1. The accounting bug, in one picture

The executor queue holds `MAX_TASKS = 4096` futures. The old `v6_benchmark`
spawn loop ignored `spawn()` failures and timed 1,000,000 attempts anyway:

```text
What the old code COUNTED (numerator = 1,000,000):

 |████████████████████████████████████████████████████████████| 1,000,000 attempted
 |██| 4,096 actually accepted into the 4096-slot queue
  ^^^
  The other ~995,904 spawns returned None and were silently dropped,
  yet divided into the headline rate: "202.15 Mops/s, 4.95 ns/task".

What the fixed code counts (numerator = completed work only):

 attempted  1,000,000  |████████████████████████████████████████| 100%
 accepted   1,000,000  |████████████████████████████████████████| 100%
 completed  1,000,000  |████████████████████████████████████████| 100%
                        (every run prints this triple; any mismatch
                         zeroes the metrics and exits non-zero)
```

Old-vs-honest scale (Mops/s, higher is better; the first bar is phantom work):

```text
 old headline claim   202.15  |████████████████████████████████████████ ...truncated (≈202 chars at 1 char/Mop)
 honest baseline       15.4   |███████████████
 honest improved       26.4   |██████████████████████████
```

## 2. Why the improved code is faster (same work, less overhead)

Per 1,000,000 `async {}` tasks, single `LocalExecutor`, `BATCH = 1024`:

```text
 BEFORE (per 1024-task batch: ~16 ticks of 64)
   spawn 1024 ─▶ tick ─▶ tick ─▶ … ─▶ tick (16×)
                        │                each tick = TLS guard pair + io_uring_enter syscall
   syscalls per 1M tasks: 16 × 977  = 15,632
   TLS guard pairs:       16 × 977  = 15,632

 AFTER (per 1024-task batch: 1 drain)
   spawn 1024 ─▶ drain_all ─▶ done (exact count checked)
   syscalls per 1M tasks:  1 × 977  = 977      (−93.75%)
   TLS guard pairs:        1 × 977  = 977      (−93.75%)
```

Rough attribution (labeled estimate, not a measurement): 14,655 eliminated
syscalls × ~1.9 µs per `io_uring_enter` on this hardened kernel
(IBRS/IBPB mitigations) ≈ ~28 ms — essentially the entire ~65 ms → ~37 ms
per-1M delta. The benchmark still spawns, polls, and completes 1,000,000
real futures; the removed work was pure overhead, so the gain is genuine
executor improvement, not a measurement change.

Also fixed along the way (correctness, not speed): a future that called
`wake_by_ref()` inside its own `poll` had the wake silently dropped and
parked forever. New `is_polling` / `woke_while_polling` flags re-queue it
instead — covered by a new unit test. And `Task<F>` now stores the future
inline (one fewer heap allocation per spawn for real futures).

## 3. Like-for-like results (all runs valid 1M/1M/1M, serial, back-to-back)

Mean Mops/s across 10 runs each (higher is better):

```text
 normal release, 57 hugepages:
   v6  base  15.40  |███████████████
   v6  new   26.03  |██████████████████████████                     1.69x
   v7  base  15.91  |████████████████
   v7  new   27.41  |███████████████████████████                    1.72x

 native (-C target-cpu=native), 57 hugepages:
   v6  base  15.68  |████████████████
   v6  new   26.01  |██████████████████████████                     1.66x
   v7  base  15.39  |███████████████
   v7  new   26.37  |██████████████████████████                     1.71x

 normal release, 324 hugepages:
   v6  base  15.04  |███████████████
   v6  new   25.90  |██████████████████████████                     1.72x
   v7  base  15.53  |████████████████
   v7  new   26.62  |███████████████████████████                    1.71x

 native, 324 hugepages:
   v6  base  15.64  |████████████████
   v6  new   25.75  |██████████████████████████                     1.65x
   v7  base  15.67  |████████████████
   v7  new   27.32  |███████████████████████████                    1.74x
```

Latency per task (mean ns, lower is better): baseline ~63–67 ns → improved
~37–39 ns, both binaries, both build modes, both hugepage states.

Two findings that matter more than the speedup:

1. **Hugepages don't move this benchmark.** Baseline at 57 vs 324 pages is
   identical within noise (15.40 → 15.04, 15.91 → 15.53, …). The executor
   test is pure heap and consumes no hugetlb — we re-ran the whole matrix
   at 324 pages for like-for-like rigor, not because it helps here.
2. **`target-cpu=native` barely moves it either** (base 15.40 → 15.68).
   The hot loop is pointer-chasing and syscalls, not vectorizable math —
   exactly what you'd expect once you look at the code.

## 4. The machine: Homer was never quiet

No dedicated test bench, no stripped-down OS — a shared 7-day-uptime
workstation, measured as-is:

```text
 Homer (homertest bench? no — daily driver)
   CPU:  13th Gen i9-13900K, 32 CPUs, governor 'performance' (verified)
   RAM:  125 GiB total, ~108-110 GiB used, ~14-17 GiB available, 239 GiB swap
   Load: usually ~5-7 with 29 users; spiked to 38 mid-retest
         (cintpos -slow @100%, a mame arcade session, Firefox,
          other opencode agents — plus the resident Egg qemu VM:
          ~112% CPU / ~63 GiB, which we never touched)
   Huge: requested 1024 → granted 57; after 2 VMs stopped: requested
         2048 → 292, then drop_caches + requested 4096 → 324 (648 MB).
         Fragmentation ceiling with a 63 GiB VM pinned; available RAM
         never dropped below 14 GiB. No OOM, Egg stayed running.
```

That load spike visibly polluted one native set (bimodal 13–24 Mops). It was
discarded, disclosed in `PERFORMANCE.md`, and re-run after load decayed —
the re-runs reproduce the same ~1.7x gap. Every baseline/new pair ran
back-to-back under the same load, which is what keeps the comparison fair
on a machine like this.

## 5. What we do NOT claim

- The executor test measures `async {}` spawn+poll+completion on one thread.
  It says nothing about QUIC, NVMe, SmartNICs, or full-stack ingestion —
  those harness sections were left as-is and their labels deserve the same
  scepticism as the original 4096 review.
- `v7_benchmark` has a pre-existing intermittent `thread 'main' has
  overflowed its stack` abort (~3 in ~115 runs, both code versions, both
  build modes, always before any benchmark section). Documented, not fixed
  here; `v6_benchmark` never crashed in 80+ runs.
- Homer numbers are Homer numbers (i9-13900K, rustc 1.99). The claim is the
  *ratio* under identical conditions, not a universal constant.

---

## 6. Pure-C port: pricing the abstraction (`c-executor/`)

Same architecture in C99-or-so C17 — 4096 slots, free-list stack, circular
run queue, heap-allocated task objects, indirect dispatch call, 1M no-op
tasks in batches of 1024, identical fail-closed `EXECUTOR` contract
(`c-executor/task_bench.c`, built with plain `make`). Dispatch verified
real: an instrumented scratch build fired exactly 1,000,000 callbacks
(no dead-code elimination).

```text
 gcc -O3             81.80 Mops/s mean (79.74–83.89)  | 12.23 ns/task
 gcc -O3 -march=native  79.03 Mops/s mean (73.62–85.58)  | 12.70 ns/task
 improved Rust (v6)  25.90 Mops/s mean                | 38.70 ns/task

 Rust ─▶ |██████████████████████████  25.90
 C    ─▶ |██████████████████████████████████████████████████████████████████████████████████  81.80
```

So yes, on this workload: **lean C dispatches empty tasks ~3.2x faster
than the improved Rust executor.** `-march=native` buys C nothing here
(slightly worse median, more variance) — there is nothing to vectorize,
same lesson as §3. Raw table: `akadata-report/perf_c.csv`.

What this does and doesn't prove. The C port deliberately lacks futures,
wakers, the Pending path, the thread-local guard, and the io_uring submit —
the asymmetries are listed in the source header. The ~26 ns/task gap is
*constant framework overhead per dispatch*, and constant overheads vanish
into real work: for illustration, a task body costing 1 µs would show
~1012 ns in C vs ~1039 ns in Rust — under 3% apart. The honest headline is
narrower than "C beats Rust": Rust's async machinery (waker construction,
`Context`, dynamic dispatch, queue bookkeeping, pin guarantees, wake
handling) costs roughly two dozen nanoseconds per empty task versus a bare
function-pointer loop — while buying memory safety, a real Pending/wake
model, and fearless refactoring that the C loop simply doesn't have. Pick
the tool with eyes open: for sub-100 ns dispatch loops, C still rules;
for everything else, know what the ~26 ns buys you.

Methodology detail, raw tables, and sample outputs: `akadata-report/`.
Standalone write-up with SVG graphs: `akadata-report/C_VS_RUST.md`.
