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
    // libmypaint rejects radius < 0.1 above and otherwise renders with
    // the actual radius (mypaint-tiled-surface.c:575). hokusai used to
    // floor at 0.5 here, which made very thin pencils render thicker.
    let radius = dab.radius;
    let aspect = dab.aspect_ratio.max(1.0);
    let angle = dab.angle.to_radians();
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
            let touched_paint = if paint_mode > 0.0 {
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
                    opaque_f * paint_mode,
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
                rr_at_aa(
                    px, py, cx, cy, aspect, cs, sn, inv_r2, r_aa_start,
                )
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
/// libmypaint's `draw_dab_pixels_BlendMode_Normal_and_Eraser_Paint`. Mixes
/// the source color with each pixel's reflectance in 10-channel spectral
/// space via a weighted geometric mean, then fades smoothly to plain
/// additive blending at low canvas alpha (where spectral mixing produces
/// dark fringes around antialiased edges).
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
    use crate::spectral::{rgb_to_spectral, spectral_blend_factor, spectral_to_rgb};

    // libmypaint enforces a minimum opacity for spectral blend because
    // very low-opacity dabs hit float→fix15 rounding errors that look
    // worse than the additive fallback.
    let opaque = opaque.max(150.0 / FIX15_ONE as f32);
    let alpha_eraser = alpha_eraser.clamp(0.0, 1.0);
    let src = clamp_color(src_color);
    let spec_a = rgb_to_spectral(src.r, src.g, src.b);

    let mut painted = false;
    for ly in ly0..=ly1 {
        let py = (oy + ly as i32) as f32;
        for lx in lx0..=lx1 {
            let px = (ox + lx as i32) as f32;
            let rr = if r_aa_start >= 0.0 {
                rr_at_aa(
                    px, py, cx, cy, aspect, cs, sn, inv_r2, r_aa_start,
                )
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

            let opa_a = (opa * alpha_eraser).clamp(0.0, 1.0);
            let opa_top = opa.clamp(0.0, 1.0);
            let opa_b = 1.0 - opa_top;

            let dst = &mut tile[ly][lx];
            let dr = dst[0] as f32 / FIX15_ONE as f32;
            let dg = dst[1] as f32 / FIX15_ONE as f32;
            let db = dst[2] as f32 / FIX15_ONE as f32;
            let da = dst[3] as f32 / FIX15_ONE as f32;

            let opa_out = opa_a + opa_b * da;
            let spectral_factor = spectral_blend_factor(da).clamp(0.0, 1.0);
            let additive_factor = 1.0 - spectral_factor;

            // Additive contribution — same shape as the Normal pass, but
            // computed in float here so the spectral mix can be lerped
            // against it.
            let add_r = opa_a * src.r + opa_b * dr;
            let add_g = opa_a * src.g + opa_b * dg;
            let add_b = opa_a * src.b + opa_b * db;

            let (mut new_r, mut new_g, mut new_b) = (add_r, add_g, add_b);

            if spectral_factor > 0.0 && da > 0.0 {
                // Un-premultiply for the spectral upsample. libmypaint
                // does the same — straight-alpha reflectance is what the
                // spectral tables expect.
                let inv_da = 1.0 / da;
                let bot_r = (dr * inv_da).clamp(0.0, 1.0);
                let bot_g = (dg * inv_da).clamp(0.0, 1.0);
                let bot_b = (db * inv_da).clamp(0.0, 1.0);
                let spec_b = rgb_to_spectral(bot_r, bot_g, bot_b);

                let mut fac_a = opa_top / (opa_top + opa_b * da).max(1e-6);
                fac_a *= alpha_eraser;
                let fac_b = 1.0 - fac_a;

                let mut mix = [0.0_f32; 10];
                for i in 0..10 {
                    // libmypaint's draw_dab_pixels_BlendMode_Normal_and_Eraser_Paint
                    // uses fastpow (brushmodes.c:393) for its spectral
                    // mix. Use the same approximation for parity.
                    mix[i] = crate::spectral::fastpow(spec_a[i].max(1e-6), fac_a)
                        * crate::spectral::fastpow(spec_b[i].max(1e-6), fac_b);
                }
                let (sr, sg, sb) = spectral_to_rgb(&mix);

                new_r = additive_factor * add_r + spectral_factor * sr * opa_out;
                new_g = additive_factor * add_g + spectral_factor * sg * opa_out;
                new_b = additive_factor * add_b + spectral_factor * sb * opa_out;
            }

            dst[0] = (new_r.clamp(0.0, 1.0) * FIX15_ONE as f32) as u16;
            dst[1] = (new_g.clamp(0.0, 1.0) * FIX15_ONE as f32) as u16;
            dst[2] = (new_b.clamp(0.0, 1.0) * FIX15_ONE as f32) as u16;
            dst[3] = (opa_out.clamp(0.0, 1.0) * FIX15_ONE as f32) as u16;
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
                rr_at_aa(
                    px, py, cx, cy, aspect, cs, sn, inv_r2, r_aa_start,
                )
            } else {
                rr_at(px, py, cx, cy, aspect, cs, sn, inv_r2)
            };
            if rr > 1.0 {
                continue;
            }
            let mut opa = if rr <= 1.0 {
                opa_at(rr, hardness)
            } else {
                0.0
            };
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
        return RgbaF32 { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    }
    RgbaF32 {
        r: (sum_r / sum_a).clamp(0.0, 1.0),
        g: (sum_g / sum_a).clamp(0.0, 1.0),
        b: (sum_b / sum_a).clamp(0.0, 1.0),
        a: alpha.clamp(0.0, 1.0),
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
    let radius = radius.max(1.0); // libmypaint floors get_color radius at 1.0 (mypaint-tiled-surface.c:659)
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
            // libmypaint's get_color_internal requests each tile in the
            // sample radius unconditionally; tiles that don't exist yet
            // come back as transparent (alpha 0) but still contribute
            // their mask weight to sum_weight. Mirroring that here keeps
            // smudge brushes sampling near the canvas edge in step with
            // libmypaint instead of overweighting the painted pixels.
            let tile_opt = surface.tile_lookup(tx, ty);
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
                    sum_w += w;
                    if let Some(tile) = tile_opt {
                        let p = tile[ly][lx];
                        sum_r += fix15::to_f32(p[0]) * w;
                        sum_g += fix15::to_f32(p[1]) * w;
                        sum_b += fix15::to_f32(p[2]) * w;
                        sum_a += fix15::to_f32(p[3]) * w;
                    }
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

/// Port of libmypaint's `Surface2::get_color_pigment`: a mask-weighted
/// running average that blends a 10-channel spectral WGM with the
/// alpha-weighted linear average by `paint`. The spectral path is what
/// gives pigment-style brushes (blender, watercolour, …) a colour mix
/// closer to real paint — without it, sampling the canvas under a
/// blue+yellow gradient comes back as muddy grey instead of green.
///
/// Returns straight-alpha. Like libmypaint, pixels with `rgba.a == 0`
/// don't contribute (they still count toward `sum_weight` but the
/// running average is unchanged).
pub fn get_color_pigment_default<S: TiledSurface + ?Sized>(
    surface: &S,
    x: f32,
    y: f32,
    radius: f32,
    paint: f32,
) -> RgbaF32 {
    use crate::spectral::{rgb_to_spectral, spectral_to_rgb};

    let radius = radius.max(1.0); // libmypaint floors get_color radius at 1.0 (mypaint-tiled-surface.c:659)
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

    let paint = paint.clamp(0.0, 1.0);

    let mut sum_weight = 0.0_f32;
    let mut sum_a = 0.0_f32;
    let mut avg_rgb = [0.0_f32; 3];
    let mut avg_spectral = [0.0_f32; 10];
    let mut spectral_seeded = false;

    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            // libmypaint requests every tile in the sample bounds —
            // non-existent tiles contribute their mask weight (with
            // alpha = 0) so the running averages stay normalised against
            // the full disc area. Iterate the pixel range even when the
            // tile is missing so sum_weight tracks libmypaint.
            let tile_opt = surface.tile_lookup(tx, ty);
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
                    let mask = opa_at(rr, 0.5);
                    sum_weight += mask;
                    let Some(tile) = tile_opt else {
                        continue;
                    };
                    let p = tile[ly][lx];
                    let pa = fix15::to_f32(p[3]);
                    // `a` is the pixel's alpha contribution weighted by
                    // the mask, matching libmypaint's
                    // `a = mask[0] * rgba[3] / (1 << 30)`.
                    let a = mask * pa;
                    if pa <= 0.0 {
                        continue;
                    }
                    // Running alpha-weighted average:
                    //   fac_a = a / (a + sum_a)
                    let alpha_sums = a + sum_a;
                    let (fac_a, fac_b) = if alpha_sums > 0.0 {
                        let fa = a / alpha_sums;
                        (fa, 1.0 - fa)
                    } else {
                        (1.0, 1.0)
                    };
                    // Un-premultiply the canvas pixel for the
                    // running-average inputs.
                    let inv_pa = 1.0 / pa;
                    let sr = fix15::to_f32(p[0]) * inv_pa;
                    let sg = fix15::to_f32(p[1]) * inv_pa;
                    let sb = fix15::to_f32(p[2]) * inv_pa;

                    if paint > 0.0 {
                        let spectral = rgb_to_spectral(sr, sg, sb);
                        if !spectral_seeded {
                            // First contributing pixel seeds the
                            // spectral accumulator outright (no WGM
                            // with the zeroed bucket).
                            avg_spectral = spectral;
                            spectral_seeded = true;
                        } else {
                            for i in 0..10 {
                                avg_spectral[i] =
                                    crate::spectral::fastpow(
                                        spectral[i].max(1e-6),
                                        fac_a,
                                    ) * crate::spectral::fastpow(
                                        avg_spectral[i].max(1e-6),
                                        fac_b,
                                    );
                            }
                        }
                    }
                    if paint < 1.0 {
                        avg_rgb[0] = sr * fac_a + avg_rgb[0] * fac_b;
                        avg_rgb[1] = sg * fac_a + avg_rgb[1] * fac_b;
                        avg_rgb[2] = sb * fac_a + avg_rgb[2] * fac_b;
                    }
                    sum_a += a;
                }
            }
        }
    }
    if sum_weight <= 0.0 {
        return RgbaF32::TRANSPARENT;
    }
    let a = (sum_a / sum_weight).clamp(0.0, 1.0);
    if a <= 0.0 {
        return RgbaF32::TRANSPARENT;
    }

    let (r, g, b) = if paint > 0.0 {
        let (sr, sg, sb) = spectral_to_rgb(&avg_spectral);
        if paint >= 1.0 {
            (sr, sg, sb)
        } else {
            (
                paint * sr + (1.0 - paint) * avg_rgb[0],
                paint * sg + (1.0 - paint) * avg_rgb[1],
                paint * sb + (1.0 - paint) * avg_rgb[2],
            )
        }
    } else {
        (avg_rgb[0], avg_rgb[1], avg_rgb[2])
    };

    RgbaF32 {
        r: r.clamp(0.0, 1.0),
        g: g.clamp(0.0, 1.0),
        b: b.clamp(0.0, 1.0),
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
    fn get_color_via_sample_averages_solid_fill() {
        // Sample a 5×5 region painted pure red at full alpha. The
        // mask-weighted average should be ≈ (1, 0, 0, 1).
        let sample = |_px: i32, _py: i32| [crate::fix15::FIX15_ONE as u16, 0, 0, crate::fix15::FIX15_ONE as u16];
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
