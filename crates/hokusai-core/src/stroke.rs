//! Stroke engine entry point. Real implementation lands in M2 (port of
//! libmypaint's `mypaint_brush_stroke_to`).

use crate::brush::Brush;
use crate::state::BrushState;
use crate::surface::TiledSurface;

impl Brush {
    /// Feed one pointer event. `dtime` is seconds since the previous call.
    ///
    /// Returns `true` if at least one dab was painted.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_to<S: TiledSurface>(
        &self,
        _state: &mut BrushState,
        _surface: &mut S,
        _x: f32,
        _y: f32,
        _pressure: f32,
        _xtilt: f32,
        _ytilt: f32,
        _dtime: f64,
    ) -> bool {
        // TODO(M2): port mypaint_brush_stroke_to from libmypaint's mypaint-brush.c
        false
    }
}
