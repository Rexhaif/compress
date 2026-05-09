# compress

`compress` is a Rust compression binary with a static algorithm registry shape
and from-scratch `.xz`/LZMA2 and `.bz2` implementation paths.

Current commands:

```text
compress -a lzma2 [options] [file...]
compress xz [options] [file...]
compress bzip2 [options] [file...]
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

Multi-threaded compression (`-T2`, `-T4`, `-T0`, and so on) compresses
independent XZ Blocks in parallel and preserves stream order in the output.
When no `--block-size` is provided, multi-threaded mode uses smaller automatic
Blocks than single-threaded mode so enough work is available for multiple cores.

This is an interoperability-first implementation. XZ Blocks may contain multiple
LZMA2 chunks and retain cross-chunk match history inside each Block. A true
optimal parser and true binary-tree match finder are still future work. The
decoder implements the `.xz` container, LZMA2 chunks, and raw LZMA range decoding
for ordinary LZMA2 streams.

The bzip2 path writes standard `.bz2` streams with in-repo RLE, cyclic BWT,
move-to-front, Huffman coding, and CRC handling. Multi-threaded bzip2
compression splits input into independent bzip2 blocks, compresses them in
parallel, and writes one ordered stream compatible with `bzip2` and `pbzip2`.
The decoder reads ordinary single-stream `.bz2` files and concatenated
pbzip2-style streams, with independent block decoding dispatched in parallel
when block markers can be validated.

The core codec implementation is Rust. CRC32 and CRC64 use fast Rust
implementations; SHA-256 and `none` are implemented in-tree.

## Examples

```sh
compress xz -c input > input.xz
compress -a lzma2 -T0 --block-size=64M input
compress xz -9 --set mode=normal --set mf=bt4 --set nice=128 -c input > input.xz
compress xz --set mode=optimal -c input > input.xz
compress xz -dc input.xz > input
compress xz -t input.xz
compress xz -l input.xz
compress bzip2 -9 -T0 -c input > input.bz2
compress bzip2 -dc input.bz2 > input
```

`mode=normal` is the default speed-oriented LZMA parser. `mode=optimal` enables
the experimental bounded dynamic-programming parser for ratio experiments.

Named-file compression follows `xz`/`gzip` style: successful compression or
decompression removes the input unless `--keep` or `--stdout` is used.

## Benchmarks

Benchmark metadata lives under `bench/`. Build the runner with:

```sh
cargo build --release --bin bench-run
cp target/release/bench-run bench/run
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
