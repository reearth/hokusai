//! Load a real libmypaint `.myb` brush file and render a sample stroke.
//!
//! Run with:
//! ```text
//! cargo run --example myb_to_png --features "tile-mem myb-json" \
//!   -- hokusai/examples/fixtures/charcoal.myb out.png
//! ```
//!
//! Renders 4 horizontal strokes at pressures 0.25 / 0.5 / 0.75 / 1.0 so the
//! brush's pressure dynamics are visible.

#![allow(clippy::needless_range_loop)]

use std::path::PathBuf;

use hokusai::color::linear_to_srgb;
use hokusai::myb;
use hokusai::tile::TILE_SIZE;
use hokusai::tile_mem::MemSurface;
use hokusai::{fix15, Brush, BrushSetting, BrushState, TiledSurface};

const WIDTH: u32 = 600;
const HEIGHT: u32 = 280;

fn draw_strokes<S: TiledSurface>(brush: &Brush, surface: &mut S) {
    let pressures = [0.25, 0.5, 0.75, 1.0];
    let dtime = 0.012;
    let n = 220;

    for (row, &p) in pressures.iter().enumerate() {
        let y = 40.0 + row as f32 * 60.0;
        let mut state = BrushState::default();
        for i in 0..=n {
            let t = i as f32 / n as f32;
            let x = 30.0 + t * (WIDTH as f32 - 60.0);
            brush.stroke_to(&mut state, surface, x, y, p, 0.0, 0.0, dtime);
        }
    }
}

fn flatten(surface: &MemSurface, path: &str) -> image::ImageResult<()> {
    let mut img = image::RgbaImage::from_pixel(WIDTH, HEIGHT, image::Rgba([255, 255, 255, 255]));
    let ts = TILE_SIZE as i32;
    let tiles_x = (WIDTH as i32 + ts - 1) / ts;
    let tiles_y = (HEIGHT as i32 + ts - 1) / ts;

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let Some(tile) = surface.tile(tx, ty) else {
                continue;
            };
            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    let wx = tx * ts + lx as i32;
                    let wy = ty * ts + ly as i32;
                    if wx < 0 || wy < 0 || wx >= WIDTH as i32 || wy >= HEIGHT as i32 {
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
                    let bg = img.get_pixel(wx as u32, wy as u32);
                    let br = (bg[0] as f32 / 255.0).powf(2.2);
                    let bgg = (bg[1] as f32 / 255.0).powf(2.2);
                    let bb = (bg[2] as f32 / 255.0).powf(2.2);
                    let or = r * a + br * (1.0 - a);
                    let og = g * a + bgg * (1.0 - a);
                    let ob = b * a + bb * (1.0 - a);
                    img.put_pixel(
                        wx as u32,
                        wy as u32,
                        image::Rgba([
                            (linear_to_srgb(or).clamp(0.0, 1.0) * 255.0).round() as u8,
                            (linear_to_srgb(og).clamp(0.0, 1.0) * 255.0).round() as u8,
                            (linear_to_srgb(ob).clamp(0.0, 1.0) * 255.0).round() as u8,
                            255,
                        ]),
                    );
                }
            }
        }
    }
    img.save(path)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let brush_path: PathBuf = args
        .next()
        .unwrap_or_else(|| "hokusai/examples/fixtures/charcoal.myb".into())
        .into();
    let out = args.next().unwrap_or_else(|| "out.png".into());

    let json = std::fs::read_to_string(&brush_path).unwrap_or_else(|e| {
        eprintln!("read {}: {}", brush_path.display(), e);
        std::process::exit(2);
    });
    let mut brush = myb::from_str(&json).unwrap_or_else(|e| {
        eprintln!("parse {}: {}", brush_path.display(), e);
        std::process::exit(2);
    });

    // Force a black ink so the brush dynamics — not the colour — are what we see.
    brush.get_mut(BrushSetting::ColorH).base_value = 0.0;
    brush.get_mut(BrushSetting::ColorS).base_value = 0.0;
    brush.get_mut(BrushSetting::ColorV).base_value = 0.0;

    let mut surface = MemSurface::new();
    draw_strokes(&brush, &mut surface);

    match flatten(&surface, &out) {
        Ok(()) => eprintln!(
            "{} → {} ({}x{}, {} tiles, radius_log={:.2})",
            brush_path.display(),
            out,
            WIDTH,
            HEIGHT,
            surface.tile_count(),
            brush.get(BrushSetting::Radius).base_value,
        ),
        Err(e) => {
            eprintln!("write {out}: {e}");
            std::process::exit(1);
        }
    }
}
