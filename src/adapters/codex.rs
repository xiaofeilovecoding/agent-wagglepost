//! Codex adapter: `codex exec --json --sandbox <mode> -C <worktree>` with the task
//! body on stdin. Events verified on-device (codex-cli 0.166.0-alpha.5.1):
//! thread.started / turn.started / item.started / item.completed / turn.completed.
//! Resume: `codex exec resume <thread_id> --json ...` sends the delta to the same
//! native session when handoff version matches.

use super::{Adapter, DispatchContext, RunState, WireEvent};
use crate::model::AgentResult;
use crate::runner::{ExitOutcome, SpawnSpec};
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct CodexAdapter {
    pub exe: PathBuf,
    pub version: String,
}

impl CodexAdapter {
    pub fn new(exe: PathBuf, version: String) -> Self {
        CodexAdapter { exe, version }
    }
}

impl Adapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn build_spec(&self, ctx: &DispatchContext) -> Result<SpawnSpec, String> {
        let sandbox = match ctx.role {
            crate::model::Role::Writer => "workspace-write",
            crate::model::Role::Reviewer => "read-only",
        };
        let mut args: Vec<String> = vec!["exec".into()];
        let resume_delta = ctx.handoff.mode == "resume_delta";
        if resume_delta {
            let thread = ctx
                .resume_native_id
                .clone()
                .ok_or("resume_delta requires native session id")?;
            args.push("resume".into());
            args.push(thread);
        }
        args.extend([
            "--json".into(),
            "--sandbox".into(),
            sandbox.into(),
            "--skip-git-repo-check".into(),
            "-C".into(),
            ctx.worktree_path.clone(),
            "-".into(),
        ]);
        Ok(SpawnSpec {
            exe: self.exe.clone(),
            args,
            cwd: PathBuf::from(&ctx.worktree_path),
            env: crate::runner::filter_env(&[]),
            stdin_text: ctx.handoff.text.clone(),
        })
    }

    fn parse_line(&mut self, line: &str) -> Vec<WireEvent> {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match typ {
            "thread.started" => vec![WireEvent {
                kind: "agent_started".into(),
                payload: json!({"thread_id": v.get("thread_id")}),
                native_id: v.get("thread_id").and_then(|s| s.as_str()).map(|s| s.to_string()),
                usage: None,
                text: None,
                error: None,
                result: None,
            }],
            "turn.started" => vec![WireEvent {
                kind: "agent_turn_started".into(),
                payload: json!({}),
                native_id: None,
                usage: None,
                text: None,
                error: None,
                result: None,
            }],
            "item.started" | "item.completed" | "item.updated" => {
                let phase = typ.strip_prefix("item.").unwrap_or("updated");
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let item_id = item.get("id").and_then(|s| s.as_str()).unwrap_or("");
                match item_type {
                    "command_execution" => {
                        let cmd = item.get("command").and_then(|c| c.as_str()).unwrap_or("");
                        let exit_code = item.get("exit_code").cloned();
                        let status = item.get("status").and_then(|s| s.as_str()).unwrap_or("");
                        vec![WireEvent {
                            kind: if phase == "started" { "agent_tool" } else { "agent_tool_end" }.into(),
                            payload: json!({
                                "item_id": item_id,
                                "command": cmd,
                                "exit_code": exit_code,
                                "status": status,
                            }),
                            native_id: None,
                            usage: None,
                            text: None,
                            error: None,
                            result: None,
                        }]
                    }
                    "file_change" => vec![WireEvent {
                        kind: "agent_file_change".into(),
                        payload: json!({
                            "item_id": item_id,
                            "changes": item.get("changes"),
                            "status": item.get("status"),
                        }),
                        native_id: None,
                        usage: None,
                        text: None,
                        error: None,
                        result: None,
                    }],
                    "agent_message" => {
                        let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                        vec![WireEvent {
                            kind: "agent_message".into(),
                            payload: json!({"item_id": item_id}),
                            native_id: None,
                            usage: None,
                            text: Some(text),
                            error: None,
                            result: None,
                        }]
                    }
                    "error" => {
                        let msg = item.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
                        vec![WireEvent {
                            kind: "agent_error".into(),
                            payload: json!({"item_id": item_id}),
                            native_id: None,
                            usage: None,
                            text: None,
                            error: Some(msg),
                            result: None,
                        }]
                    }
                    other => vec![WireEvent {
                        kind: "agent_item".into(),
                        payload: json!({"item_type": other, "item_id": item_id, "phase": phase}),
                        native_id: None,
                        usage: None,
                        text: None,
                        error: None,
                        result: None,
                    }],
                }
            }
            "turn.completed" => vec![WireEvent {
                kind: "agent_turn_completed".into(),
                payload: json!({}),
                native_id: None,
                usage: v.get("usage").cloned(),
                text: None,
                error: None,
                result: None,
            }],
            "turn.failed" => vec![WireEvent {
                kind: "agent_turn_failed".into(),
                payload: json!({"error": v.get("error")}),
                native_id: None,
                usage: None,
                text: None,
                error: v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string()),
                result: None,
            }],
            _ => vec![],
        }
    }

    fn result_from_run(&self, state: &RunState, exit: &ExitOutcome) -> AgentResult {
        if let Some(t) = &state.last_text {
            if let Some(r) = super::extract_agent_result(t) {
                return r;
            }
        }
        // quota/auth signals from stderr or errors
        let stderr = String::from_utf8_lossy(&exit.stderr).to_lowercase();
        if stderr.contains("usage limit") || stderr.contains("quota") || state.errors.iter().any(|e| e.to_lowercase().contains("usage limit")) {
            return AgentResult {
                status: "blocked".into(),
                quota_blocked: true,
                blocked_reason: Some("codex quota/usage limit".into()),
                ..Default::default()
            };
        }
        if stderr.contains("unauthorized") || stderr.contains("not logged in") || stderr.contains("auth") {
            return AgentResult {
                status: "blocked".into(),
                auth_required: true,
                blocked_reason: Some("codex authentication required".into()),
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
