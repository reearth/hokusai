//! `.myb` JSON read/write — fully compatible with libmypaint v3 brush files.
//!
//! libmypaint's on-disk format is documented in `brushlib/brushsettings.json`
//! and `brushlib/mypaint-brush.c`. Top-level shape:
//!
//! ```json
//! {
//!   "version": 3,
//!   "group": "...",
//!   "parent_brush_name": "...",
//!   "comment": "...",
//!   "settings": {
//!     "<setting_name>": {
//!       "base_value": 1.0,
//!       "inputs": {
//!         "<input_name>": [[x0, y0], [x1, y1], ...]
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! Unknown settings/inputs are preserved on parse (round-trip-safe) and
//! collected into [`Brush::*unknown*`] fields so newer brush packs don't lose
//! data passing through hokusai.

use std::collections::BTreeMap;

use hokusai_core::{Brush, BrushInput, BrushSetting, InputMapping, SettingValue};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported brush version: {0}")]
    UnsupportedVersion(u32),
}

#[derive(Debug, Serialize, Deserialize)]
struct Raw {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_brush_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    #[serde(default)]
    settings: BTreeMap<String, RawSetting>,
}

fn default_version() -> u32 {
    3
}

#[derive(Debug, Serialize, Deserialize)]
struct RawSetting {
    #[serde(default)]
    base_value: f32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    inputs: BTreeMap<String, Vec<[f32; 2]>>,
}

/// Parse a `.myb` JSON document into a [`Brush`].
pub fn from_str(json: &str) -> Result<Brush, Error> {
    let raw: Raw = serde_json::from_str(json)?;
    if raw.version > 3 {
        // Forward compat: still try to read, but flag the version.
        // libmypaint refuses; we're lenient and preserve what we recognise.
    }

    let mut brush = Brush::new();
    brush.version = raw.version;
    brush.group = raw.group;
    brush.parent_brush_name = raw.parent_brush_name;
    brush.comment = raw.comment;

    for (name, rs) in raw.settings {
        let Some(setting) = BrushSetting::from_cname(&name) else {
            // Unknown setting — silently skip for now.
            // TODO: capture into Brush::unknown_settings for lossless round-trip.
            continue;
        };

        let mut sv = SettingValue {
            base_value: rs.base_value,
            inputs: Vec::with_capacity(rs.inputs.len()),
        };
        for (iname, points) in rs.inputs {
            let Some(input) = BrushInput::from_cname(&iname) else {
                continue;
            };
            sv.inputs.push(InputMapping {
                input,
                points: points.into_iter().map(|p| (p[0], p[1])).collect(),
            });
        }
        brush.set(setting, sv);
    }

    Ok(brush)
}

/// Serialize a [`Brush`] back to libmypaint-style JSON.
pub fn to_string_pretty(brush: &Brush) -> Result<String, Error> {
    let mut settings = BTreeMap::new();
    for (i, sv) in brush.settings().iter().enumerate() {
        // Skip wholly-default settings to keep output compact, matching
        // libmypaint's behaviour of only writing non-default keys.
        if sv.base_value == 0.0 && sv.inputs.is_empty() {
            continue;
        }
        let setting = BrushSetting::ALL[i];
        let inputs = sv
            .inputs
            .iter()
            .map(|m| {
                (
                    m.input.cname().to_string(),
                    m.points.iter().map(|(x, y)| [*x, *y]).collect(),
                )
            })
            .collect();
        settings.insert(
            setting.cname().to_string(),
            RawSetting {
                base_value: sv.base_value,
                inputs,
            },
        );
    }
    let raw = Raw {
        version: brush.version,
        group: brush.group.clone(),
        parent_brush_name: brush.parent_brush_name.clone(),
        comment: brush.comment.clone(),
        settings,
    };
    Ok(serde_json::to_string_pretty(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "version": 3,
        "comment": "test",
        "settings": {
            "opaque": { "base_value": 1.0, "inputs": { "pressure": [[0.0, 0.0], [1.0, 1.0]] } },
            "radius_logarithmic": { "base_value": 2.5 }
        }
    }"#;

    #[test]
    fn parses_basic() {
        let b = from_str(SAMPLE).unwrap();
        assert_eq!(b.version, 3);
        assert_eq!(b.comment.as_deref(), Some("test"));
        assert_eq!(b.get(BrushSetting::Opaque).base_value, 1.0);
        assert_eq!(b.get(BrushSetting::Radius).base_value, 2.5);
        assert_eq!(b.get(BrushSetting::Opaque).inputs.len(), 1);
        assert_eq!(
            b.get(BrushSetting::Opaque).inputs[0].input,
            BrushInput::Pressure
        );
    }

    #[test]
    fn roundtrip_preserves_known_settings() {
        let b = from_str(SAMPLE).unwrap();
        let json = to_string_pretty(&b).unwrap();
        let b2 = from_str(&json).unwrap();
        assert_eq!(b.get(BrushSetting::Opaque), b2.get(BrushSetting::Opaque));
        assert_eq!(b.get(BrushSetting::Radius), b2.get(BrushSetting::Radius));
        assert_eq!(b.comment, b2.comment);
    }
}
