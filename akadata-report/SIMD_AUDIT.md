# SIMD implementation and reproducibility audit — addendum

Companion to `BENCHMARKS.md` §7. Documentation-only: **no SIMD source was
changed** in producing this file. Branch `akadata/executor-performance`,
Homer (i9-13900K, AVX2 yes / AVX-512 no).

## 1. Hardware path actually taken on Homer

- `/proc/cpuinfo`: `avx2` on all 32 CPUs; no `avx512*` flag on any CPU.
- Dispatchers read in source: `wire-simd/src/lib.rs:42-62` (`parse_one`),
  `wire-exchange/src/itch.rs:257-281` (`parse_itch_message`),
  `wire-echo/src/bin/v6_benchmark.rs:89-127` (measurement harness).
- Consequence: on Homer the AVX2 branches execute and every AVX-512 branch
  is rejected at runtime. The v6 dashboard prints
  `AVX-512 … N/A / Host CPU (AVX2 Active)`; v7's ITCH row prints a rate but
  never names the path that produced it.
- Binary presence (not execution): 21 `zmm` register mentions and 169
  `vpshufb` mentions in `target/release/v6_benchmark` (`objdump -d`).

## 2. What the labelled code actually vectorizes

AVX2 (`wire-simd/src/avx2.rs`): one `_mm256_loadu_si256`, one
`_mm256_shuffle_epi8`, one `_mm256_storeu_si256` of the shuffled tuple;
`_mm256_extract_epi8/16_unwrap` (`avx2.rs:115-125`) are whole-register
stores to stack arrays plus scalar indexing (store-forwarding roundtrips,
not extracts); checksum loop and all TCP/UDP field decoding scalar;
IPv4-only (anything else returns default).

AVX-512 (`wire-simd/src/avx512.rs:7-15`, `wire-exchange/src/itch.rs:230-237`):
one `_mm512_loadu_si512`, one `_mm512_storeu_si512` to a stack buffer, then
scalar byte/index extraction for the whole parse. No vector compare, blend,
shuffle, or ALU decoding stage exists after the wide load in any of these
functions.

Verdict used in §7: the code uses vector registers, but "fully vectorized
parser/decoder" overstates it — there is effectively no vector ALU decoding
pipeline after the wide load.

Measured Homer inversion (author's own harness,
`akadata-report/samples/v6_normal_sample.txt`):

```text
Scalar Fallback Parser         228.91 Mpps    13.11 cycles / packet
AVX2 Vector Parser (256-bit)    57.99 Mpps    51.73 cycles / packet
```

Not generalised beyond Homer: on this silicon the executable SIMD path is
≈4x slower than scalar. That is a separate, genuine optimization project —
out of scope here by instruction.

## 3. Focused test transcript (scratch, deleted afterwards)

A throwaway integration test (`wire-simd/tests/ipv6_probe_scratch.rs`,
removed after the run; `git status` confirms only documentation changed)
fed identical frames to each path on Homer. IPv4 frame: valid header
(checksum `0x66AE` independently recomputed), `10.0.0.1:12345 →
10.0.0.2:80`. IPv6 frame: same layout as the v6 benchmark's synthetic
frame, `2001:0db8::1:8080 → 2001:0db8::2:443`.

```text
scalar(ipv4): valid=true  tuple=10.0.0.1 → 10.0.0.2 : 12345 → 80, proto 6
avx2(ipv4):   valid=true  tuple=aa:bb:08:00 → … : 8755 → 17493, proto 6
scalar(ipv6): valid=false (zero tuple)
avx2(ipv6):   valid=false (zero tuple)
parse_one(ipv6 on Homer): valid=false (zero tuple)
```

Result: 4 passed, 0 failed. The IPv4 pair is the shuffle-lane proof —
`_mm256_shuffle_epi8` is lane-local (128-bit lanes), but
`ETH_TCP_SHUFFLE_OCTETS` (`wire-simd/src/masks.rs`) puts source indices
26–38 in output positions 0–12 (lane 0), so each wraps mod 16: output
bytes 0–3 read input bytes 10–13 (`aa bb 08 00`, MAC tail + ethertype),
output bytes 8–9 read input bytes 2–3 (`22 33` = 8755), and so on. Both
paths claim `valid=true` with different tuples, and the benchmark measures
time without comparing output. Per instruction this is recorded, not fixed.

## 4. IPv6 handling (correctness issue, intent left open)

Representation: `PackedTuple` (`wire-core/src/types.rs:4-11`) carries
4-byte `src_ip`/`dst_ip` — an IPv4-shaped tuple; its constructor takes
`Ipv4Address`. A 16-byte IPv6 address cannot fit by construction.

Proven by the test above: scalar, AVX2, and dispatched `parse_one` all
return invalid for an IPv6/TCP frame on Homer.

Code reading for the untestable-here path: `parse_one_avx512` alone
accepts ethertype `0x86dd`, walks extension headers, and stores
`buf[22..26]` / `buf[38..42]` into the 4-byte fields (`avx512.rs:99-100`).
Offset check against the frame layout (14-byte Ethernet + 40-byte IPv6
header; src address bytes 22..38, dst 38..54): those slices are the
**leading /32 of each address** (e.g. `20 01 0d b8` for `2001:0db8::/32`),
deterministic but lossy — any two hosts sharing a /32 alias to the same
tuple.

What is NOT claimed: whether that truncation is deliberate aliasing or
simply wrong offsets. Intent cannot be inferred from the code, and the
path cannot execute on Homer to observe it in action. What IS established:
identical IPv6 input is invalid on Homer and valid-with-truncation on an
AVX-512 machine — dispatch-dependent validity for the same bytes on the
wire.

## 5. Feature-gate defect (static finding)

`parse_one_avx512` and `parse_itch_add_order_avx512` carry
`#[target_feature(enable = "avx512f", enable = "avx512bw",
enable = "avx512dq", enable = "avx512vl")]`, but every call site gates
only on `is_x86_feature_detected!("avx512f")`. The gate therefore does not
prove all features required by the compiled function are available. On a
CPU with F but without BW/DQ/VL this is a latent illegal-instruction /
UB hazard (the current bodies only emit F instructions, so it sleeps —
until someone adds a BW/DQ intrinsic inside the same gate).

## 6. Author's machine: what can and cannot be established

The upstream README states "AVX-512 / AVX2 Enabled", but does not publish
the CPU model, lscpu flags, CPUID output, or selected runtime dispatch
path. Therefore AKADATA cannot independently verify that the published
AVX-512-labelled figures were produced on an AVX-512 path. No fabrication
is alleged — the evidence required to reproduce the claim was simply not
published.

Minimum reproducibility bar for any SIMD benchmark row: CPU model, AVX2
yes/no, AVX512F/BW/DQ/VL yes/no each, and the parser path actually taken.
