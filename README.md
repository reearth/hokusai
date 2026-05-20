# Hokusai

[![Crates.io](https://img.shields.io/crates/v/hokusai.svg)](https://crates.io/crates/hokusai)
[![Docs.rs](https://docs.rs/hokusai/badge.svg)](https://docs.rs/hokusai)
[![CI](https://github.com/reearth/hokusai/actions/workflows/ci.yml/badge.svg)](https://github.com/reearth/hokusai/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/hokusai.svg)](#license)

A pure Rust brush engine inspired by [libmypaint](https://github.com/mypaint/libmypaint), designed for WebAssembly and native targets.

🎨 **[Try the live demo](https://reearth.github.io/hokusai/)** — draws in your browser using the real libmypaint brushes, with stylus pressure and tilt where the device supports it.

## Goals

- 🦀 **Pure Rust, no `unsafe`** — clean WASM (`wasm32-unknown-unknown`) story.
- 📦 **libmypaint `.myb` JSON compatibility** — brushes authored for MyPaint / Krita load and round-trip without translation.
- 🎯 **Pixel-level parity with libmypaint** — same fix15 math, same tile layout, same stroke math. Compatibility is the design priority; the "Hokusai" name does not imply behavioural divergence.
- 🔌 **Pluggable surfaces** via the `TiledSurface` trait. Backends are split into feature-gated crates.
- 🗺️ **Tile-based infinite canvas** — 64×64 RGBA fix15 tiles, matching libmypaint exactly so dab traversal and rounding stay bit-identical.

## Workspace layout

```
hokusai/
├── crates/
│   ├── hokusai-core/        # Brush types, stroke engine, fix15, tiles, brushmodes
│   ├── hokusai-brush/       # libmypaint `.myb` JSON read / write
│   ├── hokusai-tile-mem/    # Reference in-memory TiledSurface
│   ├── hokusai-tiny-skia/   # Flatten TiledSurface tiles into a tiny-skia Pixmap
│   ├── hokusai-compat/      # Snapshot regression harness (libmypaint parity track)
│   └── hokusai-wasm/        # wasm-bindgen bindings + browser demo
└── hokusai/                 # Umbrella crate that re-exports the above via features
    └── examples/            # stroke_to_png, myb_to_png (+ vendored .myb fixtures)
```

## Quick look

```rust
use hokusai::{Brush, BrushSetting, BrushState};
use hokusai::myb;
use hokusai::tile_mem::MemSurface;

let json = std::fs::read_to_string("charcoal.myb")?;
let brush: Brush = myb::from_str(&json)?;

let mut state = BrushState::default();
let mut surface = MemSurface::new();

// First call seeds position only; subsequent calls emit dabs.
brush.stroke_to(&mut state, &mut surface,  10.0, 50.0, 0.0, 0.0, 0.0, 0.01);
brush.stroke_to(&mut state, &mut surface, 200.0, 50.0, 1.0, 0.0, 0.0, 0.01);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Run the bundled examples to render to PNG:

```sh
cargo run --example stroke_to_png --features tile-mem
cargo run --example myb_to_png --features "tile-mem myb-json" -- \
    hokusai/examples/fixtures/calligraphy.myb out.png
```

## Cargo features (umbrella `hokusai` crate)

| Feature     | Default | What it enables                              |
|-------------|---------|----------------------------------------------|
| `myb-json`  | ✅      | `.myb` JSON parser / serializer              |
| `tile-mem`  | ✅      | Reference `HashMap`-backed `TiledSurface`    |
| `tiny-skia` | —       | `tiny-skia` Pixmap flattening helpers         |

(`hokusai-wasm` ships as its own `cdylib` crate rather than an umbrella feature — point your `wasm-pack` at `crates/hokusai-wasm` directly.)

## libmypaint parity

**137 / 196 stock brushes (≈ 70 %) match libmypaint at MAD ≤ 0.5** under the brush-pack parity harness, with another 41 inside MAD ≤ 5. Measured against libmypaint v1.6.1 + the upstream [mypaint-brushes](https://github.com/mypaint/mypaint-brushes) pack via `cargo xtask brush-pack-report` (see [`CONTRIBUTING.md`](CONTRIBUTING.md#libmypaint-parity-testing) for the setup). Remaining gaps are tracked in [TODO](#todo).

## Features

🖌️ **Brush data**
- All ~50 libmypaint settings as a strongly-typed enum with canonical string keys
- All inputs (`pressure`, `speed1/2`, `random`, `stroke`, `direction`, `tilt`, `custom`, `gridmap_*`, `attack_angle`, `viewzoom`, `barrel_rotation`, `brush_radius`, `tilt_declinationx/y`, …)
- `.myb` v3 JSON parse / serialize, round-trip safe (unknown top-level settings preserved verbatim)

✏️ **Stroke engine** (libmypaint-faithful port of `update_states_and_setting_values`)
- Per-dab setting evaluation (`base_value + Σ curve(input)`) with per-dab interpolation of pressure / speed across each segment
- `slow_tracking` + `slow_tracking_per_dab` cursor lag, with `count_dabs_to` re-counted after every dab
- Speed low-pass (`speed1_slowness` / `speed2_slowness`) and `speed1_gamma` / `speed2_gamma` log mapping
- `direction_filter` low-pass on stroke direction (with the 180°-folded variant for 1D direction curves)
- `tracking_noise` gaussian jitter (radius-scaled, distance-coalesced via `skip_distance`)
- `offset_by_random` / `offset_by_speed` jitter; full `directional_offsets` port (`offset_x/y`, `offset_angle*`, `offset_multiplier`, `STATE.FLIP` mirroring)
- `radius_by_random` per-dab radius jitter with libmypaint's `(orig / new)²` opacity correction
- `opaque_linearize` per-dab overlap compensation
- Tilt inputs (`tilt`, `tilt_declination`, `tilt_ascension`) with the libmypaint 90° declination default and ramp-from-zero seeding
- `attack_angle` (signed angular difference between pen ascension and stroke direction + 90°)
- `Stroke` input with `stroke_duration_logarithmic`, `stroke_holdtime`, `stroke_threshold` gating
- Gridmap inputs (`gridmap_x`, `gridmap_y`) sampled from `STATE.ACTUAL_X/Y` via `gridmap_scale[_x/_y]`
- Custom input chain (`custom_input_slowness` smoothing `SETTING(custom_input)` into `INPUT(CUSTOM)`)
- Per-dab HSV / HSL drift (`change_color_h` / `_v` / `_hsv_s` / `_hsl_s` / `change_color_l`)
- Spectral pigment mix (`paint_mode`) — 10-channel WGM via `rgb_to_spectral` / `spectral_to_rgb` with the `spectral_blend_factor` sigmoid
- Smudge bucket sampling + mixing with lazy `smudge_length_log`-gated resample (`PREV_COL_RECENTNESS`), `apply_smudge` / `eraser_target_alpha` source-alpha bias, and `smudge_transparency` opacity-gated rejection
- Fresh-stroke / long-pause detection

🎨 **Pixel blending (`draw_dab`)**
- Normal + Eraser blend in linear sRGB fix15 (premultiplied alpha)
- Spectral `paint_mode` blend (`BlendMode_Normal_and_Eraser_Paint`) with low-alpha additive fade
- Colorize blend (replace hue/sat, keep value)
- Two-segment hardness falloff with `anti_aliasing` sub-pixel edge feathering (port of `calculate_rr_antialiased`)
- Elliptical dabs (`aspect_ratio`, `angle`)
- `lock_alpha` masking, `posterize` quantization as its own post-pass (so `paint_mode = 1` brushes still posterize)
- Spectral `get_color` (`Surface2::get_color_pigment`) for smudge sampling when `paint_mode > 0`

🔬 **Compatibility**
- Knuth lagged-Fibonacci PRNG port of libmypaint's `rng-double.c` (TAOCP 3.6-15, KK=10 LL=7 TT=7, seed 1000) with the same `rand_gauss` scaling
- libmypaint-sourced PNG goldens via a small C wrapper around `mypaint_brush_stroke_to_2` (`cargo xtask regenerate-goldens`)
- Brush-pack parity tool (`cargo xtask brush-pack-report`) — drives all 196 stock brushes through a fixed pressure-ramp curve. Current state: **137 / 196** stock brushes pass MAD ≤ 0.50, 41 amber (≤ 5.0), 18 red
- Per-dab tracing (`HOKUSAI_TRACE_DABS=1`) prints identical-format dab lines from both engines for `paste`-diff debugging
- `HOKUSAI_UPDATE_GOLDENS=1` snapshot harness for in-tree regression

🧱 **Infrastructure / backends**
- Tile-aware traversal across arbitrary canvas extents (64×64 RGBA fix15, libmypaint-identical)
- `hokusai-tiny-skia` — flatten any `TiledSurface` into a `tiny_skia::Pixmap`
- `hokusai-wasm` — `wasm-bindgen` JS bindings + browser demo
- CI: fmt, clippy `-Dwarnings`, test on Linux/macOS/Windows, wasm32 build check, MSRV 1.88

## TODO

The brush-pack-report (`cargo xtask brush-pack-report`) is the source of truth
for what's left. As of the latest run, 18 brushes are red (MAD > 5) and 41 are
amber. The remaining gaps cluster into:

- **RNG-divergent scatter brushes** (`Tail_Feathers`, `Tail_Feathers2`,
  `Flight_Feathers`, `Fan#1`, `imp_details`, `impressionism`, `puantilism2`,
  `spray`, `spray2`, `particules_eraser`, `DNA_brush`, `Clouds`, `texture-06`,
  `oil-0{1,3}-paint`, `coarse_bulk_1`). The individual dab formulas match;
  the dab *sequence* diverges because some upstream RNG consumer is offset
  by an unknown count. Use `HOKUSAI_TRACE_DABS=1 paste`-diff (see
  `CONTRIBUTING.md`) — the first drifting column points at the missing
  consumer.
- **Surfacemap-input brushes** (`Posterizer`, `Round#1`). libmypaint's
  v1.6.1 dylib outputs all-white for these because the reference C wrapper
  rejects them via surfacemap; hokusai actually paints. Either teach the
  wrapper to accept these brushes, or skip them in the report.
- **`anti_aliasing > 1.0` and full-dab feathering edge cases** — landed
  recently but only covered by one fixture; needs sweep across more brushes.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build / test / snapshot workflow,
fixture conventions, and commit-message style.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Vendored brush fixtures under `hokusai/examples/fixtures/` are unmodified copies from [mypaint-brushes](https://github.com/mypaint/mypaint-brushes) (CC0 1.0).
