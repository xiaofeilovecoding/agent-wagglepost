//! Fake agent body shared by the `wagglepost __fake-agent` subcommand and the
//! `wagglepost-fake-agent` helper binary used by integration tests.

use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

pub fn run(args: &[String]) -> i32 {
    let flag_value = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    // child-process mode FIRST (no script needed): sleep then exit — used by the
    // spawn_child behavior so the root exits while a job child still runs (WP07)
    if let Some(secs) = flag_value("--child-sleep") {
        let secs: u64 = secs.parse().unwrap_or(0);
        std::thread::sleep(Duration::from_secs(secs));
        return 0;
    }
    let script_path = match flag_value("--script") {
        Some(p) => p,
        None => {
            eprintln!("fake-agent: --script required");
            return 3;
        }
    };
    let capture_path = flag_value("--capture");
    // read the full handoff body from stdin and capture it (G7a evidence)
    let mut body = String::new();
    let _ = std::io::stdin().read_to_string(&mut body);
    if let Some(cap) = capture_path {
        if let Some(parent) = std::path::Path::new(&cap).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cap, &body);
    }
    let script: Value = match std::fs::read_to_string(&script_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => {
            eprintln!("fake-agent: bad script");
            return 3;
        }
    };
    let out = |v: Value| {
        println!("{}", serde_json::to_string(&v).unwrap_or_default());
        use std::io::Write;
        let _ = std::io::stdout().flush();
    };
    out(json!({"fake": "1", "type": "started", "session_id": format!("fake-session-{}", &crate::ids::new_token()[..8]), "simulated": true}));
    let behaviors = script
        .get("behaviors")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();
    let has = |name: &str| behaviors.iter().any(|b| b.as_str() == Some(name));
    if has("duplicate_events") {
        let ev = json!({"fake": "1", "type": "duplicate", "key": script.get("dup_key").cloned().unwrap_or(json!("probe"))});
        out(ev.clone());
        out(ev);
    }
    if has("tool") {
        out(json!({"fake": "1", "type": "tool", "name": "Read", "detail": "reading task inputs", "simulated": true}));
    }
    if has("write_files") {
        if let Some(files) = script.get("files").and_then(|f| f.as_object()) {
            for (path, content) in files {
                let content = content.as_str().unwrap_or("");
                // WP13: the fake agent may only write INSIDE the task worktree.
                // Reject absolute paths, drive-relative paths, parent traversal and
                // symlink/reparse-point escapes — fail closed with a nonzero exit.
                if let Err(reason) = validate_workspace_path(std::path::Path::new(path)) {
                    out(json!({"fake": "1", "type": "error", "message": format!("write rejected: {reason}"), "simulated": true}));
                    eprintln!("fake-agent: write rejected ({reason}): {path}");
                    return 3;
                }
                let full = std::path::Path::new(path);
                // 晚间复审 WP13: validate BEFORE creating anything — create_dir_all
                // used to run first and followed a junction, leaving directories
                // behind in the link target even though the write itself was refused.
                if let Err(reason) = validate_no_reparse_ancestors(full) {
                    out(json!({"fake": "1", "type": "error", "message": format!("write rejected: {reason}"), "simulated": true}));
                    eprintln!("fake-agent: write rejected ({reason}): {path}");
                    return 3;
                }
                if let Err(reason) = create_dirs_validated(full) {
                    out(json!({"fake": "1", "type": "error", "message": format!("write rejected: {reason}"), "simulated": true}));
                    eprintln!("fake-agent: write rejected ({reason}): {path}");
                    return 3;
                }
                // re-check after creation: an ancestor may have been swapped
                if let Err(reason) = validate_no_reparse_ancestors(full) {
                    out(json!({"fake": "1", "type": "error", "message": format!("write rejected: {reason}"), "simulated": true}));
                    eprintln!("fake-agent: write rejected ({reason}): {path}");
                    return 3;
                }
                let ok = std::fs::write(full, content).is_ok();
                out(json!({"fake": "1", "type": "file_write", "path": path, "ok": ok, "simulated": true}));
            }
        }
    }
    if has("hang") {
        let secs = script.get("hang_secs").and_then(|h| h.as_u64()).unwrap_or(600);
        std::thread::sleep(Duration::from_secs(secs));
    }
    if has("spawn_child") {
        // spawn a job child that outlives the root by child_secs seconds
        let secs = script.get("child_secs").and_then(|h| h.as_u64()).unwrap_or(3);
        let exe = std::env::current_exe().ok();
        if let Some(exe) = exe {
            let _ = std::process::Command::new(exe)
                .args(["__fake-agent", "--child-sleep", &secs.to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        out(json!({"fake": "1", "type": "message", "text": "child spawned", "simulated": true}));
    }
    if let Some(msgs) = script.get("messages").and_then(|m| m.as_array()) {
        for m in msgs {
            out(json!({"fake": "1", "type": "message", "text": m.as_str().unwrap_or(""), "simulated": true}));
        }
    }
    if let Some(result) = script.get("result") {
        out(json!({"fake": "1", "type": "result", "result": result, "usage": {"input_tokens": 0, "output_tokens": 0, "simulated": true}}));
    }
    let exit_code = script.get("exit_code").and_then(|e| e.as_i64()).unwrap_or(0);
    if has("quota_block") {
        // quota simulation: blocked result + nonzero exit
        out(json!({"fake": "1", "type": "result", "result": {"status": "blocked", "quota_blocked": true, "blocked_reason": "simulated quota exhausted"}, "simulated": true}));
        return 42;
    }
    exit_code.clamp(0, 255) as i32
}

/// WP13: lexical + link validation — the fake agent may only write inside the
/// task worktree it was launched in.
fn validate_workspace_path(rel: &std::path::Path) -> Result<(), String> {
    use std::path::Component;
    if rel.is_absolute() {
        return Err("absolute path".into());
    }
    let mut has_drive = false;
    for comp in rel.components() {
        match comp {
            Component::ParentDir => return Err("parent traversal".into()),
            Component::Prefix(_) => has_drive = true,
            Component::RootDir => return Err("rooted path".into()),
            _ => {}
        }
    }
    if has_drive {
        return Err("drive-scoped path".into());
    }
    Ok(())
}

/// Reject symlink/reparse-point escapes: any existing ancestor of the target
/// (below the cwd) that is a link means the write could leave the workspace.
/// Windows junctions / mount points are reparse points that `is_symlink()` may
/// not report, so the FILE_ATTRIBUTE_REPARSE_POINT bit is checked explicitly.
fn validate_no_reparse_ancestors(target: &std::path::Path) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let mut cur = cwd.clone();
    for comp in target.components() {
        cur = cur.join(comp);
        if let Ok(md) = std::fs::symlink_metadata(&cur) {
            if is_link(&md) {
                return Err(format!("reparse-point escape at {}", cur.display()));
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_link(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_type().is_symlink() || (md.file_attributes() & 0x400) != 0 // FILE_ATTRIBUTE_REPARSE_POINT
}

#[cfg(not(windows))]
fn is_link(md: &std::fs::Metadata) -> bool {
    md.file_type().is_symlink()
}

/// Create the missing parent directories of `target` ONE level at a time,
/// re-validating each level after it appears — never `create_dir_all`, which
/// follows junctions/symlinks and would build inside the link target. Only
/// directories at already-validated, link-free levels are ever created, so a
/// rejection leaves no external directory behind.
fn create_dirs_validated(target: &std::path::Path) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let comps: Vec<_> = target.components().collect();
    let dir_count = comps.len().saturating_sub(1); // everything except the file name
    let mut cur = cwd.clone();
    for comp in comps.iter().take(dir_count) {
        cur = cur.join(comp);
        match std::fs::create_dir(&cur) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("mkdir {}: {e}", cur.display())),
        }
        if let Ok(md) = std::fs::symlink_metadata(&cur) {
            if is_link(&md) {
                return Err(format!("reparse-point escape at {}", cur.display()));
            }
            if !md.is_dir() {
                return Err(format!("not a directory: {}", cur.display()));
            }
        }
    }
    Ok(())
}

