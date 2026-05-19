//! Brush settings — 1:1 with libmypaint's `brushsettings.json` keys.
//!
//! The string keys returned by [`BrushSetting::cname`] and accepted by
//! [`BrushSetting::from_cname`] are the exact identifiers used in `.myb`
//! JSON files, so brushes authored for libmypaint round-trip cleanly.

use core::fmt;

macro_rules! define_settings {
    ($($variant:ident => $name:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(usize)]
        pub enum BrushSetting {
            $($variant),*
        }

        impl BrushSetting {
            pub const ALL: &'static [BrushSetting] = &[
                $(BrushSetting::$variant),*
            ];

            /// libmypaint canonical string name (matches `.myb` JSON keys).
            pub const fn cname(self) -> &'static str {
                match self {
                    $(BrushSetting::$variant => $name),*
                }
            }

            pub fn from_cname(s: &str) -> Option<Self> {
                match s {
                    $($name => Some(BrushSetting::$variant),)*
                    _ => None,
                }
            }

            #[inline]
            pub const fn index(self) -> usize { self as usize }
        }

        pub const NUM_SETTINGS: usize = [$(BrushSetting::$variant),*].len();
    };
}

// Order mirrors libmypaint's brushsettings.json.
// Keeping the source-of-truth list here (rather than via build.rs) until M1-2.
define_settings! {
    Opaque                       => "opaque",
    OpaqueMultiply               => "opaque_multiply",
    OpaqueLinearize              => "opaque_linearize",
    Radius                       => "radius_logarithmic",
    Hardness                     => "hardness",
    AntiAliasing                 => "anti_aliasing",
    DabsPerBasicRadius           => "dabs_per_basic_radius",
    DabsPerActualRadius          => "dabs_per_actual_radius",
    DabsPerSecond                => "dabs_per_second",
    RadiusByRandom               => "radius_by_random",
    Speed1Slowness               => "speed1_slowness",
    Speed2Slowness               => "speed2_slowness",
    Speed1Gamma                  => "speed1_gamma",
    Speed2Gamma                  => "speed2_gamma",
    OffsetByRandom               => "offset_by_random",
    OffsetBySpeed                => "offset_by_speed",
    OffsetBySpeedSlowness        => "offset_by_speed_slowness",
    SlowTracking                 => "slow_tracking",
    SlowTrackingPerDab           => "slow_tracking_per_dab",
    TrackingNoise                => "tracking_noise",
    ColorH                       => "color_h",
    ColorS                       => "color_s",
    ColorV                       => "color_v",
    RestoreColor                 => "restore_color",
    ChangeColorH                 => "change_color_h",
    ChangeColorL                 => "change_color_l",
    ChangeColorHslS              => "change_color_hsl_s",
    ChangeColorV                 => "change_color_v",
    ChangeColorHsvS              => "change_color_hsv_s",
    Smudge                       => "smudge",
    SmudgeLength                 => "smudge_length",
    SmudgeLengthLog              => "smudge_length_log",
    SmudgeRadiusLog              => "smudge_radius_log",
    Eraser                       => "eraser",
    StrokeThreshold              => "stroke_threshold",
    StrokeDurationLogarithmic    => "stroke_duration_logarithmic",
    StrokeHoldtime               => "stroke_holdtime",
    CustomInput                  => "custom_input",
    CustomInputSlowness          => "custom_input_slowness",
    EllipticalDabRatio           => "elliptical_dab_ratio",
    EllipticalDabAngle           => "elliptical_dab_angle",
    DirectionFilter              => "direction_filter",
    LockAlpha                    => "lock_alpha",
    Colorize                     => "colorize",
    SnapToPixel                  => "snap_to_pixel",
    PressureGainLog              => "pressure_gain_log",
    Posterize                    => "posterize",
    PosterizeNum                 => "posterize_num",
    // Newer mypaint additions (kept for round-trip compatibility):
    Paint                        => "paint_mode",
    SmudgeTransparency           => "smudge_transparency",
    OffsetY                      => "offset_y",
    OffsetX                      => "offset_x",
    OffsetAngle                  => "offset_angle",
    OffsetAngleAsc               => "offset_angle_asc",
    OffsetAngleView              => "offset_angle_view",
    OffsetAngle2                 => "offset_angle_2",
    OffsetAngle2Asc              => "offset_angle_2_asc",
    OffsetAngle2View             => "offset_angle_2_view",
    OffsetAngleAdj               => "offset_angle_adj",
    OffsetMultiplier             => "offset_multiplier",
    GridmapScale                 => "gridmap_scale",
    GridmapScaleX                => "gridmap_scale_x",
    GridmapScaleY                => "gridmap_scale_y",
}

impl fmt::Display for BrushSetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.cname())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_cnames() {
        for s in BrushSetting::ALL {
            assert_eq!(BrushSetting::from_cname(s.cname()), Some(*s));
        }
    }

    #[test]
    fn num_settings_matches_all() {
        assert_eq!(NUM_SETTINGS, BrushSetting::ALL.len());
    }
}
