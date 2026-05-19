//! Setting evaluator: turn a [`Brush`] + normalized input values into the
//! per-setting evaluated values needed by the dab loop.
//!
//! Matches the core of libmypaint's `update_states_and_setting_values` —
//! input normalization (pressure, speed, tilt …) is the stroke engine's job;
//! this module is the pure `(base_value + Σ mapping.eval(input_value))` step.

use crate::brush::Brush;
use crate::input::{BrushInput, NUM_INPUTS};
use crate::setting::NUM_SETTINGS;

/// Normalized input values, indexed by [`BrushInput`]. The stroke engine
/// fills this each event before calling [`evaluate`].
#[derive(Debug, Clone, Copy, Default)]
pub struct InputValues {
    pub values: [f32; NUM_INPUTS],
}

impl InputValues {
    pub const fn new() -> Self {
        Self {
            values: [0.0; NUM_INPUTS],
        }
    }

    #[inline]
    pub fn get(&self, input: BrushInput) -> f32 {
        self.values[input.index()]
    }

    #[inline]
    pub fn set(&mut self, input: BrushInput, v: f32) {
        self.values[input.index()] = v;
    }
}

/// Evaluated setting values, indexed by [`crate::BrushSetting`].
#[derive(Debug, Clone)]
pub struct SettingValues {
    pub values: [f32; NUM_SETTINGS],
}

impl SettingValues {
    pub fn new() -> Self {
        Self {
            values: [0.0; NUM_SETTINGS],
        }
    }

    #[inline]
    pub fn get(&self, s: crate::BrushSetting) -> f32 {
        self.values[s.index()]
    }
}

impl Default for SettingValues {
    fn default() -> Self {
        Self::new()
    }
}

/// Evaluate every setting on `brush` at the given normalized inputs.
///
/// For each setting: `value = base_value + Σ mapping.eval(inputs[mapping.input])`.
/// libmypaint clamps a handful of settings after evaluation (e.g. `opaque`
/// into `[0, 2]`); that clamping is intentionally **not** done here so the
/// stroke engine can apply setting-specific post-processing in one place.
pub fn evaluate(brush: &Brush, inputs: &InputValues) -> SettingValues {
    let mut out = SettingValues::new();
    for (i, sv) in brush.settings().iter().enumerate() {
        let mut v = sv.base_value;
        for m in &sv.inputs {
            v += m.eval(inputs.get(m.input));
        }
        out.values[i] = v;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{InputMapping, SettingValue};
    use crate::BrushSetting;

    #[test]
    fn base_value_only() {
        let mut brush = Brush::new();
        brush.set(BrushSetting::Radius, SettingValue::constant(2.0));
        let out = evaluate(&brush, &InputValues::new());
        assert_eq!(out.get(BrushSetting::Radius), 2.0);
    }

    #[test]
    fn sums_base_and_mapped_inputs() {
        let mut brush = Brush::new();
        brush.set(
            BrushSetting::Opaque,
            SettingValue {
                base_value: 0.2,
                inputs: vec![InputMapping {
                    input: BrushInput::Pressure,
                    points: vec![(0.0, 0.0), (1.0, 0.5)],
                }],
                ..Default::default()
            },
        );
        let mut inputs = InputValues::new();
        inputs.set(BrushInput::Pressure, 1.0);
        let out = evaluate(&brush, &inputs);
        assert!((out.get(BrushSetting::Opaque) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn multiple_input_mappings_sum() {
        let mut brush = Brush::new();
        brush.set(
            BrushSetting::Radius,
            SettingValue {
                base_value: 1.0,
                inputs: vec![
                    InputMapping {
                        input: BrushInput::Pressure,
                        points: vec![(0.0, 0.0), (1.0, 1.0)],
                    },
                    InputMapping {
                        input: BrushInput::Speed1,
                        points: vec![(0.0, 0.0), (4.0, -0.5)],
                    },
                ],
                ..Default::default()
            },
        );
        let mut inputs = InputValues::new();
        inputs.set(BrushInput::Pressure, 0.5);
        inputs.set(BrushInput::Speed1, 2.0);
        let out = evaluate(&brush, &inputs);
        // 1.0 + 0.5 + (-0.25) = 1.25
        assert!((out.get(BrushSetting::Radius) - 1.25).abs() < 1e-6);
    }
}
