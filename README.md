# Rusty Cycles

An interpretation of TidalCycles written in Rust plus more.

Rust CLI that plays WAV one-shots on a **cycle** clock, with **live reload** when you save a pattern file. Each cycle it re-reads the file and reloads when the **contents** change (the OS watcher is an extra hint). Updates apply at the **start** of the next cycle; parse errors go to stderr and the last good pattern keeps playing.

## Setup

Install Rust via [rustup](https://rustup.rs) (Apple Silicon uses `aarch64-apple-darwin` by default).

Put short WAV clips in [`samples/`](samples/) — for example `kick.wav` and `snare.wav` — matching the paths in your pattern file.

**Hearing “wrong” hit times or tempo:** The scheduler fires where your `.seq` says (check stderr after each save: `hits=[…]` per track). If two tracks use `0` but one *sounds* late, the WAV often has **leading silence** or a **late transient** — trim the file or use tighter one-shots. Long loops or noisy tails can also mask BPM; use `--verbose` to compare expected cycle length in ms vs wall time and `drift_frames` after each cycle boundary.

**Audio output:** Playback uses **cpal** with a single output stream at the device’s sample rate. The default device must support **f32** samples (macOS CoreAudio usually does). Output may be **mono or stereo**; mono sums L+R. Hits are scheduled in **output frames** on the audio callback clock so layered triggers stay time-aligned.

**Same instant, several tracks:** Multiple hits in one cycle are **mixed** in the output callback (each hit is a short “voice”), so simultaneous fractions (e.g. both `0`) sound together instead of queuing.

## Run

From this directory:

```bash
cargo run --release -- music.seq
```

- **`--default-bpm 120`** — BPM used when the file has no `bpm` line (default 120).
- **`--sample-root DIR`** — resolve relative sample paths against `DIR` (default: directory containing the pattern file).
- **`--no-watch`** — load once; do not watch the file (ignored when multiple scenes are active).
- **`--scene FILE`** (repeatable) — scene playlist mode; rotate files with `--scene-switch-cycles`.
- **`--scene-switch-cycles N`** — auto-switch to next scene every N cycles (0 disables).
- **`--swing X`** — delay odd 1/8 slots by `X` (0..1, approximate 16th swing).
- **`--humanize-ms X`** — deterministic per-hit jitter in milliseconds (0 disables).
- **`--reload-quantum-cycles N`** — apply parsed reloads every N cycle boundaries (file alias: `reload_quantum_cycles` / `quantum`).
- **`--limiter none|soft`** — optional master limiter (soft uses tanh saturation).
- **`--verbose`** — each cycle, print `idx` (0-based cycle counter, used by `<>` in mini-notation), BPM, cycle length in frames, expected length in ms (from frame count), wall time for the iteration, and `drift_frames` (playback position vs scheduled cycle end).

Example with tmux: one pane running the command above, another editing `music.seq` in vim.

## Pattern format

- Optional: `bpm <n>` with `n` in 1–400 (can appear more than once; later lines override earlier ones in the same file).
- Optional: **`master <g>`** — global mix gain (default **1.0**), applied **after** per-track **`gain`** and **`# gain`** step multipliers. Must be finite and **≥ 0** (can appear more than once; later lines override).
- Optional: **`swing <x>`** — file-level global swing in **[0,1]** (overrides `--swing` while active).
- Optional: **`humanize_ms <x>`** or **`humanize <x>`** — file-level deterministic jitter in milliseconds (**>=0**, overrides `--humanize-ms` while active).
- Optional: **`reload_quantum_cycles <n>`** or **`quantum <n>`** — file-level reload apply quantization (**>=1**, overrides `--reload-quantum-cycles` while active).
- Optional: **`include "path"`** or **`include path`** — textually insert another `.seq` before parsing; relative paths resolve next to the **file being loaded** (the CLI passes that directory). Nested includes and **cycle detection** are supported; `include` in a string with no on-disk path errors with a clear message.
- Optional: **`let NAME "…"`** — bind a **mini-notation fragment**; later **`s "…"`** bodies and **`# … "…"`** lane strings expand **whole identifiers** only (longest name wins), e.g. `let kit "bd sn"` then `s "kit"`.
- Optional: **`mute <name>`** — mute this track/orbit in the current file snapshot (to unmute, remove/comment the line and save).
- Optional: **`mute all`** — mute all tracks in the current file snapshot.
- Optional: **`solo <name>`** — if any solos exist, only soloed tracks are scheduled (to clear solos, remove/comment all `solo` lines and save).
- **`silence <name>`** — mute the track/orbit **`name`** (no samples scheduled for it). Later lines win: repeat **`silence d1`** idempotently; **`unsilence d1`** removes **`d1`** from the mute set. Matching is **exact** on the track name (`d1` ≠ `D1`).
- **Pattern tracks** use mini-notation inside **`s "…"`** (Tidal **`sound`**). The keyword **`pat`** is rejected; use **`s`** only.
  - Form: **`dN $ [gain <g>] [TRANSFORM $ ...] s "…"`** (or bare **`s "…"`** = `d1`) with optional lanes:
    - **`# gain "..."`**
    - **`# pan "..."`**
    - **`# prob "..."`** (hit probability in **[0,1]**)
    - **`# dur "..."`** or **`# duration "..."`** (milliseconds)
    - **`# legato "..."`** (relative duration multiplier)
    - **`# cut "..."`** (group, optional voices)
    - **`# cutoff "..."`** (cut release ms)

    Lanes require quoted patterns (`"1 0.5"`). Gain/pan/prob lanes cycle by hit index. **`~`** is neutral for gain/pan/prob and no-override for dur/legato/cut/cutoff. If both `dur` and `legato` are present for a hit, `dur` wins.
  - **`gain`** is optional (default **1.0**); it must be finite and **≥ 0**, and scales that track at mix time.
- **Mini-notation:** inside **`s "…"`**, **spaces** split the cycle into equal steps (**sequence**). **`[a b]`** = **simultaneous** stack at one step. **`stack [ p , q ]`** = **layers**: full-cycle patterns mixed (comma-separated); each `p` / `q` uses the usual mini rules (often written `stack [ bd sn, hh cr ]`). **`<a b>`** = **alternate** (global cycle index). **`~`** = rest. **`bd*3`** repeats **`bd`** for three subdivisions inside that sequence slot.
- **Tempo (Tidal-style, Phase 1):** optional **`slow N $`** or **`fast N $`** **after** optional `gain` and **before** **`s`** (e.g. `d1 $ slow 2 $ s "bd sn"`). **`slow N`** maps the inner pattern’s timeline across **N** physical cycles (one “chunk” of the pattern per cycle; **`<>`** uses a **logical** cycle index of `wall_cycle / N`). **`fast N`** runs **N** inner cycles inside one wall cycle, each in a **1/N** time slice. **`{}`**, further **`$`** plumbing, and other Tidal shorthands are still **not** full parity.
- **Transform combinators (Tidal-style placement):** **`$` is right-associative** — chain as in Tidal (e.g. `d1 $ rev $ pal $ s "bd sn"` applies **`rev`** around **`pal (sound)`**). Put combinators before **`s`**, each followed by **`$`** (e.g. `d1 $ pal $ rot 1 $ s "bd sn hh"` or `d1 $ every 2 pal $ s "bd sn hh"`). Inside-quote transform keywords (`rev`, `pal`, `rot`, `every`, `fast`, `slow`) are not supported.
- **Orbit prefix:** a line may start with **`d` + digits + `$`** (e.g. **`d1 $`**, **`d16 $`**). If the line begins with **`s`** / **`slow`** / **`fast`** (or `gain …` before them) and has no orbit prefix, it is treated as **`d1`**.
- **Multi-line statements:** physical lines are merged until **`[` / `]`** depth outside **`"…"`** is **0** and double-quotes are balanced (Tidal-style blocks). A newline between merged lines becomes a **space** in the combined text.
- After each cycle, if the **master** mix exceeded **±1.0** on any output channel for any frame, a **`[clip]`** line is printed (counts output frames affected; no limiting is applied).
- **Samples** (mini-Tidal v0.1):
  - **File path** (relative to `--sample-root` / pattern directory, or absolute): `samples/kick.wav`
  - **Bank:index** — directory `<sample_root>/<bank>/` filled with `.wav` files sorted by **filename**. Index is **1-based**: `bd:3` is the third file (e.g. `a.wav`, `b.wav`, `c.wav` → `bd:2` is `b.wav`).
  - **Bank folder, default index** — if a token has **no** `:` and `<sample_root>/<token>/` is a **directory**, it is treated as that bank’s **first** file (same as `token:1`). Paths with `/` or `\` are never treated this way.
  - **`alias`** — `alias NAME REF` where `REF` is a single token (`bd:2` or `mysample.wav`). Then use **`NAME`** as a token inside **`s "…"`**.
- **Comments:** `--` outside `"…"` starts an end-of-line comment (Tidal/Haskell style). Inside quotes, `--` is literal.

**Bank layout:** default `<sample_root>` is the directory **containing** your `.seq` file. Either put `bd/*.wav` **beside** that file, or keep banks under [`samples/`](samples/) and run with `--sample-root samples` so `bd:1` resolves to `samples/bd/…`.

Examples: [`music.seq`](music.seq), full feature gallery + CI parse test: [`examples/showcase.seq`](examples/showcase.seq). Short checklist: [`examples/feature-tour.seq`](examples/feature-tour.seq).

Phase 0 (scheduler contract + regression goldens): [`docs/phase0.md`](docs/phase0.md). Integration tests: `tests/phase0_golden.rs`.

### Tidal-style roadmap (subset)

Implemented: **`s "…"`**, **`master`**, **`dN $`** orbit prefix, **`include`**, **`let`**, **`silence`** / **`unsilence`** / **`mute`** / **`solo`** (plus **`mute all`**), **`slow N $` / `fast N $`**, nested **`$`** combinator chains, outer transform combinators (**`rev`**, **`pal`**, **`rot`**, **`every`**), **multiline** statement merging (brackets / quotes), **`stack [ a , b ]`** layers (inside the quoted mini-string), **`token*N`** repeats, **`# gain`** (cycling), **`# pan`** (cycling), **`# prob`** (deterministic hit gating), **`# dur`/`# duration`**, **`# legato`**, **`# cut`**/`#cutoff`, mini **`[]`** / **`<>`** / **`~`**, **`euclid(k,n[,rot]) sample`**, **banks**, **aliases**, cycle index for **`<>`**, scene rotation, swing, humanize jitter, quantized reload, and optional soft limiter. Not SuperDirt (no separate synth server). **Next:** **`{}`**, deeper Tidal **expression** surface.

## Build

```bash
cargo build --release
# binary: target/release/sequencer
```
