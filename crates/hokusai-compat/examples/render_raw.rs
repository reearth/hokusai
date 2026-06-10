//! Render a fixture script with hokusai and write the raw RGBA8 buffer
//! (white-composited, sRGB) to stdout — byte-compatible with
//! `tools/libmypaint-render`, for ad-hoc pixel diffing:
//!
//! ```sh
//! cargo run -p hokusai-compat --example render_raw script.json > hok.rgba
//! libmypaint-render script.json brush.myb > lmp.rgba
//! cmp hok.rgba lmp.rgba
//! ```

use std::io::Write;

fn main() {
    let script_path = std::env::args()
        .nth(1)
        .expect("usage: render_raw <script.json>");
    let script_path = std::path::PathBuf::from(&script_path);
    let script = hokusai_compat::load_script(&script_path).expect("load script");
    let brush_path = script_path.parent().unwrap().join(&script.brush);
    let brush = hokusai_compat::load_brush(&brush_path).expect("load brush");
    let pixels = hokusai_compat::render(&brush, &script);
    std::io::stdout().write_all(&pixels).expect("write rgba");
}
