//! Pixel blending — port of libmypaint's `brushmodes.c`.
//!
//! Coordinate / color conventions:
//! - Tile pixels are RGBA **fix15** (`u16` in `[0, 32768]`), **premultiplied**
//!   alpha, **linear sRGB**.
//! - Dab position `(x, y)` is in world-pixel space (sub-pixel f32).
//! - Dab `color` is straight-alpha linear sRGB; we premultiply at blend time.
//!
//! This first cut implements `BlendMode_Normal_and_Eraser` only. Colorize,
//! Posterize, LockAlpha, and the spectral `paint` mode are TODOs for M3-followup.

#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

use crate::color::RgbaF32;
use crate::fix15::{self, FIX15_ONE};
use crate::surface::{Dab, TiledSurface};
use crate::tile::{TilePixels, TILE_SIZE};

/// Two-segment hardness falloff matching libmypaint's `calculate_opa`.
///
/// `rr` is squared normalized distance (`r² / radius²`). Returns 0 outside
/// the dab. At `rr = hardness` the two segments meet at value `hardness`,
/// giving a smooth ramp from 1 at center to 0 at the edge.
#[inline]
fn opa_at(rr: f32, hardness: f32) -> f32 {
    if rr > 1.0 {
        return 0.0;
    }
    if hardness >= 1.0 {
        // Solid disk; no falloff.
        return if rr <= 1.0 { 1.0 } else { 0.0 };
    }
    if hardness <= 0.0 {
        // Degenerate — libmypaint treats as infinitely soft (linear from 1→0).
        return 1.0 - rr;
    }
    if rr <= hardness {
        // segment1: 1 + rr*(1 - 1/hardness)
        1.0 + rr * (1.0 - 1.0 / hardness)
    } else {
        // segment2: hardness/(1-hardness) * (1 - rr)
        (hardness / (1.0 - hardness)) * (1.0 - rr)
    }
}

/// Compute squared normalized distance from dab center, accounting for
/// `aspect_ratio` (≥1) and `angle` (degrees).
#[inline]
fn rr_at(px: f32, py: f32, x: f32, y: f32, aspect: f32, cs: f32, sn: f32, inv_r2: f32) -> f32 {
    // libmypaint uses pixel center coordinates (px + 0.5, py + 0.5).
    let yy = py + 0.5 - y;
    let xx = px + 0.5 - x;
    let yyr = (yy * cs - xx * sn) * aspect;
    let xxr = yy * sn + xx * cs;
    (yyr * yyr + xxr * xxr) * inv_r2
}

/// Render `dab` into `surface`. Returns whether any pixel was modified.
///
/// This is the function `TiledSurface::draw_dab` defaults to.
pub fn draw_dab_default<S: TiledSurface + ?Sized>(surface: &mut S, dab: &Dab) -> bool {
    let radius = dab.radius.max(0.5);
    let aspect = dab.aspect_ratio.max(1.0);
    let angle = dab.angle.to_radians();
    let cs = angle.cos();
    let sn = angle.sin();
    let inv_r2 = 1.0 / (radius * radius);
    // Anti-aliasing band in rr-space (rr is r² normalized). ~1 px feather
    // at the dab edge when `anti_aliasing` is 1.0, scaled by radius.
    let aa_band = (dab.anti_aliasing.clamp(0.0, 1.0) * 2.0) / radius;

    // Conservative AABB: enlarge by aspect_ratio so the rotated ellipse fits.
    let r_ext = radius * aspect + 1.0;
    let x0 = (dab.x - r_ext).floor() as i32;
    let y0 = (dab.y - r_ext).floor() as i32;
    let x1 = (dab.x + r_ext).ceil() as i32;
    let y1 = (dab.y + r_ext).ceil() as i32;

    let tx0 = x0.div_euclid(TILE_SIZE as i32);
    let ty0 = y0.div_euclid(TILE_SIZE as i32);
    let tx1 = x1.div_euclid(TILE_SIZE as i32);
    let ty1 = y1.div_euclid(TILE_SIZE as i32);

    // Premultiplied source color in fix15 (premultiplied by opaque * eraser
    // happens per pixel because the mask varies; only the base color is set
    // here, in straight-alpha form).
    let src = clamp_color(dab.color);
    let src_r = (src.r * FIX15_ONE as f32) as u32;
    let src_g = (src.g * FIX15_ONE as f32) as u32;
    let src_b = (src.b * FIX15_ONE as f32) as u32;
    let alpha_eraser_f = dab.alpha_eraser.clamp(0.0, 1.0);
    let opaque_f = dab.opaque.clamp(0.0, 1.0);
    let hardness = dab.hardness.clamp(0.0, 1.0);

    let mut painted = false;
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            // Tile origin in world space.
            let ox = tx * TILE_SIZE as i32;
            let oy = ty * TILE_SIZE as i32;

            // Intersection of dab bbox with this tile, in tile-local coords.
            let lx0 = (x0 - ox).max(0) as usize;
            let ly0 = (y0 - oy).max(0) as usize;
            let lx1 = (x1 - ox).min(TILE_SIZE as i32 - 1) as usize;
            let ly1 = (y1 - oy).min(TILE_SIZE as i32 - 1) as usize;
            if lx0 > lx1 || ly0 > ly1 {
                continue;
            }

            let tile = surface.tile_request_start(tx, ty);
            let touched = paint_into_tile(
                tile,
                ox,
                oy,
                lx0,
                ly0,
                lx1,
                ly1,
                dab.x,
                dab.y,
                aspect,
                cs,
                sn,
                inv_r2,
                hardness,
                opaque_f,
                alpha_eraser_f,
                aa_band,
                dab.lock_alpha.clamp(0.0, 1.0),
                dab.posterize.clamp(0.0, 1.0),
                dab.posterize_num.max(1.0),
                src_r,
                src_g,
                src_b,
            );
            surface.tile_request_end(tx, ty);
            painted |= touched;
        }
    }
    painted
}

#[allow(clippy::too_many_arguments)]
fn paint_into_tile(
    tile: &mut TilePixels,
    ox: i32,
    oy: i32,
    lx0: usize,
    ly0: usize,
    lx1: usize,
    ly1: usize,
    cx: f32,
    cy: f32,
    aspect: f32,
    cs: f32,
    sn: f32,
    inv_r2: f32,
    hardness: f32,
    opaque: f32,
    alpha_eraser: f32,
    aa_band: f32,
    lock_alpha: f32,
    posterize: f32,
    posterize_num: f32,
    src_r: u32,
    src_g: u32,
    src_b: u32,
) -> bool {
    let mut painted = false;
    let aa_edge = 1.0 + aa_band;
    let do_posterize = posterize > 0.0;
    let pnum = posterize_num.round().max(1.0);
    let posterize_fix15 = (posterize * FIX15_ONE as f32) as u32;

    for ly in ly0..=ly1 {
        let py = (oy + ly as i32) as f32;
        for lx in lx0..=lx1 {
            let px = (ox + lx as i32) as f32;
            let rr = rr_at(px, py, cx, cy, aspect, cs, sn, inv_r2);
            if rr >= aa_edge {
                continue;
            }
            // Smooth the outer aa_band-wide ring linearly from full opa→0.
            let mut opa = if aa_band > 0.0 && rr > 1.0 - aa_band {
                let inner = opa_at((1.0 - aa_band).max(0.0), hardness);
                inner * ((aa_edge - rr) / (2.0 * aa_band)).clamp(0.0, 1.0)
            } else if rr <= 1.0 {
                opa_at(rr, hardness)
            } else {
                0.0
            };
            opa *= opaque;
            if opa <= 0.0 {
                continue;
            }

            // fix15 mask values.
            let mask = (opa.clamp(0.0, 1.0) * FIX15_ONE as f32) as u32;
            let opa_alpha_raw = fix15::mul(mask, (alpha_eraser * FIX15_ONE as f32) as u32);
            let inv_mask = FIX15_ONE - mask;

            let dst = &mut tile[ly][lx];
            let dr = dst[0] as u32;
            let dg = dst[1] as u32;
            let db = dst[2] as u32;
            let da = dst[3] as u32;

            // Lock alpha: when set, the dab is masked by the existing alpha
            // (so only previously-painted areas get coloured) and dst.a is
            // unchanged. Blend smoothly via `lock_alpha`.
            let (color_opa_alpha, write_alpha) = if lock_alpha > 0.0 {
                let locked = fix15::mul(opa_alpha_raw, da);
                let blended = lerp_fix15(opa_alpha_raw, locked, lock_alpha);
                (blended, lock_alpha < 1.0)
            } else {
                (opa_alpha_raw, true)
            };

            dst[0] = blend(dr, inv_mask, src_r, color_opa_alpha);
            dst[1] = blend(dg, inv_mask, src_g, color_opa_alpha);
            dst[2] = blend(db, inv_mask, src_b, color_opa_alpha);
            if write_alpha {
                dst[3] = blend(da, inv_mask, FIX15_ONE, opa_alpha_raw);
            }

            // Posterize: snap each channel toward (round(c * N) / N) by
            // `posterize` amount. Applied in straight alpha — convert,
            // quantize, convert back.
            if do_posterize {
                posterize_pixel(dst, pnum, posterize_fix15);
                painted = true;
                continue;
            }
            painted = true;
        }
    }
    painted
}

/// Linear interpolation in fix15 space: `a*(1-t) + b*t`.
#[inline]
fn lerp_fix15(a: u32, b: u32, t: f32) -> u32 {
    let t_fix = (t.clamp(0.0, 1.0) * FIX15_ONE as f32) as u32;
    fix15::mul(a, FIX15_ONE - t_fix) + fix15::mul(b, t_fix)
}

/// Posterize `pixel` toward `pnum` quantization levels by `amount` (fix15).
fn posterize_pixel(pixel: &mut [u16; 4], pnum: f32, amount: u32) {
    let a = pixel[3] as f32 / FIX15_ONE as f32;
    if a <= 0.0 {
        return;
    }
    for ch in 0..3 {
        let c_premul = pixel[ch] as f32 / FIX15_ONE as f32;
        let c_straight = (c_premul / a).clamp(0.0, 1.0);
        let q = (c_straight * pnum).round() / pnum;
        let blended = c_straight * (1.0 - amount as f32 / FIX15_ONE as f32)
            + q * (amount as f32 / FIX15_ONE as f32);
        let new_premul = (blended * a).clamp(0.0, 1.0);
        pixel[ch] = (new_premul * FIX15_ONE as f32) as u16;
    }
}

/// `(dst * inv_mask + src_premul_channel * opa_alpha) >> 15` with libmypaint's
/// half-step rounding, clamped to u16 (FIX15_ONE).
#[inline]
fn blend(dst: u32, inv_mask: u32, src_channel: u32, opa_alpha: u32) -> u16 {
    let s_contrib = fix15::mul(src_channel, opa_alpha);
    let d_contrib = fix15::mul(dst, inv_mask);
    let sum = s_contrib + d_contrib;
    if sum > FIX15_ONE {
        FIX15_ONE as u16
    } else {
        sum as u16
    }
}

#[inline]
fn clamp_color(c: RgbaF32) -> RgbaF32 {
    RgbaF32 {
        r: c.r.clamp(0.0, 1.0),
        g: c.g.clamp(0.0, 1.0),
        b: c.b.clamp(0.0, 1.0),
        a: c.a.clamp(0.0, 1.0),
    }
}

/// Average color in a circle of `radius` around `(x, y)`, mask-weighted
/// with the same falloff a hardness=0.5 dab produces. Uses
/// [`TiledSurface::tile_lookup`] for read-only sampling; backends that
/// don't implement that get a transparent result.
pub fn get_color_default<S: TiledSurface + ?Sized>(
    surface: &S,
    x: f32,
    y: f32,
    radius: f32,
) -> RgbaF32 {
    let radius = radius.max(0.5);
    let inv_r2 = 1.0 / (radius * radius);
    let r_ext = radius + 1.0;
    let x0 = (x - r_ext).floor() as i32;
    let y0 = (y - r_ext).floor() as i32;
    let x1 = (x + r_ext).ceil() as i32;
    let y1 = (y + r_ext).ceil() as i32;
    let tx0 = x0.div_euclid(TILE_SIZE as i32);
    let ty0 = y0.div_euclid(TILE_SIZE as i32);
    let tx1 = x1.div_euclid(TILE_SIZE as i32);
    let ty1 = y1.div_euclid(TILE_SIZE as i32);

    let mut sum_r = 0.0f32;
    let mut sum_g = 0.0f32;
    let mut sum_b = 0.0f32;
    let mut sum_a = 0.0f32;
    let mut sum_w = 0.0f32;

    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let Some(tile) = surface.tile_lookup(tx, ty) else {
                continue;
            };
            let ox = tx * TILE_SIZE as i32;
            let oy = ty * TILE_SIZE as i32;
            let lx0 = (x0 - ox).max(0) as usize;
            let ly0 = (y0 - oy).max(0) as usize;
            let lx1 = (x1 - ox).min(TILE_SIZE as i32 - 1) as usize;
            let ly1 = (y1 - oy).min(TILE_SIZE as i32 - 1) as usize;
            if lx0 > lx1 || ly0 > ly1 {
                continue;
            }
            for ly in ly0..=ly1 {
                let py = (oy + ly as i32) as f32;
                for lx in lx0..=lx1 {
                    let px = (ox + lx as i32) as f32;
                    let rr = rr_at(px, py, x, y, 1.0, 1.0, 0.0, inv_r2);
                    if rr > 1.0 {
                        continue;
                    }
                    let w = opa_at(rr, 0.5);
                    let p = tile[ly][lx];
                    sum_r += fix15::to_f32(p[0]) * w;
                    sum_g += fix15::to_f32(p[1]) * w;
                    sum_b += fix15::to_f32(p[2]) * w;
                    sum_a += fix15::to_f32(p[3]) * w;
                    sum_w += w;
                }
            }
        }
    }
    if sum_w <= 0.0 {
        return RgbaF32::TRANSPARENT;
    }
    // Tile pixels are premultiplied; un-premultiply for callers that want
    // straight-alpha (libmypaint's smudge sampler does the same).
    let a = sum_a / sum_w;
    if a <= 0.0 {
        return RgbaF32::TRANSPARENT;
    }
    RgbaF32 {
        r: (sum_r / sum_w) / a,
        g: (sum_g / sum_w) / a,
        b: (sum_b / sum_w) / a,
        a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opa_endpoints() {
        // Center is fully covered; rr=1 is the edge.
        assert!((opa_at(0.0, 0.5) - 1.0).abs() < 1e-6);
        assert!(opa_at(1.0, 0.5).abs() < 1e-6);
        // At rr=hardness the two segments meet at value `hardness`.
        assert!((opa_at(0.5, 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn hardness_1_is_solid_disk() {
        assert_eq!(opa_at(0.0, 1.0), 1.0);
        assert_eq!(opa_at(0.99, 1.0), 1.0);
        assert_eq!(opa_at(1.01, 1.0), 0.0);
    }

    #[test]
    fn rr_increases_with_distance() {
        // Center exactly at pixel center (0.5, 0.5), radius² = 1.
        let near = rr_at(0.0, 0.0, 0.5, 0.5, 1.0, 1.0, 0.0, 1.0);
        let far = rr_at(2.0, 0.0, 0.5, 0.5, 1.0, 1.0, 0.0, 1.0);
        assert!(far > near);
    }
}
