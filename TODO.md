# TODO

Follow-ups to the one-pass walk. None started.

## 1. One track type for both coordinate widths (format change)

- Now: `AxisTrack` (16-bit) and `FloatTrack` (f32 bits) per origin axis; an origin update
  touches only the track of the width in force (FORMAT.md 5.2).
- Change: one track with u32 values, shared by both widths. `FloatTrack`,
  `player_float_origins`, `entity_float_origins` and the width rule go.
- Cost: one bad prediction per axis after a width switch. Tracks grow from 8 to 16 bytes;
  cache effect unmeasured.
- Bytes change only for a segment that switches width: regenerate
  `two-matches.normal.mvdz` and `width-switch.normal.mvdz`. Rust, Go and FORMAT.md 4.3,
  4.9, 5.2, 5.3, 5.5 change together.

## 2. Per-player and per-entity state in one struct

- Now: eight walker arrays (`players`, `player_tracks`, `player_float_origins`, `entities`,
  `entity_origins`, `entity_float_origins`, `usercmds`, `usercmd_tracks`).
- Change: fold the tracks into `PlayerState`, `EntityState` and `UsercmdState`. Five arrays
  without item 1, three with it. Do after item 1.
- No format change. Time decode before and after: entity state gets bigger per entry.

## 3. One mechanism for direction in the Rust walker

- Now: direction is the `IS_ENCODE` const and the `SymbolIo` trait. Each IO stubs the
  other direction's blob methods, and the walker forks on `if IS_ENCODE` in 16 places.
- Step 1: one concrete IO holding both halves, as Go's `segmentStreams`; the trait and the
  stubs go.
- Step 2: the Go shape. The encoder walks a copy of the segment and proposes each symbol
  from the bytes under the cursor; one path, no forks. Rust walker is 1150 lines against
  Go's 980 for walker plus numerics.
- No format change. Biggest win and biggest risk: time encode and decode before and after.
