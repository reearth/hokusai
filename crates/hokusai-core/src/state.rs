//! Per-stroke mutable state. Mirrors libmypaint's `MyPaintBrush` runtime
//! fields so the stroke engine port can be a near-line-for-line translation.

use crate::rng::BrushRng;

#[derive(Debug, Clone)]
pub struct BrushState {
    // Smoothed input position.
    pub actual_x: f32,
    pub actual_y: f32,

    // Last raw input event (for speed/direction calculation).
    pub last_event_x: f32,
    pub last_event_y: f32,
    pub last_event_time: f64,

    // Filtered radius (slow_tracking_per_dab applies here).
    pub actual_radius: f32,

    // Speed filter state — two parallel low-pass filters per libmypaint.
    pub norm_dx_slow: f32,
    pub norm_dy_slow: f32,
    pub norm_speed1_slow: f32,
    pub norm_speed2_slow: f32,

    // Stroke accounting.
    pub stroke_total_painting_time: f64,
    pub stroke_current_idling_time: f64,

    // Distance accumulated since last dab (so dab count is fractional-stable).
    pub dist_past_dab: f32,
    pub last_dab_x: f32,
    pub last_dab_y: f32,
    pub last_dab_time: f64,

    // Painting color (HSV held independently; libmypaint's
    // change_color_* mutates these between dabs).
    pub actual_h: f32,
    pub actual_s: f32,
    pub actual_v: f32,

    // Smudge bucket: filtered colour for the smudge setting.
    pub smudge_ra: f32,
    pub smudge_ga: f32,
    pub smudge_ba: f32,
    pub smudge_a: f32,

    pub rng: BrushRng,

    /// `false` until the first `stroke_to` has been processed. While `false`,
    /// `stroke_to` only seeds the position; no dabs are emitted. Mirrors
    /// libmypaint's "fresh stroke" handling.
    pub started: bool,
}

impl BrushState {
    pub fn new(seed: u32) -> Self {
        Self {
            actual_x: 0.0,
            actual_y: 0.0,
            last_event_x: 0.0,
            last_event_y: 0.0,
            last_event_time: 0.0,
            actual_radius: 0.0,
            norm_dx_slow: 0.0,
            norm_dy_slow: 0.0,
            norm_speed1_slow: 0.0,
            norm_speed2_slow: 0.0,
            stroke_total_painting_time: 0.0,
            stroke_current_idling_time: 0.0,
            dist_past_dab: 0.0,
            last_dab_x: 0.0,
            last_dab_y: 0.0,
            last_dab_time: 0.0,
            actual_h: 0.0,
            actual_s: 0.0,
            actual_v: 0.0,
            smudge_ra: 0.0,
            smudge_ga: 0.0,
            smudge_ba: 0.0,
            smudge_a: 0.0,
            rng: BrushRng::new(seed),
            started: false,
        }
    }
}

impl BrushState {
    /// Reset back to the "no stroke in progress" state, preserving the PRNG
    /// stream so re-strokes are reproducible.
    pub fn reset(&mut self) {
        let rng = self.rng.clone();
        *self = Self::new(0);
        self.rng = rng;
    }
}

impl Default for BrushState {
    fn default() -> Self {
        Self::new(0xC0FFEE)
    }
}
