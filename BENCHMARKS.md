# Wire Executor Benchmarks: What Was Wrong, What We Fixed, What It Measures Now

Branch: `akadata/executor-performance` (fork `akadata/wire`).
Raw per-run data: `akadata-report/` (`REPORT.md`, `PERFORMANCE.md`,
`perf_*.csv`, `samples/`). Upstream README claims are left untouched above;
everything below is AKADATA-measured on Homer.

**TL;DR:** the old executor benchmark divided 1,000,000 by elapsed time while
the queue only holds 4,096 — counting ~244x phantom work. We fixed the
accounting (all runs must prove `attempted == accepted == completed ==
1,000,000` or exit non-zero), then made the executor genuinely faster
(~1.7x: ~15.5 → ~26.5 Mops/s single-threaded) by collapsing 15,625
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
   syscalls per 1M tasks: 976×16 + 9 (final 576-task batch) = 15,625
   TLS guard pairs:       same 15,625 (one per tick)

 AFTER (per 1024-task batch: 1 drain)
   spawn 1024 ─▶ drain_all ─▶ done (exact count checked)
   syscalls per 1M tasks:  1 × 977  = 977      (−93.75%)
   TLS guard pairs:        1 × 977  = 977      (−93.75%)
```

Rough attribution (labeled estimate, not a measurement): 14,648 eliminated
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

---

## 7. SIMD implementation and reproducibility audit

The upstream README states, under "Hardware Test Bench Context":

> CPU Host: x86_64 Architecture (AVX-512 / AVX2 Enabled)

Three separate questions follow. They are kept separate on purpose.

### 7.1 What hardware path actually ran on Homer?

Homer is an i9-13900K: `avx2` present on all 32 CPUs, **no `avx512*` flag
anywhere** in `/proc/cpuinfo`. The dispatch gates
(`wire-simd/src/lib.rs:46,52`, `wire-exchange/src/itch.rs:261,269`,
`wire-echo/src/bin/v6_benchmark.rs:89,104`) therefore resolve as follows:

- AVX2: **supported and executed.** The v6 benchmark calls
  `parse_one_avx2` directly and measures it; the binary contains real VEX
  code (`vpshufb` ×169 in `target/release/v6_benchmark`).
- AVX-512: **unsupported; the benchmark prints N/A.** Both v6 AVX-512 rows
  report `N/A / Host CPU (AVX2 Active)`. The code is compiled in (21 `zmm`
  mentions in the binary) but the dispatcher rejects it, so no AVX-512
  instruction from this codebase executes on Homer.

Measured Homer inversion (`akadata-report/samples/v6_normal_sample.txt`):

```text
 Scalar Fallback Parser         228.91 Mpps    13.11 cycles / packet
 AVX2 Vector Parser (256-bit)    57.99 Mpps    51.73 cycles / packet
```

Therefore on Homer: **the AVX2 path is ≈4x slower than the scalar path.**
That result is not generalised to all CPUs — it is a Homer measurement on
this silicon — but it is the most interesting finding here: not merely
"Homer lacks AVX-512", but the SIMD path we *can* execute loses badly to
scalar, which says there is another genuine optimization project sitting
there (see §7.2 for why).

### 7.2 What does the code labelled AVX2/AVX-512 actually vectorize?

AVX2 (`wire-simd/src/avx2.rs`):

- vector load (`_mm256_loadu_si256`),
- vector shuffle (`_mm256_shuffle_epi8`),
- store/extract back through memory (the `_mm256_extract_*_unwrap`
  helpers at `avx2.rs:115-125` are full-register stores to the stack
  followed by scalar array reads — store-forwarding roundtrips, not
  extracts),
- predominantly scalar field decoding afterwards (checksum loop, TCP
  fields, length checks all scalar).

AVX-512 (`wire-simd/src/avx512.rs`, `wire-exchange/src/itch.rs:230-237`):

- one 512-bit load (`_mm512_loadu_si512`),
- one 512-bit store to local storage (`_mm512_storeu_si512`),
- scalar byte/index extraction afterwards (the entire parse body).

Stated plainly: the current implementation uses vector registers, but
calling it a fully vectorized parser/decoder overstates what the code is
doing. **There is effectively no vector ALU decoding pipeline after the
wide load.** The v7 ITCH "vector parser" is the same shape — one wide
load, one wide store, scalar field extraction.

### 7.3 What can/cannot be established about the author's benchmark machine?

The upstream README states "AVX-512 / AVX2 Enabled", but does not publish
the CPU model, lscpu flags, CPUID output, or selected runtime dispatch
path. Therefore AKADATA cannot independently verify that the published
AVX-512-labelled figures were produced on an AVX-512 path. Note also that
v7's ITCH benchmark calls the dynamic dispatcher (`itch.rs:257-281`) and
does not report whether AVX-512, AVX2 or scalar actually handled the
measured workload. No accusation is made that results were fabricated; the
point is strictly reproducibility: the evidence needed to reproduce the
claim was not published.

Reproducibility recommendation — every SIMD benchmark row should print:

- CPU model
- AVX2 yes/no
- AVX512F/BW/DQ/VL yes/no (each level separately)
- selected parser path (the branch actually taken)

Then there is no ambiguity.

### 7.4 Verdict table

| Claim                         | Homer evidence             | Verdict                  |
| :---                          | :---                       | :---                     |
| AVX2 enabled                  | yes                        | proven                   |
| AVX2 faster                   | no                         | ~4x slower here          |
| AVX-512 enabled on Homer      | no                         | impossible               |
| AVX-512 code compiled         | yes                        | proven                   |
| AVX-512 executed on Homer     | no                         | dispatch rejects         |
| upstream AVX-512 host         | undocumented               | unverified               |
| fully vectorized decoder      | no                         | wide load + scalar extraction |

### 7.5 Correctness findings (proven by focused test, code unchanged)

Two dispatch-dependent divergences were proven with a throwaway integration
test (same input frames, all three paths, run on Homer; the scratch test
was deleted afterwards — **no SIMD code was changed**):

1. **AVX2 shuffle-lane divergence (runs on Homer).** `_mm256_shuffle_epi8`
   operates per 128-bit lane, but `ETH_TCP_SHUFFLE_OCTETS`
   (`wire-simd/src/masks.rs`) places indices 26–38 in output positions
   0–12, i.e. in lane 0, so they wrap to input bytes 10–22. Measured on one
   valid IPv4 frame: scalar reports the correct tuple
   (`10.0.0.1 → 10.0.0.2 : 12345 → 80`, valid), while AVX2 reports
   (`aa:bb:08:00 → … : 8755 → 17493`, valid) — MAC bytes as IPs/ports.
   Both claim `valid=true`; the benchmark times the path without ever
   comparing results. Full transcript: `akadata-report/SIMD_AUDIT.md` §3.
2. **IPv6 validity depends on the CPU path.** `PackedTuple` holds 4-byte
   address fields (an IPv4-shaped representation), and the scalar and AVX2
   paths reject IPv6 ethertype outright (proven: all return invalid on an
   IPv6/TCP frame). The AVX-512 path alone accepts IPv6 and stores
   `buf[22..26]` / `buf[38..42]` — the leading /32 of each 16-byte address
   — into those 4-byte fields (established by code reading; the path
   cannot execute on Homer). Whether that truncation is intentional
   aliasing or simply wrong offsets is **not established: intent is not
   inferred from the code.** What is established is the observable fact:
   identical IPv6 input is invalid on Homer and valid-with-truncation on an
   AVX-512 machine. Full detail: `akadata-report/SIMD_AUDIT.md` §4.
