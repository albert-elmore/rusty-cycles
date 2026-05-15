# Phase 0 — test contract & goldens

This document fixes **what “correct” means** for the sampler-first, Tidal-inspired expression path *before* we grow full expression-language parity.

## Goals

- **Comparable observables**: pattern evaluation must produce **scheduled hits** we can snapshot and regress in tests.
- **No SuperDirt**: synth names, FX buses, and OSC targets are **out of scope** for parity. Sample tokens (`bd:1`, paths, aliases) map to **`.wav`** files under `--sample-root`.
- **Rust-first goldens**: tests compare normalized scheduler output to **hand-written expectations** in code (extendable later to JSON snapshots or a Haskell oracle).

## Tempo (`slow` / `fast`)

Pattern tracks may use **`slow N $`** / **`fast N $`** before **`s`** (see README). Goldens that include tempo should assert both the `TrackKind::Pattern tempo` field and `events_for_track` across **multiple** `cycle_index` values when using **`slow`**.

## Observable: `NormalizedHit`

After `Program::events_for_track`, use `sequencer::event_spec::normalize_schedule`:

| Field | Meaning |
|--------|---------|
| `phase` | Onset in `[0, 1)` for one cycle (same as scheduler today). |
| `sample_relpath` | Path relative to `sample_root`, POSIX `/`, when resolution succeeds; else filename fallback. |
| `gain` | Effective gain for that hit (`master` × track gain × `# gain` step multiplier when applicable). |

Hits are **sorted** by `(phase, sample_relpath, gain)` for stable comparison.

Equality uses **epsilon** floats (`phase` ~1e-8, `gain` ~1e-5) so trivial FP noise does not fail tests.

## Explicit non-goals (Phase 0 doc scope)

Stream muting is implemented as **`silence` / `unsilence`** in the parser (see README). Remaining non-goals for **full** Tidal parity:

- **Full Tidal AST** (`slow`, `{}`, nested `$` outside current `.seq` rules).
- **Haskell / GHC** in CI — optional future “oracle” dump.

## Adding tests

1. Build a temp `sample_root` with bank folders (see `tests/phase0_golden.rs`).
2. `parse` a `.seq` fragment.
3. `events_for_track` for one or more `cycle_index` values.
4. `normalize_schedule` and `assert_eq!` against expected `NormalizedHit` slices.

This corpus should grow with every new combinator or mini-notation feature.
