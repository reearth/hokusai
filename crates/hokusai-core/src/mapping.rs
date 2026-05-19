//! Per-setting input → value mapping (piecewise-linear, matches libmypaint).

use crate::input::BrushInput;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputMapping {
    pub input: BrushInput,
    /// `(input_value, output_offset)` knots. libmypaint requires `x` strictly
    /// ascending. Output is added to `base_value` after summing all inputs.
    pub points: Vec<(f32, f32)>,
}

impl InputMapping {
    pub fn new(input: BrushInput) -> Self {
        Self {
            input,
            points: Vec::new(),
        }
    }

    /// Evaluate the piecewise-linear curve at `x`. Outside the knot range we
    /// extend with the slope of the nearest segment (libmypaint behaviour).
    pub fn eval(&self, x: f32) -> f32 {
        let p = &self.points;
        match p.len() {
            0 => 0.0,
            1 => p[0].1,
            _ => {
                if x <= p[0].0 {
                    let (x0, y0) = p[0];
                    let (x1, y1) = p[1];
                    let slope = (y1 - y0) / (x1 - x0);
                    y0 + slope * (x - x0)
                } else if x >= p[p.len() - 1].0 {
                    let (x0, y0) = p[p.len() - 2];
                    let (x1, y1) = p[p.len() - 1];
                    let slope = (y1 - y0) / (x1 - x0);
                    y1 + slope * (x - x1)
                } else {
                    // Linear search is fine: curves have ≤ ~8 knots in practice.
                    let i = p.iter().position(|(px, _)| *px > x).unwrap();
                    let (x0, y0) = p[i - 1];
                    let (x1, y1) = p[i];
                    let t = (x - x0) / (x1 - x0);
                    y0 + t * (y1 - y0)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingValue {
    pub base_value: f32,
    pub inputs: Vec<InputMapping>,
}

impl SettingValue {
    pub const fn constant(v: f32) -> Self {
        Self {
            base_value: v,
            inputs: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_in_range() {
        let m = InputMapping {
            input: BrushInput::Pressure,
            points: vec![(0.0, 0.0), (1.0, 1.0)],
        };
        assert!((m.eval(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn extrapolates_with_segment_slope() {
        let m = InputMapping {
            input: BrushInput::Pressure,
            points: vec![(0.0, 0.0), (1.0, 2.0)],
        };
        assert!((m.eval(2.0) - 4.0).abs() < 1e-6);
        assert!((m.eval(-1.0) - (-2.0)).abs() < 1e-6);
    }
}
