//! OpenCode adapter — 0.2 native non-interactive dispatch.
//!
//! Transport verified on-device against the pinned installed binary
//! (v2.2.6 standalone zip, sha256 f75c16b3…, evidence:
//! artifacts/agent-router/WAGGLEPOST-002/real-runs/opencode-min-20261110/):
//! `opencode run --standalone --format json` streams NDJSON
//! `step_start`/`text`/`tool_use`/`step_finish` parts, each carrying the
//! `sessionID`; `step_finish.part.tokens` reports provider usage and cost.
//! `--standalone` is REQUIRED: without it the run attaches to the shared
//! background service (H2 禁止借默认共享 daemon 执行写任务).
//!
//! WP21 permission isolation — the reviewer cwd is a MANAGED SNAPSHOT, never the
//! candidate worktree: OpenCode merges the cwd's own opencode.json AFTER the
//! global config and last-rule-wins, so a candidate repo could otherwise turn
//! our denies into allows (Codex 0200 WP21, verified with the real CLI).
//! `prepare_snapshot` copies the frozen review worktree into the managed root
//! while STRIPPING every candidate-supplied agent config/instruction surface
//! (opencode.json(c), .opencode/, AGENTS.md, CLAUDE.md, .mcp.json, editor
//! configs, .git*) and skipping symlinks/reparse points (链接逃逸). The managed
//! per-attempt policy denies by default and permits only read, glob and grep;
//! the offline boundary probe (no model) asserts via `debug config --standalone`
//! that ONLY the managed document loads and the deny rules are in effect.
//!
//! Same-session resume is NOT claimed for OpenCode: sessions live under
//! XDG_DATA_HOME, which is per-attempt isolated (WP22) — resume_delta refuses.
//!
//! H4: the task body travels in the snapshot file `.wagglepost-handoff.md`
//! (referenced via `--file` inside the cwd); the positional message is a short
//! fixed instruction — the body never crosses argv, no shell string is built.
//! The user's own OpenCode config is never read or modified.

use super::{Adapter, DispatchContext, RunState, WireEvent};
use crate::model::AgentResult;
use crate::runner::{ExitOutcome, SpawnSpec};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const DEFAULT_MODEL_ENV: &str = "WAGGLEPOST_OPENCODE_MODEL";
/// Model used when the operator has DEEPSEEK_API_KEY in the environment and no
/// explicit model override (the authorized key channel: 只读环境, forwarded to
/// the isolated process only).
pub const FALLBACK_MODEL: &str = "deepseek/deepseek-flash";
/// In-snapshot task body file (candidate configs are stripped, so this name
/// cannot collide with anything the candidate shipped).
pub const HANDOFF_FILE: &str = ".wagglepost-handoff.md";

const BODY_INSTRUCTION: &str =
    "The attached file is your complete task brief. Execute it exactly as written. \
     End your run with exactly one fenced JSON block tagged wagglepost-result.";

/// WP21: candidate-supplied files that must never reach the runtime as config or
/// instructions. The snapshot keeps candidate CONTENT (code/diffs) but strips
/// every surface that could reconfigure or re-instruct the agent.
const STRIP_FILES: &[&str] = &[
    "opencode.json",
    "opencode.jsonc",
    "agents.md",
    "claude.md",
    "gemini.md",
    ".mcp.json",
    ".git",
    HANDOFF_FILE,
];
const STRIP_DIRS: &[&str] = &[
    ".git",
    ".opencode",
    ".claude",
    ".cursor",
    ".windsurf",
    ".gemini",
    ".wagglepost",
    // WP29: candidate-supplied compat skill/agent definitions are an execution
    // and context-injection surface — never copied into the snapshot
    ".agents",
];

/// WP25: Windows filesystems are case-insensitive — the strip comparison must be
/// too, or "OpenCode.JSON" survives the copy and overrides the managed config.
fn strip_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    STRIP_FILES.contains(&lower.as_str())
}

fn strip_dir_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    STRIP_DIRS.contains(&lower.as_str())
}

pub struct OpencodeAdapter {
    pub exe: PathBuf,
    pub version: String,
    model_override: Option<String>,
}

impl OpencodeAdapter {
    pub fn new(exe: PathBuf, version: String, model_override: Option<String>) -> Self {
        Self { exe, version, model_override }
    }

    fn model(&self) -> Result<String, String> {
        if let Some(model) = &self.model_override {
            return Ok(model.clone());
        }
        if let Ok(m) = std::env::var(DEFAULT_MODEL_ENV) {
            if !m.trim().is_empty() {
                return Ok(m.trim().to_string());
            }
        }
        if std::env::var("DEEPSEEK_API_KEY").is_ok() {
            return Ok(FALLBACK_MODEL.to_string());
        }
        Err("opencode model not configured: set WAGGLEPOST_OPENCODE_MODEL or authenticate (no key found in the environment)".into())
    }
}

/// WP21: copy the review worktree into a managed snapshot dir, stripping
/// candidate-supplied agent config/instruction surfaces and skipping
/// symlinks/junctions (链接逃逸). Returns the snapshot path.
pub fn prepare_snapshot(worktree: &Path, snapshot: &Path) -> Result<PathBuf, String> {
    // refuse nested copies: a snapshot inside its own source would recurse
    let (a, b) = (worktree.canonicalize().unwrap_or_else(|_| worktree.to_path_buf()), snapshot.canonicalize().unwrap_or_else(|_| snapshot.to_path_buf()));
    if a.starts_with(&b) || b.starts_with(&a) {
        return Err("snapshot dir must not contain or be contained by the worktree".into());
    }
    copy_tree_filtered(worktree, snapshot, 0)?;
    Ok(snapshot.to_path_buf())
}

fn copy_tree_filtered(src: &Path, dst: &Path, depth: usize) -> Result<(), String> {
    if depth > 32 {
        return Err("snapshot depth exceeded (path too deep?)".into());
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("snapshot mkdir {}: {e}", dst.display()))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("snapshot read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy().to_string();
        // reparse points (symlinks/junctions) are never followed
        let meta = std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        let is_reparse = {
            use std::os::windows::fs::MetadataExt;
            (meta.file_attributes() & 0x400) != 0 // FILE_ATTRIBUTE_REPARSE_POINT
        };
        #[cfg(not(windows))]
        let is_reparse = meta.file_type().is_symlink();
        if is_reparse {
            continue;
        }
        if meta.is_dir() {
            if strip_dir_name(&name_s) {
                continue;
            }
            copy_tree_filtered(&entry.path(), &dst.join(&name), depth + 1)?;
        } else if meta.is_file() {
            if strip_name(&name_s) {
                continue;
            }
            copy_snapshot_file(&entry.path(), &dst.join(&name))
                .map_err(|e| format!("snapshot copy {}: {e}", name_s))?;
        }
    }
    Ok(())
}

/// Windows scanners can briefly deny sharing of a freshly created file.
fn copy_snapshot_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    for delay in [25, 50, 100, 200] {
        match std::fs::copy(src, dst) {
            Err(e) if cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)) => {
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            result => return result.map(|_| ()),
        }
    }
    std::fs::copy(src, dst).map(|_| ())
}

/// Exact ordered policy written to disk and checked against the CLI's report.
const REVIEWER_CONFIG: &str = r#"{"permissions":[
    {"action":"*","resource":"*","effect":"deny"},
    {"action":"read","resource":"*","effect":"allow"},
    {"action":"glob","resource":"*","effect":"allow"},
    {"action":"grep","resource":"*","effect":"allow"},
    {"action":"external_directory","resource":"*","effect":"deny"}]}"#;

pub fn write_reviewer_config(config_dir: &Path) -> Result<PathBuf, String> {
    let dir = config_dir.join("opencode");
    std::fs::create_dir_all(&dir).map_err(|e| format!("reviewer config dir: {e}"))?;
    let path = dir.join("opencode.json");
    std::fs::write(&path, REVIEWER_CONFIG).map_err(|e| format!("reviewer config: {e}"))?;
    Ok(path)
}

/// Probe and dispatch use the same isolated homes. Only dispatch can add a key.
pub fn probe_child_env(base: &Path, config_dir: &Path) -> Result<Vec<(String, String)>, String> {
    let home = base.join("home");
    for dir in [home.clone(), base.join("data"), base.join("cache"), base.join("state")] {
        std::fs::create_dir_all(dir).map_err(|e| format!("OpenCode state directory: {e}"))?;
    }
    let mut env = crate::runner::filter_env(&[]);
    env.retain(|(key, _)| {
        !matches!(key.as_str(), "HOME" | "USERPROFILE" | "APPDATA" | "LOCALAPPDATA" | "HOMEDRIVE" | "HOMEPATH")
    });
    for key in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        env.push((key.into(), home.to_string_lossy().into_owned()));
    }
    env.extend([
        ("XDG_CONFIG_HOME".into(), config_dir.to_string_lossy().into_owned()),
        ("XDG_DATA_HOME".into(), base.join("data").to_string_lossy().into_owned()),
        ("XDG_CACHE_HOME".into(), base.join("cache").to_string_lossy().into_owned()),
        ("XDG_STATE_HOME".into(), base.join("state").to_string_lossy().into_owned()),
        ("OPENCODE_DISABLE_PROJECT_CONFIG".into(), "1".into()),
    ]);
    Ok(env)
}

/// WP25/WP26: run `debug config --standalone` with the managed config and the
/// given cwd through the bounded, credential-free probe; returns raw stdout.
pub fn debug_config(
    exe: &Path,
    config_dir: &Path,
    state_base: &Path,
    cwd: &Path,
) -> Result<String, String> {
    crate::runner::bounded_probe(
        exe,
        &["debug", "config", "--standalone"],
        Some(cwd),
        &probe_child_env(state_base, config_dir)?,
        std::time::Duration::from_secs(30),
        |_| true,
    )
}

/// Accept only the generated policy and its containing directory. Comparing the
/// whole ordered rule array rejects resource narrowing, extra rules and plugins.
pub fn assert_boundary_holds(raw: &str, managed_config_file: &str) -> Result<(), String> {
    fn same_path(reported: &str, expected: &Path) -> bool {
        #[cfg(windows)]
        {
            reported.replace('/', "\\").eq_ignore_ascii_case(&expected.to_string_lossy().replace('/', "\\"))
        }
        #[cfg(not(windows))]
        {
            Path::new(reported) == expected
        }
    }

    let file = Path::new(managed_config_file);
    let dir = file.parent().ok_or("managed config has no directory")?;
    let docs: Vec<Value> = serde_json::from_str(raw).map_err(|e| format!("debug config JSON: {e}"))?;
    let expected: Value = serde_json::from_str(REVIEWER_CONFIG).expect("fixed reviewer policy is JSON");
    let mut saw_file = false;
    let mut saw_dir = false;
    for doc in docs {
        let object = doc.as_object().ok_or("unknown config source shape")?;
        let path = doc.get("path").and_then(Value::as_str).ok_or("missing config source path")?;
        match doc.get("type").and_then(Value::as_str) {
            Some("document") if !saw_file && same_path(path, file) && object.len() == 3
                && doc.get("info") == Some(&expected) => saw_file = true,
            Some("directory") if !saw_dir && same_path(path, dir) && object.len() == 2 => saw_dir = true,
            _ => return Err("unexpected config source or non-managed policy".into()),
        }
    }
    if !saw_file {
        return Err("managed config document was not loaded".into());
    }
    Ok(())
}

/// WP25: the per-launch permission gate. Runs the REAL `debug config` against
/// the EXACT cwd/env this launch will use — offline, credential-free, bounded.
/// Any loaded document outside the managed config dir, or any missing deny rule
/// (case-variant configs, ancestor configs, future vendor changes) refuses the
/// launch BEFORE the model process spawns. This is the load-bearing check; the
/// snapshot strip is defense in depth, never the proof.
pub fn preflight_effective_config(
    exe: &Path,
    config_dir: &Path,
    state_base: &Path,
    cwd: &Path,
) -> Result<(), String> {
    let out = debug_config(exe, config_dir, state_base, cwd)?;
    assert_boundary_holds(&out, &config_dir.join("opencode").join("opencode.json").to_string_lossy())
}

impl Adapter for OpencodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn build_spec(&self, ctx: &DispatchContext) -> Result<SpawnSpec, String> {
        let model = self.model()?;
        let uses_deepseek = model.starts_with("deepseek/");
        let base = ctx
            .managed_root
            .join("opencode-runs")
            .join(&ctx.attempt_id);
        let config_dir = base.join("config");
        let mut args: Vec<String> = vec![
            "run".into(),
            "--standalone".into(),
            "--format".into(),
            "json".into(),
            "--model".into(),
            model,
        ];
        let (cwd, file_arg) = match ctx.role {
            crate::model::Role::Reviewer => {
                // WP21: the reviewer runs in a MANAGED SNAPSHOT of the frozen
                // candidate — never the candidate worktree itself
                if ctx.handoff.mode == "resume_delta" {
                    // WP22: per-attempt data isolation means sessions cannot be
                    // adopted across attempts; refuse instead of pretending
                    return Err(
                        "opencode same-session resume is not verified (per-attempt data isolation)".into(),
                    );
                }
                let snapshot = base.join("snapshot");
                prepare_snapshot(Path::new(&ctx.worktree_path), &snapshot)?;
                // task body inside the cwd (no external reads needed)
                std::fs::write(snapshot.join(HANDOFF_FILE), &ctx.handoff.text)
                    .map_err(|e| format!("snapshot handoff: {e}"))?;
                std::fs::create_dir_all(&config_dir).map_err(|e| format!("config dir: {e}"))?;
                write_reviewer_config(&config_dir)?;
                // WP25: verify the EFFECTIVE config for THIS exact launch offline
                // (bounded, credential-free). Failure refuses the spawn — the
                // model process never starts.
                preflight_effective_config(&self.exe, &config_dir, &base, &snapshot)?;
                (
                    snapshot.clone(),
                    snapshot.join(HANDOFF_FILE).to_string_lossy().to_string(),
                )
            }
            crate::model::Role::Writer => {
                // experimental role (never auto-routed, dispatch refuses it):
                // runs in the worktree with an empty managed config; --auto keeps
                // non-interactive runs from hanging on vendor ask-prompts
                std::fs::create_dir_all(&config_dir.join("opencode"))
                    .map_err(|e| format!("config dir: {e}"))?;
                std::fs::write(config_dir.join("opencode").join("opencode.json"), "{}")
                    .map_err(|e| format!("writer config: {e}"))?;
                let body = base.join("handoff-body.md");
                std::fs::create_dir_all(&base).map_err(|e| format!("run dir: {e}"))?;
                std::fs::write(&body, &ctx.handoff.text).map_err(|e| format!("handoff file: {e}"))?;
                args.push("--auto".into());
                (PathBuf::from(&ctx.worktree_path), body.to_string_lossy().to_string())
            }
        };
        if ctx.handoff.mode == "resume_delta" {
            let session = ctx
                .resume_native_id
                .clone()
                .ok_or("resume_delta requires native session id")?;
            args.extend(["--session".into(), session]);
        }
        args.extend(["--file".into(), file_arg, BODY_INSTRUCTION.into()]);
        let mut env = probe_child_env(&base, &config_dir)?;
        if uses_deepseek {
            if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
                env.push(("DEEPSEEK_API_KEY".into(), key));
            }
        }
        Ok(SpawnSpec {
            exe: self.exe.clone(),
            args,
            cwd,
            env,
            stdin_text: String::new(),
        })
    }

    fn parse_line(&mut self, line: &str) -> Vec<WireEvent> {
        // framing fault tolerance: broken/partial JSON lines are ignored (R1)
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let native_id = v
            .get("sessionID")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        match typ.as_str() {
            "step_start" => {
                if native_id.is_some() {
                    vec![WireEvent {
                        kind: "agent_started".into(),
                        payload: json!({"session_id": native_id}),
                        native_id,
                        usage: None,
                        text: None,
                        error: None,
                        result: None,
                    }]
                } else {
                    vec![]
                }
            }
            "text" => {
                let text = v
                    .pointer("/part/text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                if text.is_empty() {
                    return vec![];
                }
                vec![WireEvent {
                    kind: "agent_message".into(),
                    payload: json!({}),
                    native_id: None,
                    usage: None,
                    text: Some(text),
                    error: None,
                    result: None,
                }]
            }
            "tool_use" => {
                let tool = v.pointer("/part/tool").and_then(|t| t.as_str()).unwrap_or("");
                let status = v
                    .pointer("/part/state/status")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                let completed = status == "completed" || status == "error";
                vec![WireEvent {
                    kind: if completed { "agent_tool_end" } else { "agent_tool" }.into(),
                    payload: json!({
                        "tool": tool,
                        "status": status,
                    }),
                    native_id: None,
                    usage: None,
                    text: None,
                    error: if status == "error" {
                        Some(format!("opencode tool {tool:?} failed"))
                    } else {
                        None
                    },
                    result: None,
                }]
            }
            "step_finish" => {
                let usage = v.pointer("/part/tokens").map(|t| {
                    json!({
                        "input_tokens": t.get("input"),
                        "output_tokens": t.get("output"),
                        "cache_read": t.pointer("/cache/read"),
                        "cache_write": t.pointer("/cache/write"),
                        "cost_usd": v.pointer("/part/cost"),
                        "source": "opencode step_finish.part.tokens",
                    })
                });
                let reason = v
                    .pointer("/part/reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
                vec![WireEvent {
                    kind: "agent_step".into(),
                    payload: json!({"reason": reason}),
                    native_id: None,
                    usage,
                    text: None,
                    error: if reason == "error" {
                        Some("opencode step finished with error".into())
                    } else {
                        None
                    },
                    result: None,
                }]
            }
            "error" => {
                let msg = v
                    .pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("opencode error event")
                    .to_string();
                vec![WireEvent {
                    kind: "agent_error".into(),
                    payload: json!({}),
                    native_id: None,
                    usage: None,
                    text: None,
                    error: Some(msg),
                    result: None,
                }]
            }
            // unknown event tolerance (R1): record shape, never fail the stream
            other => vec![WireEvent {
                kind: "agent_item".into(),
                payload: json!({"opencode_type": other}),
                native_id: None,
                usage: None,
                text: None,
                error: None,
                result: None,
            }],
        }
    }

    fn result_from_run(&self, state: &RunState, exit: &ExitOutcome) -> AgentResult {
        if let Some(t) = &state.last_text {
            if let Some(r) = super::extract_agent_result(t) {
                return r;
            }
        }
        let stderr = String::from_utf8_lossy(&exit.stderr).to_lowercase();
        if stderr.contains("quota") || stderr.contains("rate limit") || stderr.contains("credit") {
            return AgentResult {
                status: "blocked".into(),
                quota_blocked: true,
                blocked_reason: Some("opencode quota/usage limit".into()),
                ..Default::default()
            };
        }
        if stderr.contains("unauthorized") || stderr.contains("api key") || stderr.contains("no auth") {
            return AgentResult {
                status: "blocked".into(),
                auth_required: true,
                blocked_reason: Some("opencode authentication required".into()),
                ..Default::default()
            };
        }
        if exit.killed {
            return AgentResult {
                status: "blocked".into(),
                blocked_reason: Some("stopped before reporting".into()),
                ..Default::default()
            };
        }
        if exit.exit_code != Some(0) {
            return AgentResult {
                status: "blocked".into(),
                blocked_reason: Some(format!("exit code {:?}", exit.exit_code)),
                ..Default::default()
            };
        }
        AgentResult {
            status: "done".into(),
            summary: state.last_text.clone().map(|t| t.chars().take(400).collect()),
            ..Default::default()
        }
    }
}
