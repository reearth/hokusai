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
- [x] **Speed slowness low-pass** (`speed1_slowness`, `speed2_slowness`) — `1 - exp(-step_dtime / slow)`, advanced per dab
- [x] **`speed1_gamma` / `speed2_gamma`** mapping (`log(gamma + speed) * m + q` with libmypaint's fix-points)
- [x] **Tilt-derived inputs** (`tilt_declination` defaults to 90°, `tilt_ascension`)
- [x] **Per-dab interpolation** of pressure / speed inside a segment
- [x] **Per-iteration `count_dabs_to`** — re-counted after every dab against the freshly advanced state
- [x] **`slow_tracking_per_dab`** — lags the dab centre behind the slow-tracked cursor
- [x] **`opaque_linearize`** — compensates per-dab alpha for overlap
- [x] **`offset_by_random`** / **`offset_by_speed`** dab position jitter (libmypaint-correct scale)
- [x] **`tracking_noise`** — gaussian jitter added to the raw input before slow_tracking, scaled by `base_radius`
- [x] **`attack_angle`** input — smallest angular difference between pen ascension and stroke direction + 90°
- [x] **`Stroke` input** + `stroke_duration_logarithmic` + `stroke_holdtime` + `stroke_threshold` start/end gating
- [x] **Gridmap inputs** (`gridmap_x`, `gridmap_y`) sampled from `STATE.ACTUAL_X/Y` via `gridmap_scale[_x/_y]`
- [x] **Extra inputs** (`tilt_declinationx`, `tilt_declinationy`, `viewzoom`, `barrel_rotation`, `brush_radius`)
- [x] **Custom input** — `custom_input_slowness` smooths `SETTING(custom_input)` into `INPUT(CUSTOM)` so curves can chain lagged outputs back in
- [x] **Direction smoothing** — `direction_filter` low-pass on `STATE.DIRECTION_*` and `DIRECTION_ANGLE_*`, with the 180°-folded variant for 1D direction curves
- [x] **Offset settings** (`offset_x`, `offset_y`, `offset_angle*`, `offset_multiplier`) — full `directional_offsets` port with `STATE.FLIP` mirroring
- [x] **`tracking_noise` skip coalescing** — events drop into a `skip_distance` window so jitter samples track cursor distance, not input frequency
- [x] **`radius_by_random`** — per-dab radius gaussian jitter with libmypaint's `(orig / new)² opacity correction
- [x] **`apply_smudge` / `eraser_target_alpha`** — smudge bucket's alpha drives the dab's source alpha so blender/smear/watercolor brushes thin out instead of bulldozing
- [x] **Spectral `mix_colors`** — smudge bucket update + apply both run through the libmypaint WGM mix when `paint_mode > 0`

### Pixel blending
- [x] **Colorize** — replace dst hue/sat with the dab's, keep dst value
- [x] **Spectral `paint` mode** — 10-channel pigment WGM blending (libmypaint `BlendMode_Normal_and_Eraser_Paint`), with the `spectral_blend_factor` sigmoid fading to additive at low canvas alpha. Brush JSON's `paint_mode` now binds to the engine setting; the previous `paint` cname was stashing every pigment brush as an unknown setting.
- [x] **`change_color_hsl_s`** / **`change_color_l`** — HSL-space colour drift
- [x] Direct `tile_lookup`-free `get_color` path — backends override `TiledSurface::get_color` and forward to `brushmodes::get_color_via_sample` with a per-pixel reader closure
- [x] **Posterize as its own pass** — runs after Normal + Paint so `paint_mode = 1` brushes still posterize; `posterize_num` JSON value multiplied by 100 and clamped to [1, 128] per libmypaint
- [x] **Spectral `get_color`** when `paint_mode > 0` — `Surface2::get_color_pigment` ported: mask-weighted running WGM in spectral space, optionally blended with the alpha-weighted linear average by `paint`. Smudge update calls it whenever the brush's `paint_mode` is non-zero.
- [x] **Smudge lazy resample** — `smudge_length_log`-gated canvas re-sample via `PREV_COL_RECENTNESS` counter; the cached sample is reused while recentness stays above the libmypaint threshold.
- [x] **`smudge_transparency` rejection** — sampled-alpha-gate around the dab so transparent-canvas-only / opaque-canvas-only smudge brushes behave like libmypaint.

### Compatibility
- [x] **libmypaint-sourced golden snapshots** — `tools/libmypaint-render/` is a small C wrapper around `mypaint_brush_stroke_to`, and `cargo xtask regenerate-goldens` drives it across the fixture set so `crates/hokusai-compat/fixtures/*.png` is upstream output. `cargo xtask parity-report` renders a side-by-side HTML diff for eyeballing the parity surface
- [x] **Knuth lagged Fibonacci PRNG** — port of libmypaint's `rng-double.c` (TAOCP 3.6-15) with the same `rand_gauss` scaling (`sum*√3 − 2√3`) and per-dab `random_input` refresh order. Seeding mirrors `rng_double_new(1000)`.
- [x] **Lossless round-trip** for unknown top-level `.myb` settings (unknown inputs *inside* a known setting are still dropped)
- [x] **Brush-pack parity tool** — `cargo xtask brush-pack-report` walks `tmp/mypaint-brushes/` (override via `HOKUSAI_BRUSH_PACK`), drives every `.myb` through a fixed pressure-ramp curve in both libmypaint and hokusai (via the Surface2 path so `paint_mode` brushes get real spectral blending on both sides), and writes a sortable Markdown table of per-brush MAD to `tmp/brush-pack-report.md`. Current state: **120 of 196** stock brushes pass MAD ≤ 0.50; another ~55 sit in the amber band (≤ 5.0). Remaining red brushes are mostly RNG-heavy scatter / particle brushes whose dab placements diverge from libmypaint's sequence even when each formula matches.

### Backends
- [x] **`hokusai-tiny-skia`** — flatten any `TiledSurface` into a `tiny_skia::Pixmap` (over-white or transparent variants), with a `hokusai_compat::render` parity test
- [x] **`hokusai-wasm`** — `wasm-bindgen` JS bindings + browser demo

## Cargo features (umbrella `hokusai` crate)

| Feature     | Default | What it enables                              |
|-------------|---------|----------------------------------------------|
| `myb-json`  | ✅      | `.myb` JSON parser / serializer              |
| `tile-mem`  | ✅      | Reference `HashMap`-backed `TiledSurface`    |
| `tiny-skia` | —       | `tiny-skia` Pixmap flattening helpers         |

(`hokusai-wasm` ships as its own `cdylib` crate rather than an umbrella feature — point your `wasm-pack` at `crates/hokusai-wasm` directly.)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build / test / snapshot workflow,
fixture conventions, and commit-message style.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Vendored brush fixtures under `hokusai/examples/fixtures/` are unmodified copies from [mypaint-brushes](https://github.com/mypaint/mypaint-brushes) (CC0 1.0).
