use std::path::Path;

#[path = "src/asset_guard.rs"]
mod asset_guard;

// Build-time guard (WP12): the embedded web bundle must be exactly what the
// current build inputs produce. `verify_manifest` checks the content digests
// of the recorded build inputs (sources, build.mjs, lockfiles) AND of the
// produced artifacts, plus set completeness — an empty, partial, duplicate or
// unlisted entry fails the build. Merely touching mtimes can no longer pass a
// stale bundle. No web/dist yet (before the first `npm run build`) keeps the
// explicit dev placeholder path below.
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
    if let Err(e) = asset_guard::verify_manifest(dist, Path::new("web")) {
        panic!("web assets MISMATCH: {e}");
    }
}
