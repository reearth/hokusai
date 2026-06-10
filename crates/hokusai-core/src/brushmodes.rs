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
    // regardless of rotation (mypaint-tiled-surface.c floors all four
    // corners).
    let r_ext = radius + 1.0;
    let x0 = (dab.x - r_ext).floor() as i32;
    let y0 = (dab.y - r_ext).floor() as i32;
    let x1 = (dab.x + r_ext).floor() as i32;
    let y1 = (dab.y + r_ext).floor() as i32;

    let tx0 = x0.div_euclid(TILE_SIZE as i32);
    let ty0 = y0.div_euclid(TILE_SIZE as i32);
    let tx1 = x1.div_euclid(TILE_SIZE as i32);
    let ty1 = y1.div_euclid(TILE_SIZE as i32);

    // C truncates the straight-alpha colour to fix15 once, up front
    // (`op->color_r = color_r * (1<<15)`).
    let src = clamp_color(dab.color);
    let src_r = (src.r * FIX15_ONE as f32) as u32;
    let src_g = (src.g * FIX15_ONE as f32) as u32;
    let src_b = (src.b * FIX15_ONE as f32) as u32;
    let color_a = dab.alpha_eraser.clamp(0.0, 1.0);
    let opaque_f = dab.opaque.clamp(0.0, 1.0);
    let hardness = dab.hardness.clamp(0.0, 1.0);
    let lock_alpha = dab.lock_alpha.clamp(0.0, 1.0);
    let colorize = dab.colorize.clamp(0.0, 1.0);
    let posterize = dab.posterize.clamp(0.0, 1.0);
    let posterize_num = dab.posterize_num.clamp(1.0, 128.0) as u32;
    let paint = dab.paint.clamp(0.0, 1.0);
    // process_op: `op->normal = (1-lock_alpha)(1-colorize)(1-posterize)`.
    let normal_f = (1.0 - lock_alpha) * (1.0 - colorize) * (1.0 - posterize);

    // C passes each blend's opacity as `float-product * (1<<15)` into a
    // uint16_t parameter — truncation happens exactly once, here.
    let fix = |v: f32| -> u32 { (v * FIX15_ONE as f32) as u32 };

    let mut mask_entries: Vec<(usize, usize, u32)> = Vec::new();
    let mut painted = false;
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            // render_dab_mask works in tile-local coordinates and computes
            // the bbox from the local centre.
            let xl = dab.x - (tx * TILE_SIZE as i32) as f32;
            let yl = dab.y - (ty * TILE_SIZE as i32) as f32;
            collect_dab_mask(
                &mut mask_entries,
                xl,
                yl,
                radius,
                hardness,
                aspect,
                cs,
                sn,
                r_aa_start,
            );
            if mask_entries.is_empty() {
                continue;
            }

            let tile = surface.tile_request_start(tx, ty);
            // The pass structure and opacity compositions mirror
            // process_op (mypaint-tiled-surface.c) exactly.
            if paint < 1.0 {
                if normal_f != 0.0 {
                    let opacity = fix(normal_f * opaque_f * (1.0 - paint));
                    if color_a == 1.0 {
                        blend_normal(tile, &mask_entries, src_r, src_g, src_b, opacity);
                    } else {
                        blend_normal_and_eraser(
                            tile,
                            &mask_entries,
                            src_r,
                            src_g,
                            src_b,
                            fix(color_a),
                            opacity,
                        );
                    }
                }
                if lock_alpha != 0.0 && color_a != 0.0 {
                    let opacity = fix(
                        lock_alpha * opaque_f * (1.0 - colorize) * (1.0 - posterize)
                            * (1.0 - paint),
                    );
                    blend_lock_alpha(tile, &mask_entries, src_r, src_g, src_b, opacity);
                }
            }
            if paint > 0.0 {
                if normal_f != 0.0 {
                    let opacity = fix(normal_f * opaque_f * paint);
                    if color_a == 1.0 {
                        blend_normal_paint(tile, &mask_entries, src_r, src_g, src_b, opacity);
                    } else {
                        blend_normal_and_eraser_paint(
                            tile,
                            &mask_entries,
                            src_r,
                            src_g,
                            src_b,
                            fix(color_a),
                            opacity,
                        );
                    }
                }
                if lock_alpha != 0.0 && color_a != 0.0 {
                    let opacity = fix(
                        lock_alpha * opaque_f * (1.0 - colorize) * (1.0 - posterize) * paint,
                    );
                    blend_lock_alpha_paint(tile, &mask_entries, src_r, src_g, src_b, opacity);
                }
            }
            if colorize != 0.0 {
                blend_color(
                    tile,
                    &mask_entries,
                    src_r,
                    src_g,
                    src_b,
                    fix(colorize * opaque_f),
                );
            }
            if posterize != 0.0 {
                blend_posterize(
                    tile,
                    &mask_entries,
                    fix(posterize * opaque_f),
                    posterize_num,
                );
            }
            surface.tile_request_end(tx, ty);
            painted = true;
        }
    }
    painted
}

/// Build the per-tile dab mask exactly like `render_dab_mask`: bbox from
/// `floor(local centre ± (radius+1))` clamped to the tile, sub-pixel AA
/// for radius < 3, and fix15 quantisation (`opa * (1<<15)` truncated) with
/// zero entries dropped.
#[allow(clippy::too_many_arguments)]
fn collect_dab_mask(
    entries: &mut Vec<(usize, usize, u32)>,
    xl: f32,
    yl: f32,
    radius: f32,
    hardness: f32,
    aspect: f32,
    cs: f32,
    sn: f32,
    r_aa_start: f32,
) {
    entries.clear();
    let r_fringe = radius + 1.0;
    let x0 = ((xl - r_fringe).floor() as i32).max(0);
    let y0 = ((yl - r_fringe).floor() as i32).max(0);
    let x1 = ((xl + r_fringe).floor() as i32).min(TILE_SIZE as i32 - 1);
    let y1 = ((yl + r_fringe).floor() as i32).min(TILE_SIZE as i32 - 1);
    if x0 > x1 || y0 > y1 {
        return;
    }
    let inv_r2 = 1.0 / (radius * radius);
    for yp in y0..=y1 {
        for xp in x0..=x1 {
            let rr = if r_aa_start >= 0.0 {
                rr_at_aa(xp as f32, yp as f32, xl, yl, aspect, cs, sn, inv_r2, r_aa_start)
            } else {
                rr_at(xp as f32, yp as f32, xl, yl, aspect, cs, sn, inv_r2)
            };
            let opa = if rr > 1.0 { 0.0 } else { opa_at(rr, hardness) };
            let mask = (opa * FIX15_ONE as f32) as u32;
            if mask != 0 {
                entries.push((xp as usize, yp as usize, mask));
            }
        }
    }
}

/// `draw_dab_pixels_BlendMode_Normal` — premultiplied source-over in fix15
/// integers, libmypaint's exact truncation points.
fn blend_normal(
    tile: &mut TilePixels,
    entries: &[(usize, usize, u32)],
    src_r: u32,
    src_g: u32,
    src_b: u32,
    opacity: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, ly, mask) in entries {
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let p = &mut tile[ly][lx];
        p[3] = (opa_a + opa_b * p[3] as u32 / ONE) as u16;
        p[0] = ((opa_a * src_r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * src_g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * src_b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_Normal_and_Eraser` — the erase-aware variant
/// (smudging dabs): the source contribution scales with `color_a` while the
/// destination keeps the un-scaled coverage, so `color_a < 1` erases.
fn blend_normal_and_eraser(
    tile: &mut TilePixels,
    entries: &[(usize, usize, u32)],
    src_r: u32,
    src_g: u32,
    src_b: u32,
    color_a: u32,
    opacity: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, ly, mask) in entries {
        let mut opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        opa_a = opa_a * color_a / ONE;
        let p = &mut tile[ly][lx];
        p[3] = (opa_a + opa_b * p[3] as u32 / ONE) as u16;
        p[0] = ((opa_a * src_r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * src_g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * src_b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_LockAlpha` — colour-only blend masked by the
/// destination alpha; dst alpha is left untouched.
fn blend_lock_alpha(
    tile: &mut TilePixels,
    entries: &[(usize, usize, u32)],
    src_r: u32,
    src_g: u32,
    src_b: u32,
    opacity: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, ly, mask) in entries {
        let mut opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let p = &mut tile[ly][lx];
        opa_a *= p[3] as u32;
        opa_a /= ONE;
        p[0] = ((opa_a * src_r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * src_g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * src_b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_LockAlpha_Paint` — the spectral flavour of
/// lock-alpha (min opacity 150 like Normal_Paint).
fn blend_lock_alpha_paint(
    tile: &mut TilePixels,
    entries: &[(usize, usize, u32)],
    src_r: u32,
    src_g: u32,
    src_b: u32,
    opacity: u32,
) {
    use crate::spectral::{fastpow, rgb_to_spectral, spectral_to_rgb};
    const ONE: u32 = FIX15_ONE;
    let opacity = opacity.max(150);
    let spec_a = rgb_to_spectral(
        src_r as f32 / ONE as f32,
        src_g as f32 / ONE as f32,
        src_b as f32 / ONE as f32,
    );
    for &(lx, ly, mask) in entries {
        let mut opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let p = &mut tile[ly][lx];
        opa_a *= p[3] as u32;
        opa_a /= ONE;
        let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
        if a0 == 0 {
            p[0] = ((opa_a * src_r + opa_b * r0) / ONE) as u16;
            p[1] = ((opa_a * src_g + opa_b * g0) / ONE) as u16;
            p[2] = ((opa_a * src_b + opa_b * b0) / ONE) as u16;
            continue;
        }
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
        let a_out = opa_a + opa_b * a0 / ONE;
        p[3] = a_out as u16;
        p[0] = (sr * a_out as f32 + 0.5) as u16;
        p[1] = (sg * a_out as f32 + 0.5) as u16;
        p[2] = (sb * a_out as f32 + 0.5) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_Color` — Adobe "Color" non-separable blend:
/// transfer the destination's BT.601 luminance onto the source hue/chroma,
/// in the same scaled-int arithmetic (LUMA coefficients are floats, the
/// clip steps are integer divisions).
fn blend_color(
    tile: &mut TilePixels,
    entries: &[(usize, usize, u32)],
    src_r: u32,
    src_g: u32,
    src_b: u32,
    opacity: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, ly, mask) in entries {
        let p = &mut tile[ly][lx];
        let a = p[3] as u32;
        // De-premult (C leaves r=g=b=0 when alpha is 0).
        let (mut r, mut g, mut b) = (0u16, 0u16, 0u16);
        if a != 0 {
            r = (ONE * p[0] as u32 / a) as u16;
            g = (ONE * p[1] as u32 / a) as u16;
            b = (ONE * p[2] as u32 / a) as u16;
        }
        set_rgb16_lum_from_rgb16(src_r as u16, src_g as u16, src_b as u16, &mut r, &mut g, &mut b);
        // Re-premult.
        let r = r as u32 * a / ONE;
        let g = g as u32 * a / ONE;
        let b = b as u32 * a / ONE;
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        p[0] = ((opa_a * r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// BT.601 luminance of a fix15 triple, as the C `LUMA` macro: float
/// coefficients pre-scaled by 2^15, so the product needs a /2^15 after.
#[inline]
fn luma(r: f32, g: f32, b: f32) -> f32 {
    const LUMA_RED_COEFF: f32 = 0.2126 * (1 << 15) as f32;
    const LUMA_GREEN_COEFF: f32 = 0.7152 * (1 << 15) as f32;
    const LUMA_BLUE_COEFF: f32 = 0.0722 * (1 << 15) as f32;
    r * LUMA_RED_COEFF + g * LUMA_GREEN_COEFF + b * LUMA_BLUE_COEFF
}

/// Port of brushmodes.c `set_rgb16_lum_from_rgb16` (Adobe SetLum +
/// ClipColor in scaled ints).
fn set_rgb16_lum_from_rgb16(
    topr: u16,
    topg: u16,
    topb: u16,
    botr: &mut u16,
    botg: &mut u16,
    botb: &mut u16,
) {
    const ONE: i32 = FIX15_ONE as i32;
    let botlum = (luma(*botr as f32, *botg as f32, *botb as f32) / ONE as f32) as u16;
    let toplum = (luma(topr as f32, topg as f32, topb as f32) / ONE as f32) as u16;
    let diff = botlum as i32 - toplum as i32;
    let mut r = topr as i32 + diff;
    let mut g = topg as i32 + diff;
    let mut b = topb as i32 + diff;

    let lum = (luma(r as f32, g as f32, b as f32) / ONE as f32) as i32;
    let cmin = r.min(g).min(b);
    let cmax = r.max(g).max(b);
    if cmin < 0 {
        r = lum + ((r - lum) * lum) / (lum - cmin);
        g = lum + ((g - lum) * lum) / (lum - cmin);
        b = lum + ((b - lum) * lum) / (lum - cmin);
    }
    if cmax > ONE {
        r = lum + ((r - lum) * (ONE - lum)) / (cmax - lum);
        g = lum + ((g - lum) * (ONE - lum)) / (cmax - lum);
        b = lum + ((b - lum) * (ONE - lum)) / (cmax - lum);
    }
    *botr = r as u16;
    *botg = g as u16;
    *botb = b as u16;
}

/// `draw_dab_pixels_BlendMode_Posterize` — quantise the (premultiplied)
/// canvas under the dab toward `posterize_num` levels.
fn blend_posterize(
    tile: &mut TilePixels,
    entries: &[(usize, usize, u32)],
    opacity: u32,
    posterize_num: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, ly, mask) in entries {
        let p = &mut tile[ly][lx];
        let r = p[0] as f32 / ONE as f32;
        let g = p[1] as f32 / ONE as f32;
        let b = p[2] as f32 / ONE as f32;
        // C: `(1<<15) * ROUND(r * posterize_num) / posterize_num` with
        // ROUND(x) = floor(x + 0.5) and integer division.
        let post_r = ONE * ((r * posterize_num as f32 + 0.5).floor() as u32) / posterize_num;
        let post_g = ONE * ((g * posterize_num as f32 + 0.5).floor() as u32) / posterize_num;
        let post_b = ONE * ((b * posterize_num as f32 + 0.5).floor() as u32) / posterize_num;
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        p[0] = ((opa_a * post_r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * post_g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * post_b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_Normal_Paint` — full WGM spectral
/// source-over for non-smudging paint dabs (min opacity 150/2^15).
fn blend_normal_paint(
    tile: &mut TilePixels,
    entries: &[(usize, usize, u32)],
    src_r: u32,
    src_g: u32,
    src_b: u32,
    opacity: u32,
) {
    use crate::spectral::{fastpow, rgb_to_spectral, spectral_to_rgb};
    const ONE: u32 = FIX15_ONE;
    // "pigment-mode does not like very low opacity" — brushmodes.c:87.
    let opacity = opacity.max(150);
    let spec_a = rgb_to_spectral(
        src_r as f32 / ONE as f32,
        src_g as f32 / ONE as f32,
        src_b as f32 / ONE as f32,
    );
    for &(lx, ly, mask) in entries {
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let p = &mut tile[ly][lx];
        let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
        let a_out = opa_a + opa_b * a0 / ONE;
        if a0 == 0 {
            // Nothing to mix with — plain additive.
            p[3] = a_out as u16;
            p[0] = ((opa_a * src_r + opa_b * r0) / ONE) as u16;
            p[1] = ((opa_a * src_g + opa_b * g0) / ONE) as u16;
            p[2] = ((opa_a * src_b + opa_b * b0) / ONE) as u16;
            continue;
        }
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
        p[3] = a_out as u16;
        // C: `rgba[i] = (rgb_result[i] * rgba[3]) + 0.5;`
        p[0] = (sr * a_out as f32 + 0.5) as u16;
        p[1] = (sg * a_out as f32 + 0.5) as u16;
        p[2] = (sb * a_out as f32 + 0.5) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_Normal_and_Eraser_Paint` — erase-aware
/// spectral blend that fades from additive to WGM with
/// `spectral_blend_factor(canvas alpha)`; no minimum opacity.
fn blend_normal_and_eraser_paint(
    tile: &mut TilePixels,
    entries: &[(usize, usize, u32)],
    src_r: u32,
    src_g: u32,
    src_b: u32,
    color_a: u32,
    opacity: u32,
) {
    use crate::spectral::{fastpow, rgb_to_spectral, spectral_blend_factor, spectral_to_rgb};
    const ONE: u32 = FIX15_ONE;
    let spec_a = rgb_to_spectral(
        src_r as f32 / ONE as f32,
        src_g as f32 / ONE as f32,
        src_b as f32 / ONE as f32,
    );
    for &(lx, ly, mask) in entries {
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let opa_a2 = opa_a * color_a / ONE;
        let p = &mut tile[ly][lx];
        let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
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
            fac_a *= color_a as f32 / ONE as f32;
            let fac_b = 1.0 - fac_a;
            let mut mix = [0.0_f32; 10];
            for i in 0..10 {
                mix[i] = fastpow(spec_a[i], fac_a) * fastpow(spec_b[i], fac_b);
            }
            let (sr, sg, sb) = spectral_to_rgb(&mix);
            let res = [sr, sg, sb];
            for i in 0..3 {
                // uint32 ← float expression: truncation, like C.
                rgb[i] = (additive_factor * rgb[i] as f32
                    + spectral_factor * res[i] * opa_out as f32) as u32;
            }
        }

        p[3] = opa_out as u16;
        p[0] = rgb[0] as u16;
        p[1] = rgb[1] as u16;
        p[2] = rgb[2] as u16;
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
