//! Cross-check: `flatten_over_white` should produce the same RGBA bytes
//! as `hokusai_compat::render` (which does the same fix15 → sRGB-over-
//! white walk by hand). If they ever diverge, one of the conversion
//! pipelines has bit-rotted.

use hokusai_compat::{load_brush, load_script, render};
use hokusai_core::{Brush, BrushState};
use hokusai_tile_mem::MemSurface;
use hokusai_tiny_skia::flatten_over_white;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hokusai-compat/fixtures")
        .join(name)
}

fn render_to_surface(brush: &Brush, script: &hokusai_compat::Script) -> MemSurface {
    let mut state = BrushState::default();
    let mut surface = MemSurface::new();
    if let Some(first) = script.events.first() {
        brush.stroke_to(
            &mut state,
            &mut surface,
            first[0],
            first[1],
            0.0,
            0.0,
            0.0,
            10.0,
        );
    }
    for ev in &script.events {
        brush.stroke_to(
            &mut state,
            &mut surface,
            ev[0],
            ev[1],
            ev[2],
            0.0,
            0.0,
            ev[3] as f64,
        );
    }
    surface
}

#[test]
fn flatten_matches_compat_render() {
    let script_path = fixture("calligraphy_wave.json");
    let script = load_script(&script_path).unwrap();
    let brush_path = script_path.parent().unwrap().join(&script.brush);
    let brush = load_brush(&brush_path).unwrap();

    // hokusai_compat::render → premultiplied-then-flattened RGBA8 bytes.
    let compat = render(&brush, &script);

    // tiny-skia flatten over the same MemSurface output.
    let surf = render_to_surface(&brush, &script);
    let pm = flatten_over_white(&surf, script.width, script.height);

    // tiny-skia stores premul; flatten_over_white uses opaque alpha so
    // premul == straight here. Compare raw bytes.
    let pm_bytes = pm.data();
    assert_eq!(pm_bytes.len(), compat.len(), "buffer length");
    // Allow ±1 LSB rounding noise on the colour channels; alpha is
    // always 255 on both paths.
    let mut max_diff = 0u8;
    let mut mismatches = 0u64;
    for (a, b) in pm_bytes.iter().zip(compat.iter()) {
        let d = a.abs_diff(*b);
        if d > 0 {
            mismatches += 1;
            if d > max_diff {
                max_diff = d;
            }
        }
    }
    assert!(
        max_diff <= 1,
        "tiny-skia flatten diverged: max byte diff {max_diff} over {mismatches} pixels"
    );
}
