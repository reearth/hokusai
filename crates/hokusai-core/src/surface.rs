//! `TiledSurface` abstraction and the `Dab` description passed to it.
//!
//! Backends only need to implement tile lending; `draw_dab` and `get_color`
//! ship as default implementations so every backend gets identical pixels.
//! The defaults are TODO until the brushmodes port (M2/M3).

use crate::color::RgbaF32;
use crate::tile::TilePixels;

#[derive(Debug, Clone, Copy)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub color: RgbaF32, // linear, straight alpha at this boundary
    pub opaque: f32,
    pub hardness: f32,
    pub alpha_eraser: f32,
    pub aspect_ratio: f32,
    pub angle: f32, // degrees
    pub lock_alpha: f32,
    pub colorize: f32,
    pub posterize: f32,
    pub posterize_num: f32,
    pub paint: f32,
}

pub trait TiledSurface {
    fn tile_request_start(&mut self, tx: i32, ty: i32) -> &mut TilePixels;
    fn tile_request_end(&mut self, tx: i32, ty: i32);

    fn begin_atomic(&mut self) {}
    /// Returns the list of tiles modified since the last `begin_atomic`.
    fn end_atomic(&mut self) -> Vec<(i32, i32)> {
        Vec::new()
    }

    /// Render one dab. Returns whether any pixel was modified.
    /// Default impl will be provided in M3 (port of libmypaint's `brushmodes.c`).
    fn draw_dab(&mut self, _dab: &Dab) -> bool {
        // TODO(M3): port draw_dab_pixels_BlendMode_Normal_and_Eraser + variants.
        false
    }

    /// Average color within `radius` of `(x, y)`. Used by smudge / color picker.
    fn get_color(&self, _x: f32, _y: f32, _radius: f32) -> RgbaF32 {
        // TODO(M3): port get_color_pixels_accumulate.
        RgbaF32::TRANSPARENT
    }
}
