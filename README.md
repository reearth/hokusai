# Hokusai

A pure Rust brush engine inspired by [libmypaint](https://github.com/mypaint/libmypaint), designed for WebAssembly and native targets.

> **Status:** Early development (M1). Brush data types, `.myb` JSON I/O, tiled-surface abstraction, and an in-memory reference surface are in place. The stroke engine itself (M2) and pixel blending (M3) are not yet implemented.

## Goals

- **Pure Rust, no `unsafe`** — clean WASM (`wasm32-unknown-unknown`) story.
- **libmypaint `.myb` JSON compatibility** — brushes authored for MyPaint / Krita load and round-trip without translation.
- **Pixel-level parity with libmypaint** — same fix15 math, same tile layout, same stroke math. Compatibility is the design priority; the "Hokusai" name does not imply behavioral divergence.
- **Pluggable surfaces** via the `TiledSurface` trait. Backends are split into feature-gated crates (`tile-mem`, `tiny-skia` planned, …).
- **Tile-based infinite canvas** — 64×64 RGBA fix15 tiles, matching libmypaint exactly so dab traversal and rounding stay bit-identical.

## Workspace layout

```
hokusai/
├── crates/
│   ├── hokusai-core/        # Brush types, state, fix15, tiles, surface trait
│   ├── hokusai-brush/       # libmypaint `.myb` JSON read / write
│   └── hokusai-tile-mem/    # Reference in-memory TiledSurface
└── hokusai/                 # Umbrella crate that re-exports the above via features
```

Planned crates: `hokusai-tiny-skia` (raster output), `hokusai-wasm` (`wasm-bindgen` glue), `hokusai-compat-tests` (libmypaint parity fixtures).

## Quick look

```rust
use hokusai::{Brush, BrushSetting};
use hokusai::myb;
use hokusai::tile_mem::MemSurface;

let json = std::fs::read_to_string("classic_pen.myb")?;
let brush: Brush = myb::from_str(&json)?;

assert_eq!(brush.get(BrushSetting::Radius).base_value, 2.5);

let mut _surface = MemSurface::new();
// brush.stroke_to(&mut state, &mut surface, x, y, pressure, xtilt, ytilt, dtime);
// ^ available once the M2 stroke engine lands.
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Cargo features (umbrella `hokusai` crate)

| Feature   | Default | What it enables                                  |
|-----------|---------|--------------------------------------------------|
| `myb-json`| ✅      | `.myb` JSON parser / serializer                  |
| `tile-mem`| ✅      | Reference `HashMap`-backed `TiledSurface`        |
| `tiny-skia` | —     | (planned) `tiny-skia` Pixmap backend             |
| `wasm`    | —       | (planned) `wasm-bindgen` JS bindings             |

## Roadmap

- **M1** — Types, `.myb` I/O, tile / surface scaffolding ✅
- **M2** — Stroke engine port (`mypaint-brush.c` → Rust) + GRand-compatible PRNG
- **M3** — `draw_dab` / `get_color` port (`brushmodes.c`) with pixel-parity tests
- **M4** — `tiny-skia` surface backend
- **M5** — `wasm-bindgen` bindings + browser demo
- **M6** — Spectral color mixing (`paint` setting), modern MyPaint additions

## Compatibility

Settings and input names use libmypaint's canonical string identifiers (see `BrushSetting::cname` and `BrushInput::cname`). Unknown keys are skipped on parse today; lossless round-trip for unrecognised settings is planned.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
