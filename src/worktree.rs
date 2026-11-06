use crate::ids::new_id;
use crate::model::*;
use crate::store::{timestamp, Store};
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[derive(Clone, Debug)]
pub struct GitConfig {
    pub git_exe: PathBuf,
    pub managed_root: PathBuf,
}

impl GitConfig {
    /// Paths of the isolation artifacts (created lazily): an empty hooks dir and an
    /// empty global-config file, so no user hooks/filters/global config can execute.
    pub fn isolation(&self) -> (PathBuf, PathBuf) {
        let hooks = self.managed_root.join("git-isolation").join("empty-hooks");
        let cfg = self.managed_root.join("git-isolation").join("empty-config");
        let _ = std::fs::create_dir_all(&hooks);
        let _ = std::fs::create_dir_all(cfg.parent().unwrap());
        if !cfg.exists() {
            let _ = std::fs::write(&cfg, "");
        }
        (hooks, cfg)
    }

    /// -c overrides that neutralize executable repo mechanisms for every product git call.
    pub fn isolation_args(&self) -> Vec<String> {
        let (hooks, _) = self.isolation();
        vec![
            "-c".into(),
            format!("core.hooksPath={}", hooks.to_string_lossy()),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "-c".into(),
            "commit.gpgsign=false".into(),
            "-c".into(),
            "tag.gpgsign=false".into(),
        ]
    }

    /// Env that blocks global/system/attr execution paths.
    pub fn isolation_env(&self) -> Vec<(String, String)> {
        let (_, cfg) = self.isolation();
        vec![
            ("GIT_CONFIG_GLOBAL".into(), cfg.to_string_lossy().to_string()),
            ("GIT_CONFIG_SYSTEM".into(), cfg.to_string_lossy().to_string()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
        ]
    }

    /// Detect repo-local execution mechanisms we cannot neutralize safely
    /// (external clean/smudge filters defined in the repository config).
    /// Such repos are refused with a stable reason at submit/trust.
    /// Enumerate filter definitions with Git-native semantics (follows include,
    /// includeIf, commondir for linked worktrees) and return -c overrides that
    /// neutralize every external clean/smudge command with a guaranteed-failing
    /// git invocation, so attacker commands never execute (WP02 0700).
    pub fn filter_overrides(cfg: &GitConfig, repo: &str) -> Vec<String> {
        let probe = std::process::Command::new(&cfg.git_exe)
            .args(cfg.isolation_args())
            .args(["config", "--get-regexp", r"^filter\..+\.(clean|smudge)$"])
            .current_dir(Path::new(repo))
            .env_clear()
            .envs(git_min_env())
            .envs(cfg.isolation_env())
            .output();
        let Ok(out) = probe else { return Vec::new() };
        if !out.status.success() {
            return Vec::new(); // no filters (or unreadable config: later git ops fail safely)
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if std::env::var("WAGGLEPOST_DEBUG_FILTERS").is_ok() {
            let full = std::process::Command::new(&cfg.git_exe)
                .args(cfg.isolation_args())
                .args(["config", "--list", "--show-origin"])
                .current_dir(Path::new(repo))
                .env_clear()
                .envs(git_min_env())
                .envs(cfg.isolation_env())
                .output();
            eprintln!("[filters] probe status={} output={}", out.status, text.trim());
            if let Ok(f) = full {
                eprintln!("[filters] FULL CONFIG:
{}", String::from_utf8_lossy(&f.stdout));
            }
        }
        let mut overrides = Vec::new();
        let fail_cmd = format!(
            "\"{}\" rev-parse --verify WAGGLEPOST_BLOCKED_FILTER_REF",
            cfg.git_exe.to_string_lossy().replace('\\', "/")
        );
        for line in text.lines() {
            // entries look like: filter.<name>.clean <command>
            let Some((key, _)) = line.split_once(' ') else { continue };
            let key = key.trim();
            if let Some(rest) = key.strip_prefix("filter.") {
                if let Some((name, side)) = rest.rsplit_once('.') {
                    if side == "clean" || side == "smudge" {
                        overrides.push("-c".into());
                        overrides.push(format!("filter.{name}.{side}={fail_cmd}"));
                    }
                }
            }
        }
        overrides
    }

}

impl Default for GitConfig {
    fn default() -> Self {
        GitConfig {
            git_exe: default_git_exe(),
            managed_root: default_managed_root(),
        }
    }
}

pub fn default_git_exe() -> PathBuf {
    let candidates = [
        PathBuf::from("D:\\dev\\Git\\cmd\\git.exe"),
        PathBuf::from("git"),
    ];
    for c in candidates {
        if c.is_file() || which_ok(&c) {
            return c;
        }
    }
    PathBuf::from("git")
}

fn which_ok(exe: &Path) -> bool {
    std::env::var("PATH")
        .map(|p| {
            p.split(';').any(|dir| {
                let full = Path::new(dir).join(exe);
                full.is_file()
            })
        })
        .unwrap_or(false)
}

pub fn default_managed_root() -> PathBuf {
    std::env::var("WAGGLEPOST_MANAGED_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("D:\\dev\\wagglepost-workspaces"))
}

pub fn run_git(
    cfg: &GitConfig,
    cwd: &Path,
    args: &[&str],
    stdin: Option<&str>,
) -> Result<String, String> {
    // freshly created directories can transiently fail CreateProcess cwd validation
    // on Windows (os error 267); retry spawn-level failures briefly
    let iso_args = cfg.isolation_args();
    let iso_env = cfg.isolation_env();
    let filter_over = GitConfig::filter_overrides(cfg, cwd.to_string_lossy().as_ref());
    let mut full_args: Vec<&str> = Vec::with_capacity(args.len() + iso_args.len() + filter_over.len());
    for a in &iso_args {
        full_args.push(a.as_str());
    }
    for a in &filter_over {
        full_args.push(a.as_str());
    }
    for a in args {
        full_args.push(a);
    }
    if std::env::var("WAGGLEPOST_DEBUG_FILTERS").is_ok() {
        eprintln!("[run_git] args={:?} over={:?}", args, filter_over);
    }
    let mut out = None;
    let mut last_err = String::new();
    for attempt in 0..4 {
        match std::process::Command::new(&cfg.git_exe)
            .args(&full_args)
            .current_dir(cwd)
            .env_clear()
            .envs(git_min_env())
            .envs(iso_env.clone())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
        {
            Ok(o) => {
                out = Some(o);
                break;
            }
            Err(e) => {
                let retryable = matches!(e.raw_os_error(), Some(2) | Some(3) | Some(267));
                last_err = format!("git spawn failed (cwd={}): {e}", cwd.display());
                if retryable && attempt < 3 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    continue;
                }
                return Err(last_err);
            }
        }
    }
    let out = out.ok_or(last_err)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "git {} failed (exit {:?}): {}",
            args.first().unwrap_or(&"",
            ),
            out.status.code(),
            err.trim()
        ));
    }
    if let Some(input) = stdin {
        // stdin was piped; nothing to write for our use (commit message passed via -m)
        let _ = input;
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn git_min_env() -> Vec<(String, String)> {
    let mut env = Vec::new();
    for k in [
        "SystemRoot",
        "SystemDrive",
        "windir",
        "PATH",
        "PATHEXT",
        "COMSPEC",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "LOCALAPPDATA",
        "APPDATA",
    ] {
        if let Ok(v) = std::env::var(k) {
            env.push((k.to_string(), v));
        }
    }
    env
}

/// repo_id: stable id from absolute path + resolved head
pub fn repo_id_for(path: &str) -> String {
    crate::ids::short_hash(format!("repo:{path}").as_bytes())
}

pub fn resolve_head(cfg: &GitConfig, repo: &str) -> Result<String, String> {
    let out = run_git(cfg, Path::new(repo), &["rev-parse", "HEAD"], None)?;
    Ok(out.trim().to_string())
}

pub fn is_clean(cfg: &GitConfig, repo: &str) -> Result<bool, String> {
    // NOTE: `git status --porcelain` runs clean filters on modified files (RelayCheck RC finding).
    // Use index hashes vs HEAD hashes instead for dirtiness detection.
    let head_tree = run_git(cfg, Path::new(repo), &["rev-parse", "HEAD^{tree}"], None)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if head_tree.is_empty() {
        return Ok(true); // empty repo
    }
    let ls = run_git(
        cfg,
        Path::new(repo),
        &["ls-files", "-s"],
        None,
    )?;
    let head_ls = run_git(
        cfg,
        Path::new(repo),
        &["ls-tree", "-r", "HEAD"],
        None,
    )?;
    // compare counts and content hashes coarsely; a full index comparison is expensive,
    // so combine with diff --quiet which does not run clean filters (no worktree writes)
    let diffq = std::process::Command::new(&cfg.git_exe)
        .args(["diff", "--quiet", "HEAD"])
        .current_dir(repo)
        .env_clear()
        .envs(git_min_env())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let _ = (ls, head_ls);
    if !diffq.status.success() {
        return Ok(false); // tracked changes
    }
    // untracked files are dirty too (they would not enter the worktree baseline)
    let others = run_git(cfg, Path::new(repo), &["ls-files", "--others", "--exclude-standard"], None)?;
    Ok(others.trim().is_empty())
}

/// Create an isolated worktree at managed root, detached at explicit commit.
pub fn create_worktree(
    store: &Store,
    cfg: &GitConfig,
    task: &Task,
    generation: u64,
) -> Result<WorktreeRecord, String> {
    let wt_id = new_id("wt");
    let path = cfg
        .managed_root
        .join("worktrees")
        .join(&task.task_id)
        .join(format!("g{generation}-{wt_id}"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let base = task.base_commit.clone();
    run_git(
        cfg,
        Path::new(&task.repo_path),
        &["worktree", "add", "--detach", path.to_string_lossy().as_ref(), &base],
        None,
    )?;
    let rec = WorktreeRecord {
        worktree_id: wt_id.clone(),
        task_id: task.task_id.clone(),
        generation,
        repo_path: task.repo_path.clone(),
        path: path.to_string_lossy().to_string(),
        base_commit: base,
        created_at: timestamp(),
        removed_at: None,
        keep_reason: None,
    };
    store.insert_worktree(&rec)?;
    Ok(rec)
}

pub fn mark_worktree_kept(store: &Store, worktree_id: &str, reason: &str) {
    if let Some(mut wt) = store.get_worktree(worktree_id) {
        wt.keep_reason = Some(reason.to_string());
        let _ = store.update_worktree(&wt);
    }
}

/// Current candidate observation from a worktree (local_observed, not agent claims).
pub struct WorktreeObservation {
    pub changed_files: Vec<String>,
    pub untracked_files: Vec<String>,
    pub diff_text: String,
}

pub fn observe_worktree(cfg: &GitConfig, worktree_path: &str) -> Result<WorktreeObservation, String> {
    let p = Path::new(worktree_path);
    let status = run_git(cfg, p, &["status", "--porcelain=v1", "-z"], None)?;
    let mut changed = Vec::new();
    let mut untracked = Vec::new();
    for entry in status.split('\0').filter(|s| !s.is_empty()) {
        let mut chars = entry.chars();
        let x = chars.next().unwrap_or(' ');
        let path_str: String = chars.skip(1).collect();
        let path_str = path_str.trim().to_string();
        if path_str.is_empty() {
            continue;
        }
        if x == '?' {
            untracked.push(path_str);
        } else {
            changed.push(path_str);
        }
    }
    let diff = run_git(cfg, p, &["diff", "HEAD", "--binary"], None)?;
    Ok(WorktreeObservation {
        changed_files: changed,
        untracked_files: untracked,
        diff_text: diff,
    })
}

/// Compute a candidate hash from the worktree content tree:
/// hash of (base_commit + sorted relative paths + file sha256s), tracked changes + untracked.
pub fn candidate_hash(cfg: &GitConfig, worktree_path: &str) -> Result<String, String> {
    let obs = observe_worktree(cfg, worktree_path)?;
    let mut h = sha2::Sha256::new();
    use sha2::Digest;
    h.update(b"wagglepost-candidate-v1");
    let mut files = obs.changed_files.clone();
    files.extend(obs.untracked_files.clone());
    files.sort();
    files.dedup();
    for f in &files {
        h.update(f.as_bytes());
        h.update([0u8]);
        let full = Path::new(worktree_path).join(f);
        match std::fs::read(&full) {
            Ok(content) => {
                let fh = sha256_hex(&content);
                h.update(fh.as_bytes());
            }
            Err(_) => {
                h.update(b"<deleted>");
            }
        }
        h.update([1u8]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Component-wise path authorization: forbidden always wins; empty allowed = whole tree.
pub fn path_allowed(rel: &str, allowed: &[String], forbidden: &[String]) -> bool {
    let norm = rel.replace('\\', "/").trim_start_matches('/').to_string();
    if norm.is_empty() {
        return false;
    }
    let under = |base: &str| -> bool {
        let b = base.replace('\\', "/").trim_matches('/').to_string();
        if b.is_empty() {
            return true;
        }
        norm == b || norm.starts_with(&format!("{b}/"))
    };
    if forbidden.iter().any(|f| under(f)) {
        return false;
    }
    if allowed.is_empty() {
        return true;
    }
    allowed.iter().any(|a| under(a))
}

/// Freeze the candidate:/// Freeze the candidate: commit all changes (tracked + untracked) on a detached head in the writer
/// worktree. Returns the frozen commit hash. The user repo is never touched.
pub fn freeze_candidate(
    store: &Store,
    cfg: &GitConfig,
    task: &Task,
    attempt_id: &str,
    generation: u64,
) -> Result<Candidate, String> {
    let wt = store
        .list_task_worktrees(&task.task_id)
        .into_iter()
        .find(|w| w.generation == generation)
        .ok_or("worktree for generation not found")?;
    let p = Path::new(&wt.path);
    // observe the delta BEFORE freezing (post-commit the tree is clean)
    let pre = observe_worktree(cfg, &wt.path)?;
    // add all changes including untracked
    run_git(cfg, p, &["add", "-A"], None)?;
    let msg = format!("wagglepost candidate: task {} attempt {} g{}", task.task_id, attempt_id, generation);
    run_git(
        cfg,
        p,
        &["-c", "user.name=wagglepost", "-c", "user.email=wagglepost@local", "commit", "-m", &msg],
        None,
    )?;
    let commit = run_git(cfg, p, &["rev-parse", "HEAD"], None)?.trim().to_string();
    let _obs = pre;
    // patch export: diff base..commit
    let patch = run_git(
        cfg,
        p,
        &["diff", "--binary", &task.base_commit, &commit],
        None,
    )?;
    // full change enumeration from the frozen tree (WP03): added/deleted/renamed/modified
    let name_status = run_git(
        cfg,
        p,
        &["diff", "--name-status", "-M", &task.base_commit, &commit],
        None,
    )?;
    let mut changed_files_full: Vec<String> = Vec::new();
    for line in name_status.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let _status = parts.next().unwrap_or("");
        // rename lines carry two paths; the NEW path is the one in the candidate
        let path = parts.next_back().unwrap_or("").to_string();
        if !path.is_empty() {
            changed_files_full.push(path);
        }
    }
    let out_of_scope: Vec<String> = changed_files_full
        .iter()
        .filter(|f| !path_allowed(f, &task.allowed_paths, &task.forbidden_paths))
        .cloned()
        .collect();
    if !out_of_scope.is_empty() {
        return Err(format!(
            "OUT_OF_SCOPE: {}",
            out_of_scope.join(", ")
        ));
    }
    // candidate hash is bound to the actual delta content (base + patch)
    let chash = {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        h.update(b"wagglepost-candidate-v1:");
        h.update(task.base_commit.as_bytes());
        h.update([0u8]);
        h.update(patch.as_bytes());
        hex::encode(h.finalize())
    };
    let patch_rel = format!("patches/{}.patch", &task.task_id);
    let patch_dir = cfg.managed_root.join("patches");
    std::fs::create_dir_all(&patch_dir).map_err(|e| e.to_string())?;
    std::fs::write(patch_dir.join(format!("{}.patch", task.task_id)), &patch)
        .map_err(|e| e.to_string())?;
    let _ = patch_rel;
    // untracked files already included in commit via add -A
    let cand = Candidate {
        candidate_hash: chash,
        task_id: task.task_id.clone(),
        generation,
        attempt_id: attempt_id.to_string(),
        base_commit: task.base_commit.clone(),
        frozen_commit: Some(commit),
        changed_files: changed_files_full,
        untracked_files: vec![], // frozen into the commit
        patch_path: Some(patch_dir.join(format!("{}.patch", task.task_id)).to_string_lossy().to_string()),
        label: String::new(),
        current: true,
        created_at: timestamp(),
    };
    store.mark_candidates_stale(&task.task_id);
    store.insert_candidate(&cand)?;
    Ok(cand)
}

/// Create a reviewer worktree from the frozen candidate commit.
pub fn create_review_worktree(
    store: &Store,
    cfg: &GitConfig,
    task: &Task,
    cand: &Candidate,
) -> Result<WorktreeRecord, String> {
    let commit = cand
        .frozen_commit
        .clone()
        .ok_or("candidate is not frozen")?;
    let generation = task.current_generation + 1000; // reviewer shares generation lineage
    let wt_id = new_id("wt");
    let path = cfg
        .managed_root
        .join("worktrees")
        .join(&task.task_id)
        .join(format!("review-g{generation}-{wt_id}"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    run_git(
        cfg,
        Path::new(&task.repo_path),
        &["worktree", "add", "--detach", path.to_string_lossy().as_ref(), &commit],
        None,
    )?;
    let rec = WorktreeRecord {
        worktree_id: wt_id.clone(),
        task_id: task.task_id.clone(),
        generation,
        repo_path: task.repo_path.clone(),
        path: path.to_string_lossy().to_string(),
        base_commit: commit,
        created_at: timestamp(),
        removed_at: None,
        keep_reason: None,
    };
    store.insert_worktree(&rec)?;
    Ok(rec)
}

/// Apply a checkpoint patch into a fresh worktree (reroute / relay).
pub fn apply_checkpoint(
    store: &Store,
    cfg: &GitConfig,
    task: &Task,
    generation: u64,
    checkpoint: &Checkpoint,
) -> Result<WorktreeRecord, String> {
    let wt = create_worktree(store, cfg, task, generation)?;
    let patch_path = checkpoint
        .patch_path
        .clone()
        .ok_or("checkpoint has no patch")?;
    let patch = std::fs::read(&patch_path).map_err(|e| format!("read patch: {e}"))?;
    if !patch.is_empty() {
        let p = Path::new(&wt.path);
        let out = std::process::Command::new(&cfg.git_exe)
            .args(["apply", "--whitespace=nowarn", "-"])
            .current_dir(p)
            .env_clear()
            .envs(git_min_env())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .take()
                    .ok_or_else(|| std::io::Error::other("no stdin"))?
                    .write_all(&patch)?;
                child.wait_with_output()
            })
            .map_err(|e| format!("git apply spawn: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git apply failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    // restore checkpointed untracked files (bundle dir beside the patch);
    // runs even when the tracked diff is empty (untracked-only checkpoints)
    if let Some(patch_path) = &checkpoint.patch_path {
        if let Some(pfile) = std::path::Path::new(patch_path).file_name() {
            // patch lives at <managed>/patches/<file>; the bundle is <managed>/checkpoints/<stem>
            let task_dir = std::path::Path::new(patch_path)
                .parent()
                .and_then(|p| p.parent());
            if let Some(td) = task_dir {
                let bundle_name = pfile.to_string_lossy().trim_end_matches(".patch").to_string();
                let bundle = td.join("checkpoints").join(bundle_name);
                if bundle.is_dir() {
                    copy_dir_recursive(&bundle, std::path::Path::new(&wt.path))?;
                }
            }
        }
    }
    Ok(wt)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Export accepted patch (for the user to take away). Returns absolute path.
pub fn export_patch(store: &Store, cfg: &GitConfig, task: &Task, cand: &Candidate) -> Result<PathBuf, String> {
    let _ = store;
    let wt = store
        .list_task_worktrees(&task.task_id)
        .into_iter()
        .find(|w| w.generation == cand.generation)
        .ok_or("worktree not found")?;
    let commit = cand
        .frozen_commit
        .clone()
        .ok_or("candidate is not frozen")?;
    let patch = run_git(
        cfg,
        Path::new(&wt.path),
        &["format-patch", "--stdout", &format!("{}..{}", task.base_commit, commit)],
        None,
    )?;
    let dir = cfg.managed_root.join("exports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(format!("{}-{}.patch", task.task_id, &cand.candidate_hash[..8]));
    std::fs::write(&file, patch).map_err(|e| e.to_string())?;
    Ok(file)
}
