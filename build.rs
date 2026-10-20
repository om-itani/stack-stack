// Build script for stack-stack.
//
// Two jobs:
//
// 1. Guarantee `frontend/dist` exists before `rust-embed` expands, so a fresh
//    clone can run `cargo check` / `cargo build` before the frontend has ever
//    been built. On the Fujitsu there is no Node toolchain by design: the
//    prebuilt `dist/` is committed and embedded verbatim.
//
// 2. Fingerprint `frontend/dist` and expose it as an environment variable.
//    `rust-embed` only depends on the asset *filenames* present when the derive
//    last expanded, so rebuilding the frontend (which rotates Vite's hashed
//    filenames) would otherwise leave a stale bundle embedded. A changed
//    fingerprint changes a `rustc-env` value and therefore forces a recompile.

use std::path::{Path, PathBuf};
use std::{env, fs, io};

const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>stack-stack</title>
  </head>
  <body style="font-family: ui-monospace, monospace; background:#0b0f14; color:#c9d1d9; padding:2rem">
    <h1>stack-stack</h1>
    <p>frontend/dist has not been built yet. On Windows run <code>npm install &amp;&amp; npm run build</code> in <code>frontend/</code>.</p>
  </body>
</html>
"#;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Directory-level watch: catches files being added or removed, which the
    // per-file directives below cannot (cargo only knows about paths the
    // previous run declared).
    println!("cargo:rerun-if-changed=frontend/dist");

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    println!("cargo:rustc-env=STACK_STACK_BUILD_PROFILE={profile}");

    // Guard on index.html, not just the directory: a `dist/` that exists but is
    // empty would otherwise embed nothing and 404 every non-asset route.
    let dist = Path::new("frontend/dist");
    let index = dist.join("index.html");
    if !index.is_file() {
        fs::create_dir_all(dist).expect("create frontend/dist");
        fs::write(&index, PLACEHOLDER).expect("write frontend/dist placeholder");
    }

    let mut files = Vec::new();
    collect(dist, dist, &mut files).expect("read frontend/dist");
    files.sort();

    let mut hash = FNV_OFFSET;
    for (relative, path) in &files {
        // Track every file individually so the script re-runs on any edit.
        println!("cargo:rerun-if-changed={}", path.display());
        fnv1a(relative.as_bytes(), &mut hash);
        match fs::read(path) {
            Ok(bytes) => fnv1a(&bytes, &mut hash),
            // Deleted between listing and reading: fold the name in and move on.
            Err(_) => fnv1a(b"<unreadable>", &mut hash),
        }
    }

    println!("cargo:rustc-env=STACK_STACK_ASSETS_FINGERPRINT={hash:016x}");
}

fn collect(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect(&path, base, out)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((relative, path));
        }
    }
    Ok(())
}

fn fnv1a(bytes: &[u8], hash: &mut u64) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}
