//! Claude Code adapter: `claude --print --output-format stream-json --verbose` with the
//! task body on stdin (prompt via stdin). Reviewer role locks tools to Read/Grep/Glob,
//! strict MCP config with an empty server set. Resume: `--resume <session_id>`.
//! Events verified on-device (2.1.278): system:init / assistant / user / result.

use super::{Adapter, DispatchContext, RunState, WireEvent};
use crate::model::AgentResult;
use crate::runner::{ExitOutcome, SpawnSpec};
use serde_json::{json, Value};

/// Truncate to a UTF-8 char boundary at or below max_bytes; never panics on
/// multibyte input (WP08).
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.as_bytes().len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let ellipsis = "\u{2026}";
    format!("{}{}", &s[..end], ellipsis)
}

pub struct ClaudeAdapter {
    /// resolved command name/path (claude.cmd requires going through node)
    pub cmd: String,
    pub version: String,
}

impl ClaudeAdapter {
    pub fn new(cmd: String, version: String) -> Self {
        ClaudeAdapter { cmd, version }
    }
}

impl Adapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn build_spec(&self, ctx: &DispatchContext) -> Result<SpawnSpec, String> {
        let mut args: Vec<String> = vec![
            "--print".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
        ];
        match ctx.role {
            crate::model::Role::Reviewer => {
                args.push("--allowedTools".into());
                args.push("Read".into());
                args.push("Grep".into());
                args.push("Glob".into());
                args.push("--disallowedTools".into());
                args.push("Bash".into());
                args.push("Edit".into());
                args.push("Write".into());
                args.push("NotebookEdit".into());
                args.push("WebFetch".into());
                args.push("WebSearch".into());
                args.push("--strict-mcp-config".into());
                args.push("--mcp-config".into());
                args.push("{\"mcpServers\":{}}".into());
            }
            crate::model::Role::Writer => {
                // writer keeps default tooling but stays inside the worktree cwd;
                // permissions enforced by the worktree boundary + job object.
            }
        }
        let resume_delta = ctx.handoff.mode == "resume_delta";
        if resume_delta {
            let session = ctx
                .resume_native_id
                .clone()
                .ok_or("resume_delta requires native session id")?;
            args.push("--resume".into());
            args.push(session);
        }
        args.push("--max-turns".into());
        args.push("32".into());
        Ok(SpawnSpec {
            exe: resolve_claude_entry(&self.cmd)?,
            args,
            cwd: std::path::PathBuf::from(&ctx.worktree_path),
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
            "system" => {
                let subtype = v.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
                if subtype == "init" {
                    vec![WireEvent {
                        kind: "agent_started".into(),
                        payload: json!({
                            "session_id": v.get("session_id"),
                            "model": v.get("model"),
                            "cwd": v.get("cwd"),
                            "tools": v.get("tools"),
                            "permissionMode": v.get("permissionMode"),
                        }),
                        native_id: v.get("session_id").and_then(|s| s.as_str()).map(|s| s.to_string()),
                        usage: None,
                        text: None,
                        error: None,
                        result: None,
                    }]
                } else {
                    vec![]
                }
            }
            "assistant" => {
                let mut events = Vec::new();
                if let Some(content) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        let bt = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match bt {
                            "text" => {
                                let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                                if !text.is_empty() {
                                    events.push(WireEvent {
                                        kind: "agent_message".into(),
                                        payload: json!({}),
                                        native_id: None,
                                        usage: None,
                                        text: Some(text),
                                        error: None,
                                        result: None,
                                    });
                                }
                            }
                            "tool_use" => {
                                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                let input = block.get("input").cloned().unwrap_or(Value::Null);
                                // bound the echoed input (avoid huge payloads)
                                let input_s = serde_json::to_string(&input).unwrap_or_default();
                                let input_s = truncate_utf8(&input_s, 400);
                                events.push(WireEvent {
                                    kind: "agent_tool".into(),
                                    payload: json!({"tool": name, "input": input_s}),
                                    native_id: None,
                                    usage: None,
                                    text: None,
                                    error: None,
                                    result: None,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                events
            }
            "user" => {
                // tool results; treat errors from permission denials as evidence
                let mut events = Vec::new();
                if let Some(content) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            let is_err = block.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                            if is_err {
                                let txt = match block.get("content") {
                                    Some(Value::String(s)) => s.clone(),
                                    Some(Value::Array(a)) => a
                                        .iter()
                                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                                        .collect::<Vec<_>>()
                                        .join(" "),
                                    _ => String::new(),
                                };
                                let lower = txt.to_lowercase();
                                if lower.contains("permission") || lower.contains("denied") || lower.contains("not allowed") {
                                    events.push(WireEvent {
                                        kind: "agent_permission_denied".into(),
                                        payload: json!({"detail_bound": txt.chars().take(300).collect::<String>()}),
                                        native_id: None,
                                        usage: None,
                                        text: None,
                                        error: Some("permission denied".into()),
                                        result: None,
                                    });
                                }
                            }
                        }
                    }
                }
                events
            }
            "result" => {
                let usage = v.get("usage").cloned();
                let result_text = v.get("result").and_then(|r| r.as_str()).unwrap_or("").to_string();
                let is_error = v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                vec![WireEvent {
                    kind: if is_error { "agent_turn_failed" } else { "agent_turn_completed" }.into(),
                    payload: json!({"is_error": is_error, "num_turns": v.get("num_turns")}),
                    native_id: None,
                    usage,
                    text: if result_text.is_empty() { None } else { Some(result_text) },
                    error: if is_error {
                        Some("claude reported an error result".into())
                    } else {
                        None
                    },
                    result: None,
                }]
            }
            _ => vec![],
        }
    }

    fn result_from_run(&self, state: &RunState, exit: &ExitOutcome) -> AgentResult {
        if let Some(t) = &state.last_text {
            if let Some(r) = super::extract_agent_result(t) {
                return r;
            }
        }
        let stderr = String::from_utf8_lossy(&exit.stderr).to_lowercase();
        if stderr.contains("credit") || stderr.contains("usage limit") || stderr.contains("rate limit") {
            return AgentResult {
                status: "blocked".into(),
                quota_blocked: true,
                blocked_reason: Some("claude quota/usage limit".into()),
                ..Default::default()
            };
        }
        if stderr.contains("unauthorized") || stderr.contains("api key") || stderr.contains("login") {
            return AgentResult {
                status: "blocked".into(),
                auth_required: true,
                blocked_reason: Some("claude authentication required".into()),
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

/// Resolve the claude entry: on Windows `claude` is a .cmd shim; CreateProcessW needs a
/// real executable, so route through node with the CLI JS, or use the shim via cmd.exe.
/// Prefer the direct node entry when discoverable.
fn resolve_claude_entry(cmd: &str) -> Result<std::path::PathBuf, String> {
    let p = std::path::PathBuf::from(cmd);
    if p.extension().and_then(|e| e.to_str()) == Some("exe") && p.is_file() {
        return Ok(p);
    }
    Err(format!(
        "claude entry must be a real executable path, got {cmd}"
    ))
}
