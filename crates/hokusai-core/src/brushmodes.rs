//! Pixel blending — port of libmypaint's `brushmodes.c`.
//!
//! Coordinate / color conventions:
//! - Tile pixels are RGBA **fix15** (`u16` in `[0, 32768]`), **premultiplied**
//!   alpha, **linear sRGB**.
//! - Dab position `(x, y)` is in world-pixel space (sub-pixel f32).
//! - Dab `color` is straight-alpha linear sRGB; we premultiply at blend time.
//!
//! Implements libmypaint's `BlendMode_Normal_and_Eraser` plus the
//! `Colorize` / `Posterize` / `LockAlpha` overlays. The spectral `paint`
//! pigment-mixing mode is still deferred.

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

/// libmypaint's `calculate_r_sample` — squared elliptical distance from
/// the dab centre (in pixel-relative coordinates, not normalised).
#[inline]
fn r_sample(x: f32, y: f32, aspect: f32, sn: f32, cs: f32) -> f32 {
    let yyr = (y * cs - x * sn) * aspect;
    let xxr = y * sn + x * cs;
    yyr * yyr + xxr * xxr
}

/// libmypaint's `sign_point_in_line`.
#[inline]
fn sign_point_in_line(px: f32, py: f32, vx: f32, vy: f32) -> f32 {
    (px - vx) * (-vy) - vx * (py - vy)
}

/// libmypaint's `closest_point_to_line` — orthogonal projection onto
/// the line through the origin spanned by `(lx, ly)`.
#[inline]
fn closest_point_to_line(lx: f32, ly: f32, px: f32, py: f32) -> (f32, f32) {
    let l2 = lx * lx + ly * ly;
    let dot = px * lx + py * ly;
    let t = dot / l2;
    (lx * t, ly * t)
}

/// libmypaint's `calculate_rr_antialiased`. Returns an AA-corrected
/// squared normalised distance for sub-pixel edge fading. Used for
/// dabs with `radius < 3` where the plain `rr_at` value would alias
/// hard at the dab boundary.
#[inline]
fn rr_at_aa(
    px: f32,
    py: f32,
    x: f32,
    y: f32,
    aspect: f32,
    cs: f32,
    sn: f32,
    inv_r2: f32,
    r_aa_start: f32,
) -> f32 {
    let pixel_right = x - px;
    let pixel_bottom = y - py;
    let pixel_center_x = pixel_right - 0.5;
    let pixel_center_y = pixel_bottom - 0.5;
    let pixel_left = pixel_right - 1.0;
    let pixel_top = pixel_bottom - 1.0;

    let (nearest_x, nearest_y, rr_near);
    if pixel_left < 0.0 && pixel_right > 0.0 && pixel_top < 0.0 && pixel_bottom > 0.0 {
        nearest_x = 0.0;
        nearest_y = 0.0;
        rr_near = 0.0;
    } else {
        let (nx, ny) = closest_point_to_line(cs, sn, pixel_center_x, pixel_center_y);
        nearest_x = nx.clamp(pixel_left, pixel_right);
        nearest_y = ny.clamp(pixel_top, pixel_bottom);
        let r_near = r_sample(nearest_x, nearest_y, aspect, sn, cs);
        rr_near = r_near * inv_r2;
    }

    if rr_near > 1.0 {
        return rr_near;
    }

    let center_sign = sign_point_in_line(pixel_center_x, pixel_center_y, cs, -sn);
    let rad_area_1 = (1.0_f32 / core::f32::consts::PI).sqrt();
    let (farthest_x, farthest_y) = if center_sign < 0.0 {
        (nearest_x - sn * rad_area_1, nearest_y + cs * rad_area_1)
    } else {
        (nearest_x + sn * rad_area_1, nearest_y - cs * rad_area_1)
    };

    let r_far = r_sample(farthest_x, farthest_y, aspect, sn, cs);
    let rr_far = r_far * inv_r2;

    if r_far < r_aa_start {
        return (rr_far + rr_near) * 0.5;
    }

    let visibility_near = 1.0 - rr_near;
    let delta = rr_far - rr_near;
    let delta2 = 1.0 + delta;
    let visibility_near_norm = visibility_near / delta2;
    1.0 - visibility_near_norm
}

/// Render `dab` into `surface`. Returns whether any pixel was modified.
///
/// This is the function `TiledSurface::draw_dab` defaults to.
pub fn draw_dab_default<S: TiledSurface + ?Sized>(surface: &mut S, dab: &Dab) -> bool {
    // libmypaint's draw_dab_internal rejects degenerate dabs outright —
    // mirror that so low-pressure ramps don't lay down a sub-pixel "smear"
    // hokusai used to render as a linear falloff. Without the hardness ≤ 0
    // check, AA pushes the brush into a regime libmypaint treats as a
    // no-op.
    if dab.radius < 0.1 || dab.hardness <= 0.0 || dab.opaque <= 0.0 {
        return false;
    }
    if std::env::var("HOKUSAI_TRACE_DABS").is_ok() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!(
            "  hok#{}: ({:9.4},{:9.4}) r={:8.5} hard={:7.5} opaq={:7.5} aspect={:7.4} ang={:8.3} paint={:4.2} rgb=({:8.5},{:8.5},{:8.5}) er={:7.5}",
            n, dab.x, dab.y, dab.radius, dab.hardness, dab.opaque,
            dab.aspect_ratio, dab.angle, dab.paint,
            dab.color.r, dab.color.g, dab.color.b, dab.alpha_eraser,
        );
    }
    // libmypaint rejects radius < 0.1 above and otherwise renders with
    // the actual radius (mypaint-tiled-surface.c:575). hokusai used to
    // floor at 0.5 here, which made very thin pencils render thicker.
    let radius = dab.radius;
    let aspect = dab.aspect_ratio.max(1.0);
    // C render_dab_mask: `float angle_rad = angle/360*2*M_PI` — f32 up to
    // the `*2`, then promoted to double for the M_PI multiply and narrowed
    // back. Keep the same rounding points.
    let angle = ((dab.angle / 360.0 * 2.0) as f64 * std::f64::consts::PI) as f32;
    let cs = angle.cos();
    let sn = angle.sin();
    let inv_r2 = 1.0 / (radius * radius);
    // libmypaint's render_dab_mask uses per-pixel sub-pixel sampling
    // (calculate_rr_antialiased) for `radius < 3`. Larger dabs fall back
    // to the plain rr formula since aliasing is invisible. Pre-compute
    // `r_aa_start` here so the render functions can branch on a single
    // scalar instead of re-deriving it per pixel.
    let r_aa_start = if radius < 3.0 {
        let aa_border = 1.0_f32;
        let start = if radius > aa_border {
            radius - aa_border
        } else {
            0.0
        };
        start * start / aspect
    } else {
        -1.0
    };
    // anti_aliasing was baked into the (radius, hardness) pair earlier
    // in stroke.rs::build_dab — same trick libmypaint uses in
    // prepare_and_draw_dab. dab.anti_aliasing is unused on the render
    // side; the per-pixel sub-pixel sampling for small dabs is driven by
    // the libmypaint-correct `r_aa_start` path computed above.
    let _ = dab.anti_aliasing;

    // libmypaint's AABB is `radius + 1` for both axes — the elliptical
    // dab's extent in WORLD coords is bounded by the major axis (radius),
    // regardless of rotation. hokusai used to use `radius * aspect + 1`
    // which over-included pixels for elliptical brushes but produced
    // identical output (the rr > 1 check filters them out).
    let r_ext = radius + 1.0;
    // libmypaint floors all four corners (mypaint-tiled-surface.c:350-353)
    // — hokusai used ceil for x1/y1 which picked up one extra row/column
    // of pixels past where upstream stopped. For the AA path those edge
    // pixels render with sub-pixel weight, so the difference is real.
    let x0 = (dab.x - r_ext).floor() as i32;
    let y0 = (dab.y - r_ext).floor() as i32;
    let x1 = (dab.x + r_ext).floor() as i32;
    let y1 = (dab.y + r_ext).floor() as i32;

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
            let paint_mode = dab.paint.clamp(0.0, 1.0);
            // Pass 1: regular Normal+Eraser blend, with opacity scaled
            // down by the paint factor. libmypaint draws both passes
            // back-to-back with `(1 - paint) * opaque` and `paint *
            // opaque` respectively, then sums them via the buffer.
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
                opaque_f * (1.0 - paint_mode),
                alpha_eraser_f,
                r_aa_start,
                dab.lock_alpha.clamp(0.0, 1.0),
                dab.posterize.clamp(0.0, 1.0),
                dab.posterize_num.max(1.0),
                dab.colorize.clamp(0.0, 1.0),
                src,
                src_r,
                src_g,
                src_b,
            );
            // Pass 2: spectral pigment blend on top of the Normal pass.
            // libmypaint scales the paint blend's opacity by `op->normal =
            // (1-lock_alpha)(1-colorize)(1-posterize)` (process_op) and
            // skips it entirely when that factor is 0.
            let normal_f = (1.0 - dab.lock_alpha.clamp(0.0, 1.0))
                * (1.0 - dab.colorize.clamp(0.0, 1.0))
                * (1.0 - dab.posterize.clamp(0.0, 1.0));
            let touched_paint = if paint_mode > 0.0 && normal_f > 0.0 {
                paint_blend_into_tile(
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
                    normal_f * opaque_f * paint_mode,
                    alpha_eraser_f,
                    r_aa_start,
                    src,
                )
            } else {
                false
            };
            // Pass 3: posterize. libmypaint runs this as a fourth blend
            // mode AFTER normal+paint (and colorize), operating on the
            // already-blended canvas. Pass 1's per-pixel loop short-
            // circuits when its scaled opacity is zero (e.g. paint_mode=1
            // brushes), so posterize would otherwise never run on those.
            let touched_post = if dab.posterize > 0.0 {
                posterize_pass_into_tile(
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
                    r_aa_start,
                    dab.posterize.clamp(0.0, 1.0),
                    dab.posterize_num.max(1.0),
                )
            } else {
                false
            };
            surface.tile_request_end(tx, ty);
            painted |= touched | touched_paint | touched_post;
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
    r_aa_start: f32,
    lock_alpha: f32,
    posterize: f32,
    posterize_num: f32,
    colorize: f32,
    src_color: RgbaF32,
    src_r: u32,
    src_g: u32,
    src_b: u32,
) -> bool {
    let mut painted = false;
    let _ = (posterize, posterize_num); // handled by posterize_pass_into_tile.

    for ly in ly0..=ly1 {
        let py = (oy + ly as i32) as f32;
        for lx in lx0..=lx1 {
            let px = (ox + lx as i32) as f32;
            // libmypaint picks sub-pixel AA for dabs whose radius is
            // below 3 px; for larger dabs the plain rr formula doesn't
            // alias visibly.
            let rr = if r_aa_start >= 0.0 {
                rr_at_aa(px, py, cx, cy, aspect, cs, sn, inv_r2, r_aa_start)
            } else {
                rr_at(px, py, cx, cy, aspect, cs, sn, inv_r2)
            };
            if rr > 1.0 {
                continue;
            }
            let mut opa = opa_at(rr, hardness);
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

            // Colorize: replace dst's hue and saturation (HSV) with the dab's,
            // preserving dst's value. When colorize=0 the regular Normal blend
            // applies. Done in straight alpha → convert, replace, repremul.
            if colorize > 0.0 && da > 0 {
                colorize_pixel(dst, src_color, mask, colorize);
                painted = true;
                continue;
            }

            dst[0] = blend(dr, inv_mask, src_r, color_opa_alpha);
            dst[1] = blend(dg, inv_mask, src_g, color_opa_alpha);
            dst[2] = blend(db, inv_mask, src_b, color_opa_alpha);
            if write_alpha {
                dst[3] = blend(da, inv_mask, FIX15_ONE, opa_alpha_raw);
            }

            // (Posterize is its own pass now — see `posterize_pass_into_tile`
            // called from `draw_dab_default` after the paint pass.)
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

/// Colorize: replace the pixel's hue+saturation with `src_color`'s, blended
/// by `mask` (dab coverage) and `amount` (colorize strength). dst.a stays.
fn colorize_pixel(pixel: &mut [u16; 4], src_color: RgbaF32, mask: u32, amount: f32) {
    let a = pixel[3] as f32 / FIX15_ONE as f32;
    if a <= 0.0 {
        return;
    }
    let dr = (pixel[0] as f32 / FIX15_ONE as f32) / a;
    let dg = (pixel[1] as f32 / FIX15_ONE as f32) / a;
    let db = (pixel[2] as f32 / FIX15_ONE as f32) / a;
    let dst_hsv = crate::color::rgb_to_hsv(dr, dg, db);
    let src_hsv = crate::color::rgb_to_hsv(src_color.r, src_color.g, src_color.b);
    // Keep dst.v, take src.h + src.s by `amount`.
    let mixed = crate::color::hsv_to_rgb(crate::color::Hsv {
        h: src_hsv.h,
        s: dst_hsv.s + (src_hsv.s - dst_hsv.s) * amount,
        v: dst_hsv.v,
    });
    let mask_f = mask as f32 / FIX15_ONE as f32;
    let nr = (dr + (mixed.r - dr) * mask_f * amount) * a;
    let ng = (dg + (mixed.g - dg) * mask_f * amount) * a;
    let nb = (db + (mixed.b - db) * mask_f * amount) * a;
    pixel[0] = (nr.clamp(0.0, 1.0) * FIX15_ONE as f32) as u16;
    pixel[1] = (ng.clamp(0.0, 1.0) * FIX15_ONE as f32) as u16;
    pixel[2] = (nb.clamp(0.0, 1.0) * FIX15_ONE as f32) as u16;
}

/// Posterize `pixel` toward `pnum` quantization levels by `amount` (fix15).
/// Matches libmypaint's `draw_dab_pixels_BlendMode_Posterize`: works on
/// the premultiplied channel values directly (no un-premul / re-premul
/// round-trip), so it leaves the alpha channel alone.
fn posterize_pixel(pixel: &mut [u16; 4], pnum: f32, amount: u32) {
    if amount == 0 {
        return;
    }
    let inv_amount = FIX15_ONE - amount;
    for ch in 0..3 {
        let c_premul = pixel[ch] as f32 / FIX15_ONE as f32;
        // libmypaint quantises the premultiplied value directly.
        let post = (c_premul * pnum).round() / pnum;
        let post_fix15 = (post.clamp(0.0, 1.0) * FIX15_ONE as f32) as u32;
        let dst = pixel[ch] as u32;
        let blended = (amount * post_fix15 + inv_amount * dst) >> 15;
        pixel[ch] = blended.min(FIX15_ONE) as u16;
    }
}

/// Spectral pigment blend over an already-blended Normal pass, mirroring
/// libmypaint's spectral paint pass. Upstream dispatches on `color_a`
/// (process_op, mypaint-tiled-surface.c):
///
/// - `color_a == 1.0` → `draw_dab_pixels_BlendMode_Normal_Paint`: a full
///   weighted-geometric-mean spectral mix with a minimum opacity of
///   150/32768 (low opacity hits int round-trip noise), additive only on
///   alpha-0 pixels.
/// - `color_a != 1.0` → `draw_dab_pixels_BlendMode_Normal_and_Eraser_Paint`:
///   the erase-aware variant that fades from additive to spectral with
///   `spectral_blend_factor(canvas alpha)` and has NO minimum opacity.
///
/// hokusai used to run the eraser variant unconditionally (with the other
/// variant's minimum-opacity clamp), which rendered every non-smudging
/// `paint_mode` brush far too light — the additive fade dominates on a
/// mostly-transparent canvas.
///
/// Arithmetic is done in fix15 integers exactly like brushmodes.c so the
/// truncation/rounding points line up.
#[allow(clippy::too_many_arguments)]
fn paint_blend_into_tile(
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
    r_aa_start: f32,
    src_color: RgbaF32,
) -> bool {
    use crate::spectral::{fastpow, rgb_to_spectral, spectral_blend_factor, spectral_to_rgb};

    const ONE: u32 = FIX15_ONE; // 1 << 15

    let alpha_eraser = alpha_eraser.clamp(0.0, 1.0);
    let eraser_variant = alpha_eraser != 1.0;
    // C: the blend functions take `uint16_t opacity` ← float truncation.
    let mut opacity = (opaque.clamp(0.0, 1.0) * ONE as f32) as u32;
    if !eraser_variant {
        // Normal_Paint only: "pigment-mode does not like very low opacity".
        opacity = opacity.max(150);
    }
    if opacity == 0 && !eraser_variant {
        // unreachable (max(150)), kept for shape parity
    }
    // C: `op->color_a = color_a` (float), passed as `color_a * (1<<15)`
    // into a uint16_t parameter — truncation.
    let color_a_fix = (alpha_eraser * ONE as f32) as u32;
    let src = clamp_color(src_color);
    let src_r = (src.r * ONE as f32) as u32;
    let src_g = (src.g * ONE as f32) as u32;
    let src_b = (src.b * ONE as f32) as u32;
    // C converts the fix15 color back to float for the spectral upsample.
    let spec_a = rgb_to_spectral(
        src_r as f32 / ONE as f32,
        src_g as f32 / ONE as f32,
        src_b as f32 / ONE as f32,
    );

    let mut painted = false;
    for ly in ly0..=ly1 {
        let py = (oy + ly as i32) as f32;
        for lx in lx0..=lx1 {
            let px = (ox + lx as i32) as f32;
            let rr = if r_aa_start >= 0.0 {
                rr_at_aa(px, py, cx, cy, aspect, cs, sn, inv_r2, r_aa_start)
            } else {
                rr_at(px, py, cx, cy, aspect, cs, sn, inv_r2)
            };
            if rr > 1.0 {
                continue;
            }
            // render_dab_mask quantises the mask to fix15 and skips zeros.
            let mask = (opa_at(rr, hardness) * ONE as f32) as u32;
            if mask == 0 {
                continue;
            }

            let opa_a = mask * opacity / ONE;
            let opa_b = ONE - opa_a;

            let dst = &mut tile[ly][lx];
            let (r0, g0, b0, a0) = (dst[0] as u32, dst[1] as u32, dst[2] as u32, dst[3] as u32);

            if !eraser_variant {
                // draw_dab_pixels_BlendMode_Normal_Paint
                let a_out = opa_a + opa_b * a0 / ONE;
                if a0 == 0 {
                    // nothing to mix with — plain additive
                    dst[0] = ((opa_a * src_r + opa_b * r0) / ONE) as u16;
                    dst[1] = ((opa_a * src_g + opa_b * g0) / ONE) as u16;
                    dst[2] = ((opa_a * src_b + opa_b * b0) / ONE) as u16;
                    dst[3] = a_out as u16;
                } else {
                    let fac_a = opa_a as f32 / (opa_a + opa_b * a0 / ONE) as f32;
                    let fac_b = 1.0 - fac_a;
                    let spec_b = rgb_to_spectral(
                        r0 as f32 / a0 as f32,
                        g0 as f32 / a0 as f32,
                        b0 as f32 / a0 as f32,
                    );
                    let mut mix = [0.0_f32; 10];
                    for i in 0..10 {
                        mix[i] = fastpow(spec_a[i], fac_a) * fastpow(spec_b[i], fac_b);
                    }
                    let (sr, sg, sb) = spectral_to_rgb(&mix);
                    dst[3] = a_out as u16;
                    // C: `rgba[i] = (rgb_result[i] * rgba[3]) + 0.5;`
                    dst[0] = (sr * a_out as f32 + 0.5) as u16;
                    dst[1] = (sg * a_out as f32 + 0.5) as u16;
                    dst[2] = (sb * a_out as f32 + 0.5) as u16;
                }
            } else {
                // draw_dab_pixels_BlendMode_Normal_and_Eraser_Paint
                let opa_a2 = opa_a * color_a_fix / ONE;
                let opa_out = opa_a2 + opa_b * a0 / ONE;

                let mut rgb = [0u32; 3];
                let spectral_factor = spectral_blend_factor(a0 as f32 / ONE as f32).clamp(0.0, 1.0);
                let additive_factor = 1.0 - spectral_factor;

                if additive_factor != 0.0 {
                    rgb[0] = (opa_a2 * src_r + opa_b * r0) / ONE;
                    rgb[1] = (opa_a2 * src_g + opa_b * g0) / ONE;
                    rgb[2] = (opa_a2 * src_b + opa_b * b0) / ONE;
                }

                if spectral_factor != 0.0 && a0 != 0 {
                    let spec_b = rgb_to_spectral(
                        r0 as f32 / a0 as f32,
                        g0 as f32 / a0 as f32,
                        b0 as f32 / a0 as f32,
                    );
                    let mut fac_a = opa_a as f32 / (opa_a + opa_b * a0 / ONE) as f32;
                    fac_a *= color_a_fix as f32 / ONE as f32;
                    let fac_b = 1.0 - fac_a;
                    let mut mix = [0.0_f32; 10];
                    for i in 0..10 {
                        mix[i] = fastpow(spec_a[i], fac_a) * fastpow(spec_b[i], fac_b);
                    }
                    let (sr, sg, sb) = spectral_to_rgb(&mix);
                    let res = [sr, sg, sb];
                    for i in 0..3 {
                        // C: `rgb[i] = (additive_factor * rgb[i]) +
                        //              (spectral_factor * rgb_result[i] * opa_out);`
                        // (uint32 ← float, truncation)
                        rgb[i] = (additive_factor * rgb[i] as f32
                            + spectral_factor * res[i] * opa_out as f32)
                            as u32;
                    }
                }

                dst[3] = opa_out as u16;
                dst[0] = rgb[0] as u16;
                dst[1] = rgb[1] as u16;
                dst[2] = rgb[2] as u16;
            }
            painted = true;
        }
    }
    painted
}

/// Posterize as a standalone pass over the dab's footprint. Mirrors
/// libmypaint's `draw_dab_pixels_BlendMode_Posterize`, which runs after
/// the normal + paint + colorize passes (and is gated only on
/// `dab.posterize > 0`, not on the normal pass's scaled opacity).
#[allow(clippy::too_many_arguments)]
fn posterize_pass_into_tile(
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
    r_aa_start: f32,
    posterize: f32,
    posterize_num: f32,
) -> bool {
    let mut painted = false;
    let pnum = posterize_num.round().max(1.0);
    let post_amount_fix15 = (posterize.clamp(0.0, 1.0) * FIX15_ONE as f32) as u32;
    for ly in ly0..=ly1 {
        let py = (oy + ly as i32) as f32;
        for lx in lx0..=lx1 {
            let px = (ox + lx as i32) as f32;
            let rr = if r_aa_start >= 0.0 {
                rr_at_aa(px, py, cx, cy, aspect, cs, sn, inv_r2, r_aa_start)
            } else {
                rr_at(px, py, cx, cy, aspect, cs, sn, inv_r2)
            };
            if rr > 1.0 {
                continue;
            }
            let mut opa = if rr <= 1.0 { opa_at(rr, hardness) } else { 0.0 };
            opa *= opaque;
            if opa <= 0.0 {
                continue;
            }
            // libmypaint multiplies `posterize` by the dab mask. We
            // scale `post_amount_fix15` by the per-pixel `opa` so the
            // quantisation strength fades with the dab edge, matching
            // `draw_dab_pixels_BlendMode_Posterize(... opacity * (1<<15) ...)`.
            let mask = (opa.clamp(0.0, 1.0) * FIX15_ONE as f32) as u32;
            let scaled = ((post_amount_fix15 as u64 * mask as u64) >> 15) as u32;
            posterize_pixel(&mut tile[ly][lx], pnum, scaled);
            painted = true;
        }
    }
    painted
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

/// Backend-agnostic version of [`get_color_default`]: takes a
/// `sample(px, py)` closure that returns a single fix15 RGBA pixel
/// (`[r, g, b, a]`, premultiplied, linear sRGB). Backends that can't
/// hand out raw `TilePixels` (e.g. a `Pixmap`-only surface) can
/// override `TiledSurface::get_color` to delegate here, passing a
/// closure that reads from their own buffer.
///
/// Coordinates outside the painted area should be reported as fully
/// transparent (`[0, 0, 0, 0]`).
pub fn get_color_via_sample<F>(x: f32, y: f32, radius: f32, sample: F) -> RgbaF32
where
    F: Fn(i32, i32) -> [u16; 4],
{
    let radius = radius.max(1.0); // libmypaint floors get_color radius at 1.0 (mypaint-tiled-surface.c:659)
    let inv_r2 = 1.0 / (radius * radius);
    let r_ext = radius + 1.0;
    let x0 = (x - r_ext).floor() as i32;
    let y0 = (y - r_ext).floor() as i32;
    let x1 = (x + r_ext).ceil() as i32;
    let y1 = (y + r_ext).ceil() as i32;

    let mut sum_r = 0.0f32;
    let mut sum_g = 0.0f32;
    let mut sum_b = 0.0f32;
    let mut sum_a = 0.0f32;
    let mut sum_w = 0.0f32;

    for py in y0..=y1 {
        for px in x0..=x1 {
            let rr = rr_at(px as f32, py as f32, x, y, 1.0, 1.0, 0.0, inv_r2);
            if rr > 1.0 {
                continue;
            }
            let w = opa_at(rr, 0.5);
            let p = sample(px, py);
            sum_r += fix15::to_f32(p[0]) * w;
            sum_g += fix15::to_f32(p[1]) * w;
            sum_b += fix15::to_f32(p[2]) * w;
            sum_a += fix15::to_f32(p[3]) * w;
            sum_w += w;
        }
    }
    if sum_w <= 0.0 {
        return RgbaF32 {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
    }
    // libmypaint's get_color_internal returns STRAIGHT-alpha color, not
    // premultiplied (mypaint-tiled-surface.c:758-765). The accumulator
    // sums premultiplied pixels, then divides RGB by sum_a after the
    // mask-weighted alpha average. hokusai was returning the
    // premultiplied average straight to the caller, so smudge brushes
    // sampling a partially-transparent canvas mixed in artificially
    // dark RGB.
    let alpha = sum_a / sum_w;
    if alpha <= 0.0 {
        return RgbaF32 {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
    }
    RgbaF32 {
        r: (sum_r / sum_a).clamp(0.0, 1.0),
        g: (sum_g / sum_a).clamp(0.0, 1.0),
        b: (sum_b / sum_a).clamp(0.0, 1.0),
        a: alpha.clamp(0.0, 1.0),
    }
}

/// macOS/BSD libc `rand()` (Park-Miller 7^5, seed 1) — libmypaint's
/// `get_color_pixels_accumulate` consumes it for the sparse pixel sampling
/// it does when the sample radius exceeds 2 px. libc's state is
/// process-global; ours is thread-local and reset per render via
/// [`reset_get_color_rng`] so each replay matches a fresh reference
/// process. (glibc uses a different generator — sparse sampling parity is
/// only bit-exact against a macOS/BSD-built libmypaint.)
const C_RAND_MAX: i32 = 2147483647;

thread_local! {
    static C_RAND_STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// Reset the libc-`rand()` mirror to its fresh-process state (seed 1).
/// Call before replaying a stroke that should match an independent
/// libmypaint render.
pub fn reset_get_color_rng() {
    C_RAND_STATE.with(|s| s.set(1));
}

fn c_rand() -> i32 {
    C_RAND_STATE.with(|s| {
        let mut ctx = s.get();
        if ctx == 0 {
            ctx = 123459876;
        }
        let hi = (ctx / 127773) as i64;
        let lo = (ctx % 127773) as i64;
        let mut x = 16807 * lo - 2836 * hi;
        if x < 0 {
            x += 0x7fffffff;
        }
        s.set(x as u64);
        (x % (C_RAND_MAX as i64 + 1)) as i32
    })
}

/// The per-tile pixel sweep shared by both get_color flavours — mirrors
/// `render_dab_mask`'s geometry exactly: per-tile bbox from
/// `floor(centre ± (radius+1))`, hardness-0.5 falloff, the sub-pixel AA
/// path for radius < 3, and the fix15 mask quantisation (`opa * (1<<15)`
/// truncated; zero entries are skipped without advancing the sample
/// interval counter).
#[inline]
fn for_each_mask_pixel<F: FnMut(usize, usize, u32)>(
    x: f32,
    y: f32,
    radius: f32,
    tx: i32,
    ty: i32,
    mut f: F,
) {
    const HARDNESS: f32 = 0.5;
    let xl = x - (tx * TILE_SIZE as i32) as f32;
    let yl = y - (ty * TILE_SIZE as i32) as f32;
    let r_fringe = radius + 1.0;
    let x0 = ((xl - r_fringe).floor() as i32).max(0);
    let y0 = ((yl - r_fringe).floor() as i32).max(0);
    let x1 = ((xl + r_fringe).floor() as i32).min(TILE_SIZE as i32 - 1);
    let y1 = ((yl + r_fringe).floor() as i32).min(TILE_SIZE as i32 - 1);
    if x0 > x1 || y0 > y1 {
        return;
    }
    let inv_r2 = 1.0 / (radius * radius);
    let r_aa_start = if radius < 3.0 {
        let s = if radius > 1.0 { radius - 1.0 } else { 0.0 };
        s * s / 1.0
    } else {
        -1.0
    };
    for yp in y0..=y1 {
        for xp in x0..=x1 {
            let rr = if r_aa_start >= 0.0 {
                rr_at_aa(
                    xp as f32, yp as f32, xl, yl, 1.0, 1.0, 0.0, inv_r2, r_aa_start,
                )
            } else {
                rr_at(xp as f32, yp as f32, xl, yl, 1.0, 1.0, 0.0, inv_r2)
            };
            let opa = if rr > 1.0 { 0.0 } else { opa_at(rr, HARDNESS) };
            let mask = (opa * FIX15_ONE as f32) as u32;
            if mask == 0 {
                continue;
            }
            f(xp as usize, yp as usize, mask);
        }
    }
}

/// Average color in a circle of `radius` around `(x, y)`, mask-weighted
/// with the same falloff a hardness=0.5 dab produces — bit-exact port of
/// libmypaint's legacy sampler (`get_color_pixels_legacy`), which
/// accumulates per tile in u32 fix15 arithmetic (`opa * channel >> 15`
/// with truncation) before widening to float. Uses
/// [`TiledSurface::tile_lookup`] for read-only sampling.
pub fn get_color_default<S: TiledSurface + ?Sized>(
    surface: &S,
    x: f32,
    y: f32,
    radius: f32,
) -> RgbaF32 {
    const ONE: u32 = FIX15_ONE;
    let radius = if radius < 1.0 { 1.0 } else { radius };
    let r_fringe = radius + 1.0;
    let tx0 = ((x - r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let tx1 = ((x + r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let ty0 = ((y - r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let ty1 = ((y + r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);

    let mut sum_weight = 0.0_f32;
    let mut sum_r = 0.0_f32;
    let mut sum_g = 0.0_f32;
    let mut sum_b = 0.0_f32;
    let mut sum_a = 0.0_f32;

    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let tile_opt = surface.tile_lookup(tx, ty);
            // Per-tile u32 accumulators, like get_color_pixels_legacy.
            let (mut weight, mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for_each_mask_pixel(x, y, radius, tx, ty, |lx, ly, mask| {
                weight += mask;
                if let Some(tile) = tile_opt {
                    let p = tile[ly][lx];
                    r += mask * p[0] as u32 / ONE;
                    g += mask * p[1] as u32 / ONE;
                    b += mask * p[2] as u32 / ONE;
                    a += mask * p[3] as u32 / ONE;
                }
            });
            sum_weight += weight as f32;
            sum_r += r as f32;
            sum_g += g as f32;
            sum_b += b as f32;
            sum_a += a as f32;
        }
    }

    if sum_weight <= 0.0 {
        return RgbaF32::TRANSPARENT;
    }
    sum_a /= sum_weight;
    let a_out = sum_a.clamp(0.0, 1.0);
    if sum_a > 0.0 {
        // Legacy sampling un-premultiplies with the weight-normalised
        // alpha (`demul`), clamping against rounding errors.
        RgbaF32 {
            r: (sum_r / sum_weight / sum_a).clamp(0.0, 1.0),
            g: (sum_g / sum_weight / sum_a).clamp(0.0, 1.0),
            b: (sum_b / sum_weight / sum_a).clamp(0.0, 1.0),
            a: a_out,
        }
    } else {
        // All transparent — libmypaint hands back an "ugly" green so bugs
        // show up; the colour must round-trip identically through the
        // smudge bucket cache.
        RgbaF32 {
            r: 0.0,
            g: 1.0,
            b: 0.0,
            a: a_out,
        }
    }
}

/// Port of libmypaint's `Surface2::get_color_pigment`
/// (`get_color_pixels_accumulate` with `paint >= 0`): a mask-weighted
/// running average blending a 10-channel spectral WGM with the
/// alpha-weighted linear average by `paint`. For radii above 2 px the
/// reference only samples every `radius*7`-th masked pixel plus a
/// `1/(7*radius)` random subset (via libc `rand()` — see [`c_rand`]);
/// smaller radii sample every pixel.
///
/// Returns straight-alpha.
pub fn get_color_pigment_default<S: TiledSurface + ?Sized>(
    surface: &S,
    x: f32,
    y: f32,
    radius: f32,
    paint: f32,
) -> RgbaF32 {
    use crate::spectral::{fastpow, rgb_to_spectral, spectral_to_rgb};

    let radius = if radius < 1.0 { 1.0 } else { radius };
    let r_fringe = radius + 1.0;
    let tx0 = ((x - r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let tx1 = ((x + r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let ty0 = ((y - r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let ty1 = ((y + r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);

    let sample_interval: u32 = if radius <= 2.0 {
        1
    } else {
        (radius * 7.0) as u32
    };
    let random_sample_rate = 1.0_f32 / (7.0 * radius);
    let random_sample_threshold = (random_sample_rate * C_RAND_MAX as f32) as i32;

    let mut sum_weight = 0.0_f32;
    let mut sum_a = 0.0_f32;
    // The spectral accumulator starts from the spectrum of black — the
    // reference never special-cases the first sample, it just WGM-mixes
    // into this initial value with fac_a = 1.
    let mut avg_spectral = rgb_to_spectral(0.0, 0.0, 0.0);
    let mut avg_rgb = [0.0_f32; 3];

    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let tile_opt = surface.tile_lookup(tx, ty);
            // Rolling counter — a fresh one per tile, like the local in
            // get_color_pixels_accumulate. Advances only over non-zero
            // mask entries (zeros are RLE-skipped in the reference).
            let mut interval_counter: u32 = 0;
            for_each_mask_pixel(x, y, radius, tx, ty, |lx, ly, mask| {
                let sampled = interval_counter == 0
                    || (sample_interval > 1 && c_rand() < random_sample_threshold);
                if sampled {
                    let p = tile_opt.map(|t| t[ly][lx]).unwrap_or([0, 0, 0, 0]);
                    let pa = p[3] as u32;
                    let a = mask as f32 * pa as f32 / (1u32 << 30) as f32;
                    let alpha_sums = a + sum_a;
                    sum_weight += mask as f32 / FIX15_ONE as f32;
                    let (mut fac_a, mut fac_b) = (1.0_f32, 1.0_f32);
                    if alpha_sums > 0.0 {
                        fac_a = a / alpha_sums;
                        fac_b = 1.0 - fac_a;
                    }
                    if paint > 0.0 && pa > 0 {
                        let spectral = rgb_to_spectral(
                            p[0] as f32 / pa as f32,
                            p[1] as f32 / pa as f32,
                            p[2] as f32 / pa as f32,
                        );
                        for i in 0..10 {
                            avg_spectral[i] =
                                fastpow(spectral[i], fac_a) * fastpow(avg_spectral[i], fac_b);
                        }
                    }
                    if paint < 1.0 && pa > 0 {
                        for i in 0..3 {
                            avg_rgb[i] = p[i] as f32 * fac_a / pa as f32 + avg_rgb[i] * fac_b;
                        }
                    }
                    sum_a += a;
                }
                interval_counter = (interval_counter + 1) % sample_interval;
            });
        }
    }

    if sum_weight <= 0.0 {
        return RgbaF32::TRANSPARENT;
    }
    sum_a /= sum_weight;

    let spec_rgb = spectral_to_rgb(&avg_spectral);
    let sum_r = spec_rgb.0 * paint + (1.0 - paint) * avg_rgb[0];
    let sum_g = spec_rgb.1 * paint + (1.0 - paint) * avg_rgb[1];
    let sum_b = spec_rgb.2 * paint + (1.0 - paint) * avg_rgb[2];

    let a_out = sum_a.clamp(0.0, 1.0);
    if sum_a > 0.0 {
        RgbaF32 {
            r: sum_r.clamp(0.0, 1.0),
            g: sum_g.clamp(0.0, 1.0),
            b: sum_b.clamp(0.0, 1.0),
            a: a_out,
        }
    } else {
        RgbaF32 {
            r: 0.0,
            g: 1.0,
            b: 0.0,
            a: a_out,
        }
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
    fn get_color_via_sample_averages_solid_fill() {
        // Sample a 5×5 region painted pure red at full alpha. The
        // mask-weighted average should be ≈ (1, 0, 0, 1).
        let sample = |_px: i32, _py: i32| {
            [
                crate::fix15::FIX15_ONE as u16,
                0,
                0,
                crate::fix15::FIX15_ONE as u16,
            ]
        };
        let c = get_color_via_sample(2.0, 2.0, 1.0, sample);
        assert!((c.r - 1.0).abs() < 1e-3, "red: {}", c.r);
        assert!(c.g.abs() < 1e-3);
        assert!(c.b.abs() < 1e-3);
        assert!((c.a - 1.0).abs() < 1e-3);
    }

    #[test]
    fn rr_increases_with_distance() {
        // Center exactly at pixel center (0.5, 0.5), radius² = 1.
        let near = rr_at(0.0, 0.0, 0.5, 0.5, 1.0, 1.0, 0.0, 1.0);
        let far = rr_at(2.0, 0.0, 0.5, 0.5, 1.0, 1.0, 0.0, 1.0);
        assert!(far > near);
    }
}
