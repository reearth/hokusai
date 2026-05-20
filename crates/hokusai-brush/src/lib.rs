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

use hokusai_core::brush::UnknownSetting;
use hokusai_core::{Brush, BrushInput, BrushSetting, InputMapping, SettingValue};
use serde::{Deserialize, Serialize};

/// libmypaint's `brushsettings.json` defaults for every setting whose
/// `def` is non-zero, plus the special `opaque_multiply` pressure
/// mapping that `mypaint_brush_set_defaults` installs. Settings absent
/// from the .myb JSON inherit these values, matching libmypaint's
/// behaviour where `mypaint_brush_new` seeds the brush with the JSON
/// defaults before any per-brush override.
fn apply_libmypaint_defaults(brush: &mut Brush) {
    use BrushSetting::*;
    const DEFAULTS: &[(BrushSetting, f32)] = &[
        (Opaque, 1.0),
        (OpaqueLinearize, 0.9),
        (Radius, 2.0),
        (Hardness, 0.8),
        (AntiAliasing, 1.0),
        (DabsPerActualRadius, 2.0),
        (Speed1Slowness, 0.04),
        (Speed2Slowness, 0.8),
        (Speed1Gamma, 4.0),
        (Speed2Gamma, 4.0),
        (OffsetBySpeedSlowness, 1.0),
        (SmudgeLength, 0.5),
        (StrokeDurationLogarithmic, 4.0),
        (EllipticalDabRatio, 1.0),
        (EllipticalDabAngle, 90.0),
        (DirectionFilter, 2.0),
        (GridmapScaleX, 1.0),
        (GridmapScaleY, 1.0),
        (PosterizeNum, 0.05),
        // libmypaint v1.6.1's compiled default for PAINT_MODE is 0.0,
        // NOT the 1.0 the current master brushsettings.json shows
        // (the JSON was bumped post-v1.6.1 but our reference dylib
        // uses the older C-side `setting_info()->def` of 0.0). Tracing
        // an imp_details stroke through libmypaint shows paint=0.00
        // on every dab — proof the runtime default is 0, not 1.
        (Paint, 0.0),
    ];
    for (setting, def) in DEFAULTS {
        if brush.get(*setting).base_value == 0.0
            && brush.get(*setting).inputs.is_empty()
            && brush.get(*setting).unknown_inputs.is_empty()
        {
            brush.set(*setting, SettingValue::constant(*def));
        }
    }
    // opaque_multiply default: base 0 with pressure mapping [(0,0),(1,1)].
    // We only install it when the brush left the setting completely
    // untouched — a brush that explicitly clears it should stay cleared.
    let om = brush.get(OpaqueMultiply);
    if om.base_value == 0.0 && om.inputs.is_empty() && om.unknown_inputs.is_empty() {
        let mut sv = SettingValue::constant(0.0);
        sv.inputs.push(InputMapping {
            input: BrushInput::Pressure,
            points: vec![(0.0, 0.0), (1.0, 1.0)],
        });
        brush.set(OpaqueMultiply, sv);
    }
}

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

    // libmypaint initialises every setting to its `brushsettings.json`
    // default before applying overrides from the .myb file
    // (mypaint_brush_new → set_defaults in mypaint-brush.c). Many
    // commonly-omitted settings have non-zero defaults — most
    // visibly `paint_mode = 1.0` (spectral pigment mixing) and
    // `opaque_multiply = 0` with a pressure curve [(0,0),(1,1)] —
    // so brushes that don't list them in JSON still pick up the
    // libmypaint behaviour. Apply the same defaults here.
    apply_libmypaint_defaults(&mut brush);

    for (name, rs) in raw.settings {
        let Some(setting) = BrushSetting::from_cname(&name) else {
            // Unknown setting — preserve for lossless round-trip.
            let mut u = UnknownSetting {
                base_value: rs.base_value,
                inputs: BTreeMap::new(),
            };
            for (iname, points) in rs.inputs {
                u.inputs
                    .insert(iname, points.into_iter().map(|p| (p[0], p[1])).collect());
            }
            brush.unknown_settings.insert(name, u);
            continue;
        };

        let mut sv = SettingValue {
            base_value: rs.base_value,
            inputs: Vec::with_capacity(rs.inputs.len()),
            unknown_inputs: BTreeMap::new(),
        };
        for (iname, points) in rs.inputs {
            let Some(input) = BrushInput::from_cname(&iname) else {
                // Stash inputs hokusai doesn't recognise so the
                // `to_string_pretty` path can put them back. They don't
                // participate in evaluation — newer brush packs that ship
                // exotic inputs will lose dynamics until those inputs are
                // implemented, but the JSON survives a round trip.
                sv.unknown_inputs
                    .insert(iname, points.into_iter().map(|p| (p[0], p[1])).collect());
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
        if sv.base_value == 0.0 && sv.inputs.is_empty() && sv.unknown_inputs.is_empty() {
            continue;
        }
        let setting = BrushSetting::ALL[i];
        let mut inputs: BTreeMap<String, Vec<[f32; 2]>> = sv
            .inputs
            .iter()
            .map(|m| {
                (
                    m.input.cname().to_string(),
                    m.points.iter().map(|(x, y)| [*x, *y]).collect(),
                )
            })
            .collect();
        for (iname, pts) in &sv.unknown_inputs {
            inputs.insert(iname.clone(), pts.iter().map(|(x, y)| [*x, *y]).collect());
        }
        settings.insert(
            setting.cname().to_string(),
            RawSetting {
                base_value: sv.base_value,
                inputs,
            },
        );
    }
    // Emit unknown settings back out so a parse+serialize cycle preserves
    // every key the brush was originally authored with.
    for (name, u) in &brush.unknown_settings {
        let inputs = u
            .inputs
            .iter()
            .map(|(iname, pts)| (iname.clone(), pts.iter().map(|(x, y)| [*x, *y]).collect()))
            .collect();
        settings.insert(
            name.clone(),
            RawSetting {
                base_value: u.base_value,
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
    fn unknown_settings_roundtrip_losslessly() {
        let json = r#"{
            "version": 3,
            "settings": {
                "opaque": { "base_value": 1.0 },
                "future_blink_blink": {
                    "base_value": 0.42,
                    "inputs": { "future_zoom": [[0.0, 0.0], [1.0, 1.0]] }
                }
            }
        }"#;
        let b = from_str(json).unwrap();
        assert_eq!(b.unknown_settings.len(), 1);
        let u = b.unknown_settings.get("future_blink_blink").unwrap();
        assert_eq!(u.base_value, 0.42);
        assert_eq!(u.inputs.get("future_zoom").unwrap().len(), 2);

        // Roundtrip retains the unknown setting.
        let out = to_string_pretty(&b).unwrap();
        let b2 = from_str(&out).unwrap();
        assert_eq!(b.unknown_settings, b2.unknown_settings);
    }

    #[test]
    fn unknown_inputs_inside_known_settings_roundtrip() {
        // `opaque` is a hokusai-known setting, but `future_input` is not
        // a known input. Round-tripping must keep the future_input curve.
        let json = r#"{
            "version": 3,
            "settings": {
                "opaque": {
                    "base_value": 0.5,
                    "inputs": {
                        "pressure": [[0.0, 0.0], [1.0, 1.0]],
                        "future_input": [[0.0, 0.25], [1.0, -0.25]]
                    }
                }
            }
        }"#;
        let b = from_str(json).unwrap();
        let opa = b.get(BrushSetting::Opaque);
        assert_eq!(opa.inputs.len(), 1, "known input parsed");
        assert_eq!(opa.unknown_inputs.len(), 1, "unknown input stashed");
        let pts = opa.unknown_inputs.get("future_input").unwrap();
        assert_eq!(pts.len(), 2);
        assert!((pts[1].1 - (-0.25)).abs() < 1e-6);

        let out = to_string_pretty(&b).unwrap();
        let b2 = from_str(&out).unwrap();
        assert_eq!(b.get(BrushSetting::Opaque), b2.get(BrushSetting::Opaque));
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
