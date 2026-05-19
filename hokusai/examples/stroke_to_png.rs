//! Render a hardcoded brush stroke to `out.png`.
//!
//! Run with: `cargo run --example stroke_to_png --features tile-mem`

#![allow(clippy::needless_range_loop)]

use hokusai::color::linear_to_srgb;
use hokusai::fix15;
use hokusai::mapping::{InputMapping, SettingValue};
use hokusai::tile::TILE_SIZE;
use hokusai::tile_mem::MemSurface;
use hokusai::{Brush, BrushInput, BrushSetting, BrushState, TiledSurface};

const WIDTH: u32 = 512;
const HEIGHT: u32 = 256;

fn make_brush() -> Brush {
    let mut b = Brush::new();

    // Modest brush:
    //   radius_logarithmic = log2(8) = 3 → 8 px base radius
    //   hardness 0.5, opaque 1.0 (modulated by pressure)
    //   dab spacing: 4 per actual radius → smooth strokes
    //   color: warm orange (HSV)
    b.set(BrushSetting::Radius, SettingValue::constant(3.0));
    b.set(BrushSetting::Hardness, SettingValue::constant(0.6));
    b.set(
        BrushSetting::DabsPerActualRadius,
        SettingValue::constant(4.0),
    );
    b.set(BrushSetting::ColorH, SettingValue::constant(0.07));
    b.set(BrushSetting::ColorS, SettingValue::constant(0.9));
    b.set(BrushSetting::ColorV, SettingValue::constant(1.0));

    // Opaque ramps with pressure: 0 at p=0, 1 at p=1.
    b.set(
        BrushSetting::Opaque,
        SettingValue {
            base_value: 0.0,
            inputs: vec![InputMapping {
                input: BrushInput::Pressure,
                points: vec![(0.0, 0.0), (1.0, 1.0)],
            }],
            ..Default::default()
        },
    );
    // Radius also responds to pressure: ±0.5 in log2 → ~1.4× / 0.7× swing.
    b.get_mut(BrushSetting::Radius).inputs.push(InputMapping {
        input: BrushInput::Pressure,
        points: vec![(0.0, -0.5), (1.0, 0.5)],
    });

    b
}

fn run_stroke<S: TiledSurface>(brush: &Brush, state: &mut BrushState, surface: &mut S) {
    // A wavy stroke across the canvas with rising-then-falling pressure.
    let n = 200;
    let dtime = 0.01;
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let x = 40.0 + t * (WIDTH as f32 - 80.0);
        let y = HEIGHT as f32 / 2.0 + (t * std::f32::consts::PI * 4.0).sin() * 40.0;
        // Triangular pressure profile, max 1.0 at the middle.
        let pressure = 1.0 - (2.0 * t - 1.0).abs();
        brush.stroke_to(state, surface, x, y, pressure, 0.0, 0.0, dtime);
    }
}

fn flatten_to_png(surface: &MemSurface, path: &str) -> image::ImageResult<()> {
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
                    let wx = tx * TILE_SIZE as i32 + lx as i32;
                    let wy = ty * TILE_SIZE as i32 + ly as i32;
                    if wx < 0 || wy < 0 || wx >= WIDTH as i32 || wy >= HEIGHT as i32 {
                        continue;
                    }
                    let p = tile[ly][lx];
                    // fix15 premultiplied linear → sRGB straight-alpha 8-bit,
                    // composited over the white background already in `img`.
                    let a = fix15::to_f32(p[3]);
                    if a <= 0.0 {
                        continue;
                    }
                    let r_lin = fix15::to_f32(p[0]) / a;
                    let g_lin = fix15::to_f32(p[1]) / a;
                    let b_lin = fix15::to_f32(p[2]) / a;
                    let bg = img.get_pixel(wx as u32, wy as u32);
                    let bg_r = (bg[0] as f32 / 255.0).powf(2.2);
                    let bg_g = (bg[1] as f32 / 255.0).powf(2.2);
                    let bg_b = (bg[2] as f32 / 255.0).powf(2.2);
                    let out_r = r_lin * a + bg_r * (1.0 - a);
                    let out_g = g_lin * a + bg_g * (1.0 - a);
                    let out_b = b_lin * a + bg_b * (1.0 - a);
                    img.put_pixel(
                        wx as u32,
                        wy as u32,
                        image::Rgba([
                            (linear_to_srgb(out_r).clamp(0.0, 1.0) * 255.0).round() as u8,
                            (linear_to_srgb(out_g).clamp(0.0, 1.0) * 255.0).round() as u8,
                            (linear_to_srgb(out_b).clamp(0.0, 1.0) * 255.0).round() as u8,
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
    let brush = make_brush();
    let mut state = BrushState::default();
    let mut surface = MemSurface::new();

    run_stroke(&brush, &mut state, &mut surface);

    let out = "out.png";
    match flatten_to_png(&surface, out) {
        Ok(()) => {
            eprintln!(
                "painted {} tiles → {} ({}x{})",
                surface.tile_count(),
                out,
                WIDTH,
                HEIGHT
            );
        }
        Err(e) => {
            eprintln!("failed to write {out}: {e}");
            std::process::exit(1);
        }
    }
}
