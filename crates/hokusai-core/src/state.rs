//! Per-stroke mutable state. Mirrors libmypaint's `MyPaintBrush` runtime
//! fields so the stroke engine port can be a near-line-for-line translation.

use crate::rng::BrushRng;

#[derive(Debug, Clone)]
pub struct BrushState {
    // Smoothed input position.
    pub actual_x: f32,
    pub actual_y: f32,
    /// libmypaint's `STATE.ACTUAL_X` / `STATE.ACTUAL_Y` — the dab centre,
    /// which is lagged behind the slow-tracked cursor (`actual_x` here) by
    /// `slow_tracking_per_dab`. The two coincide when that setting is 0.
    pub actual_dab_x: f32,
    pub actual_dab_y: f32,

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
    /// Smoothed motion vector with 180° symmetry — libmypaint
    /// `STATE.DIRECTION_DX/DY`. Feeds `INPUT(DIRECTION)` which is
    /// mapped to `[0, 180)`. The 180° fold is done at update time:
    /// libmypaint picks the closest of `±(step_dx, step_dy)` so a
    /// stroke that flips back along itself doesn't oscillate.
    pub direction_dx: f32,
    pub direction_dy: f32,
    /// Smoothed motion vector without symmetry — libmypaint
    /// `STATE.DIRECTION_ANGLE_DX/DY`. Feeds `INPUT(DIRECTION_ANGLE)`
    /// (`[0, 360)`).
    pub direction_angle_dx: f32,
    pub direction_angle_dy: f32,
    /// libmypaint's `STATE.CUSTOM_INPUT` — the smoothed value of
    /// `SETTING(CUSTOM_INPUT)` with time constant `CUSTOM_INPUT_SLOWNESS`.
    /// Feeds `INPUT(CUSTOM)` so a brush can chain a curve's output back
    /// in as a (lagged) input on the next dab.
    pub custom_input: f32,
    /// libmypaint's `STATE.FLIP`: alternates `+1.0` / `-1.0` per dab so
    /// the `offset_angle_2*` settings can mirror the dab back and forth
    /// across the stroke direction (used by stamping / scatter brushes).
    /// `brush_reset` initialises it to `-1` so the first dab toggles to
    /// `+1` — matching the upstream comment.
    pub flip: f32,
    /// libmypaint's `tracking_noise` skip distance — coalesces incoming
    /// events until the cursor has travelled past `0.5 * noise * base_radius`
    /// pixels, at which point one new noise sample is consumed. Keeps
    /// noise-heavy brushes (DNA_brush, particle scatters, …) producing the
    /// same point density regardless of how often the app sends pointer
    /// events.
    pub skip_distance: f32,
    pub skip_last_x: f32,
    pub skip_last_y: f32,
    pub skipped_dtime: f64,
    /// libmypaint's `STATE.ASCENSION` — the dab-by-dab smoothed ascension
    /// angle (degrees). Advanced inside the dab loop by `frac *
    /// smallest_angular_difference(STATE.ASCENSION, target_ascension)`
    /// so directional offsets and `INPUT(TILT_ASCENSION)` see a lagged
    /// pen-rotation rather than a hard jump on each event.
    pub ascension: f32,
    /// libmypaint's `STATE.DECLINATION` — same idea as `ascension`, for
    /// the pen's tilt declination.
    pub declination: f32,
    /// libmypaint's `STATE.DECLINATIONX` / `DECLINATIONY`. Tilt
    /// components in degrees (`xtilt * 60`, `ytilt * 60`), smoothed
    /// per-dab so the per-axis input curves can ride on them.
    pub declination_x: f32,
    pub declination_y: f32,

    // Stroke accounting.
    pub stroke_total_painting_time: f64,
    pub stroke_current_idling_time: f64,
    /// libmypaint's `STATE.STROKE`. Accumulates `norm_dist * exp(-stroke_duration_logarithmic)`
    /// each dab and feeds `INPUT(STROKE)` (clamped to ≤ 1.0). Wrapped by
    /// `1 + stroke_holdtime`; once stroke_holdtime > 9.9 the value
    /// saturates at 1.0 until the stroke ends.
    pub stroke_state: f32,
    /// libmypaint's `STATE.STROKE_STARTED`. Flips on when `pressure`
    /// crosses above `stroke_threshold`, and off when it drops back below
    /// `stroke_threshold * 0.9`. On the rising edge the next dab resets
    /// `stroke_state` to 0 so the `Stroke` input starts a fresh ramp.
    pub stroke_started: bool,

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
    /// libmypaint's `PREV_COL_R/G/B/A/RECENTNESS` smudge-bucket slots —
    /// the most recent canvas sample plus a counter that decays each dab
    /// and is reset to 1.0 each time the canvas is actually re-sampled.
    /// `smudge_length_log` sets how long the cached value stays in use:
    /// for the default 0 it expires immediately (re-sample every dab),
    /// for larger values libmypaint can go many dabs between
    /// `get_color` calls.
    pub prev_col_r: f32,
    pub prev_col_g: f32,
    pub prev_col_b: f32,
    pub prev_col_a: f32,
    pub prev_col_recentness: f32,

    pub rng: BrushRng,

    /// Pressure of the most recent stroke event. Used by `finish_stroke` to
    /// keep the trailing catch-up dabs at the same ink density the user was
    /// drawing with, rather than painting nothing at pressure=0.
    pub last_pressure: f32,
    /// Cached `INPUT(RANDOM)` value, mirroring libmypaint's
    /// `self->random_input`. libmypaint feeds the current value into
    /// every dab's setting evaluation and refreshes it from the PRNG
    /// *after* the dab is drawn, so consecutive dabs see distinct
    /// random samples without forcing the caller to re-read `next_unit`
    /// per setting query.
    pub random_input: f32,

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
            actual_dab_x: 0.0,
            actual_dab_y: 0.0,
            last_event_x: 0.0,
            last_event_y: 0.0,
            last_event_time: 0.0,
            actual_radius: 0.0,
            norm_dx_slow: 0.0,
            norm_dy_slow: 0.0,
            norm_speed1_slow: 0.0,
            norm_speed2_slow: 0.0,
            direction_dx: 0.0,
            direction_dy: 0.0,
            direction_angle_dx: 0.0,
            direction_angle_dy: 0.0,
            custom_input: 0.0,
            flip: -1.0,
            skip_distance: 0.0,
            skip_last_x: 0.0,
            skip_last_y: 0.0,
            skipped_dtime: 0.0,
            ascension: 0.0,
            // libmypaint zero-initialises STATE via brush_reset's memset
            // (mypaint-brush.c:159) — DECLINATION starts at 0 and ramps
            // toward 90 (the no-tilt target) over the per-dab step
            // deltas of the first event. Hokusai used to seed this at
            // 90 outright, so curves keyed on tilt_declination saw the
            // saturated 90° value from the first dab instead of the
            // libmypaint ramp.
            declination: 0.0,
            declination_x: 0.0,
            declination_y: 0.0,
            stroke_total_painting_time: 0.0,
            stroke_current_idling_time: 0.0,
            stroke_state: 0.0,
            stroke_started: false,
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
            prev_col_r: 0.0,
            prev_col_g: 0.0,
            prev_col_b: 0.0,
            prev_col_a: 0.0,
            prev_col_recentness: 0.0,
            rng: BrushRng::new(seed),
            last_pressure: 0.0,
            random_input: 0.0,
            started: false,
        }
    }
}

impl BrushState {
    /// Reset back to the "no stroke in progress" state, preserving the PRNG
    /// stream so re-strokes are reproducible.
    pub fn reset(&mut self) {
        let mut fresh = Self::new(0);
        // Move the existing RNG into `fresh` so it survives the `*self =
        // fresh` assignment — no clone of the lagged-Fibonacci buffer.
        std::mem::swap(&mut fresh.rng, &mut self.rng);
        *self = fresh;
    }
}

impl Default for BrushState {
    fn default() -> Self {
        // libmypaint seeds its per-brush PRNG with `1000`; matching it here
        // is necessary for byte-exact parity with the upstream goldens.
        Self::new(1000)
    }
}
