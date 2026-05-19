//! Snapshot regression tests.
//!
//! Iterates over every `fixtures/*.json` script, renders it, and compares
//! the result against the matching `<name>.png` golden. Set
//! `HOKUSAI_UPDATE_GOLDENS=1` to overwrite mismatched goldens.

use std::path::{Path, PathBuf};

use hokusai_compat::{diff_mad, load_brush, load_script, render};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures() -> Vec<PathBuf> {
    let dir = manifest_dir().join("fixtures");
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect()
}

#[test]
fn all_snapshots() {
    let update = std::env::var_os("HOKUSAI_UPDATE_GOLDENS").is_some();
    // Mean-abs-diff per channel. Stays generous until libmypaint-sourced
    // goldens replace the self-generated ones (see lib.rs docs).
    const TOLERANCE: f32 = 0.5;

    let cases = fixtures();
    assert!(!cases.is_empty(), "no fixtures found");

    let mut failed = Vec::new();
    for script_path in &cases {
        let script = load_script(script_path).expect("load script");
        let brush_path = resolve_relative(script_path, &script.brush);
        let brush = load_brush(&brush_path).expect("load brush");

        let actual = render(&brush, &script);

        let golden_path = script_path.with_extension("png");
        let golden = read_png(&golden_path);

        match golden {
            Some(expected) if expected.len() == actual.len() => {
                let d = diff_mad(&actual, &expected);
                if d > TOLERANCE {
                    if update {
                        write_png(&golden_path, &actual, script.width, script.height);
                        eprintln!("UPDATED {} (mad was {d:.2})", golden_path.display());
                    } else {
                        write_png(
                            &script_path.with_extension("actual.png"),
                            &actual,
                            script.width,
                            script.height,
                        );
                        failed.push(format!(
                            "{}: mad {:.2} > {:.2}",
                            script_path.display(),
                            d,
                            TOLERANCE
                        ));
                    }
                }
            }
            _ => {
                if update || golden.is_none() {
                    write_png(&golden_path, &actual, script.width, script.height);
                    eprintln!("WROTE   {}", golden_path.display());
                } else {
                    failed.push(format!(
                        "{}: dimension mismatch with golden",
                        script_path.display()
                    ));
                }
            }
        }
    }

    if !failed.is_empty() {
        panic!(
            "snapshot mismatches ({}):\n  {}",
            failed.len(),
            failed.join("\n  ")
        );
    }
}

fn resolve_relative(script_path: &Path, brush_path: &Path) -> PathBuf {
    if brush_path.is_absolute() {
        brush_path.to_path_buf()
    } else {
        script_path.parent().unwrap().join(brush_path)
    }
}

fn read_png(path: &Path) -> Option<Vec<u8>> {
    let img = image::open(path).ok()?.to_rgba8();
    Some(img.into_raw())
}

fn write_png(path: &Path, rgba: &[u8], w: u32, h: u32) {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec()).expect("size matches");
    img.save(path)
        .unwrap_or_else(|e| panic!("save {}: {e}", path.display()));
}
