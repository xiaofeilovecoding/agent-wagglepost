// Rebuild the crate when the embedded web bundle changes (include_dir does not
// register its files with the incremental-compile tracker).
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-changed=web/src");
    if Path::new("web/dist/index.html").exists() {
        println!("cargo:rustc-cfg=has_web_dist");
    }
}
