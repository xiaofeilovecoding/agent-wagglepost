use std::path::{Path, PathBuf};

#[path = "src/asset_guard.rs"]
mod asset_guard;

// Build-time guard (WP12): the embedded web bundle must never be stale or
// tampered. Verifies (1) no build input newer than dist (stale), and (2) every
// dist asset content digest matches ASSETS.sha (manifest).
fn input_dirs() -> Vec<PathBuf> {
    vec![
        PathBuf::from("web/src"),
        PathBuf::from("web/build.mjs"),
        PathBuf::from("web/package.json"),
        PathBuf::from("web/package-lock.json"),
    ]
}

fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/build.mjs");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/package-lock.json");
    let dist = Path::new("web/dist");
    if !dist.join("index.html").exists() {
        println!("cargo:warning=web/dist missing — the embedded UI will be a placeholder; run `npm run build` in web/ first");
        return;
    }
    // 1. staleness: any build input newer than the newest dist output fails
    let inputs: Vec<PathBuf> = input_dirs().into_iter().filter(|p| p.exists()).collect();
    if let Err(e) = asset_guard::check_inputs_fresh(&inputs, dist) {
        panic!("web assets are STALE: {e}");
    }
    // 2. content: every asset digest must match ASSETS.sha (touching mtimes cannot
    //    mask a content change; a replaced app.js fails here)
    if let Err(e) = asset_guard::verify_manifest(dist) {
        panic!("web assets MISMATCH: {e}");
    }
}
