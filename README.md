# mvdz

**mvdz** is a lossless compressor and file format for QuakeWorld MVD demos
(`.mvd`). An `.mvdz` file is about a fifth of the raw demo and a third of
the demo's gzip, decodes back to the exact original bytes (checked by
CRC-32), and can be read in order from a pipe as it arrives. Its 60-byte
header gives the demo's size, duration and identity (the SHA-256 of the
raw demo) without decoding.

It comes as a command-line tool and a Rust library, and builds for
WebAssembly. `mvdz_go/` is a second implementation, in Go: its encoder writes
the same bytes as the Rust one. The format is specified in [FORMAT.md](FORMAT.md), precisely
enough to write an independent decoder, and `vectors/` holds valid
and hostile test files. The work now is the specification and these two
implementations; C and other languages follow once the format is settled.

**Status: 1.0.0-rc.4, a release candidate for comments.** The file format
may still change before 1.0: files written by a release candidate are not
guaranteed to be readable by 1.0, so keep the original `.mvd` files for
now. The format has no extension points: a reader refuses any version
but its own.

How it works: the demo is cut into segments of about 256 KiB, and each is
walked command by command (`svc_*` layouts from mvdsv and ezQuake). Every
field goes to its own stream; numeric fields (player and entity origins,
view angles, sound and temp-entity coordinates) are coded as residuals
against predictors. Structural and numeric streams are coded with static
rANS under per-context models stored once per file; every other byte is
coded as LZ77 tokens under the same models. Anything that does not parse is
stored as it is, so damaged and truncated demos round-trip too.

## What reads .mvdz today?

The `mvdz` command-line tool, the Rust library and the WebAssembly build.
**No QuakeWorld client plays `.mvdz` files yet**: to watch one, decompress
it to `.mvd` first (`mvdz decompress game.mvdz game.mvd`).

## Install and build

**Linux binaries.** Each release on GitHub has static Linux binaries
(x86-64, any distribution) of the `mvdz` tool, and a `SHA256SUMS` file:

| download | for |
|---|---|
| `mvdz-<version>-x86_64-unknown-linux-musl-v3.tar.gz` | any x86-64 CPU from about 2015 on (AVX2); about 5 % faster |
| `mvdz-<version>-x86_64-unknown-linux-musl.tar.gz` | any x86-64 CPU (if unsure) |

**From source**, on any platform (only Linux is tested): the latest stable
Rust (1.100 at the time of writing).

```
git clone https://github.com/galfthan/mvdz && cd mvdz/mvdz_rust
cargo build --release            # target/release/mvdz
cargo install --path mvdz_cli    # or: put mvdz on your PATH
```

The default build runs on any CPU of its architecture. Building for
x86-64 CPUs with AVX2, with `RUSTFLAGS="-C target-cpu=x86-64-v3" cargo
build --release`, compresses about 3 % and decodes about 5 % faster on one
thread. The release downloads are static musl builds, which run on any
Linux but are slower than a glibc build (musl's allocator).

## Quickstart

```
mvdz compress game.mvd                       # -> game.mvdz
mvdz compress *.mvd                          # every file, several at once
gzip -dc game.mvd.gz | mvdz compress - game.mvdz   # from a .mvd.gz
mvdz decompress game.mvdz                    # -> game.mvd
mvdz info game.mvdz | jq .sha256             # the header, without decoding
```

A file holds the demo in segments of about 256 KiB of raw demo, each
decodable on its own: a reader gets demo bytes after the first segment, and
segments decode in parallel.

## Ratios and speeds

Sizes: weighted to a 72 GB archive of gzip-compressed demos (about 112 GB
raw), measured on a stratified sample of 379 of its demos, with 95 %
intervals. Speeds: `mvdz decompress`, one process per file, on 8 demos
(18.4 MB raw); a glibc build for baseline x86-64, pinned to four cores of
an Intel Core i5-12400F. Method, samples, per-era ratios and more speeds
are in [BENCHMARKS.md](BENCHMARKS.md).

| format | size / raw | decode, 1 process |
|---|---|---|
| mvdz, `-j 1` | 0.194 [0.191-0.197] | 81 MB/s |
| mvdz, `-j 4` | 0.194 | 211 MB/s |
| gzip -9 | 0.566 | 144 MB/s |
| xz -9e | 0.429 | 38 MB/s |
| zstd -19 | 0.508 | 406 MB/s |

The decode speeds of the last three rows are for `gzip -9`, `xz -9` and
`zstd -19` files of the 8 demos. The archive's own gzip files are 0.593 of
raw; the whole archive would take about 23.6 GB as mvdz.

MB = 10^6 bytes of raw demo. Recent demos compress better: about 0.18 for
2023-24 and 0.16 for 2025-26, against 0.24 to 0.26 before 2021. Current
mvdsv records a snapshot every server frame (`sv_demofps 77`; about 62 a
second in practice) where older demos mostly hold 20-27; the smaller time
step makes each snapshot easier to predict. Every decode checks the CRC-32
of each segment and of the whole demo; it does not recompute the SHA-256
(see Hashes and trust). Compression runs at about 30 MB/s on one thread
and 73 MB/s with `-j 4`, including a check that the output decodes to the
input.

## Command line

```
mvdz compress   [-j threads] [-f] [--rm] files... | in out.mvdz | -o out.mvdz in
mvdz decompress [-j threads] [-f] [--rm] files... | in.mvdz out | -o out in.mvdz
mvdz info       file.mvdz...                 header as JSON
```

`compress` and `decompress` work like zstd: each output is named after its
input and written beside it (`game.mvd` gives `game.mvdz`; `game.mvdz`
gives `game.mvd`). Inputs are kept; `--rm` deletes each one after its
output is written. An existing output is not overwritten without `-f`.
`-j` is 0 to 255, at most the logical cores; 0 (the default) is all of them. With several files, `-j` of them run
at once (one thread each), a file that fails is reported and the rest go
on, and the exit status is 1 if any failed; one file uses `-j` threads.
Two names where the second is an output (for `compress` a `*.mvdz` or `-`,
for `decompress` anything but `*.mvdz`) are an input and its output, and
`-o` names the output of a single input. `-` is standard input or output
(`mvdz decompress game.mvdz - | ...`). Compression has no settings, and it
decodes its own output and compares before writing anything. `info` prints
the header as one JSON object per file: `size_bytes`, `duration_ms`,
`crc32` and `sha256`; the checksums are what the file claims, not verified.

```
mvdz compress game.mvd
mvdz compress -o game.mvdz - < game.mvd
mvdz decompress demos/*.mvdz
mvdz info game.mvdz | jq .duration_ms
```

**gzip.** mvdz reads and writes raw demos only. A `.mvd.gz`, as most
archives serve them, is decompressed first, and a demo can be gzipped
again on the way out; both work through pipes (`-` is standard input or
output):

```
gzip -dc game.mvd.gz | mvdz compress - game.mvdz
mvdz decompress game.mvdz - | gzip > game.mvd.gz
```

The file holds the raw demo and its identity is the SHA-256 of the raw
demo (not of a `.gz`). A gzip file given to `compress` is refused with a
message saying so.

**Input that is not a demo is refused**: empty input, and input whose
first frame is not a whole MVD frame carrying messages that parse (or, if
that frame is damaged, followed by 16 more whole frames). Nothing is
written. Truncated demos and demos damaged after their start compress as
before, with what does not parse stored raw. When standard output closes
early (`mvdz info *.mvdz | head`), the tool stops quietly.

## Hashes and trust

* **Identity**: the SHA-256 of the original `.mvd`, in the file's 60-byte
  header. Decoding does not recompute it: a consumer that must
  authenticate a demo hashes the decoded bytes.
* **Integrity**: CRC-32 (as in zlib) per segment and over the whole demo,
  and the demo's size, checked on every decode.

Files from elsewhere are untrusted input: decoders bound their memory
(a few segments at a time when streaming; whole-file decoding refuses a
demo of more than 1 GiB), and the
format bounds their work by the input's size; demos can carry console commands (`svc_stufftext`) that players
must filter.

## Rust

Two crates: the library `mvdz` and the tool `mvdz_cli` (binary `mvdz`); a library
user builds no command-line code. Not on crates.io yet: depend on it by git
(`mvdz = { git = "https://github.com/galfthan/mvdz" }`) or by path.

```rust
// whole buffers; `max_threads` (u8) is capped at the logical cores, 0: one per core
let file = mvdz::encode(&demo, 0)?;
let demo2 = mvdz::decode(&file, 4)?;

// in order from any io::Read, up to `max_threads` segments decoded at once
let mut decoder = mvdz::Decoder::new(std::fs::File::open("game.mvdz")?)?;
eprintln!("{} ms", decoder.header().duration_ms());       // before decoding, as gzip
std::io::copy(&mut decoder, &mut std::io::stdout())?;
```

Errors (`mvdz::Error`): `NotMvd` (`encode` given input that is not an MVD
demo), `NotMvdz`, `UnsupportedVersion`, `Corrupt` (breaks a rule of
FORMAT.md), `MemoryLimit`, `DemoTooLarge` (more than 65,536 segments), `Io`
and `Internal`. Decoders never panic on corrupt input; they return an error.
`Decoder` yields only checked bytes: each segment's CRC-32 is checked
before its bytes are returned, the whole demo's before the end.

## WebAssembly

The library builds for `wasm32-unknown-unknown` (`cargo build --lib -p mvdz
--target wasm32-unknown-unknown`). There, encoding and decoding are
single-threaded; every dependency is pure Rust, so nothing changes between
targets.

## Tests

`cargo nextest run --release` (in `mvdz_rust/`; `cargo install
cargo-nextest` once) runs the unit tests, the golden vectors (valid and
hostile files through every reader), round trips, corrupt-input and
hostile-file tests, and the regressions of the pre-release reviews. `cargo test --doc` runs the documentation examples, which
nextest leaves out. Everything runs on synthetic demos; tests that can also use
real demos read them from the directory in `MVDZ_TEST_DATA` (subdirectory
`sample/` with demos named `<sha256>.mvd`, `gen/` with constructed demos)
and say so when it is unset.
`scripts/fetch_samples.sh sample40 testdata/sample` downloads the
development sample there (`MVDZ_TEST_DATA=$PWD/testdata`).
`MVDZ_WRITE_VECTORS=1` regenerates the vectors from two demos in
`$MVDZ_TEST_DATA/sample/`: set8's `3fe477dc…` and stratified's `03fc8ec2…`. Fuzz targets are in `mvdz_rust/mvdz/fuzz/` (`cargo +nightly
fuzz run decode`; also `stream_reader`, `compress_rt` and the
structure-aware `gen_rt`); they build the library with its `fuzz` feature.

Speed: `MVDZ_BENCH_DEMOS=$PWD/demos/set8 cargo bench --manifest-path
mvdz_rust/Cargo.toml --bench codec` times compress and decompress of every `*.mvd` in that
directory, one file at a time at 4 threads and at 1, with criterion
(`-- --save-baseline name` and `-- --baseline name` compare two builds).
`scripts/fetch_samples.sh set8 demos/set8` downloads the speed set
([BENCHMARKS.md](BENCHMARKS.md), Samples).

## FAQ

**Why not just compress the `.mvd` with zstd or xz?** On the same
stratified sample, `zstd -19` gives 0.508 of raw and `xz -9e` 0.429,
against 0.194 for mvdz: mvdz files are about 2.6 times smaller than
zstd -19 and 2.2 times smaller than xz -9e ([BENCHMARKS.md](BENCHMARKS.md)).
A general-purpose compressor sees bytes; mvdz parses the demo, puts each
field in its own stream and codes positions and angles as residuals
against predictions. The container also gives decoding in order as the
file arrives, segments that decode on their own, and the demo's size,
duration and identity hash without decoding.

**What is the licence?** MIT ([LICENSE](LICENSE)), and every dependency is
permissive (MIT, Apache-2.0 or BSD). MIT code can be linked into GPL
programs such as the GPL-2 QuakeWorld engines and servers.

**Is the format stable?** Not yet. This is a release candidate: the
format may change before 1.0, and files written by a release candidate
are not guaranteed to be readable by 1.0, so keep the original `.mvd`
files. The format has no extension points, so the goal is that 1.0 does
not change: a reader refuses any version but its own.

**Where do I give feedback?** Open an issue on GitHub
(<https://github.com/galfthan/mvdz/issues>), for the release and the
format alike.

## Contributing

Issues and pull requests are welcome on GitHub. Please run `cargo fmt`,
`cargo clippy --all-targets` and `cargo nextest run` (CI runs them on Linux, with
a wasm build). Changes to the file format go through FORMAT.md first. See [CHANGELOG.md](CHANGELOG.md) for releases.
Pull requests from automated agents without a human author who reviews
and answers for them are closed.

## Licence and dependencies

MIT, see [LICENSE](LICENSE). Dependencies (all permissive, all pure
Rust): `sha2` and its RustCrypto dependencies and `crc32fast`
(MIT/Apache-2.0); the tests also use `serde_json`.
