//! Workspace task runner.
//!
//! Currently implements:
//!   `cargo xtask regenerate-goldens [pattern]`
//!     Rebuilds the libmypaint-render C wrapper if needed, then walks
//!     `crates/hokusai-compat/fixtures/*.json`, drives the wrapper, and
//!     writes the resulting PNG snapshot beside each script.
//!
//! Environment overrides:
//!   HOKUSAI_LIBMYPAINT_RENDER  Path to a prebuilt wrapper binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Script {
    brush: PathBuf,
    width: u32,
    height: u32,
    // events: not needed by xtask, the C wrapper parses them itself.
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at xtask/, parent is the workspace.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must live in a workspace")
        .to_path_buf()
}

fn ensure_wrapper(root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("HOKUSAI_LIBMYPAINT_RENDER") {
        return PathBuf::from(p);
    }
    let tool_dir = root.join("tools/libmypaint-render");
    let bin = tool_dir.join("libmypaint-render");
    let src = tool_dir.join("render.c");
    let needs_build = !bin.exists()
        || std::fs::metadata(&src).and_then(|m| m.modified()).ok()
            > std::fs::metadata(&bin).and_then(|m| m.modified()).ok();
    if needs_build {
        eprintln!("building libmypaint-render…");
        let status = Command::new("make")
            .current_dir(&tool_dir)
            .status()
            .expect("failed to invoke make");
        if !status.success() {
            panic!("make failed");
        }
    }
    bin
}

fn regenerate_one(wrapper: &Path, script_path: &Path) -> Result<(), String> {
    let script_text = std::fs::read_to_string(script_path)
        .map_err(|e| format!("read {}: {e}", script_path.display()))?;
    let script: Script = serde_json::from_str(&script_text)
        .map_err(|e| format!("parse {}: {e}", script_path.display()))?;

    let script_dir = script_path.parent().unwrap();
    let brush_path = script_dir.join(&script.brush);
    let brush_path = brush_path
        .canonicalize()
        .map_err(|e| format!("brush {}: {e}", brush_path.display()))?;

    let out = Command::new(wrapper)
        .arg(script_path)
        .arg(&brush_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("spawn wrapper: {e}"))?;
    if !out.status.success() {
        return Err(format!("wrapper exited {}", out.status));
    }

    let expected = (script.width as usize) * (script.height as usize) * 4;
    if out.stdout.len() != expected {
        return Err(format!(
            "wrapper produced {} bytes, expected {}",
            out.stdout.len(),
            expected
        ));
    }

    let png_path = script_path.with_extension("png");
    image::save_buffer(
        &png_path,
        &out.stdout,
        script.width,
        script.height,
        image::ColorType::Rgba8,
    )
    .map_err(|e| format!("save {}: {e}", png_path.display()))?;
    println!("wrote {}", png_path.display());
    Ok(())
}

fn cmd_regenerate(filter: Option<&str>) {
    let root = workspace_root();
    let wrapper = ensure_wrapper(&root);
    let fixtures = root.join("crates/hokusai-compat/fixtures");

    let mut entries: Vec<_> = std::fs::read_dir(&fixtures)
        .expect("read fixtures dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    entries.sort();

    let mut failed = 0;
    for path in &entries {
        if let Some(f) = filter {
            if !path.file_stem().is_some_and(|s| s.to_string_lossy().contains(f)) {
                continue;
            }
        }
        if let Err(e) = regenerate_one(&wrapper, path) {
            eprintln!("FAIL {}: {e}", path.display());
            failed += 1;
        }
    }
    if failed > 0 {
        std::process::exit(1);
    }
}

/// Render the current hokusai output for every fixture, then write
/// `tmp/parity.html` — a side-by-side grid of libmypaint golden, hokusai
/// actual, and per-fixture MAD. Lets you eyeball the whole parity surface
/// in one page instead of opening images individually.
fn cmd_parity_report() {
    use std::fmt::Write;

    let root = workspace_root();
    let fixtures = root.join("crates/hokusai-compat/fixtures");
    let out_dir = root.join("tmp");
    std::fs::create_dir_all(&out_dir).expect("create tmp dir");

    // Run the snapshot test in update-actual mode by invoking the snapshot
    // test binary; rendering happens through hokusai_compat::render so the
    // .actual.png files reflect the current code.
    eprintln!("rendering current actuals via snapshot test…");
    let _ = std::process::Command::new("cargo")
        .args([
            "test", "-p", "hokusai-compat", "--test", "snapshots",
            "--", "--quiet",
        ])
        .current_dir(&root)
        .status();

    let mut entries: Vec<_> = std::fs::read_dir(&fixtures)
        .expect("read fixtures dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    entries.sort();

    let mut rows = String::new();
    for path in &entries {
        let stem = path.file_stem().unwrap().to_string_lossy();
        let golden = fixtures.join(format!("{stem}.png"));
        let actual = fixtures.join(format!("{stem}.actual.png"));
        let actual_exists = actual.exists();
        let mad = if actual_exists {
            compute_mad(&golden, &actual).unwrap_or(f32::NAN)
        } else {
            0.0
        };
        let status = if !actual_exists {
            "passing".to_string()
        } else if mad <= 0.5 {
            format!("{mad:.2} ≤ 0.50 (passing)")
        } else {
            format!("{mad:.2}")
        };
        let row_class = if !actual_exists || mad <= 0.5 {
            "pass"
        } else if mad <= 5.0 {
            "warn"
        } else {
            "fail"
        };
        let actual_src = if actual_exists {
            format!("../crates/hokusai-compat/fixtures/{stem}.actual.png")
        } else {
            format!("../crates/hokusai-compat/fixtures/{stem}.png")
        };
        write!(
            rows,
            r##"<tr class="{row_class}"><th>{stem}</th>
<td><img src="../crates/hokusai-compat/fixtures/{stem}.png" alt="golden"></td>
<td><img src="{actual_src}" alt="actual"></td>
<td class="mad">{status}</td></tr>
"##,
        )
        .unwrap();
    }

    let html = format!(
        r##"<!doctype html>
<meta charset="utf-8">
<title>hokusai ↔ libmypaint parity</title>
<style>
  body {{ font: 13px/1.4 system-ui, sans-serif; margin: 24px; background: #1b1d22; color: #ddd; }}
  table {{ border-collapse: collapse; }}
  th, td {{ padding: 6px 10px; vertical-align: middle; }}
  th {{ text-align: left; font-weight: 600; min-width: 220px; }}
  img {{ display: block; image-rendering: pixelated; max-width: 720px; background: #fff; }}
  tr.pass {{ background: #20302a; }}
  tr.warn {{ background: #3a3220; }}
  tr.fail {{ background: #3c2424; }}
  td.mad {{ font-variant-numeric: tabular-nums; }}
  h1 {{ margin: 0 0 16px; font-size: 18px; }}
  .legend {{ margin-bottom: 12px; color: #aaa; }}
</style>
<h1>hokusai ↔ libmypaint parity report</h1>
<p class="legend">Left: libmypaint golden &middot; Right: hokusai current &middot; MAD = mean abs diff per channel (0–255). Green ≤ 0.50, amber ≤ 5, red &gt; 5.</p>
<table>
  <thead><tr><th>fixture</th><th>libmypaint</th><th>hokusai</th><th>MAD</th></tr></thead>
  <tbody>
{rows}  </tbody>
</table>
"##
    );
    let out_path = out_dir.join("parity.html");
    std::fs::write(&out_path, html).expect("write html");
    println!("wrote {}", out_path.display());
}

fn compute_mad(a: &std::path::Path, b: &std::path::Path) -> Option<f32> {
    let ia = image::open(a).ok()?.to_rgba8().into_raw();
    let ib = image::open(b).ok()?.to_rgba8().into_raw();
    if ia.len() != ib.len() {
        return None;
    }
    let mut sum = 0u64;
    for (x, y) in ia.iter().zip(ib.iter()) {
        sum += x.abs_diff(*y) as u64;
    }
    Some(sum as f32 / ia.len() as f32)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("regenerate-goldens") => cmd_regenerate(args.get(1).map(String::as_str)),
        Some("parity-report") => cmd_parity_report(),
        _ => {
            eprintln!("usage: cargo xtask <regenerate-goldens [pattern] | parity-report>");
            std::process::exit(2);
        }
    }
}
