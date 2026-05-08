# XZ Utils v5.8.3 Encoder Notes

Reference source:

- Directory: `reference/xz-utils-v5.8.3`
- Commit: `4b73f2e`
- Upstream tag: `v5.8.3`

This tree is for reading only. Do not vendor or link against it from the Rust
encoder path. The project requirement still stands: built-in LZMA/LZMA2 must be
implemented from scratch in pure Rust.

## High-Value Files

- `src/liblzma/lzma/lzma_encoder_optimum_normal.c`
- `src/liblzma/lzma/lzma_encoder_optimum_fast.c`
- `src/liblzma/lzma/lzma_encoder.c`
- `src/liblzma/lzma/lzma_encoder_private.h`
- `src/liblzma/lzma/lzma2_encoder.c`
- `src/liblzma/lz/lz_encoder_mf.c`
- `src/liblzma/lz/lz_encoder_hash.h`
- `src/liblzma/lz/lz_encoder.c`
- `src/liblzma/rangecoder/price.h`
- `src/liblzma/common/stream_encoder_mt.c`
- `src/liblzma/common/block_buffer_encoder.c`

## Main Gaps In Our Encoder

1. Our `Bt4` is a hash-chain-style finder, not liblzma's real binary tree.
   liblzma stores two children per cyclic slot and updates the tree while
   searching. It tracks known common-prefix lengths on both tree branches to
   reduce repeated byte comparisons.

2. Our parser is greedy plus lazy heuristics. liblzma's normal mode uses a
   bounded dynamic-programming parser over `OPTS = 4096` entries. Each node
   stores accumulated price, LZMA state, reps, predecessor, and extra flags for
   compound paths.

3. Our match choice uses static thresholds. liblzma prices literal, rep, match
   length, distance slot, distance footer, direct bits, and align bits against
   current probability models.

4. Our match finder returns one best normal match. liblzma returns a list of
   strictly improving length-distance pairs, then the parser tests each useful
   length.

5. Our parser does not model compound paths. liblzma explicitly tries
   `literal + rep0`, `rep + literal + rep0`, and `match + literal + rep0`.

## Parser And Price Model

The normal parser entry is `lzma_lzma_optimum_normal()` in
`lzma_encoder_optimum_normal.c`.

Useful pieces to port:

- `get_literal_price()`: prices plain and matched literals using the same
  literal subcoder structure used by the range encoder.
- `get_short_rep_price()`: prices one-byte rep0.
- `get_pure_rep_price()`: prices rep selector bits before length.
- `get_rep_price()`: rep selector plus rep length.
- `get_dist_len_price()`: normal match length plus distance price.
- `fill_dist_prices()`: caches distance slot and full-distance prices.
- `fill_align_prices()`: caches four-bit align footer prices.
- `length_update_prices()` in `lzma_encoder.c`: caches length prices per
  position state.

Important thresholds:

- Refresh distance prices when `match_price_count >= 128`.
- Refresh align prices when `align_price_count >= 16`.
- Refresh only when no read-ahead is pending.
- Initialize counters high enough to force the first price-table fill.

Port shape:

1. Add range price helpers next to `RangeEncoder`.
2. Add cached price arrays to `LzmaEncoder`.
3. Add an `Optimal` node type with fields equivalent to liblzma's
   `lzma_optimal`.
4. Replace `choose_decision()` and `lazy_decision()` in normal mode with bounded
   DP.
5. Keep the current heuristic parser for fast mode.

## Fast Parser Heuristics

`lzma_encoder_optimum_fast.c` has good low-risk rules:

- `change_pair(small_dist, big_dist)`: a farther match needs extra length to
  beat a nearer one.
- Reject length-2 normal matches with distance `>= 0x80`.
- Prefer reps when they are one to three bytes shorter than a far normal match.
- Do one-byte lookahead before committing a normal match.
- After lookahead, check whether any rep at the next byte matches almost as long
  as the current normal match.

We already ported some of this, but not exactly.

## Match Finder

The real BT implementation is in `lz_encoder_mf.c`:

- `bt_find_func()` performs tree search and insertion together.
- `bt_skip_func()` performs insertion without emitting matches.
- `lzma_mf_bt4_find()` probes 2-byte and 3-byte hashes first, then searches the
  4-byte tree.
- `lzma_mf_find()` extends matches that reached `nice_len` up to
  `match_len_max`.

Memory layout:

- `hash[]`: newest position per hash bucket.
- `son[]`: cyclic child storage.
- HC uses one `u32` per cyclic slot.
- BT uses two `u32`s per cyclic slot.
- `cyclic_size = dict_size + 1`.

Default depth:

- BT: `16 + nice_len / 2`
- HC: `4 + nice_len / 4`

Reference hash layout:

- `HASH_2_SIZE = 1 << 10`
- `HASH_3_SIZE = 1 << 16`
- `HASH_4_SIZE = 1 << 20`
- For 3/4-byte finders, hash size is derived from dictionary size and includes
  fixed 2-byte and 3-byte hash sections.

Implementation note: Our recent exact 2-byte hash table diverges from liblzma's
small fixed 2-byte table. That helped local heuristics but should be re-tested
after a true BT and DP parser land.

## LZMA2 Chunking

Reference constants:

- Compressed payload max: `64 KiB`
- LZMA uncompressed chunk max: `2 MiB`
- Uncompressed LZMA2 packet max: `64 KiB`
- Header max: 6 bytes

The LZMA2 encoder state machine has these phases:

- init
- LZMA encode
- LZMA copy
- uncompressed header
- uncompressed copy

Important behavior:

- It keeps dictionary and probability state across compressed chunks when legal.
- It emits `0x80` continuation chunks when no dictionary/properties/state reset
  is needed.
- It emits `0xA0` when LZMA state reset is needed.
- It emits `0xC0` when new properties are needed without dictionary reset.
- It emits `0xE0` when dictionary reset and properties are needed.
- If a compressed chunk does not shrink, it emits uncompressed LZMA2 packets and
  marks LZMA state reset as needed for the next compressed chunk.

Optimization trick:

- liblzma writes compressed data after the maximum header space, then starts
  copying at byte 0 or 1 depending on actual header size. This avoids moving the
  compressed bytes after deciding whether properties are present.

## Block And Threading Behavior

For multithreaded encoding, liblzma splits input into independent `.xz` Blocks.
The default block size for LZMA2 is:

```text
max(3 * dict_size, 1 MiB)
```

Each worker owns:

- input buffer
- copied filter chain
- block encoder
- output buffer

Workers reserve maximum Block Header space, compress the block, then write the
final Block Header once actual sizes are known.

If worker output fills its bounded buffer before finishing, liblzma treats the
Block as incompressible and re-encodes it as uncompressed LZMA2 chunks.

## Presets

Preset dictionary sizes match our current mapping:

- 0: 256 KiB
- 1: 1 MiB
- 2: 2 MiB
- 3: 4 MiB
- 4: 4 MiB
- 5: 8 MiB
- 6: 8 MiB
- 7: 16 MiB
- 8: 32 MiB
- 9: 64 MiB

Reference mode mapping:

- Levels 0..3: fast mode, HC match finders, explicit depths `4, 8, 24, 48`.
- Levels 4..9: normal mode, BT4, automatic depth.
- Extreme: normal mode, BT4; usually `nice_len = 273` and `depth = 512`.

Our presets currently differ in fast-mode usage and depth policy. After the
real parser and match finder are closer, revisit preset compatibility.

## Suggested Implementation Order

1. Add range-coder price helpers and cached length/distance/align prices.
2. Convert `MatchFinderBt4` to return a small match list instead of only one
   best candidate.
3. Implement a bounded normal-mode optimal parser using current hash chain
   finder, to get price-model benefits before the BT rewrite.
4. Add compound `literal + rep0` paths to the DP.
5. Replace hash-chain `Bt4` with true cyclic binary tree storage.
6. Re-align presets with liblzma once parser/finder behavior is closer.
7. Re-run enwik8 and random/incompressible interop benchmarks after each step.
