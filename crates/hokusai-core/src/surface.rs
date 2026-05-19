//! `TiledSurface` abstraction and the `Dab` description passed to it.
//!
//! Backends only need to implement tile lending; `draw_dab` and `get_color`
//! ship as default implementations so every backend gets identical pixels.
//! The defaults cover libmypaint's Normal+Eraser, Colorize, Posterize, and
//! LockAlpha blend modes; the spectral `paint` mode is still deferred.

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
    pub anti_aliasing: f32,
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
    ///
    /// Default impl applies libmypaint's Normal+Eraser blend in linear sRGB
    /// fix15, plus colorize / posterize / lock_alpha when those dab fields
    /// are non-zero. The spectral `paint` mode is still deferred.
    fn draw_dab(&mut self, dab: &Dab) -> bool {
        crate::brushmodes::draw_dab_default(self, dab)
    }

    /// Read-only tile lookup, used by the default `get_color` and by any
    /// caller that wants to inspect the canvas without dirtying tiles.
    fn tile_lookup(&self, _tx: i32, _ty: i32) -> Option<&TilePixels> {
        None
    }

    /// Average color within `radius` of `(x, y)`. Used by smudge / color picker.
    /// Backends that don't implement `tile_lookup` will get transparent.
    fn get_color(&self, x: f32, y: f32, radius: f32) -> RgbaF32 {
        crate::brushmodes::get_color_default(self, x, y, radius)
    }
}
