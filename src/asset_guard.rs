//! WP12: asset freshness and content-digest verification, shared between the
//! build script (fail the build on stale/mismatched assets) and the lib (tests).
use std::path::Path;
use std::time::SystemTime;

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(&data);
    Ok(hex::encode(digest))
}

pub fn file_mtime(path: &Path) -> Result<SystemTime, String> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| format!("mtime {}: {e}", path.display()))
}

/// Verify every `digest  filename` line in the manifest against the actual files.
/// Missing manifest, missing files, or content changes all fail.
pub fn verify_manifest(dist_dir: &Path) -> Result<(), String> {
    let manifest_path = dist_dir.join("ASSETS.sha");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("ASSETS.sha missing or unreadable ({e}) — run `npm run build` first"))?;
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((digest, name)) = line.split_once("  ") else {
            return Err(format!("ASSETS.sha: malformed line {line:?}"));
        };
        let file = dist_dir.join(name);
        let actual = sha256_file(&file).map_err(|e| format!("asset {name}: {e}"))?;
        if actual != digest {
            return Err(format!(
                "asset {name} content does not match ASSETS.sha (stale or tampered bundle) — run `npm run build`"
            ));
        }
    }
    Ok(())
}

/// Fail if any build INPUT is newer than the newest dist output (stale bundle).
pub fn check_inputs_fresh(input_dirs: &[PathBuf], dist_dir: &Path) -> Result<(), String> {
    let dist_newest = newest_mtime(dist_dir)
        .ok_or_else(|| "dist dir missing or empty".to_string())?;
    for dir in input_dirs {
        if let Some(src_newest) = newest_mtime(dir) {
            if src_newest > dist_newest {
                return Err(format!(
                    "{} is newer than web/dist — run `npm run build` first (stale bundle)",
                    dir.display()
                ));
            }
        }
    }
    Ok(())
}

pub fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    newest_mtime_inner(dir, 0)
}

fn newest_mtime_inner(dir: &Path, depth: usize) -> Option<SystemTime> {
    if depth > 6 {
        return None;
    }
    let mut newest: Option<SystemTime> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let meta = entry.metadata().ok()?;
        let mtime = meta.modified().ok()?;
        if newest.is_none() || mtime > newest.unwrap() {
            newest = Some(mtime);
        }
        if meta.is_dir() {
            if let Some(inner) = newest_mtime_inner(&entry.path(), depth + 1) {
                if inner > newest.unwrap() {
                    newest = Some(inner);
                }
            }
        }
    }
    newest
}

use std::path::PathBuf;
