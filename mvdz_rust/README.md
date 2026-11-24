# mvdz_rust

The reference implementation: the library `mvdz` and the command-line tool `mvdz_cli`
(binary `mvdz`). A library user builds no command-line code. Not on crates.io yet:
depend on it by git (`mvdz = { git = "https://github.com/galfthan/mvdz" }`) or by path.

## API

```rust
// two-pass, from a seekable input, without holding the demo
impl Encoder {
    pub fn new() -> Self
    pub fn max_threads(self, max_threads: u8) -> Self     // 0, the default: one per core
    pub fn max_memory(self, max_memory: u64) -> Self      // bytes, approximate; default none
    pub fn encode(&self, input: impl Read + Seek, output: impl Write) -> Result<()>
}

// streaming: implements std::io::Read
impl<R: Read> Decoder<R> {
    pub fn new(input: R) -> Result<Self>                  // reads the header
    pub fn max_threads(self, max_threads: u8) -> Self     // 0, the default: one per core
    pub fn max_memory(self, max_memory: u64) -> Self      // default 64 MiB, at least 14 MiB
    pub fn header(&self) -> &Header
}

impl Header {
    pub fn size(&self) -> u64
    pub fn duration(&self) -> Duration
    pub fn crc32(&self) -> u32
    pub fn sha256(&self) -> &[u8; 32]                     // a claim: not checked by decoding
}

pub enum Error {
    Corrupt(&'static str), DemoTooLarge, Internal(&'static str), Io(io::ErrorKind),
    MemoryLimit(&'static str), NotMvd(&'static str), NotMvdz, UnsupportedVersion,
}
pub type Result<T> = std::result::Result<T, Error>;
```

## Example

`hello.mvd` to `hello.mvdz` and back:

```rust
use std::fs::File;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // hello.mvd -> hello.mvdz
    mvdz::Encoder::new().encode(File::open("hello.mvd")?, File::create("hello.mvdz")?)?;

    // hello.mvdz -> hello.mvd
    let mut decoder = mvdz::Decoder::new(File::open("hello.mvdz")?)?;
    std::io::copy(&mut decoder, &mut File::create("hello.mvd")?)?;
    Ok(())
}
```

In memory: give `encode` a `std::io::Cursor` over the demo and a `Vec` to write to, and
call `read_to_end` on a `Decoder` over a slice. Settings chain:
`Encoder::new().max_threads(4).max_memory(64 << 20)`, and the same two on a `Decoder`.
`decoder.header()` gives the size, duration and hashes before any decoding.

Decoders never panic on corrupt input; they return an error. `Decoder` yields only
checked bytes: each segment's CRC-32 is checked before its bytes are returned, the
whole demo's before the end.

## Command line

```
mvdz compress   [-j threads] [-M mebibytes] [-f] [--rm] files... | in out.mvdz | -o out.mvdz in
mvdz decompress [-j threads] [-M mebibytes] [-f] [--rm] files... | in.mvdz out | -o out in.mvdz
mvdz info       file.mvdz...                 header as JSON
```

`mvdz` without arguments prints the full usage. Build it with `cargo build --release`
(`target/release/mvdz`) or `cargo install --path mvdz_cli`.

## WebAssembly

The library builds for `wasm32-unknown-unknown` (`cargo build --lib -p mvdz --target
wasm32-unknown-unknown`). There, encoding and decoding are single-threaded; every
dependency is pure Rust, so nothing changes between targets.

## Tests

```
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run             # cargo install cargo-nextest once
cargo test --doc              # the documentation examples, which nextest leaves out
```

- The tests run on synthetic demos and the vectors of `../vectors`. Tests that can also
  use real demos read them from `$MVDZ_TEST_DATA/sample/` (demos named `<sha256>.mvd`)
  and say so when it is unset: `scripts/fetch_samples.sh sample8 testdata/sample`, then
  `MVDZ_TEST_DATA=$PWD/testdata`.
- `MVDZ_WRITE_VECTORS=1` regenerates the vectors from two demos in
  `$MVDZ_TEST_DATA/sample/`: sample8's `3fe477dc...` and stratified's `03fc8ec2...`.
- Fuzz targets are in `mvdz/fuzz/` (`cargo +nightly fuzz run decode`; also
  `stream_reader`, `compress_rt` and the structure-aware `gen_rt`).
- Speed: `MVDZ_BENCH_DEMOS=$PWD/demos/sample8 cargo bench --bench codec` times every
  `*.mvd` in that directory at 4 threads and at 1, with criterion.

## Dependencies

All permissive, all pure Rust: `sha2` and its RustCrypto dependencies and `crc32fast`
(MIT/Apache-2.0); the tests also use `serde_json`.
