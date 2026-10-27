//! Embeds the built web app (`web/dist`, or `$MEMO_WEB_DIST`) into the binary. Without a build the
//! server still works and answers non-API paths with a hint.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-env-changed=MEMO_WEB_DIST");
    let dist = match env::var_os("MEMO_WEB_DIST") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist"),
    };
    // A directory is scanned recursively for changes.
    println!("cargo:rerun-if-changed={}", dist.display());
    let mut files = Vec::new();
    if dist.is_dir() {
        collect(&dist, &dist, &mut files);
    }
    files.sort();
    let mut out = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for (path, absolute) in &files {
        out.push_str(&format!("    ({path:?}, include_bytes!({absolute:?})),\n"));
    }
    out.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("web_files.rs"), out).unwrap();
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(root, &path, files);
        } else {
            let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let absolute = fs::canonicalize(&path).unwrap().to_string_lossy().into_owned();
            files.push((relative, absolute));
        }
    }
}
