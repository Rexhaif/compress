# compress

`compress` is a Rust compression binary with a static algorithm registry shape
and a from-scratch `.xz`/LZMA2 implementation path.

Current commands:

```text
compress -a lzma2 [options] [file...]
compress xz [options] [file...]
```

Supported options:

```text
-z, --compress
-d, --decompress
-t, --test
-l, --list
-c, --stdout
-k, --keep
-f, --force
-0 ... -9
-e, --extreme
-T, --threads NUM
--block-size SIZE
--check none|crc32|crc64|sha256
--set dict=SIZE
--set lc=NUM
--set lp=NUM
--set pb=NUM
--set check=none|crc32|crc64|sha256
--set mode=fast|normal
--set mf=bt4
--set nice=NUM
--set depth=NUM
```

The encoder writes valid `.xz` streams with an LZMA2 filter and independent XZ
Blocks for parallel operation. The current encoder path emits compressed LZMA2
chunks using an in-tree LZMA range encoder, scalar hash2/hash3/hash4 match
search, repetition matches, normal matches, literals, lazy parsing, and
distance-aware match thresholds. LZMA2 encoding keeps dictionary and probability
state across compressed chunks when the format permits it, uses larger chunks on
compressible data, and falls back to LZMA2 uncompressed packets when a chunk
does not shrink.

This is an interoperability-first implementation. XZ Blocks may contain multiple
LZMA2 chunks and retain cross-chunk match history inside each Block. A true
optimal parser and true binary-tree match finder are still future work. The
decoder implements the `.xz` container, LZMA2 chunks, and raw LZMA range decoding
for ordinary LZMA2 streams.

The core implementation uses only the Rust standard library. Integrity checks
CRC32, CRC64, SHA-256, and `none` are implemented in-tree.

## Examples

```sh
compress xz -c input > input.xz
compress -a lzma2 -T0 --block-size=64M input
compress xz -9 --set mode=normal --set mf=bt4 --set nice=128 -c input > input.xz
compress xz --set mode=optimal -c input > input.xz
compress xz -dc input.xz > input
compress xz -t input.xz
compress xz -l input.xz
```

`mode=normal` is the default speed-oriented LZMA parser. `mode=optimal` enables
the experimental bounded dynamic-programming parser for ratio experiments.

Named-file compression follows `xz`/`gzip` style: successful compression or
decompression removes the input unless `--keep` or `--stdout` is used.

## Benchmarks

Benchmark metadata lives under `bench/`. Build the runner with:

```sh
rustc bench/run.rs -O -o bench/run
```

Then run:

```sh
bench/run target/release/compress path/to/corpus-file
bench/run --tui target/release/compress path/to/corpus-file
```

The runner invokes `compress`, `xz`, and optional installed competitors across
multiple compression levels, verifies round trips where configured, and emits
JSONL rows to stdout by default. Use `--tui` or `--mode tui` for stdin feed
progress, MiB/s throughput, and ranked summary tables.
