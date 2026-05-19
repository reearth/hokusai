# Hokusai

[![Crates.io](https://img.shields.io/crates/v/hokusai.svg)](https://crates.io/crates/hokusai)
[![Docs.rs](https://docs.rs/hokusai/badge.svg)](https://docs.rs/hokusai)
[![CI](https://github.com/reearth/hokusai/actions/workflows/ci.yml/badge.svg)](https://github.com/reearth/hokusai/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/hokusai.svg)](#license)

A pure Rust brush engine inspired by [libmypaint](https://github.com/mypaint/libmypaint), designed for WebAssembly and native targets.

🎨 **[Try the live demo](https://reearth.github.io/hokusai/)** — draws in your browser using the real libmypaint brushes, with stylus pressure and tilt where the device supports it.

The full pipeline — `.myb` brush load → stroke engine → dab blend on tiles — is implemented and can render real libmypaint brushes (`charcoal`, `calligraphy`, `marker_fat`, …). Pixel-level parity with libmypaint is the goal; the gap is tracked in [TODO](#todo).

## Goals

- **Pure Rust, no `unsafe`** — clean WASM (`wasm32-unknown-unknown`) story.
- **libmypaint `.myb` JSON compatibility** — brushes authored for MyPaint / Krita load and round-trip without translation.
- **Pixel-level parity with libmypaint** — same fix15 math, same tile layout, same stroke math. Compatibility is the design priority; the "Hokusai" name does not imply behavioural divergence.
- **Pluggable surfaces** via the `TiledSurface` trait. Backends are split into feature-gated crates.
- **Tile-based infinite canvas** — 64×64 RGBA fix15 tiles, matching libmypaint exactly so dab traversal and rounding stay bit-identical.

## Workspace layout

```
hokusai/
├── crates/
│   ├── hokusai-core/        # Brush types, stroke engine, fix15, tiles, brushmodes
│   ├── hokusai-brush/       # libmypaint `.myb` JSON read / write
│   ├── hokusai-tile-mem/    # Reference in-memory TiledSurface
│   ├── hokusai-compat/      # Snapshot regression harness (libmypaint parity track)
│   └── hokusai-wasm/        # wasm-bindgen bindings + browser demo
└── hokusai/                 # Umbrella crate that re-exports the above via features
    └── examples/            # stroke_to_png, myb_to_png (+ vendored .myb fixtures)
```

Planned: `hokusai-tiny-skia` (raster output).

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

## Features

**Brush data**
- All ~50 libmypaint settings as a strongly-typed enum with libmypaint-canonical string keys
- All inputs (`pressure`, `speed1/2`, `random`, `stroke`, `direction`, `tilt`, …)
- `.myb` v3 JSON parse / serialize, round-trip safe for known keys

**Stroke engine**
- Per-event setting evaluation (`base_value + Σ curve(input)`)
- Per-dab pressure interpolation across the segment (matches libmypaint's `update_states_and_setting_values` step)
- `slow_tracking` smoothing of the cursor path
- Speed slowness low-pass (`speed1_slowness`, `speed2_slowness`) → `speed1`/`speed2` inputs
- Distance + time based dab spacing (`dabs_per_actual_radius`, `dabs_per_basic_radius`, `dabs_per_second`)
- Tilt inputs (`tilt`, `tilt_declination`, `tilt_ascension`) with libmypaint's default of 90° declination
- Per-dab HSV drift via `change_color_h` / `change_color_v` / `change_color_hsv_s`
- Smudge bucket sampling and mixing
- Fresh-stroke / long-pause detection

**Pixel blending (`draw_dab`)**
- Normal + Eraser blend in linear sRGB fix15 (premultiplied alpha)
- Two-segment hardness falloff
- Elliptical dabs (`aspect_ratio`, `angle`)
- `anti_aliasing` edge feathering
- `lock_alpha` masking
- `posterize` per-pixel quantization

**Infrastructure**
- Tile-aware traversal across arbitrary canvas extents
- Deterministic MT19937 PRNG
- Snapshot regression harness (`hokusai-compat`) with `HOKUSAI_UPDATE_GOLDENS=1`
- CI: fmt, clippy `-Dwarnings`, test on Linux/macOS/Windows, wasm32 build check, MSRV 1.77

## TODO

### Stroke engine
- [x] **Speed slowness low-pass** (`speed1_slowness`, `speed2_slowness`)
- [x] **Tilt-derived inputs** (`tilt_declination` defaults to 90°, `tilt_ascension`)
- [x] **Per-dab pressure interpolation** within a stroke segment
- [ ] **`tracking_noise`** — random jitter added to the smoothed pointer position
- [ ] **`attack`** input — initial pressure ramp at stroke start (currently aliased to `stroke_progress`)
- [ ] **`stroke_holdtime`** + `stroke_duration_logarithmic` driving the `Stroke` input
- [ ] **`offset_by_random`** / **`offset_by_speed`** dab position jitter
- [ ] **`stroke_threshold`** — suppress dabs below a pressure floor
- [ ] **Custom input** — recursive evaluation through `custom_input` / `custom_input_slowness`
- [ ] **Slow tracking per-dab** (`slow_tracking_per_dab`) for radius smoothing
- [ ] **Gridmap inputs** (`gridmap_x`, `gridmap_y`)
- [ ] **Offset settings** (`offset_x`, `offset_y`, `offset_angle*`, `offset_multiplier`)
- [ ] **Per-dab interpolation of non-pressure inputs** (speed, tilt, position) — currently held constant inside a segment

### Pixel blending
- [x] **Colorize** — replace dst hue/sat with the dab's, keep dst value
- [ ] **Spectral `paint` mode** — modern MyPaint pigment mixing
- [x] **`change_color_hsl_s`** / **`change_color_l`** — HSL-space colour drift
- [ ] Direct `tile_lookup`-free `get_color` path for backends that can't expose tiles

### Compatibility
- [x] **libmypaint-sourced golden snapshots** — `tools/libmypaint-render/` is a small C wrapper around `mypaint_brush_stroke_to`, and `cargo xtask regenerate-goldens` drives it across the fixture set so `crates/hokusai-compat/fixtures/*.png` is upstream output. `cargo xtask parity-report` renders a side-by-side HTML diff for eyeballing the parity surface
- [x] **Knuth lagged Fibonacci PRNG** — port of libmypaint's `rng-double.c` (TAOCP 3.6-15) with the same `rand_gauss` scaling (`sum*√3 − 2√3`) and per-dab `random_input` refresh order. Seeding mirrors `rng_double_new(1000)`.
- [x] **Lossless round-trip** for unknown top-level `.myb` settings (unknown inputs *inside* a known setting are still dropped)
- [ ] Close residual MAD on `brush_pressure_ramp`, `calligraphy_*`, `charcoal_*` (≈ 1–4 vs. tolerance 0.5)
- [ ] Compatibility tests against the full libmypaint brush pack

### Backends
- [ ] **`hokusai-tiny-skia`** — flatten tiles into a `tiny-skia` `Pixmap`
- [x] **`hokusai-wasm`** — `wasm-bindgen` JS bindings + browser demo

## Cargo features (umbrella `hokusai` crate)

| Feature     | Default | What it enables                              |
|-------------|---------|----------------------------------------------|
| `myb-json`  | ✅      | `.myb` JSON parser / serializer              |
| `tile-mem`  | ✅      | Reference `HashMap`-backed `TiledSurface`    |
| `tiny-skia` | —       | (planned) `tiny-skia` Pixmap backend         |
| `wasm`      | —       | (planned) `wasm-bindgen` JS bindings         |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build / test / snapshot workflow,
fixture conventions, and commit-message style.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Vendored brush fixtures under `hokusai/examples/fixtures/` are unmodified copies from [mypaint-brushes](https://github.com/mypaint/mypaint-brushes) (CC0 1.0).
