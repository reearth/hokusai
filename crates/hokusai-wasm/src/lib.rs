//! `wasm-bindgen` bindings for the hokusai brush engine.
//!
//! Exposes three JS-facing types:
//! - [`HokusaiBrush`] — parsed `.myb` brush
//! - [`HokusaiCanvas`] — owns the tile-backed surface plus a `BrushState`
//!   for the in-flight stroke, and produces RGBA8 pixel data for
//!   `ImageData.data.set()`
//!
//! Build with:
//! ```sh
//! wasm-pack build crates/hokusai-wasm --target web --release
//! ```
//! or invoke `wasm-bindgen` directly after `cargo build --target
//! wasm32-unknown-unknown --release -p hokusai-wasm`.

#![allow(clippy::needless_range_loop)]

use hokusai_brush as myb;
use hokusai_core::color::linear_to_srgb;
use hokusai_core::tile::TILE_SIZE;
use hokusai_core::{fix15, Brush, BrushState};
use hokusai_tile_mem::MemSurface;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct HokusaiBrush {
    inner: Brush,
}

#[wasm_bindgen]
impl HokusaiBrush {
    /// Parse a `.myb` JSON document.
    #[wasm_bindgen(constructor)]
    pub fn new(myb_json: &str) -> Result<HokusaiBrush, JsError> {
        let inner = myb::from_str(myb_json).map_err(|e| JsError::new(&e.to_string()))?;
        Ok(Self { inner })
    }

    /// Override the HSV base colour (each channel in [0, 1]).
    #[wasm_bindgen(js_name = setColorHsv)]
    pub fn set_color_hsv(&mut self, h: f32, s: f32, v: f32) {
        self.inner
            .get_mut(hokusai_core::BrushSetting::ColorH)
            .base_value = h;
        self.inner
            .get_mut(hokusai_core::BrushSetting::ColorS)
            .base_value = s;
        self.inner
            .get_mut(hokusai_core::BrushSetting::ColorV)
            .base_value = v;
    }

    /// Override the base radius (libmypaint's `radius_logarithmic`, log2 px).
    #[wasm_bindgen(js_name = setRadiusLog)]
    pub fn set_radius_log(&mut self, log2_radius: f32) {
        self.inner
            .get_mut(hokusai_core::BrushSetting::Radius)
            .base_value = log2_radius;
    }

    /// Read the brush's designed radius (its `radius_logarithmic` base
    /// value). Useful for UIs that want to offset from the natural size
    /// rather than override it outright.
    #[wasm_bindgen(js_name = radiusLog)]
    pub fn radius_log(&self) -> f32 {
        self.inner
            .get(hokusai_core::BrushSetting::Radius)
            .base_value
    }
}

/// A drawable canvas. Holds the tiled surface, an active [`BrushState`],
/// and the RGBA8 output buffer reused across `pixels()` calls.
#[wasm_bindgen]
pub struct HokusaiCanvas {
    surface: MemSurface,
    state: BrushState,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

#[wasm_bindgen]
impl HokusaiCanvas {
    #[wasm_bindgen(constructor)]
    pub fn new(width: u32, height: u32) -> HokusaiCanvas {
        let mut pixels = vec![255u8; (width * height * 4) as usize];
        // Fill alpha = 255 explicitly (already done by `vec![255; …]`, but be
        // explicit about it for readers).
        for px in pixels.chunks_exact_mut(4) {
            px[0] = 255;
            px[1] = 255;
            px[2] = 255;
            px[3] = 255;
        }
        Self {
            surface: MemSurface::new(),
            state: BrushState::default(),
            width,
            height,
            pixels,
        }
    }

    /// Flush `slow_tracking` lag so the stroke's trailing pixels are painted.
    /// Call on pointer-up *before* resetting.
    #[wasm_bindgen(js_name = finishStroke)]
    pub fn finish_stroke(&mut self, brush: &HokusaiBrush) {
        brush
            .inner
            .finish_stroke(&mut self.state, &mut self.surface);
    }

    /// End the current stroke so the next `strokeTo` starts fresh.
    #[wasm_bindgen(js_name = resetStroke)]
    pub fn reset_stroke(&mut self) {
        self.state.reset();
    }

    /// Drop all painted tiles and reset stroke state. Cheaper and safer than
    /// reconstructing the canvas object on the JS side.
    pub fn clear(&mut self) {
        self.surface = MemSurface::new();
        self.state.reset();
    }

    /// Feed one pointer event. `dtime` is seconds since the previous call.
    ///
    /// `xtilt` / `ytilt` are pen tilt in [-1, 1], matching libmypaint's
    /// convention. Pass 0 for devices that don't report tilt.
    #[wasm_bindgen(js_name = strokeTo)]
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_to(
        &mut self,
        brush: &HokusaiBrush,
        x: f32,
        y: f32,
        pressure: f32,
        xtilt: f32,
        ytilt: f32,
        dtime: f64,
    ) {
        brush.inner.stroke_to(
            &mut self.state,
            &mut self.surface,
            x,
            y,
            pressure,
            xtilt,
            ytilt,
            dtime,
        );
    }

    /// Return the canvas as RGBA8 in sRGB, composited over white.
    /// JS copies via `ImageData.data.set(canvas.pixels())`. wasm-bindgen
    /// can't return a borrowed slice across the JS boundary, so the
    /// `Vec<u8>` materialisation is forced; the underlying `flatten` writes
    /// in-place into the canvas's own buffer first, so the per-frame cost
    /// is one memcpy into the JS Uint8Array.
    pub fn pixels(&mut self) -> Vec<u8> {
        flatten(&self.surface, self.width, self.height, &mut self.pixels);
        self.pixels.clone()
    }

    /// Pointer + length accessors for callers that want to skip the
    /// `pixels()` copy entirely: JS can construct
    /// `new Uint8Array(memory.buffer, canvas.pixels_ptr(),
    /// canvas.pixels_len())` and `ImageData.data.set` directly from
    /// wasm memory. Call `flush_pixels()` first to refresh the buffer.
    pub fn flush_pixels(&mut self) {
        flatten(&self.surface, self.width, self.height, &mut self.pixels);
    }

    #[wasm_bindgen(js_name = pixelsPtr)]
    pub fn pixels_ptr(&self) -> *const u8 {
        self.pixels.as_ptr()
    }

    #[wasm_bindgen(js_name = pixelsLen)]
    pub fn pixels_len(&self) -> usize {
        self.pixels.len()
    }

    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }
}

fn flatten(surface: &MemSurface, w: u32, h: u32, out: &mut [u8]) {
    // Reset to white.
    for px in out.chunks_exact_mut(4) {
        px[0] = 255;
        px[1] = 255;
        px[2] = 255;
        px[3] = 255;
    }
    let ts = TILE_SIZE as i32;
    let tiles_x = (w as i32 + ts - 1) / ts;
    let tiles_y = (h as i32 + ts - 1) / ts;

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let Some(tile) = surface.tile(tx, ty) else {
                continue;
            };
            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    let wx = tx * ts + lx as i32;
                    let wy = ty * ts + ly as i32;
                    if wx < 0 || wy < 0 || wx >= w as i32 || wy >= h as i32 {
                        continue;
                    }
                    let p = tile[ly][lx];
                    let a = fix15::to_f32(p[3]);
                    if a <= 0.0 {
                        continue;
                    }
                    let r = fix15::to_f32(p[0]) / a;
                    let g = fix15::to_f32(p[1]) / a;
                    let b = fix15::to_f32(p[2]) / a;
                    let or = r * a + 1.0 * (1.0 - a);
                    let og = g * a + 1.0 * (1.0 - a);
                    let ob = b * a + 1.0 * (1.0 - a);
                    let idx = ((wy as u32 * w + wx as u32) * 4) as usize;
                    out[idx] = (linear_to_srgb(or).clamp(0.0, 1.0) * 255.0).round() as u8;
                    out[idx + 1] = (linear_to_srgb(og).clamp(0.0, 1.0) * 255.0).round() as u8;
                    out[idx + 2] = (linear_to_srgb(ob).clamp(0.0, 1.0) * 255.0).round() as u8;
                    out[idx + 3] = 255;
                }
            }
        }
    }
}

// Panic hook: only meaningful in the browser, so gate the whole thing
// to wasm32 to keep native test builds lint-clean.
#[cfg(all(target_arch = "wasm32", not(feature = "small-panic")))]
#[wasm_bindgen(start)]
pub fn start() {
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = console)]
        fn error(s: &str);
    }
    std::panic::set_hook(Box::new(|info| {
        error(&format!("hokusai panic: {info}"));
    }));
}
