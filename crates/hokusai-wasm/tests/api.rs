//! Native-target tests for the JS-facing API surface.
//!
//! The crate is built with `crate-type = ["cdylib", "rlib"]` so these tests
//! exercise the exact same Rust functions wasm-bindgen exposes to JavaScript,
//! minus the bindgen layer itself. They catch contract regressions (wrong
//! input handling, state lifecycle, pixel-buffer dimensions) without needing
//! a browser. True in-browser behaviour still requires `wasm-bindgen-test`.

use hokusai_wasm::{HokusaiBrush, HokusaiCanvas};

const MINIMAL_BRUSH: &str = r#"{
    "version": 3,
    "settings": {
        "opaque": { "base_value": 1.0 },
        "hardness": { "base_value": 0.5 },
        "radius_logarithmic": { "base_value": 2.0 },
        "dabs_per_actual_radius": { "base_value": 2.0 }
    }
}"#;

#[test]
fn brush_parses_minimal_myb() {
    let _ = HokusaiBrush::new(MINIMAL_BRUSH).expect("parse ok");
}

// `HokusaiBrush::new` returns `JsError` whose constructor calls into JS, so
// the failure path can only be exercised in-browser via wasm-bindgen-test.
// The success path is covered by `brush_parses_minimal_myb` above.

#[test]
fn canvas_has_declared_dimensions() {
    let c = HokusaiCanvas::new(128, 64);
    assert_eq!(c.width(), 128);
    assert_eq!(c.height(), 64);
}

#[test]
fn pixels_buffer_is_rgba8_sized() {
    let mut c = HokusaiCanvas::new(32, 16);
    let px = c.pixels();
    assert_eq!(px.len(), (32 * 16 * 4) as usize);
    // Untouched canvas is white.
    assert_eq!(&px[..4], &[255, 255, 255, 255]);
}

#[test]
fn first_stroke_event_seeds_no_pixels() {
    let mut brush = HokusaiBrush::new(MINIMAL_BRUSH).unwrap();
    brush.set_color_hsv(0.0, 0.0, 0.0); // pure black
    let mut c = HokusaiCanvas::new(64, 64);
    c.stroke_to(&brush, 10.0, 10.0, 1.0, 0.0, 0.0, 0.01);
    let px = c.pixels();
    // Still white everywhere (first event only seeds position).
    assert!(px.iter().all(|&b| b == 255));
}

#[test]
fn two_events_paint_pixels() {
    let mut brush = HokusaiBrush::new(MINIMAL_BRUSH).unwrap();
    brush.set_color_hsv(0.0, 0.0, 0.0);
    let mut c = HokusaiCanvas::new(64, 64);
    c.stroke_to(&brush, 10.0, 32.0, 1.0, 0.0, 0.0, 0.01);
    c.stroke_to(&brush, 50.0, 32.0, 1.0, 0.0, 0.0, 0.01);
    let px = c.pixels();
    // At least one pixel should be non-white.
    let any_dark = px
        .chunks_exact(4)
        .any(|p| p[0] < 200 && p[1] < 200 && p[2] < 200);
    assert!(any_dark, "expected dab pixels in the stroke region");
}

#[test]
fn reset_stroke_breaks_segment() {
    // After reset_stroke, the next event must be a fresh seed — i.e. no
    // line should be drawn between the previous endpoint and the new point.
    let brush = HokusaiBrush::new(MINIMAL_BRUSH).unwrap();
    let mut c = HokusaiCanvas::new(64, 64);
    c.stroke_to(&brush, 10.0, 10.0, 1.0, 0.0, 0.0, 0.01);
    c.stroke_to(&brush, 20.0, 10.0, 1.0, 0.0, 0.0, 0.01);
    c.reset_stroke();
    // Skip across the canvas; without the reset this would draw a line.
    c.stroke_to(&brush, 50.0, 50.0, 1.0, 0.0, 0.0, 0.01);
    let px = c.pixels();
    // Sample mid-jump (around x=35, y=30) — must be untouched white.
    let idx = ((30 * 64 + 35) * 4) as usize;
    assert_eq!(&px[idx..idx + 4], &[255, 255, 255, 255]);
}

#[test]
fn tilt_arguments_accepted_and_affect_output() {
    // A brush with elliptical_dab_angle tied to tilt_ascension produces
    // visibly different dabs depending on the tilt direction.
    let json = r#"{
        "version": 3,
        "settings": {
            "opaque": { "base_value": 1.0 },
            "hardness": { "base_value": 1.0 },
            "radius_logarithmic": { "base_value": 2.0 },
            "dabs_per_actual_radius": { "base_value": 2.0 },
            "elliptical_dab_ratio": { "base_value": 4.0 },
            "elliptical_dab_angle": {
                "base_value": 0.0,
                "inputs": { "tilt_ascension": [[-180.0, -180.0], [180.0, 180.0]] }
            }
        }
    }"#;
    let brush = HokusaiBrush::new(json).unwrap();

    let mut a = HokusaiCanvas::new(80, 80);
    a.stroke_to(&brush, 20.0, 40.0, 1.0, 1.0, 0.0, 0.01);
    a.stroke_to(&brush, 60.0, 40.0, 1.0, 1.0, 0.0, 0.01);

    let mut b = HokusaiCanvas::new(80, 80);
    b.stroke_to(&brush, 20.0, 40.0, 1.0, 0.0, 1.0, 0.01);
    b.stroke_to(&brush, 60.0, 40.0, 1.0, 0.0, 1.0, 0.01);

    let pa = a.pixels();
    let pb = b.pixels();
    // The two pixel buffers must differ somewhere — tilt rotated the dab.
    assert_ne!(pa, pb, "tilt_ascension should change pixel output");
}
