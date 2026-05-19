//! The [`Brush`] value: a full configuration consumed by the stroke engine.

use crate::mapping::SettingValue;
use crate::setting::{BrushSetting, NUM_SETTINGS};

#[derive(Debug, Clone)]
pub struct Brush {
    pub version: u32,
    pub group: Option<String>,
    pub parent_brush_name: Option<String>,
    pub comment: Option<String>,
    settings: Vec<SettingValue>,
}

impl Brush {
    pub fn new() -> Self {
        Self {
            version: 3,
            group: None,
            parent_brush_name: None,
            comment: None,
            settings: vec![SettingValue::default(); NUM_SETTINGS],
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
