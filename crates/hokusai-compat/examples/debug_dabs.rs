//! Quick debug: replay a fixture, intercept draw_dab on a MemSurface, print
//! every dab's position/radius/opacity. Helps compare against libmypaint.

use hokusai_compat::{load_brush, load_script};
use hokusai_core::{Brush, BrushState, Dab, TiledSurface};
use hokusai_tile_mem::MemSurface;

fn main() {
    let script_path = std::env::args()
        .nth(1)
        .expect("usage: debug_dabs <script.json>");
    let script_path = std::path::PathBuf::from(&script_path);
    let script = load_script(&script_path).expect("load script");
    let brush_path = script_path.parent().unwrap().join(&script.brush);
    let brush: Brush = load_brush(&brush_path).expect("load brush");

    let mut state = BrushState::default();
    let mut surf = TraceSurface {
        inner: MemSurface::new(),
        count: 0,
    };
    println!("# events: {}", script.events.len());
    // Mirror `hokusai_compat::render`'s warm-up: a long-dt seed call at the
    // first event's position so the trace lines up with `libmypaint-render`
    // (which does the same and prints from the same starting state).
    if let Some(first) = script.events.first() {
        brush.stroke_to(
            &mut state, &mut surf, first[0], first[1], 0.0, 0.0, 0.0, 10.0,
        );
        surf.count = 0; // reset dab counter so dab#1 == first painted dab
    }
    for (i, ev) in script.events.iter().enumerate() {
        let painted = brush.stroke_to(
            &mut state,
            &mut surf,
            ev[0],
            ev[1],
            ev[2],
            0.0,
            0.0,
            ev[3] as f64,
        );
        println!(
            "ev{}: pos=({},{}) p={} dt={} painted={} actual_r={:.3} carry={:.3}",
            i, ev[0], ev[1], ev[2], ev[3], painted, state.actual_radius, state.dist_past_dab
        );
    }
    println!("total dabs: {}", surf.count);
}

struct TraceSurface {
    inner: MemSurface,
    count: u32,
}

impl TiledSurface for TraceSurface {
    fn tile_request_start(&mut self, tx: i32, ty: i32) -> &mut hokusai_core::tile::TilePixels {
        self.inner.tile_request_start(tx, ty)
    }
    fn tile_request_end(&mut self, tx: i32, ty: i32) {
        self.inner.tile_request_end(tx, ty)
    }
    fn tile_lookup(&self, tx: i32, ty: i32) -> Option<&hokusai_core::tile::TilePixels> {
        self.inner.tile_lookup(tx, ty)
    }
    fn draw_dab(&mut self, dab: &Dab) -> bool {
        self.count += 1;
        println!(
            "  dab#{}: ({:6.2},{:6.2}) r={:5.2} hard={:4.2} opaq={:4.2} aspect={:4.2} ang={:6.1} aa={:4.2}",
            self.count,
            dab.x,
            dab.y,
            dab.radius,
            dab.hardness,
            dab.opaque,
            dab.aspect_ratio,
            dab.angle,
            dab.anti_aliasing,
        );
        self.inner.draw_dab(dab)
    }
}
