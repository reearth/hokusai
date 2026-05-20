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
            if !path
                .file_stem()
                .is_some_and(|s| s.to_string_lossy().contains(f))
            {
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

    let mut entries: Vec<_> = std::fs::read_dir(&fixtures)
        .expect("read fixtures dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    entries.sort();

    // Render every fixture's current hokusai output and overwrite the
    // `<stem>.actual.png` next to it. The snapshot test only writes this
    // file on mismatch, so passing fixtures otherwise leave stale images
    // sitting on disk — exactly what made earlier parity reports show
    // unchanged charcoal output after charcoal was fixed.
    eprintln!(
        "rendering current hokusai actuals for {} fixtures…",
        entries.len()
    );
    for path in &entries {
        if let Err(e) = render_actual(path) {
            eprintln!("FAIL {}: {e}", path.display());
        }
    }

    let mut rows = String::new();
    for path in &entries {
        let stem = path.file_stem().unwrap().to_string_lossy();
        let golden = fixtures.join(format!("{stem}.png"));
        let actual = fixtures.join(format!("{stem}.actual.png"));
        let mad = compute_mad(&golden, &actual).unwrap_or(f32::NAN);
        let status = if mad <= 0.5 {
            format!("{mad:.2} ≤ 0.50 (passing)")
        } else {
            format!("{mad:.2}")
        };
        let row_class = if mad.is_nan() {
            "fail"
        } else if mad <= 0.5 {
            "pass"
        } else if mad <= 5.0 {
            "warn"
        } else {
            "fail"
        };
        write!(
            rows,
            r##"<tr class="{row_class}"><th>{stem}</th>
<td><img src="../crates/hokusai-compat/fixtures/{stem}.png" alt="golden"></td>
<td><img src="../crates/hokusai-compat/fixtures/{stem}.actual.png" alt="actual"></td>
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

fn render_actual(script_path: &std::path::Path) -> Result<(), String> {
    let script =
        hokusai_compat::load_script(script_path).map_err(|e| format!("load script: {e}"))?;
    let brush_path = script_path.parent().unwrap().join(&script.brush);
    let brush = hokusai_compat::load_brush(&brush_path).map_err(|e| format!("load brush: {e}"))?;
    let pixels = hokusai_compat::render(&brush, &script);
    let actual = script_path.with_extension("actual.png");
    image::save_buffer(
        &actual,
        &pixels,
        script.width,
        script.height,
        image::ColorType::Rgba8,
    )
    .map_err(|e| format!("save {}: {e}", actual.display()))
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

/// Walk an entire libmypaint brush pack, drive each brush through a
/// fixed sample stroke with both libmypaint and hokusai, and report the
/// per-brush MAD as a sortable Markdown table written to
/// `tmp/brush-pack-report.md`. The brush pack defaults to
/// `tmp/mypaint-brushes/` (clone of <https://github.com/mypaint/mypaint-brushes>);
/// override via the `HOKUSAI_BRUSH_PACK` env var.
fn cmd_brush_pack_report() {
    use std::fmt::Write;

    let root = workspace_root();
    let wrapper = ensure_wrapper(&root);
    let pack = std::env::var("HOKUSAI_BRUSH_PACK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("tmp/mypaint-brushes"));
    if !pack.exists() {
        eprintln!(
            "brush pack not found at {} — clone mypaint/mypaint-brushes there\n\
             or set HOKUSAI_BRUSH_PACK to a directory of `.myb` files.",
            pack.display()
        );
        std::process::exit(2);
    }

    // Walk for .myb files.
    let mybs = find_mybs(&pack);
    if mybs.is_empty() {
        eprintln!("no .myb files found under {}", pack.display());
        std::process::exit(2);
    }
    eprintln!("scanning {} brushes…", mybs.len());

    // The sample script: a gentle curve with a pressure ramp. Same
    // canvas for every brush — small enough to render fast, big enough
    // for stroke dynamics to settle.
    let script = make_sample_script();
    let script_path = root.join("tmp/_brush_pack_script.json");
    std::fs::create_dir_all(script_path.parent().unwrap()).ok();
    // hokusai_compat::Script doesn't implement Serialize, so format the
    // tiny JSON by hand. The `brush` field is unused by the C wrapper
    // (we pass it as a separate argv).
    let events_json: Vec<String> = script
        .events
        .iter()
        .map(|e| format!("[{},{},{},{}]", e[0], e[1], e[2], e[3]))
        .collect();
    let script_json = format!(
        r#"{{"brush":"unused","width":{w},"height":{h},"events":[{evs}]}}"#,
        w = script.width,
        h = script.height,
        evs = events_json.join(","),
    );
    std::fs::write(&script_path, script_json).expect("write sample script");

    let mut rows: Vec<(String, f32, u32, u32)> = Vec::new();
    for (i, brush_path) in mybs.iter().enumerate() {
        let rel = brush_path.strip_prefix(&pack).unwrap_or(brush_path);
        let label = rel.with_extension("").display().to_string();
        eprint!("\r[{}/{}] {label:<60}", i + 1, mybs.len());

        let lmp_path = root.join("tmp/_lmp_pack.png");
        let lmp_bytes = match run_libmypaint(
            &wrapper,
            &script_path,
            brush_path,
            script.width,
            script.height,
        ) {
            Ok(b) => b,
            Err(e) => {
                rows.push((label, f32::NAN, 0, 0));
                eprintln!("\n  libmypaint failed: {e}");
                continue;
            }
        };
        save_rgba_png(&lmp_path, &lmp_bytes, script.width, script.height);

        let hok = match hokusai_compat::load_brush(brush_path) {
            Ok(b) => b,
            Err(e) => {
                rows.push((label, f32::NAN, 0, 0));
                eprintln!("\n  hokusai parse failed: {e}");
                continue;
            }
        };
        let hok_bytes = hokusai_compat::render(&hok, &script);
        let hok_path = root.join("tmp/_hok_pack.png");
        save_rgba_png(&hok_path, &hok_bytes, script.width, script.height);

        let mad = mad_bytes(&lmp_bytes, &hok_bytes);
        rows.push((label, mad, script.width, script.height));
    }
    eprintln!();

    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let total = rows.len();
    let passing = rows.iter().filter(|r| r.1 <= 0.5).count();
    let warn = rows.iter().filter(|r| r.1 > 0.5 && r.1 <= 5.0).count();
    let fail = rows.iter().filter(|r| r.1 > 5.0 || r.1.is_nan()).count();

    let mut md = String::new();
    writeln!(md, "# Brush-pack parity report\n").unwrap();
    writeln!(md, "Pack: `{}`\n", pack.display()).unwrap();
    writeln!(
        md,
        "{total} brushes total — **{passing} passing** (MAD ≤ 0.50), {warn} amber (≤ 5), {fail} red (> 5 or parse failure).\n"
    )
    .unwrap();
    writeln!(md, "| Brush | MAD | Verdict |").unwrap();
    writeln!(md, "|-------|-----|---------|").unwrap();
    for (label, mad, _, _) in &rows {
        let v = if mad.is_nan() {
            "💥"
        } else if *mad <= 0.5 {
            "🟢"
        } else if *mad <= 5.0 {
            "🟡"
        } else {
            "🔴"
        };
        writeln!(md, "| {label} | {mad:.2} | {v} |").unwrap();
    }
    let out = root.join("tmp/brush-pack-report.md");
    std::fs::write(&out, md).expect("write report");
    println!("wrote {}", out.display());
    println!("passing: {passing}/{total}");
}

fn find_mybs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "myb") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn make_sample_script() -> hokusai_compat::Script {
    let mut events = Vec::with_capacity(40);
    let cx = 80.0_f32;
    let cy = 80.0_f32;
    for i in 0..=40 {
        let t = i as f32 / 40.0;
        let x = 20.0 + 240.0 * t;
        let y = cy + (t * std::f32::consts::PI * 2.0).sin() * 30.0;
        let p = (t * std::f32::consts::PI).sin().max(0.05);
        events.push([x, y, p, 0.02]);
    }
    let _ = cx; // canvas centre — unused but documents the layout.
    hokusai_compat::Script {
        brush: std::path::PathBuf::new(), // unused by callers
        width: 320,
        height: 160,
        events,
    }
}

fn run_libmypaint(
    wrapper: &std::path::Path,
    script_path: &std::path::Path,
    brush_path: &std::path::Path,
    w: u32,
    h: u32,
) -> Result<Vec<u8>, String> {
    let brush_path = brush_path
        .canonicalize()
        .map_err(|e| format!("canonicalize brush: {e}"))?;
    let out = std::process::Command::new(wrapper)
        .arg(script_path)
        .arg(&brush_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "wrapper exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let expected = (w as usize) * (h as usize) * 4;
    if out.stdout.len() != expected {
        return Err(format!(
            "wrapper produced {} bytes, expected {expected}",
            out.stdout.len()
        ));
    }
    Ok(out.stdout)
}

fn save_rgba_png(path: &std::path::Path, rgba: &[u8], w: u32, h: u32) {
    image::save_buffer(path, rgba, w, h, image::ColorType::Rgba8).ok();
}

fn mad_bytes(a: &[u8], b: &[u8]) -> f32 {
    if a.len() != b.len() {
        return f32::NAN;
    }
    let mut sum = 0u64;
    for (x, y) in a.iter().zip(b.iter()) {
        sum += x.abs_diff(*y) as u64;
    }
    sum as f32 / a.len() as f32
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("regenerate-goldens") => cmd_regenerate(args.get(1).map(String::as_str)),
        Some("parity-report") => cmd_parity_report(),
        Some("brush-pack-report") => cmd_brush_pack_report(),
        _ => {
            eprintln!(
                "usage: cargo xtask <regenerate-goldens [pattern] | parity-report | brush-pack-report>"
            );
            std::process::exit(2);
        }
    }
}
