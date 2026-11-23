# TODO

Implementation work, none started. The format is settled: no item changes a byte.

## 1. One mechanism for direction in the Rust walker

- Now: direction is the `IS_ENCODE` const and the `SymbolIo` trait. Each IO stubs the
  other direction's blob methods, and the walker forks on `if IS_ENCODE` in 16 places.
- Step 1: one concrete IO holding both halves, as Go's `segmentStreams`; the trait and the
  stubs go.
- Step 2: the Go shape. The encoder walks a copy of the segment and proposes each symbol
  from the bytes under the cursor; one path, no forks. Rust walker plus numerics is 1236
  lines against Go's 940.
- No format change. Biggest win and biggest risk: time encode and decode before and after.
  Profile first: on 50 4on4 demos (2026-11-23) rANS symbol decode is 64 % of decode time
  and the walk 32 %; of encode time the self-check decode is 37 %, the two walks 29 %,
  rANS encode 20 %.

## 2. Decoder table search

- Now: `rans.rs:count_le`, the search through a table's cumulative counts, is about a
  quarter of decode time (profile above). The `ponytail` note there: the SSE2 search it
  replaced was about 6 % faster overall and needs `unsafe`.
- Options: `std::simd` once stable; or a slot LUT for the few hottest tables, memory for
  speed. Measure with the codec bench before and after.
- No format change.

## 3. Encoder self-check

- Now: every encode decodes its output and compares (`encode.rs:decodes_to`): 37 % of
  encode time, and the guarantee that the file decodes.
- Option: an opt-out for callers that verify otherwise. Keep the check the default.
- No format change.
