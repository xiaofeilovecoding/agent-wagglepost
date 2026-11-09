//! Fake adapter: offline demo/test runtime, always labeled 模拟 (simulated).
//! Spawns this same binary as a hidden subcommand `__fake-agent` with a JSON script.
//! Scripted behaviors: quota block, duplicate events, lost ack, non-zero exit, hang,
//! partial writes, needs_decision, review verdicts. Production actions are never
//! driven by fake attempts.

use super::{Adapter, DispatchContext, RunState, WireEvent};
use crate::model::AgentResult;
use crate::runner::{ExitOutcome, SpawnSpec};
use serde_json::{json, Value};

pub struct FakeAdapter {
    pub version: String,
}

impl FakeAdapter {
    pub fn new() -> Self {
        FakeAdapter {
            version: "0.2.0".to_string(),
        }
    }
}

impl Default for FakeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for FakeAdapter {
    fn id(&self) -> &'static str {
        "fake"
    }

    fn build_spec(&self, ctx: &DispatchContext) -> Result<SpawnSpec, String> {
        let script = ctx
            .fake_script
            .clone()
            .ok_or("fake adapter requires a script")?;
        let exe = ctx
            .fake_exe
            .clone()
            .or_else(|| std::env::var("WAGGLEPOST_FAKE_EXE").ok().map(std::path::PathBuf::from))
            .unwrap_or(std::env::current_exe().map_err(|e| e.to_string())?);
        let script_path = ctx
            .managed_root
            .join("fake-scripts")
            .join(format!("{}.json", ctx.attempt_id));
        if let Some(parent) = script_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&script_path, serde_json::to_string(&script).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        Ok(SpawnSpec {
            exe,
            args: vec![
                "__fake-agent".to_string(),
                "--script".to_string(),
                script_path.to_string_lossy().to_string(),
                "--capture".to_string(),
                ctx.managed_root
                    .join("fake-captures")
                    .join(format!("{}.stdin.txt", ctx.attempt_id))
                    .to_string_lossy()
                    .to_string(),
            ],
            cwd: std::path::PathBuf::from(&ctx.worktree_path),
            env: crate::runner::filter_env(&[]),
            stdin_text: ctx.handoff.text.clone(),
        })
    }

    fn parse_line(&mut self, line: &str) -> Vec<WireEvent> {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                return vec![WireEvent {
                    kind: "agent_output".into(),
                    payload: json!({"simulated": true, "raw": line}),
                    native_id: None,
                    usage: None,
                    text: None,
                    error: None,
                    result: None,
                }]
            }
        };
        if v.get("fake").and_then(|f| f.as_str()) != Some("1") {
            return vec![];
        }
        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        match typ.as_str() {
            "started" => vec![WireEvent {
                kind: "agent_started".into(),
                payload: json!({"simulated": true}),
                native_id: v.get("session_id").and_then(|s| s.as_str()).map(|s| s.to_string()),
                usage: None,
                text: None,
                error: None,
                result: None,
            }],
            "tool" => vec![WireEvent {
                kind: "agent_tool".into(),
                payload: json!({"simulated": true, "name": v.get("name"), "detail": v.get("detail")}),
                native_id: None,
                usage: None,
                text: None,
                error: None,
                result: None,
            }],
            "file_write" => vec![WireEvent {
                kind: "agent_file_change".into(),
                payload: json!({"simulated": true, "path": v.get("path")}),
                native_id: None,
                usage: None,
                text: None,
                error: None,
                result: None,
            }],
            "message" => {
                let text = v
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                vec![WireEvent {
                    kind: "agent_message".into(),
                    payload: json!({"simulated": true}),
                    native_id: None,
                    usage: None,
                    text: Some(text.clone()),
                    error: None,
                    result: None,
                }]
            }
            "duplicate" => {
                // emit the same wire event twice to test dedupe
                let ev = WireEvent {
                    kind: "agent_message".into(),
                    payload: json!({"simulated": true, "dedupe_probe": v.get("key")}),
                    native_id: None,
                    usage: None,
                    text: Some(String::new()),
                    error: None,
                    result: None,
                };
                vec![ev.clone(), ev]
            }
            "error" => vec![WireEvent {
                kind: "agent_error".into(),
                payload: json!({"simulated": true}),
                native_id: None,
                usage: None,
                text: None,
                error: v.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()),
                result: None,
            }],
            "result" => {
                let r: AgentResult = v
                    .get("result")
                    .cloned()
                    .and_then(|r| serde_json::from_value(r).ok())
                    .unwrap_or(AgentResult {
                        status: "done".into(),
                        summary: Some("simulated done".into()),
                        ..Default::default()
                    });
                vec![WireEvent {
                    kind: "agent_result".into(),
                    payload: json!({"simulated": true, "status": r.status}),
                    native_id: None,
                    usage: v.get("usage").cloned(),
                    text: Some(text_of_result(&r)),
                    error: None,
                    result: None,
                }]
            }
            _ => vec![WireEvent {
                kind: "agent_output".into(),
                payload: json!({"simulated": true, "type": typ}),
                native_id: None,
                usage: None,
                text: None,
                error: None,
                result: None,
            }],
        }
    }

    fn result_from_run(&self, state: &RunState, exit: &ExitOutcome) -> AgentResult {
        // script-declared result wins (it is the structured contract); text block fallback
        if let Some(r) = &state.fake_result {
            return r.clone();
        }
        if let Some(t) = &state.last_text {
            if let Some(r) = super::extract_agent_result(t) {
                return r;
            }
        }
        if exit.killed {
            return AgentResult {
                status: "blocked".into(),
                blocked_reason: Some("simulated: stopped before reporting".into()),
                ..Default::default()
            };
        }
        if exit.exit_code != Some(0) {
            return AgentResult {
                status: "blocked".into(),
                blocked_reason: Some(format!("simulated: exit code {:?}", exit.exit_code)),
                ..Default::default()
            };
        }
        AgentResult {
            status: "done".into(),
            summary: Some("simulated run finished".into()),
            ..Default::default()
        }
    }
}

fn text_of_result(r: &AgentResult) -> String {
    let mut obj = serde_json::to_value(r).unwrap_or(json!({}));
    if let Some(o) = obj.as_object_mut() {
        o.insert("status".into(), json!(r.status));
    }
    format!("```wagglepost-result\n{}\n```", serde_json::to_string(&obj).unwrap_or_default())
}
