// Build script for stack-stack.
//
// Its only job right now is to guarantee that `frontend/dist` exists before
// `rust-embed` expands, so a fresh clone can run `cargo check` / `cargo build`
// even before the frontend has been built once. On the Fujitsu there is no Node
// toolchain by design: the prebuilt `dist/` is committed and embedded verbatim.

use std::path::Path;

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

fn main() {
    println!("cargo:rerun-if-changed=frontend/dist");
    println!("cargo:rerun-if-changed=build.rs");

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    println!("cargo:rustc-env=STACK_STACK_BUILD_PROFILE={profile}");

    let dist = Path::new("frontend/dist");
    if !dist.is_dir() {
        std::fs::create_dir_all(dist).expect("create frontend/dist");
        std::fs::write(dist.join("index.html"), PLACEHOLDER)
            .expect("write frontend/dist placeholder");
    }
}
