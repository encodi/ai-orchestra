//! Embeds the compiled web interface (`web/dist`) into the binary, so running `ai-orchestra web`
//! needs no Node.js and no separate files. If `web/dist` does not exist (the frontend was never
//! built), the table is empty and the server explains how to build it. Nothing here runs Node.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        if path.is_dir() {
            walk(&path, base, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            let name = rel.to_string_lossy().replace('\\', "/");
            if !name.starts_with('.') && !name.ends_with(".map") {
                out.push((name, path.clone()));
            }
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let dist = root.join("web").join("dist");
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-changed=build.rs");
    let mut files = Vec::new();
    walk(&dist, &dist, &mut files);
    let mut code = String::from("pub static EMBEDDED: &[(&str, &[u8])] = &[\n");
    for (name, path) in &files {
        let _ = writeln!(
            code,
            "    ({name:?}, include_bytes!({:?})),",
            path.display().to_string()
        );
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("web_assets.rs");
    let _ = std::fs::write(out, code);
}
