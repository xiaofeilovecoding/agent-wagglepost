//! WP12: content-digest verification of the embedded web bundle, shared between
//! the build script (fail the build on stale/mismatched assets) and the lib (tests).
use std::path::Path;

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(&data);
    Ok(hex::encode(digest))
}

/// Verify the v2 asset manifest `ASSETS.sha` inside `dist_dir` against reality:
///
/// ```text
/// # comment lines start with '#'
/// in  <sha256>  <path relative to web_dir>    build inputs (sources, build.mjs, lockfiles)
/// out <sha256>  <path relative to dist_dir>   produced artifacts
/// ```
///
/// The manifest must be non-empty and duplicate-free, and both sets must be
/// COMPLETE: every file under `web_dir/src` plus `build.mjs`, `package.json`
/// and `package-lock.json` has to appear as an `in` entry, and every file
/// under `dist_dir` except the manifest itself has to appear as an `out`
/// entry. Content drift on either side fails — touching mtimes cannot mask a
/// modified source or a swapped artifact, and an empty/partial manifest cannot
/// smuggle an unverified file past the check.
pub fn verify_manifest(dist_dir: &Path, web_dir: &Path) -> Result<(), String> {
    let manifest_path = dist_dir.join("ASSETS.sha");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("ASSETS.sha missing or unreadable ({e}) — run `npm run build` first"))?;

    let mut ins: Vec<(String, String)> = Vec::new();
    let mut outs: Vec<(String, String)> = Vec::new();
    let mut total = 0usize;
    for line in manifest.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let kind = parts.next().unwrap_or_default();
        let digest = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || name.is_empty()
            || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            || (kind != "in" && kind != "out")
        {
            return Err(format!("ASSETS.sha: malformed line {line:?} — run `npm run build` first"));
        }
        total += 1;
        let list = if kind == "in" { &mut ins } else { &mut outs };
        if list.iter().any(|(_, n)| n == name) {
            return Err(format!("ASSETS.sha: duplicate entry for {name}"));
        }
        list.push((digest.to_string(), name.to_string()));
    }
    if total == 0 {
        return Err("ASSETS.sha is empty — run `npm run build` first".into());
    }

    // inputs: recorded digests must match the ACTUAL current inputs — a source,
    // build script or lockfile edited after the last build fails here no matter
    // what the mtimes look like.
    for (digest, name) in &ins {
        let file = web_dir.join(name);
        let actual = sha256_file(&file).map_err(|e| format!("input {name}: {e}"))?;
        if actual != *digest {
            return Err(format!(
                "input {name} does not match ASSETS.sha (bundle built from older sources) — run `npm run build` first"
            ));
        }
    }
    // outputs: recorded digests must match the actual artifacts.
    for (digest, name) in &outs {
        let file = dist_dir.join(name);
        let actual = sha256_file(&file).map_err(|e| format!("asset {name}: {e}"))?;
        if actual != *digest {
            return Err(format!(
                "asset {name} content does not match ASSETS.sha (stale or tampered bundle) — run `npm run build` first"
            ));
        }
    }

    // completeness of the fixed sets: an unlisted file would slip past both
    // digest checks above, so presence is enforced explicitly.
    let mut listed_out: Vec<String> = outs.iter().map(|(_, n)| n.clone()).collect();
    listed_out.sort();
    let mut actual_out: Vec<String> = list_files(dist_dir)?;
    actual_out.retain(|p| p != "ASSETS.sha");
    actual_out.sort();
    if actual_out != listed_out {
        return Err(format!(
            "dist output set does not match ASSETS.sha (manifest lists {listed_out:?}, dist holds {actual_out:?}) — run `npm run build` first"
        ));
    }
    let mut listed_in: Vec<String> = ins.iter().map(|(_, n)| n.clone()).collect();
    listed_in.sort();
    let mut actual_in: Vec<String> = list_files(&web_dir.join("src"))?
        .into_iter()
        .map(|p| format!("src/{p}"))
        .collect();
    for extra in ["build.mjs", "package.json", "package-lock.json"] {
        if web_dir.join(extra).is_file() {
            actual_in.push(extra.to_string());
        }
    }
    actual_in.sort();
    if actual_in != listed_in {
        return Err(format!(
            "build input set does not match ASSETS.sha (manifest lists {listed_in:?}, web/ holds {actual_in:?}) — run `npm run build` first"
        ));
    }
    Ok(())
}

/// All files under `dir` as `/`-separated paths relative to `dir` (recursive).
fn list_files(dir: &Path) -> Result<Vec<String>, String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), String> {
        for entry in
            std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?
        {
            let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
            let rel = if prefix.is_empty() {
                entry.file_name().to_string_lossy().into_owned()
            } else {
                format!("{prefix}/{}", entry.file_name().to_string_lossy())
            };
            if entry.file_type().map_err(|e| format!("{e}"))?.is_dir() {
                walk(&entry.path(), &rel, out)?;
            } else {
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out)?;
    Ok(out)
}
