# mvdz

You are assisting with **mvdz**, a lossless compressor and file format for QuakeWorld
MVD demos (`.mvd`, Quake 1). An `.mvdz` file decodes back to the exact original bytes.

The project consists of the format specification (`FORMAT.md`) with its test vectors
(`vectors/`), a Rust workspace (`mvdz_rust/`: library `mvdz/` with fuzz targets in
`mvdz/fuzz/`, command-line tool `mvdz_cli/`), a Go package (`mvdz_go/`, standard library
only; `gofmt`, `go vet`, `go test`), a C99 decoder (`mvdz_c/`, no dependencies; `make
test`) and benchmarks (`benchmarks/`, `BENCHMARKS.md`, `scripts/`). The Rust and Go
encoders write the same bytes.

## Before working on a task

- If a `README.md` exists in the current working directory or any directory you're working in, read it before starting work.

## General coding standards

- Never read or reference code in `.bak` directories. Treat it as if it doesn't exist.
- Always use `git mv` when renaming or moving tracked files/directories, to preserve history. Never `mv` + `git add`.
- Always match the style and conventions already present in the codebase.
- Code must read from top to bottom: public API first, then private helpers/types, tests at the very bottom.
- Always prefix boolean variables and fields with a word that makes them explicitly boolean: `is_`, `has_`, `was_`, `should_`, `can_`, `with_`, etc. For example: `is_first` not `first`, `has_cache` not `cache`, `is_active` not `active`.
- Never use single-letter names for variables, closure params or functions; use the domain noun (`game`, `player`, `row`). Exceptions: position components `x`/`y`/`z`, color components `r`/`g`/`b`/`a`, `sort_by(|a, b| ...)` comparator pairs, and closures in dense one-line mappings over a named collection (`players.iter().map(|p| p.frags())`).
- Variables should have as short life span as possible (defined close to usage).
- Information hiding: give every item the strictest visibility that compiles. Start private, widen one step only when a caller outside needs it. What a module exposes is its contract; everything else stays free to change without breaking anyone. Never widen speculatively.
- Comments: names carry the meaning; comments are a fallback, only for what the name/code cannot say. ideally don't comment at all, 1-2 rows should be enough in 99% of cases where a comment is justified.
- Use american english (e.g. color, not colour)

## Naming across the spec, Rust, Go and C

- Same naming everywhere, each language's idiom. Before naming, check what the other implementation and `FORMAT.md` call it. Every name is one of:
  - Format term: `FORMAT.md`'s word (section 1 lists them), the same in the spec, Rust, Go and C. Only casing differs.
  - General programming context (lengths, cursors, reading, writing, errors, constructors, tests): the word the language's standard library uses for that context. The implementations may differ here: `Decoder::new` / `NewReader`, `read_u16` / `readUint16`, `write_u32` / `binary.LittleEndian.AppendUint32`.
  - Neither: one word, the same on both sides.
- Go's flat package prefixes the Rust module word where the bare word is ambiguous: `rans::Table` / `ransTable`, `lz::parse` / `lzParse`, `streams::TIME` / `streamTime`. C's unity build is flat too: the Go name in `snake_case` (`lz_expand`, `STREAM_TIME`).
- `size`: bytes fixed by the format or a type (`header_size`, `coord_size`). `len`: bytes of this particular data, measured or read from a length field (`raw_len`, `payload_len`). The demo's byte count is `size`.
- `count`: items. `total`: a sum. `max_*`: a limit.
- `position`: a moving cursor. `offset`: a distance in bytes from a start, never qualified by `len`. `index`: an array element. `number`: a wire number. `id`: a stream id.
- Always spell out: `source`, `destination` (`dest` as a prefix or suffix), `position`, `low`, `high`, `frequency`, `distance`, `difference`. Short forms kept: `len`, `crc`, `id`, and unit suffixes (`_ms`, `_secs`, `_min`).
- `read_*` takes from a cursor and advances. `parse_*` turns a block into a struct. `write_*` (Go `append*`) serializes. `code_*` is a walker step that runs in both directions; `encode_*` / `decode_*` run in one.

## Output

- Never express emotions.
- Answer briefly and objectively.
- If unsure, say so. Never guess.
- Write for a senior developer with lots of experience: no walls of text, state each decision once, skip restating rationale the reader can infer. This applies to answers, docs, and plan/notes files alike.
- Drop: filler (just/really/basically/actually/simply), pleasantries (sure/certainly/of course/happy to), hedging. Fragments OK. Short synonyms (big not extensive, fix not "implement a solution for"). Technical terms exact. Code blocks unchanged. Errors quoted exact.
  - Pattern: [thing] [action] [reason]. [next step]
  - Not: "Sure! I'd be happy to help you with that. The issue you're experiencing is likely caused by..."
  - Yes: "Bug in auth middleware. Token expiry check use < not <=. Fix:"

- No em dashes, smart quotes, or Unicode. ASCII only.

## Engineering principles

- The best code is code never written. Before writing, climb: not needed (YAGNI) > already in this codebase > stdlib > platform feature > installed dependency > one line > minimum code that works. Look before you write.
- Understand before simplifying: trace every file the change touches and the real flow end to end. A bug fix is the root cause in the shared function, not a guard in the reported caller.
- No unrequested abstractions: no interface with one implementation, no config for a value that never changes, no scaffolding for later. Deletion over addition, boring over clever, fewest files, shortest working diff.
- Deep modules: simple interface, complex implementation. No shallow pass-through layers. Pull complexity into the implementation. Define errors out of existence.
- Naming is design: precise, consistent names over documentation. If a name needs a comment, pick a better name.
- Readable conditionals: name compound conditions (`is_valid && is_allowed`), early return over nesting, keep the reader on the happy path.
- Don't over-DRY: simple obvious duplication beats a clever generic. Factor slowly; follow existing patterns, and introduce a better way only if you convert all uses.
- Don't remove code you cannot explain. Optimize only from a profile.
- Non-trivial logic leaves one runnable check (a small test). Trivial one-liners need none. Integration tests over mocks; regression test before fixing a bug.
- Mark a deliberate simplification with a known ceiling with a `ponytail:` comment naming the ceiling and upgrade path.
- Never simplify away validation at trust boundaries, error handling that prevents data loss, security, or anything explicitly requested.
- Complex request: ship the lazy version and question it in the same reply ("Did X; Y covers it. Need full X? Say so."). Never stall on an answer you can default.
- Stick to the boring subset of the language; readers should not need deep language knowledge.
- Major design decision: consider at least two approaches before picking one.
- Report: code first, then at most three short lines: what was skipped, when to add it.

## Domain rules

`mvdz_rust/AGENTS.md` has the Rust rules, `mvdz_c/AGENTS.md` the C rules. Read the one
of a directory before touching files there; it is loaded automatically only when the
session starts in that directory. Domain rules win over the general ones on their own
domain.
