//! The [`Brush`] value: a full configuration consumed by the stroke engine.

use std::collections::BTreeMap;

use crate::mapping::SettingValue;
use crate::setting::{BrushSetting, NUM_SETTINGS};

/// A setting hokusai doesn't (yet) recognise. Held as raw JSON so a parsed
/// brush can be re-serialised without losing data — important for working
/// with brush packs authored against newer libmypaint than this crate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UnknownSetting {
    pub base_value: f32,
    /// `(input_name, points)` — input names are kept as strings so we can
    /// passthrough inputs hokusai doesn't recognise either.
    pub inputs: BTreeMap<String, Vec<(f32, f32)>>,
}

#[derive(Debug, Clone)]
pub struct Brush {
    pub version: u32,
    pub group: Option<String>,
    pub parent_brush_name: Option<String>,
    pub comment: Option<String>,
    settings: Vec<SettingValue>,
    /// Settings hokusai didn't recognise on parse, kept by string key for
    /// lossless round-trip via `hokusai-brush`.
    pub unknown_settings: BTreeMap<String, UnknownSetting>,
}

impl Brush {
    pub fn new() -> Self {
        Self {
            version: 3,
            group: None,
            parent_brush_name: None,
            comment: None,
            settings: vec![SettingValue::default(); NUM_SETTINGS],
            unknown_settings: BTreeMap::new(),
        }
    }

    #[inline]
    pub fn get(&self, s: BrushSetting) -> &SettingValue {
        &self.settings[s.index()]
    }

    #[inline]
    pub fn get_mut(&mut self, s: BrushSetting) -> &mut SettingValue {
        &mut self.settings[s.index()]
    }

    pub fn set(&mut self, s: BrushSetting, v: SettingValue) {
        self.settings[s.index()] = v;
    }

    pub fn settings(&self) -> &[SettingValue] {
        &self.settings
    }
}

impl Default for Brush {
    fn default() -> Self {
        Self::new()
    }
}
