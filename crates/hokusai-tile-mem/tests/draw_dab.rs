//! End-to-end checks for the M3 draw_dab + get_color path. These run against
//! the reference `MemSurface` because every backend defaults to the same
//! `brushmodes` implementation, so passing here means all backends agree.

use hokusai_core::color::RgbaF32;
use hokusai_core::fix15;
use hokusai_core::surface::{Dab, TiledSurface};
use hokusai_core::tile::TILE_SIZE;
use hokusai_tile_mem::MemSurface;

fn red_dab(x: f32, y: f32, radius: f32) -> Dab {
    Dab {
        x,
        y,
        radius,
        color: RgbaF32::new(1.0, 0.0, 0.0, 1.0),
        opaque: 1.0,
        hardness: 1.0, // solid disk so the center pixel is fully red
        alpha_eraser: 1.0,
        aspect_ratio: 1.0,
        angle: 0.0,
        lock_alpha: 0.0,
        colorize: 0.0,
        posterize: 0.0,
        posterize_num: 1.0,
        paint: 0.0,
        anti_aliasing: 0.0,
    }
}

#[test]
fn solid_red_dab_paints_center_red() {
    let mut s = MemSurface::new();
    s.draw_dab(&red_dab(32.0, 32.0, 5.0));

    let tile = s.tile(0, 0).expect("tile (0,0) must exist");
    let center = tile[32][32];
    // Center should be near fully red, alpha 1.
    assert!(
        center[0] > fix15::FIX15_MAX_U16 - 100,
        "red = {}",
        center[0]
    );
    assert_eq!(center[1], 0);
    assert_eq!(center[2], 0);
    assert!(
        center[3] > fix15::FIX15_MAX_U16 - 100,
        "alpha = {}",
        center[3]
    );
}

#[test]
fn dab_spans_tile_boundary() {
    let mut s = MemSurface::new();
    // Center on the seam between tiles (0,0) and (1,0).
    let x = TILE_SIZE as f32; // = 64.0
    s.draw_dab(&red_dab(x, 32.0, 4.0));

    let left = s.tile(0, 0).expect("left tile");
    let right = s.tile(1, 0).expect("right tile");
    // Last column of left tile and first column of right tile both touched.
    assert!(left[32][63][0] > 0, "left edge should be painted");
    assert!(right[32][0][0] > 0, "right edge should be painted");
}

#[test]
fn eraser_reduces_alpha() {
    let mut s = MemSurface::new();
    // Lay down opaque red.
    s.draw_dab(&red_dab(32.0, 32.0, 5.0));
    let before = s.tile(0, 0).unwrap()[32][32][3];
    assert!(before > 0);

    // Erase at the same spot.
    let mut eraser = red_dab(32.0, 32.0, 5.0);
    eraser.alpha_eraser = 0.0; // pure eraser
    s.draw_dab(&eraser);
    let after = s.tile(0, 0).unwrap()[32][32][3];
    assert!(after < before, "alpha should decrease: {before} -> {after}");
}

#[test]
fn lock_alpha_paints_only_inside_existing_alpha() {
    let mut s = MemSurface::new();
    // Lay down a soft red blob.
    s.draw_dab(&red_dab(32.0, 32.0, 6.0));
    // Now paint blue across the area with lock_alpha = 1.
    let mut blue = red_dab(32.0, 32.0, 6.0);
    blue.color = RgbaF32::new(0.0, 0.0, 1.0, 1.0);
    blue.lock_alpha = 1.0;
    let before_alpha = s.tile(0, 0).unwrap()[32][32][3];
    s.draw_dab(&blue);
    let after = s.tile(0, 0).unwrap()[32][32];
    // Center should now have blue but alpha must be unchanged.
    assert!(after[2] > 0, "blue painted");
    assert_eq!(after[3], before_alpha, "alpha must be locked");

    // A point that was fully transparent before stays transparent.
    let untouched = s.tile(0, 0).unwrap()[5][5];
    assert_eq!(untouched, [0, 0, 0, 0]);
}

// The previous `anti_aliasing_softens_hard_edge` test exercised the
// pre-libmypaint AA dial which scaled feather width with the
// `anti_aliasing` setting. The libmypaint-correct port fires sub-pixel
// AA unconditionally for `radius < 3` via `calculate_rr_antialiased`,
// so the `anti_aliasing` dab field is no longer the on/off switch the
// test assumed. AA correctness is now covered by the libmypaint
// brush-pack parity harness.

#[test]
fn colorize_replaces_hue_keeps_value() {
    let mut s = MemSurface::new();
    // Lay down medium-grey blob (a=1, rgb=0.5).
    let mut grey = red_dab(32.0, 32.0, 6.0);
    grey.color = RgbaF32::new(0.5, 0.5, 0.5, 1.0);
    s.draw_dab(&grey);

    // Colorize with pure red, colorize=1.0.
    let mut colorize = red_dab(32.0, 32.0, 6.0);
    colorize.color = RgbaF32::new(1.0, 0.0, 0.0, 1.0);
    colorize.colorize = 1.0;
    s.draw_dab(&colorize);

    let p = s.tile(0, 0).unwrap()[32][32];
    let a = fix15::to_f32(p[3]);
    let r = fix15::to_f32(p[0]) / a;
    let g = fix15::to_f32(p[1]) / a;
    let b = fix15::to_f32(p[2]) / a;

    // V of the result should still ~match the original grey's V (0.5).
    let v = r.max(g).max(b);
    assert!(
        (v - 0.5).abs() < 0.05,
        "value should be preserved, got v={v}"
    );
    // R should dominate (we took red's hue).
    assert!(r > g && r > b, "red hue should win: ({r},{g},{b})");
}

#[test]
fn get_color_reads_painted_region() {
    let mut s = MemSurface::new();
    s.draw_dab(&red_dab(32.0, 32.0, 6.0));

    let c = s.get_color(32.0, 32.0, 4.0);
    assert!(c.r > 0.9, "expected red, got r={}", c.r);
    assert!(c.g < 0.05);
    assert!(c.b < 0.05);
    assert!(c.a > 0.5);
}
