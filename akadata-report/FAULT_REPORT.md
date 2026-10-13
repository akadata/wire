# FAULT REPORT — Wire financial-fitness review (read-only audit)

Scope: `wasif-exe/wire` as forked at `0e40e85`, plus AKADATA's
`akadata/executor-performance` branch. Method: source read-through only —
**no code was altered** for this report (verified: `git status` shows
documentation additions alone). Every finding below cites the file and
line where the behaviour lives. Severity is judged against one question:
what happens if real money flows through this code?

Related: `BENCHMARKS.md` (benchmark honesty), `SIMD_AUDIT.md` (parser
correctness proofs), `PERFORMANCE.md` (executor measurements),
`C_VS_RUST.md` (abstraction cost).

## F1 — Risk filter fails open on unknown symbols [CRITICAL]

`wire-risk/src/filter.rs:82-89`: when no limits are configured for an
order's symbol, `check_order` counts a `Pass` and returns it. The order the
risk system knows nothing about is precisely the order it waves through —
including size, price, and notional checks. A typo'd or newly listed symbol
bypasses every limit in the file. Production risk systems default to deny;
this one defaults to allow. Additionally, every limit is disabled by a zero
value (`max_order_size`, `min_price`, `max_price`, `max_notional` each gated
by `> 0`, lines 95/100/105/111), so a zero in configuration silently
removes a control instead of blocking flow.

Correction recorded honestly: an earlier draft of this audit alleged that
`(shares as u64).wrapping_mul(price as u64)` (line 110) could silently
overflow. It cannot — both operands are `u32`, and (2³²−1)² < u64::MAX, so
the widened product always fits. `wrapping_mul` is sloppy intent that
happens to be mathematically inert. The finding is withdrawn; the fail-open
design above it stands and is the actual defect.

## F2 — Order-book indexing collides at real volumes [CRITICAL]

`wire-exchange/src/lob.rs`: order state is keyed by
`order_id & 65535` (lines 88, 118, 151, 181, 208) and price levels by
`price & 65535` (`slot_idx`, lines 65–67). Real ITCH order reference
numbers are `u64` running into the hundreds of millions per session, so
slot reuse is a certainty within minutes, not a theory: a new Add
silently overwrites a resting order's price/side/shares while the old
price level keeps its shares forever (leaked phantom depth), and later
executions against that ID debit the wrong order's level. Prices 65,536
apart merge into one level's shares and count. There is no collision
detection, no error, no log — state just becomes wrong.

## F3 — Best bid/offer goes stale and never recovers [CRITICAL]

`lob.rs` lines 134–136, 144–146, 164–166, 174–176, 194–196, 204–206: when
the touch level fully fills, cancels, deletes, or is replaced away, the
code updates `best_*_shares` from the (now empty or aliased) level but
never moves `best_*_price` to the next-best active level. The book keeps
quoting a dead price — with zero shares, or worse, with shares merged in
from an aliased price — until something better-or-equal arrives. Any
strategy quoting off this BBO is adversely selected: filled at prices that
do not exist. Interacting defects: Replace keeps the *old* side and
ignores `event.side` (line 211); over-fills are hidden by
`saturating_sub` with no error; `Trade`/`BboUpdate` fall into `_ => {}`
(line 238) and vanish. The in-crate test covers add-bid/add-ask only, so
the suite certifies none of this.

## F4 — Running parser emits wrong tuples marked valid [HIGH]

Proven by focused test (transcript in `SIMD_AUDIT.md` §3; scratch test
deleted afterwards): on AVX2 machines — the path that actually executes —
`parse_one_avx2` returns e.g. `aa:bb:08:00 : 8755 → 17493` with
`valid=true` for a frame the scalar path parses correctly as
`10.0.0.1:12345 → 10.0.0.2:80`. Cause: `ETH_TCP_SHUFFLE_OCTETS`
(`wire-simd/src/masks.rs`) places source indices 26–38 in output positions
0–12, but `_mm256_shuffle_epi8` is lane-local to 128 bits, so every index
≥16 wraps mod 16 into the low lane. The benchmark times this path without
ever comparing output, so the harness cannot see it.

## F5 — No sequence tracking anywhere in the market-data path [HIGH]

ITCH `tracking_number` bytes are skipped by the scalar parser and the
numbers live only as unread struct fields; no gap detection, no
monotonicity check on timestamps, no resync path exists in the codebase
(verified by workspace-wide search). One dropped multicast packet diverges
the book from the exchange permanently, silently, with no alert. For
exchange feeds this alone disqualifies production use.

## F6 — WAL recovery voids ordering and loses data silently [HIGH]

`wire-store/src/wal.rs`: `seq` restarts at 0 on every `open()` (line 59),
so crash → reopen → continue appends duplicate sequence numbers over the
existing log, destroying dedup and ordering. Replay stops silently at the
first torn tail with no error distinguishing clean-EOF zero-pad from
corruption (lines 204–226); `sync()` drops short-write remainders without
error (lines 155–167); replay loads the entire file into RAM
(`read_to_end`, line 195 — a multi-GB log OOMs the reader); the CRC covers
op+key+val but not seq/lengths/magic. O_DIRECT, CRC32C, and fdatasync are
genuinely present — the ingredients are real, the guarantees are not.

## F7 — IPv6 validity depends on which CPU you own [MEDIUM]

`PackedTuple` holds 4-byte addresses (IPv4-shaped by construction).
Scalar and AVX2 paths reject IPv6 ethertype (proven); the AVX-512 path
alone accepts it and stores the leading /32 of each 16-byte address into
the 4-byte fields (`wire-simd/src/avx512.rs:99-100`, established by
offset-checked reading — the path cannot execute on Homer). Identical wire
bytes are therefore invalid on one machine and valid-with-truncation on
another. Whether the truncation is deliberate aliasing or wrong offsets is
not established and not inferred. Full detail: `SIMD_AUDIT.md` §4.

## F8 — AVX-512 feature gate does not cover its own requirements [MEDIUM]

`#[target_feature(enable = "avx512f", "avx512bw", "avx512dq",
"avx512vl")]` on functions gated at runtime by `avx512f` alone. On F-only
silicon this is a latent illegal-instruction hazard (dormant only because
current bodies emit F instructions). Full detail: `SIMD_AUDIT.md` §5.

---

## Nose-shrinking statement of facts (for Mr Long Nose) 👃📏

Read aloud at the workbench. Each line is checkable above; anything not
checkable was left out:

1. The benchmark counted a million tasks through a four-thousand slot —
   the nose grew ~244x, and the accounting fix has shrunk it back to
   exactly 1,000,000/1,000,000/1,000,000, every run, or the binary fails.
2. The executor is now genuinely ~1.7x faster — measured, like-for-like,
   on a loud shared workstation, not a quiet lab. That part grew shorter
   too, and it stays.
3. The "vector" parsers were a wide load in a vector costume: AVX-512
   never ran here by the author's own gate, and the AVX2 path that runs
   measures ~4x slower than scalar — and mis-reports MAC bytes as IPs.
4. The risk filter waves through whatever it hasn't been told about; the
   book forgets orders by design (`& 65535`), merges prices by design
   (`& 65535`), and quotes dead prices indefinitely. None of that is
   tuning. It is wrongness, at low latency.
5. The sequence numbers nobody checks, the WAL that restarts counting
   after every crash, and the IPv6 that exists only on some CPUs complete
   the picture: a brilliant engineering playground that must not touch
   money until its correctness core is rewritten.
6. One allegation from our side is hereby withdrawn with the nose's
   compliments: `wrapping_mul` on two widened `u32`s cannot overflow, and
   saying so was our error, corrected in F1 above. Facts cut both ways —
   that is the whole point of an audit.

Present state, honestly summarised: benchmarks fixed and faster, parsers
proven faulty, book and risk unfit for funds. The nose is now regulation
length. Keep it that way. 🐽
