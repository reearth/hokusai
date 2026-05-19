# Contributing to Hokusai

Thanks for taking a look! This document covers what you need to build, test,
and propose changes to hokusai.

## Prerequisites

- Rust **1.77** or newer (the project's MSRV). `rustup` is the easiest path.
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

> The committed goldens are currently produced by hokusai itself, so the
> harness only catches **regressions**, not true libmypaint parity. See
> `crates/hokusai-compat/src/lib.rs` for the path to upgrading the goldens
> to upstream libmypaint output.

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
