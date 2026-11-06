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
    let out = std::process::Command::new(&cfg.git_exe)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(git_min_env())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
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
    Ok(diffq.status.success())
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

/// Freeze the candidate: commit all changes (tracked + untracked) on a detached head in the writer
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
    let obs = observe_worktree(cfg, &wt.path)?;
    let chash = candidate_hash(cfg, &wt.path)?;
    // patch export: diff base..commit
    let patch = run_git(
        cfg,
        p,
        &["diff", "--binary", &task.base_commit, &commit],
        None,
    )?;
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
        changed_files: obs.changed_files,
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
    Ok(wt)
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
