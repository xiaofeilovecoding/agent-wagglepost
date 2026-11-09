//! DSH (DeepSeek Harness) adapter — 0.2 native headless dispatch.
//!
//! Transport verified on-device against 0.2.1-rc.5 (`dsh headless --help` plus a
//! real minimal `--json` run, evidence:
//! artifacts/agent-router/WAGGLEPOST-002/real-runs/dsh-min-20261109/):
//! `dsh --profile headless --json -` reads the task from stdin and emits NDJSON:
//! `session`(sessionId) → `status.turn_start` → `status.step_start` → `thinking`
//! → `text` → `status.step_end`(usage) → `status.turn_end`(reason.kind) → `final`.
//! Resume: `--session-id <id>` adopts a persisted Session.
//!
//! H2 boundary: the headless profile exposes NO native permission restriction
//! (no read-only flag, no tool deny list — `--dump-config-schema` confirms), so
//! both roles stay `experimental` in the capability table: transport may be
//! verified, but the runtime is never auto-routed until a native boundary exists.

use super::{Adapter, DispatchContext, RunState, WireEvent};
use crate::model::AgentResult;
use crate::runner::{ExitOutcome, SpawnSpec};
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct DshAdapter {
    pub exe: Option<PathBuf>,
    pub version: String,
    /// version/help probe failure reason (entry stays visible for the capability table)
    pub probe_error: Option<String>,
}

impl DshAdapter {
    pub fn new(exe: Option<PathBuf>, version: String) -> Self {
        DshAdapter {
            exe,
            version,
            probe_error: None,
        }
    }
}

/// Wrap a .cmd/.bat entry through cmd.exe (CreateProcessW cannot exec them).
fn exe_and_prefix(exe: &PathBuf) -> (PathBuf, Vec<String>) {
    let is_script = exe
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
        .unwrap_or(false);
    if cfg!(windows) && is_script {
        // CreateProcessW gets lpApplicationName — resolve cmd.exe absolutely
        // (COMSPEC is kept by filter_env; SystemRoot fallback covers the rest)
        let cmd = std::env::var("COMSPEC")
            .map(PathBuf::from)
            .ok()
            .filter(|p| p.is_file())
            .unwrap_or_else(|| {
                let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
                PathBuf::from(root).join(r"System32\cmd.exe")
            });
        (
            cmd,
            vec!["/c".into(), exe.to_string_lossy().to_string()],
        )
    } else {
        (exe.clone(), vec![])
    }
}

impl Adapter for DshAdapter {
    fn id(&self) -> &'static str {
        "dsh"
    }

    fn build_spec(&self, ctx: &DispatchContext) -> Result<SpawnSpec, String> {
        let exe = self
            .exe
            .clone()
            .ok_or("dsh CLI not found on this machine")?;
        let (prog, prefix) = exe_and_prefix(&exe);
        // argv array only (H4): no shell string, no user-injectable flags.
        // The task body goes to stdin via `-`; resume addresses the persisted
        // session id captured from the previous attempt.
        let mut args: Vec<String> = prefix;
        args.extend(["--profile".into(), "headless".into(), "--json".into()]);
        if ctx.handoff.mode == "resume_delta" {
            let session = ctx
                .resume_native_id
                .clone()
                .ok_or("resume_delta requires native session id")?;
            args.extend(["--session-id".into(), session]);
        }
        args.push("-".into());
        Ok(SpawnSpec {
            exe: prog,
            args,
            cwd: PathBuf::from(&ctx.worktree_path),
            env: crate::runner::filter_env(&[]),
            stdin_text: ctx.handoff.text.clone(),
        })
    }

    fn parse_line(&mut self, line: &str) -> Vec<WireEvent> {
        // framing fault tolerance: broken/partial JSON is ignored (R1 fixture)
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match typ {
            "session" => vec![WireEvent {
                kind: "agent_started".into(),
                payload: json!({"session_id": v.get("sessionId"), "cwd": v.get("cwd")}),
                native_id: v
                    .get("sessionId")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string()),
                usage: None,
                text: None,
                error: None,
                result: None,
            }],
            "status" => {
                let phase = v.get("phase").and_then(|p| p.as_str()).unwrap_or("");
                match phase {
                    "step_end" => {
                        let usage = v.get("usage").cloned();
                        vec![WireEvent {
                            kind: "agent_step".into(),
                            payload: json!({
                                "turn": v.get("turn"),
                                "step": v.get("step"),
                            }),
                            native_id: None,
                            usage,
                            text: None,
                            error: None,
                            result: None,
                        }]
                    }
                    "turn_end" => {
                        let reason = v
                            .get("reason")
                            .and_then(|r| r.get("kind"))
                            .and_then(|k| k.as_str())
                            .unwrap_or("");
                        let failed = reason == "error" || reason == "aborted";
                        vec![WireEvent {
                            kind: if failed { "agent_turn_failed" } else { "agent_turn_completed" }.into(),
                            payload: json!({"reason": reason, "turn": v.get("turn")}),
                            native_id: None,
                            usage: None,
                            text: None,
                            error: if failed {
                                Some(format!("dsh turn ended with reason {reason:?}"))
                            } else {
                                None
                            },
                            result: None,
                        }]
                    }
                    _ => vec![],
                }
            }
            "thinking" => {
                let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    return vec![];
                }
                // bounded echo; thinking is not agent output for the result contract
                vec![WireEvent {
                    kind: "agent_thinking".into(),
                    payload: json!({"chars": text.chars().count()}),
                    native_id: None,
                    usage: None,
                    text: None,
                    error: None,
                    result: None,
                }]
            }
            "text" | "final" => {
                let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                if text.is_empty() {
                    return vec![];
                }
                vec![WireEvent {
                    kind: "agent_message".into(),
                    payload: json!({"kind": typ}),
                    native_id: None,
                    usage: None,
                    text: Some(text),
                    error: None,
                    result: None,
                }]
            }
            "tool_call" => vec![WireEvent {
                kind: "agent_tool".into(),
                payload: json!({
                    "call_id": v.get("callId"),
                    "tool": v.get("tool"),
                }),
                native_id: None,
                usage: None,
                text: None,
                error: None,
                result: None,
            }],
            "tool_result" => {
                let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
                let failed = status != "completed";
                vec![WireEvent {
                    kind: if failed { "agent_tool_end" } else { "agent_tool_end" }.into(),
                    payload: json!({
                        "call_id": v.get("callId"),
                        "status": status,
                    }),
                    native_id: None,
                    usage: None,
                    text: None,
                    error: if failed {
                        Some(format!("dsh tool result status {status:?}"))
                    } else {
                        None
                    },
                    result: None,
                }]
            }
            // unknown event tolerance (R1): record shape, never fail the stream
            other => vec![WireEvent {
                kind: "agent_item".into(),
                payload: json!({"dsh_type": other}),
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
        if stderr.contains("quota") || stderr.contains("insufficient balance") || stderr.contains("rate limit") {
            return AgentResult {
                status: "blocked".into(),
                quota_blocked: true,
                blocked_reason: Some("dsh quota/usage limit".into()),
                ..Default::default()
            };
        }
        if stderr.contains("unauthorized") || stderr.contains("api key") || stderr.contains("not logged in") {
            return AgentResult {
                status: "blocked".into(),
                auth_required: true,
                blocked_reason: Some("dsh authentication required".into()),
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

/// Locate the DSH CLI on this machine and report its version (read-only probe).
pub fn probe() -> DshAdapter {
    let candidates: Vec<PathBuf> = std::env::var("WAGGLEPOST_DSH_EXE")
        .ok()
        .map(PathBuf::from)
        .into_iter()
        .chain(crate::worktree::find_in_path("dsh"))
        .collect();
    for c in candidates {
        if c.is_file() {
            return match run_version(&c) {
                Ok(v) => DshAdapter::new(Some(c), v),
                Err(e) => DshAdapter {
                    exe: Some(c),
                    version: String::new(),
                    probe_error: Some(e),
                },
            };
        }
    }
    DshAdapter {
        exe: None,
        version: String::new(),
        probe_error: Some("dsh CLI not found on this machine".into()),
    }
}

fn run_version(exe: &PathBuf) -> Result<String, String> {
    let (prog, prefix) = exe_and_prefix(exe);
    let mut cmd = std::process::Command::new(prog);
    cmd.args(prefix).arg("--version");
    let out = cmd
        .env_clear()
        .envs(crate::runner::filter_env(&[]))
        .output()
        .map_err(|e| e.to_string())?;
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v = s.lines().next().unwrap_or("").trim().to_string();
    if v.is_empty() {
        Err("no version output".into())
    } else {
        Ok(v)
    }
}
