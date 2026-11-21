# mvdz

**mvdz** is a lossless compressor and file format for QuakeWorld MVD demos
(`.mvd`). An `.mvdz` file is about a fifth of the raw demo and a third of
the demo's gzip, decodes back to the exact original bytes (checked by
SHA-256), can be read in order from a pipe as it arrives, and
carries the demo's metadata (identity hash, server info, roster, a census
of its contents, wall-clock time) so catalogues can read it without
decoding. Files can also carry attachments stored beside the demo.

It comes as a command-line tool and a Rust library, and builds for
WebAssembly. The format is specified in [FORMAT.md](FORMAT.md), precisely
enough to write an independent decoder, and `mvdz_rust/tests/vectors/` holds valid
and hostile test files. The work now is the specification and this Rust
implementation; implementations in C, Go and other languages follow once
the format is settled.

**Status: 1.0.0-rc.4, a release candidate for comments.** The file format
may still change before 1.0: files written by a release candidate are not
guaranteed to be readable by 1.0, so keep the original `.mvd` files for
now. Later additions after 1.0 will use the forward-compatible extension
points of FORMAT.md.

How it works: the demo is walked command by command (`svc_*` layouts from
mvdsv and ezQuake). Every field goes to its own stream; numeric fields
(player and entity origins, view angles, sound and temp-entity
coordinates) are coded as residuals against predictors. Each stream is then
entropy-coded: static rANS with per-context tables at the **normal** level
(the default), or zstd at the **fast** level (faster to compress and
decode, larger). Anything that does not parse is stored raw, so any input
round-trips.

## What reads .mvdz today?

The `mvdz` command-line tool, the Rust library and the WebAssembly build.
**No QuakeWorld client plays `.mvdz` files yet**: to watch one, decompress
it to `.mvd` first (`mvdz decompress game.mvdz game.mvd`).

## Install and build

**Linux binaries.** Each release on GitHub has static Linux binaries
(x86-64, any distribution) of the `mvdz` tool, and a `SHA256SUMS` file:

| download | for |
|---|---|
| `mvdz-<version>-x86_64-unknown-linux-musl-v3.tar.gz` | any x86-64 CPU from about 2015 on (AVX2); faster |
| `mvdz-<version>-x86_64-unknown-linux-musl.tar.gz` | any x86-64 CPU (if unsure) |

**From source**, on any platform (only Linux is tested): Rust 1.89 or newer
and a C compiler (for the bundled libzstd).

```
git clone https://github.com/galfthan/mvdz && cd mvdz/mvdz_rust
cargo build --release            # target/release/mvdz
cargo install --path .           # or: put mvdz on your PATH
```

The default build runs on any CPU of its architecture. Building for the
machine you run on, with `RUSTFLAGS="-C target-cpu=native" cargo build
--release`, is faster on x86-64 CPUs with AVX2 (about 10-15 % at the
normal level; [BENCHMARKS.md](BENCHMARKS.md)). The release downloads are
static musl builds, which run on any Linux but are slower than a glibc
build (musl's allocator).

## Quickstart

```
mvdz compress game.mvd                       # -> game.mvdz (normal level, one segment)
mvdz compress *.mvd                          # every file, several at once
gzip -dc game.mvd.gz | mvdz compress - game.mvdz   # from a .mvd.gz
mvdz compress --fast -s 1M game.mvd          # faster both ways, seekable segments
mvdz decompress game.mvdz                    # -> game.mvd
mvdz info game.mvdz | jq .sha256             # metadata, without decoding
```

### Segments: one, or `-s 1M`

A file holds the demo in one or more independently decodable segments.

* **One segment** (the default of `mvdz compress` and of the one-shot
  library and C calls): the smallest file. It cannot seek, and a reader
  gets no demo bytes until the whole demo has been decoded. Right for
  archives and downloads that are decompressed whole.
* **`-s 1M`** (about 1 MiB of raw demo per segment): about 1 % larger at
  the normal level, 2 % at the fast level ([BENCHMARKS.md](BENCHMARKS.md));
  at the normal level the segments share one set of rANS models, stored
  once in the file. A reader can seek by demo time to the start of any
  segment, delivers the first bytes after one segment has decoded, and
  can decode segments in parallel. Use it for files that will be played,
  streamed or seeked into.

The streaming writer (Rust `Writer`, C `mvdz_writer_*`), which records a
demo as it grows, always cuts segments, because it writes each one as it
fills: its default is 1 MiB. So the two defaults differ on purpose, each
the best choice for its use: one-shot compression of a finished demo
optimises size, a recording must cut. Pass a segment size explicitly to
either to get the other behaviour. A recorded file keeps a model in every
segment (it cannot know the whole demo's models before its first
segment), so at the normal level it is about 3 to 5 % larger than
`mvdz compress -s 1M` of the finished demo.

## Ratios and speeds

Sizes: on a 72 GB archive of gzip-compressed demos (about 112 GB raw),
measured on an independent stratified sample of 585 demos, with 95 %
intervals. Speeds: the release download (baseline x86-64), one core of a
4-vCPU x86-64 VM (Skylake Xeon), on 8 demos (18.9 MB raw), CLI one process
per file. Method, samples, per-era ratios, more speeds and the other
builds (the `-v3` download is 10-20 % faster) are in
[BENCHMARKS.md](BENCHMARKS.md).

| format | size / raw | decode, CLI, 1 process | decode, library, 1 thread |
|---|---|---|---|
| mvdz normal | 0.211 [0.206-0.216] | 22 MB/s | 27 MB/s |
| mvdz fast | 0.255 [0.250-0.261] | 34 MB/s | 44 MB/s |
| gzip (the archive's files) | 0.64 | 79 MB/s | |
| xz -9e | 0.47 | 27 MB/s | |
| zstd -19 | 0.55 | 104 MB/s | |

The gzip size is that of the archive's own files (level unknown; `gzip
-6` gives 0.611 on the 8 demos), and the xz size is `xz -9e` (`xz -9`:
0.468 on 300 random demos). The decode speeds of those rows are for `gzip
-6`, `xz -9` and `zstd -19` files of the 8 demos.

MB = 10^6 bytes of raw demo. The whole archive would take about 23.7 GB
at the normal level and 28.6 GB at the fast level. Recent demos (2023-26)
compress to about 0.18: current mvdsv records a snapshot every server
frame (`sv_demofps 77`; about 62 a second in practice) where older demos
mostly hold 20-27, at about the same compressed bytes per second of game.
Demos that record every player input (mvdsv's usercmd hidden blocks) are
about twice as large raw and compress to about 0.10. Every decode checks the CRC-32C of each segment and of the whole
demo, and by default the SHA-256 identity (about 20 % of the decode time
at the normal level, 30 % at the fast level); a cache that verified the
identity once can decode with `--trusted`, about 1.2 to 1.5 times faster.
Compression runs at about 8 MB/s (normal) and 13 MB/s (fast) per thread,
including a check that the output decodes to the input. The fastest
settings and what they give up are in
[BENCHMARKS.md](BENCHMARKS.md#fastest-settings).

## Command line

```
mvdz compress   [--fast] [-s size] [-j threads] [--ext key=value]... [--allow-non-demo]
                [-f] [--rm] files... | in out.mvdz | -o out.mvdz in
mvdz decompress [-j threads] [--max-memory size] [--trusted]
                [-f] [--rm] files... | in.mvdz out | -o out in.mvdz
mvdz info       file.mvdz...                 metadata, index, attachments as JSON
mvdz census     [--tsv] files-or-dirs...     census of .mvdz / .mvd / .mvd.gz files
mvdz attach add file.mvdz name path [--mime type] [--meta key=value]...
mvdz attach ls  file.mvdz [name-prefix]
mvdz attach extract file.mvdz name out
mvdz attach rm  file.mvdz name...
mvdz attach export in.mvdz out.mvdz [--keep name]... [--keep-prefix p]... [--drop name]... [--drop-prefix p]...
mvdz bench      [--levels normal,fast] [-s size] [-r reps] [-j threads] [--trusted] files...
```

`compress` and `decompress` work like zstd: each output is named after its
input and written beside it (`game.mvd` gives `game.mvdz`; `game.mvdz`
gives `game.mvd`). Inputs are
kept; `--rm` deletes each one after its output is written. An existing
output is not overwritten without `-f`. With several files, `-j` of them
run at once (one thread each), a file that fails is reported and the rest
go on, and the exit status is 1 if any failed. Two names where the second
is an output (for `compress` a `*.mvdz` or `-`, for `decompress` anything
but `*.mvdz`) are an input and its output, and `-o` names the output of a
single input. `-` is standard input or output (`mvdz decompress game.mvdz
- | ...`). Sizes take K, M, G suffixes (binary). `-s 1M` cuts the demo into
independently decodable segments: seek points and parallel decoding, at a
small cost in size; the default is one segment (a file holds at most 65536
segments). Compression decodes its own output and compares before writing
anything. `--trusted` skips the SHA-256 identity check (see above); use it
only for files whose identity you verified before. `attach add` stores the
file as given; `--mime` defaults to `application/octet-stream`; `--meta`
adds a key/value, whose key needs a prefix (`x-...` or `vendor.`):
unprefixed keys are reserved for future versions (FORMAT.md §10.1).
`attach export` writes a copy that keeps the attachments matching a
`--keep` name or `--keep-prefix` (all, if none is given) and no `--drop`
name or `--drop-prefix`, e.g. a demo download without large recordings;
the source is not changed, and the output's bundle hash goes to standard
error. `info` prints the JSON described in FORMAT.md §3.2, including
`bundle_sha256` (§10.2).

```
mvdz compress game.mvd
mvdz compress --fast -s 1M -o game.fast.mvdz game.mvd
mvdz decompress demos/*.mvdz
mvdz info game.mvdz | jq .map
mvdz attach add game.mvdz analysis/result.json result.json --mime application/json --meta x-tool=mvda
mvdz attach export game.mvdz - --drop-prefix rec/ > download.mvdz
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
before, with what does not parse stored raw. A few archive files that
carry a demo after a non-MVD header are refused too; `--allow-non-demo`
stores any input (it still round-trips, mostly stored raw). When standard output
closes early (`mvdz census demos/ | head`), the tool stops quietly.

## Hashes and trust

* **Identity**: the SHA-256 of the original `.mvd` (the hub's
  `demo_sha256`), stored in the metadata. The same for every level,
  segment size and set of attachments. Reading the metadata does not
  verify it; a full decode does (by default).
* **Integrity**: CRC-32C per segment and over the whole demo, always
  checked, also when seeking. `--trusted` decodes with these alone.
* **Bundle**: `bundle_sha256`, a SHA-256 over the identity and the
  attachments' names, MIME types, SHA-256s and keys; the same however the
  file was compressed. A download ETag, and what a signature would sign.

Files from elsewhere are untrusted input: decoders bound memory and work
(`Limits`, `max_memory`), demos can carry console commands
(`svc_stufftext`) that players must filter, and attachments are untrusted
content of any type, not access-controlled: anyone holding a file can read
all of them. FORMAT.md §14 covers this.

## Rust

Not on crates.io yet: depend on it by git
(`mvdz = { git = "https://github.com/galfthan/mvdz" }`) or by path.

```rust
use mvdz::{Attachment, Config, Level, Limits, ReadOptions};

// whole buffers
let packed = mvdz::compress(&demo, &Config::default())?;                   // normal level
let fast = mvdz::compress(&demo, &Config { level: Level::Fast, ..Default::default() })?;
let demo2 = mvdz::decompress(&packed, 4)?;                                  // 4 threads
// under a memory cap, or trusted (no SHA-256, CRC checks only)
let demo3 = mvdz::decompress_with(&packed, 1, &Limits { memory: 256 << 20, ..Default::default() }.into())?;
let demo4 = mvdz::decompress_with(&packed, 1, &ReadOptions { trusted: true, ..Default::default() })?;

// decoding in order from any io::Read, one segment at a time
let mut r = mvdz::Reader::new(std::fs::File::open("game.mvdz")?, 2, &Default::default())?;
std::io::copy(&mut r, &mut out)?;
mvdz::decompress_stream(std::io::stdin(), std::io::stdout(), 1, &ReadOptions::default())?;

// streaming compression of a demo that is still being recorded
let mut w = mvdz::Writer::new(file, &Config { segment: 1 << 20, ..Default::default() })?;
w.write_demo(&chunk)?;                    // segments are written as they fill
w.finish()?;                              // metadata, index, trailer (or w.abort())

// metadata without decoding (identity not verified); attachments
let info = mvdz::read_metadata(std::fs::File::open("game.mvdz")?)?;
println!("{} {}", info.to_json(), mvdz::api::hex(&info.bundle_hash()));
for a in mvdz::list_attachments(std::fs::File::open("game.mvdz")?, "analysis/")? { println!("{} {}", a.name, a.size); }
let result = mvdz::read_attachment(std::fs::File::open("game.mvdz")?, "analysis/result.json")?;
let a = Attachment { name: "analysis/result.json".into(), mime: "application/json".into(),
                     kv: vec![("x-tool".into(), "mvda".into())], data: json_bytes };
mvdz::update_attachments(std::path::Path::new("game.mvdz"), &["old"], &[a])?;
// a copy without some attachments, e.g. for a download
let sel = mvdz::Selection { drop_prefix: vec!["rec/".into()], ..Default::default() };
let etag = mvdz::export_attachments(std::fs::File::open("game.mvdz")?, std::fs::File::create("dl.mvdz")?, |e| sel.matches(e))?;
```

`ReadOptions` (for decoding) holds `limits` and `trusted`; `Limits` has
`memory` (bytes, default 1 GiB on 64-bit) and `work_per_byte`, and is all
that metadata and attachment reading take. Errors (`mvdz::Error`) are `Format` (not MVDZ or an
unsupported version), `Corrupt` (any rule of FORMAT.md), `Limit` (the
budget), `Invalid` (a bad argument, a configuration a reader would
refuse), `NotFound`, `Io` and `Internal`. Decoders never panic on corrupt
input; they return an error. Every size a file declares is bounded by the
bytes actually present (FORMAT.md §9), and `Limits` caps the memory one
decode may use.

## WebAssembly

The library builds for `wasm32-unknown-unknown` and `wasm32-wasip1`
(`cargo build --lib --target wasm32-unknown-unknown`). There, zstd is the
pure-Rust `ruzstd` (the `pure-zstd` feature selects it natively too) and
decoding is single-threaded; files are identical whichever zstd decoder
reads them. The format's resource rules bound memory by the input, and
`ReadOptions::limits` caps it further, which matters in a browser.

## Tests

`cargo test --release` runs the unit tests, the golden vectors (valid and
hostile files through every reader), round trips at both
levels, segmented and streamed, corrupt-input and hostile-file tests,
metadata and attachment tests, and the regressions of the pre-release
reviews. `cargo test --features pure-zstd` runs them with the pure-Rust
zstd decoder. Everything runs on synthetic demos; tests that can also use
real demos read them from the directory in `MVDZ_TEST_DATA` (subdirectory
`sample/` with demos named `<sha256>.mvd`, `gen/` with constructed demos,
`poc/` with hostile model headers) and say so when it is unset.
`MVDZ_WRITE_VECTORS=1` regenerates the vectors (it needs the sample
demos they were made from). Fuzz targets are in `mvdz_rust/fuzz/` (`cargo +nightly
fuzz run decode`; also `stream_reader`, `read_metadata`, `attachments`,
`stream_writer`, `compress_rt` and the structure-aware `gen_rt`).
The feature `scalar` forces the portable code paths (as on wasm).

## FAQ

**Why not just compress the `.mvd` with zstd or xz?** On the same
stratified sample, `zstd -19` gives 0.553 of raw and `xz -9e` 0.473,
against 0.211 for mvdz normal: mvdz files are about 2.6 times smaller
than zstd -19 and 2.2 times smaller than xz ([BENCHMARKS.md](BENCHMARKS.md)).
A general-purpose compressor sees bytes; mvdz parses the demo, puts each
field in its own stream and codes positions and angles as residuals
against predictions. The container also gives decoding in order as the
file arrives, segments that decode on their own,
metadata without decoding, the demo's identity hash and attachments.

**What is the licence?** MIT ([LICENSE](LICENSE)), and every dependency is
permissive (MIT, Apache-2.0 or BSD). MIT code can be linked into GPL
programs such as the GPL-2 QuakeWorld engines and servers.

**Is the format stable?** Not yet. This is a release candidate: the
format may change before 1.0, and files written by a release candidate
are not guaranteed to be readable by 1.0, so keep the original `.mvd`
files. After 1.0, later additions will use the forward-compatible
extension points of FORMAT.md (new sections, metadata tags and keys).

**Where do I give feedback?** Open an issue on GitHub
(<https://github.com/galfthan/mvdz/issues>), for the release and the
format alike.

## Contributing

Issues and pull requests are welcome on GitHub. Please run `cargo fmt`,
`cargo clippy --all-targets` and `cargo test` (CI runs them on Linux, with
the pure-zstd tests, a wasm build and the minimum Rust version). Changes to the file format go through FORMAT.md first. See [CHANGELOG.md](CHANGELOG.md) for releases.
Pull requests from automated agents without a human author who reviews
and answers for them are closed.

## Licence and dependencies

MIT, see [LICENSE](LICENSE). Dependencies (all permissive, all pure
Rust): `sha2` and its RustCrypto dependencies, `crc32fast` and
`miniz_oxide` (MIT/Apache-2.0); the tests also use `serde_json`.
