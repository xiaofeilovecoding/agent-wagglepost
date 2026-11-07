//! DSH (DeepSeek Harness) adapter — 0.1 scope: capability probe only.
//! The DSH CLI (0.2.1-rc5 on this machine) is checked by `doctor`; it becomes a
//! dispatch candidate only when it passes the same permission/event/cancel gates as
//! the primary runtimes (P2 gate). No dispatch path is wired before that.

use super::{Adapter, DispatchContext, RunState, WireEvent};
use crate::model::AgentResult;
use crate::runner::{ExitOutcome, SpawnSpec};
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct DshAdapter {
    pub exe: Option<PathBuf>,
    pub version: Option<String>,
    pub probe_error: Option<String>,
}

impl DshAdapter {
    pub fn new(exe: Option<PathBuf>, version: Option<String>, probe_error: Option<String>) -> Self {
        DshAdapter {
            exe,
            version,
            probe_error,
        }
    }
}

impl Adapter for DshAdapter {
    fn id(&self) -> &'static str {
        "dsh"
    }

    fn build_spec(&self, _ctx: &DispatchContext) -> Result<SpawnSpec, String> {
        Err("dsh is not an enabled dispatch target in 0.1 (capability probe only)".into())
    }

    fn parse_line(&mut self, _line: &str) -> Vec<WireEvent> {
        vec![]
    }

    fn result_from_run(&self, _state: &RunState, _exit: &ExitOutcome) -> AgentResult {
        AgentResult {
            status: "blocked".into(),
            blocked_reason: Some("dsh dispatch not enabled in 0.1".into()),
            ..Default::default()
        }
    }
}

/// Locate the DSH CLI on this machine and report its headless capability.
pub fn probe() -> DshAdapter {
    let candidates: Vec<PathBuf> = std::env::var("WAGGLEPOST_DSH_EXE")
        .ok()
        .map(PathBuf::from)
        .into_iter()
        .chain(crate::worktree::find_in_path("dsh"))
        .collect();
    for c in candidates {
        if c.is_file() {
            match run_version(&c) {
                Ok(v) => return DshAdapter::new(Some(c), Some(v), None),
                Err(e) => return DshAdapter::new(Some(c), None, Some(e)),
            }
        }
    }
    DshAdapter::new(None, None, Some("dsh CLI not found on this machine".into()))
}

fn run_version(exe: &PathBuf) -> Result<String, String> {
    let is_script = exe
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
        .unwrap_or(false);
    let mut cmd = if cfg!(windows) && is_script {
        let mut c = std::process::Command::new("cmd.exe");
        c.args(["/c", &exe.to_string_lossy(), "--version"]);
        c
    } else {
        let mut c = std::process::Command::new(exe);
        c.arg("--version");
        c
    };
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

pub fn info(a: &DshAdapter) -> crate::model::AdapterInfo {
    crate::model::AdapterInfo {
        id: "dsh".into(),
        version: a.version.clone(),
        available: a.version.is_some(),
        simulated: false,
        write_supported: false,
        read_only_supported: false,
        resume_supported: false,
        cancel_supported: false,
        probe_error: a.probe_error.clone(),
    }
}

#[allow(dead_code)]
fn unused_json(_v: &Value) -> serde_json::Value {
    json!({})
}
