# Contributing to Hokusai

Thanks for taking a look! This document covers what you need to build, test,
and propose changes to hokusai.

## Prerequisites

- Rust **1.88** or newer (the project's MSRV — `image` v0.25.10 requires
  it transitively). `rustup` is the easiest path.
- For the `wasm32-unknown-unknown` build check:
  `rustup target add wasm32-unknown-unknown`.

No system libraries are required — hokusai is pure Rust.

## Build and test

```sh
# Build everything (all crates, all features)
cargo build --workspace --all-features

# Run the whole test suite (28+ tests across core, brush, tile-mem, compat)
cargo test --workspace --all-features

# Lints used by CI — these must pass before pushing
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -Dwarnings

# Cross-target build check (must succeed; CI enforces it)
cargo build --workspace --target wasm32-unknown-unknown
```

## Examples

```sh
# Hardcoded wavy stroke → out.png
cargo run --example stroke_to_png --features tile-mem

# Real .myb brush → out.png (try any of the vendored fixtures)
cargo run --example myb_to_png --features "tile-mem myb-json" -- \
    hokusai/examples/fixtures/calligraphy.myb out.png
```

The vendored brushes under `hokusai/examples/fixtures/` come from
[mypaint-brushes](https://github.com/mypaint/mypaint-brushes) (CC0). They're
useful both as integration fixtures and as a quick way to eyeball how the
engine handles real-world brush configurations.

## Snapshot regression harness

The `hokusai-compat` crate replays brush + stroke scripts and compares the
output to committed PNG goldens.

**Run the harness:**

```sh
cargo test -p hokusai-compat
```

**Add a fixture:**

1. Create `crates/hokusai-compat/fixtures/<name>.json`:
   ```json
   {
     "brush": "../../../hokusai/examples/fixtures/charcoal.myb",
     "width": 400,
     "height": 80,
     "events": [[x, y, pressure, dtime_seconds], ...]
   }
   ```
   The `brush` path is resolved relative to the script JSON.
2. Generate the golden:
   ```sh
   HOKUSAI_UPDATE_GOLDENS=1 cargo test -p hokusai-compat
   ```
3. Inspect the resulting `<name>.png`, commit both files.

**Update an existing golden** (after an intentional pixel change):

```sh
HOKUSAI_UPDATE_GOLDENS=1 cargo test -p hokusai-compat
```

When a test fails normally, the actual output is written next to the script
as `<name>.actual.png` for inspection.

> The default `HOKUSAI_UPDATE_GOLDENS=1` path produces goldens from hokusai
> itself, so the harness only catches **regressions**. To regenerate the
> goldens from real libmypaint instead — the libmypaint-parity track — run:
>
> ```sh
> ./scripts/regenerate-goldens.sh           # all fixtures
> ./scripts/regenerate-goldens.sh smudge    # filter by name substring
> ```
>
> That script chains `setup-parity.sh` → `cargo xtask regenerate-goldens`
> → `cargo test -p hokusai-compat` so a fresh checkout reproduces the
> committed PNGs end-to-end.

## libmypaint parity testing

For checking hokusai against the real libmypaint reference, run

```sh
./scripts/setup-parity.sh
```

once to:

1. Install `libmypaint`, `json-c`, and `pkg-config` (Homebrew on macOS;
   `apt` / `dnf` / `pacman` on Linux).
2. Clone the upstream brush pack into `tmp/mypaint-brushes/` (CC0,
   <https://github.com/mypaint/mypaint-brushes>).
3. Build the small C wrapper under `tools/libmypaint-render/` that
   drives `mypaint_brush_stroke_to_2` and dumps raw RGBA8.

After that, the two parity commands are:

```sh
# Per-brush MAD table (196 brushes, written to tmp/brush-pack-report.md)
cargo xtask brush-pack-report

# HTML side-by-side gallery for the hokusai-compat fixture set
cargo xtask parity-report
```

`HOKUSAI_BRUSH_PACK=/some/dir` overrides the brush-pack location;
both commands rebuild the C wrapper on demand. Goldens for the
compat-fixture set live next to each script in
`crates/hokusai-compat/fixtures/`.

### Per-dab tracing

`HOKUSAI_TRACE_DABS=1` makes both engines print every emitted dab to
stderr in the same format:

```
  hok#1: ( 21.75, 76.66) r= 1.16 hard=0.59 opaq=0.00 aspect=8.65 ang=  0.0 paint=0.00
  lmp#1: ( 21.66, 76.70) r= 1.17 hard=0.59 opaq=0.08 aspect=8.39 ang=-180.0 paint=0.00
```

So a quick line-for-line diff:

```sh
HOKUSAI_TRACE_DABS=1 cargo xtask brush-pack-report 2> hok.log
HOKUSAI_TRACE_DABS=1 ./tools/libmypaint-render/libmypaint-render \
    tmp/_brush_pack_script.json \
    "$(realpath tmp/mypaint-brushes/brushes/classic/imp_details.myb)" \
    > /dev/null 2> lmp.log
paste <(grep hok# hok.log) <(grep lmp# lmp.log) | less
```

The dab field that drifts first is almost always the next bug to
fix. The session that landed the
`paint_mode default`, `STATE.DECLINATION ramp`, `state-update reorder`,
warm-up RNG, and seed-with-noise commits used this technique
exclusively.

## Code style

- **No `unsafe`.** The engine is pure-safe Rust by design.
- **fmt + clippy clean.** `cargo fmt --all` and `cargo clippy ... -Dwarnings`
  are CI gates; matching the configured style avoids review churn.
- **Comments explain *why*, not *what*.** Identifier names handle the
  "what". Add a comment when a constant, formula, or fall-through case
  comes from libmypaint's source and would surprise a future reader.
- **Settings and inputs use libmypaint's canonical string keys.** See
  `hokusai_core::setting::BrushSetting::cname` and
  `hokusai_core::input::BrushInput::cname`. New keys must match upstream
  exactly so `.myb` round-trip stays lossless.
- **`TODO(M2-followup)` / `TODO(M3-followup)` style markers** are used for
  intentionally deferred features. Please cross-reference the README TODO
  list when adding new ones.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/):
`feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`. Optional
scope in parentheses, e.g. `feat(core): ...` or `fix(compat): ...`.

Body should explain motivation and notable trade-offs. Reference the
relevant milestone (`M1`/`M2`/`M3`) when it helps.

## Adding a libmypaint feature

1. **Locate the upstream code.** Most behaviour traces back to
   `mypaint-brush.c` (stroke engine) or `brushmodes.c` (pixel blend).
2. **Decide on the right module.** Stroke-time dynamics belong in
   `hokusai-core/src/stroke.rs`; pixel-level blends belong in
   `hokusai-core/src/brushmodes.rs`; new settings or inputs go in
   `setting.rs` / `input.rs`.
3. **Match the canonical name.** If the feature is keyed in `.myb` JSON,
   its `cname()` must match libmypaint character-for-character.
4. **Add a snapshot fixture** under `crates/hokusai-compat/fixtures/` that
   exercises the feature, so behavioural changes are caught.
5. **Update the README TODO list** — strike completed items, add any
   newly-discovered gaps.

## Reporting issues

Please include:
- A minimal reproduction — ideally a `.myb` + stroke script that can be
  dropped into `crates/hokusai-compat/fixtures/`.
- Expected vs. actual output (an `<name>.actual.png` from the harness is
  ideal).
- libmypaint version you're comparing against, when relevant.
