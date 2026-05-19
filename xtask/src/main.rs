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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("regenerate-goldens") => cmd_regenerate(args.get(1).map(String::as_str)),
        _ => {
            eprintln!("usage: cargo xtask regenerate-goldens [pattern]");
            std::process::exit(2);
        }
    }
}
