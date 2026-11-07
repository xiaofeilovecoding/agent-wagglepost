// Build-time guard (WP12): the embedded web bundle must never be stale. If the UI
// sources are newer than the built dist, fail the build with an actionable message
// instead of silently embedding an old bundle.
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let meta = entry.metadata().ok()?;
        let mtime = meta.modified().ok()?;
        if newest.is_none() || mtime > newest.unwrap() {
            newest = Some(mtime);
        }
        if meta.is_dir() {
            if let Some(inner) = newest_mtime(&entry.path()) {
                if inner > newest.unwrap() {
                    newest = Some(inner);
                }
            }
        }
    }
    newest
}

fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-changed=web/src");
    let dist_index = PathBuf::from("web/dist/index.html");
    if !dist_index.exists() {
        println!("cargo:warning=web/dist missing — the embedded UI will be a placeholder; run `npm run build` in web/ first");
        return;
    }
    // staleness: any web/src file newer than the built bundle → fail closed
    let dist_newest = newest_mtime(Path::new("web/dist"));
    if let (Some(src_newest), Some(dist_mtime)) = (newest_mtime(Path::new("web/src")), dist_newest) {
        if src_newest > dist_mtime {
            panic!(
                "web/dist is STALE (web/src changed after the last `npm run build`). \
                 Run `npm run build` in web/ and rebuild — do not ship an old bundle."
            );
        }
    }
}
