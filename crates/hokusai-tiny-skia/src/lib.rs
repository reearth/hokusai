//! Flatten a hokusai [`TiledSurface`] into a [`tiny_skia::Pixmap`].
//!
//! Hokusai keeps its canvas as 64×64 RGBA `fix15` tiles with linear-sRGB,
//! premultiplied alpha (matching libmypaint). `tiny-skia` expects 8-bit
//! sRGB, premultiplied alpha. This crate is the conversion bridge.
//!
//! Two helpers are provided:
//! - [`flatten_over_white`] — composites the surface over an opaque white
//!   background. Useful for "save this brush stroke as a PNG" workflows
//!   where transparency would otherwise eat the strokes' lighter regions.
//! - [`flatten_transparent`] — keeps the surface's own alpha so the
//!   pixmap can be over-composited onto something else later.
//!
//! Both helpers walk world-space pixels `[0, width) × [0, height)` and
//! look tiles up via the surface's `tile_lookup` impl. Backends without
//! `tile_lookup` will just yield a transparent (or fully white) pixmap.
//!
//! ```ignore
//! use hokusai_core::TiledSurface;
//! use hokusai_tile_mem::MemSurface;
//! # fn render<S: TiledSurface>(_: &mut S) {}
//! let mut surf = MemSurface::new();
//! render(&mut surf);
//! let pixmap = hokusai_tiny_skia::flatten_over_white(&surf, 320, 120);
//! pixmap.save_png("out.png").unwrap(); // requires tiny-skia's png-format feature
//! ```

use hokusai_core::color::linear_to_srgb;
use hokusai_core::fix15::FIX15_ONE;
use hokusai_core::tile::TILE_SIZE;
use hokusai_core::TiledSurface;

use tiny_skia::{Pixmap, PremultipliedColorU8};

/// Composite the surface over an opaque white background and return the
/// resulting sRGB-encoded, premultiplied `Pixmap`. The output never has
/// translucent pixels: anywhere the brush didn't paint reads as pure
/// white at full opacity.
pub fn flatten_over_white<S: TiledSurface + ?Sized>(
    surface: &S,
    width: u32,
    height: u32,
) -> Pixmap {
    flatten(surface, width, height, true)
}

/// Same as [`flatten_over_white`] but keeps the surface's straight alpha
/// channel, so unpainted pixels stay fully transparent and partially
/// painted ones blend correctly when composited later.
pub fn flatten_transparent<S: TiledSurface + ?Sized>(
    surface: &S,
    width: u32,
    height: u32,
) -> Pixmap {
    flatten(surface, width, height, false)
}

fn flatten<S: TiledSurface + ?Sized>(
    surface: &S,
    width: u32,
    height: u32,
    over_white: bool,
) -> Pixmap {
    let mut pixmap =
        Pixmap::new(width, height).expect("non-zero width/height for tiny-skia Pixmap");
    if over_white {
        // Pre-fill so untouched tiles render as opaque white instead of
        // transparent (which `tiny-skia` would otherwise leave premul-zero).
        pixmap.fill(tiny_skia::Color::WHITE);
    }
    let pixels = pixmap.pixels_mut();

    let ts = TILE_SIZE as i32;
    let tiles_x = width.div_ceil(TILE_SIZE as u32) as i32;
    let tiles_y = height.div_ceil(TILE_SIZE as u32) as i32;
    let fix15 = FIX15_ONE as f32;

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let Some(tile) = surface.tile_lookup(tx, ty) else {
                continue;
            };
            #[allow(clippy::needless_range_loop)]
            // pixel coords feed wx/wy bounds checks; iterator rewrite loses clarity
            for ly in 0..TILE_SIZE {
                let wy = ty * ts + ly as i32;
                if wy < 0 || wy >= height as i32 {
                    continue;
                }
                #[allow(clippy::needless_range_loop)]
                for lx in 0..TILE_SIZE {
                    let wx = tx * ts + lx as i32;
                    if wx < 0 || wx >= width as i32 {
                        continue;
                    }
                    let p = tile[ly][lx];
                    let a = p[3] as f32 / fix15;
                    if a <= 0.0 && !over_white {
                        // Pixmap was freshly zeroed; nothing to write.
                        continue;
                    }

                    let idx = (wy as u32 * width + wx as u32) as usize;
                    if over_white {
                        // Composite over opaque white in linear space.
                        let r = p[0] as f32 / fix15;
                        let g = p[1] as f32 / fix15;
                        let b = p[2] as f32 / fix15;
                        // `p` is already premultiplied, so `r + (1-a)*1`
                        // gives the linear-sRGB result over white.
                        let or_ = r + (1.0 - a);
                        let og = g + (1.0 - a);
                        let ob = b + (1.0 - a);
                        pixels[idx] = PremultipliedColorU8::from_rgba(
                            srgb_byte(or_),
                            srgb_byte(og),
                            srgb_byte(ob),
                            255,
                        )
                        .expect("alpha 255 is valid premul");
                    } else {
                        // Keep straight alpha. We have linear-premul `(r,
                        // g, b, a)`; tiny-skia stores sRGB-premul, so go
                        // linear → straight → sRGB → premul-sRGB.
                        let r_straight = (p[0] as f32 / fix15) / a;
                        let g_straight = (p[1] as f32 / fix15) / a;
                        let b_straight = (p[2] as f32 / fix15) / a;
                        let a8 = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
                        let r8 = srgb_byte(r_straight);
                        let g8 = srgb_byte(g_straight);
                        let b8 = srgb_byte(b_straight);
                        // Premultiply *after* sRGB encoding (tiny-skia's
                        // convention).
                        let rp = ((r8 as u16 * a8 as u16 + 127) / 255) as u8;
                        let gp = ((g8 as u16 * a8 as u16 + 127) / 255) as u8;
                        let bp = ((b8 as u16 * a8 as u16 + 127) / 255) as u8;
                        pixels[idx] = PremultipliedColorU8::from_rgba(rp, gp, bp, a8)
                            .expect("premultiplied bytes are by construction <= alpha");
                    }
                }
            }
        }
    }
    pixmap
}

#[inline]
fn srgb_byte(v: f32) -> u8 {
    (linear_to_srgb(v.clamp(0.0, 1.0)).clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use hokusai_core::fix15::FIX15_ONE;
    use hokusai_core::tile::TILE_SIZE;
    use hokusai_tile_mem::MemSurface;

    /// An untouched surface flattened over white should be pure white
    /// everywhere with full alpha — and `flatten_transparent` should
    /// give a fully zeroed Pixmap.
    #[test]
    fn empty_surface_renders_white_or_transparent() {
        let surf = MemSurface::new();
        let pm_w = flatten_over_white(&surf, 4, 4);
        for px in pm_w.pixels() {
            assert_eq!(px.red(), 255);
            assert_eq!(px.green(), 255);
            assert_eq!(px.blue(), 255);
            assert_eq!(px.alpha(), 255);
        }
        let pm_t = flatten_transparent(&surf, 4, 4);
        for px in pm_t.pixels() {
            assert_eq!(px.alpha(), 0);
        }
    }

    /// A tile with a single fully opaque red pixel at (0, 0) should land
    /// at the matching world coordinate.
    #[test]
    fn single_painted_pixel_round_trips() {
        let mut surf = MemSurface::new();
        surf.begin_atomic();
        let tile = surf.tile_request_start(0, 0);
        tile[0][0] = [FIX15_ONE as u16, 0, 0, FIX15_ONE as u16];
        surf.tile_request_end(0, 0);
        let _ = surf.end_atomic();

        let pm = flatten_over_white(&surf, TILE_SIZE as u32, TILE_SIZE as u32);
        let px = pm.pixels()[0];
        // sRGB(1.0) = 255 for the red channel; over white background.
        assert_eq!(px.red(), 255);
        assert_eq!(px.green(), 0);
        assert_eq!(px.blue(), 0);
        assert_eq!(px.alpha(), 255);
    }
}
