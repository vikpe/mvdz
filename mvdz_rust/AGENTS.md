# Rust

Extends the root `AGENTS.md`. Rust-specific additions only.

## Coding Standards

- Before declaring a Rust change done, self-audit the diff against this file, then run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` (also reports unused dependencies), and `cargo nextest run`.
- Never use `unsafe` code. The workspace has none.
- Never use `.unwrap()` in library code — handle errors explicitly.
- Always restrict visibility as much as possible. Don't expose struct internals, use getters. Default to private, then `pub(crate)`; `pub` only when something outside the crate names it (`unreachable_pub` is warned workspace-wide and catches the leftovers). A `pub use` re-export is public API too: drop it when nothing outside imports it. `tests/`, `benches/`, and a bin's `main.rs` are separate crates, so items they use must stay `pub`; items reached only through another crate's public signature must stay `pub` as well (`private_interfaces` flags the mismatch).
- Always give a builder setter the bare value, never an `Option`: `fn ping(self, ping: u16)` stores `Some(ping)`, and a caller that has no value skips the call, leaving the `Default` `None`. `.ping(None)` must never appear at a call site. This is the std shape (`thread::Builder::name`, `OpenOptions::read`, `Command::current_dir`); std has no `maybe_`/`Option`-taking setters, so a caller holding an `Option` writes `if let Some(ping) = ping { builder = builder.ping(ping); }`. An all-fields constructor (`Type::new`) still takes `Option` args when the fields are genuinely optional.
- An item that only its own unit tests use is dead: delete it together with the test. Never keep it `pub` to silence `dead_code`: that hides the deadness behind a fake consumer.
- Always scope a constant to its narrowest user. A private (`const` or `pub(crate)`) constant referenced by exactly one function lives *inside* that function as a local `const`, not at module level. Keep a constant at module scope only when it is `pub` API or shared by two or more functions. (Moving a single-use const in is a pure scope change — it stays a `const`.)
- Always give a variable the shortest possible lifespan: bind it immediately before its first use, not in a `let` block at the top of the function. Don't compute values ahead of early-return gates that never read them; narrow further into the `if` / loop / match arm when only that arm uses it. Locality makes the reader's working set small and turns a dead binding into an obvious one.
- Always derive the basic traits when possible, in the following order: `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`, `Default`.
- Always sort enum variants in ascending order (A→Z) unless a domain order is meaningful (e.g. severity levels, wire-protocol tags pinned to integer values).
- Always use `pretty_assertions::assert_eq` instead of the default `assert_eq` in tests.
- Always keep functions and types in the most appropriate existing crate; propose a new crate only when there's a clear domain boundary.
- Always import a foreign type from its home crate; re-export only what your crate owns or wraps. One workspace pin means no version skew, so re-exporting buys nothing and costs two spellings for one type. Naming a foundation type is one line of `Cargo.toml`; calling methods on it needs no import at all. Exceptions: the source crate is an implementation detail you intend to swap, or the type exists solely to serve your API. Never leave two live paths to one type.
- Always name crates with underscores, never hyphens: `qworld_demo` not `qworld-demo`. Use the same name in the `[package] name`, the crate directory, and the root `[workspace.dependencies]` key + `path`; declare internal deps as `{ workspace = true }`. Underscores keep the package name identical to the Rust import identifier and avoid Cargo's silent `-` to `_` mapping (a hyphenated dependency key that must be `use`d with an underscore).
- Always run `cargo fmt` and `cargo clippy --workspace --all-targets` after changes.
- Always run tests with `cargo nextest run` instead of `cargo test` (it is faster). Use `cargo test --doc` only for doctests, which nextest does not run.
- Always wrap code-like tokens in doc comments (`///` / `//!`) in backticks: type/function/field/variable names, paths, file names, and constants including all-caps tokens like `SOLID_BSP`, `MOVETYPE_PUSH`, `SV_SpawnServer`. `clippy::doc_markdown` (pedantic) fires on any unticked CamelCase/snake_case/`::` token. This does NOT apply to plain `//` comments.
- Always put serde usage behind a feature flag named `"serde"`.
- Never declare `default` features on workspace crates — features are strictly opt-in. If code is required, it is not a feature; if it is optional, every consumer states it explicitly (`features = ["..."]`). A `default-features = false` on an internal crate's workspace pin is the smell that the crate has defaults it shouldn't (trimming *third-party* crates' default features stays encouraged). When a crate's *own tests* require one of its features, enable it with a self-referential dev-dependency (`[dev-dependencies] my_crate = { path = ".", features = ["..."] }` — target-gate it if the feature is single-target) so plain `cargo nextest run` stays flagless.
- Always pin third-party crates with `default-features = false` plus an explicit feature list in `[workspace.dependencies]`; a member adds only what it alone needs (`default-features = false` is ignored on `{ workspace = true }` members).
- Always include unit tests in a `#[cfg(test)] mod tests` block for any non-trivial logic.
- Prefer `.get()` over direct array indexing.
- Prefer exhaustive pattern matching over default arms where practical.
- Prefer borrowing (`&str`, `&[T]`) over owned types (`String`, `Vec<T>`) in function parameters.
- Prefer `impl Trait` over generics where the type parameter is used only once.
- Leverage the type system to make invalid states unrepresentable.
- Prefer a two-variant enum, or two named fns, over a `bool` parameter when callers in other modules pass a literal (`dock_pane(tree, pane, true)`). A `bool` stays fine for setters/builders, private helpers, and call sites whose other arguments already name the meaning (`precache_messages(&sound_names, false)`).
- Look for opportunities to reduce allocations, but only if it can be done without adding complexity.
- When performance optimizing, always write a benchmark first and validate the performance difference.
- Always use anyhow::Result in tests, dont `unwrap()`. add context via `.context()` when appropriate (dont use `.ok_or_else(|| anyhow::anyhow!())`)
- Always define bit-position constants as `1 << N`, never as hex literals (`0x0000_0008`). The shift form makes the bit number self-evident; hex hides it. Mask aggregates built by OR-ing those constants are fine to keep as-is.
- Always separate words in flag/extension constant names with `_`, even when the upstream C source runs them together: `PEXT_MODEL_DBL` not `PEXT_MODELDBL`, `PEXT_FLOAT_COORDS` not `PEXT_FLOATCOORDS`, `SET_ANGLE_REASON` not `SETANGLEREASON`, `MAX_PLAYERS` not `MAXPLAYERS`. Acronyms (`PFS`, `QTV`, `VR`) stay as one token. Trailing digits get an underscore too (`SIGIL_1`, `ENTITY_DBL_2`). Cross-reference comments still cite the original C name verbatim so grep against the reference source keeps working.
- Prefer FxHashMap to HashMap for performance critical lookups

## Web/Native Target Splits

- Always keep one shared code path; cfg-gate only genuine target differences (UDP vs WebSocket transport, fs vs localStorage, event sources). Never maintain parallel native/web variants of shared logic.
- Pick the split level by the size of the difference:
  - A single line or function differs: inline `#[cfg(target_arch = "wasm32")]` / `#[cfg(not(target_arch = "wasm32"))]`.
  - A coherent chunk differs (IO, sockets, event sources): module split `foo/{mod,native,web}.rs`. `mod.rs` holds the shared types and dispatches via cfg'd `mod` + `pub use`; `native.rs` and `web.rs` export identical names and signatures so the compiler enforces the contract per target and callers never see cfg. Template: `qworld_client/src/viewer/live_feed/`.
  - A whole file is single-target: file-level `#![cfg(...)]` as the first item. Template: `qworld_net/src/transport.rs`. A one-sided module split (`foo/{mod,native}.rs` with no `web.rs`) is fine when only one target needs the chunk.
  - Never split at the tree root; parallel trees duplicate everything that does *not* differ and drift apart. The only root-level split is the entry-point bins (`bins/qworld_native`, `bins/qworld_web`).
- Always promote a file to the module split once it accumulates several cfg blocks or cfg'd imports; isolated one-off cfgs stay inline.
- Always group cfg-gated `mod` declarations by target instead of interleaving them into the A-Z list: shared mods first (sorted A-Z, per-mod comments kept), then one banner-commented block per target (`// Native-only: ...`, `// Web-only: ...`), each block sorted A-Z with the `#[cfg]` attribute repeated per `mod` (Rust has no block form short of a wrapper module, which would change import paths). The banner names what the group covers; the target-gated surface is then visible at a glance. Template: `qworld_client/src/viewer/mod.rs`.
- Always re-export through the facade with explicit `use` lists, never globs — the shared API surface must be readable in `mod.rs`.
- Never use traits for target dispatch; cfg modules are the std (`std::sys`) and winit (`platform_impl`) idiom. A trait pays off only when both impls must compile in the same build.
