pub mod claude;
pub mod codex;
pub mod dsh;
pub mod fake;

use crate::model::*;
use crate::runner::{ExitOutcome, RunningChild, SpawnSpec};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct DispatchContext {
    pub task: Task,
    pub attempt_id: String,
    pub generation: u64,
    pub role: Role,
    pub worktree_path: String,
    pub handoff: HandoffBody,
    pub resume_native_id: Option<String>,
    pub lang: &'static str,
    pub managed_root: std::path::PathBuf,
    pub fake_script: Option<Value>,
    pub timeout_secs: u64,
    pub fake_exe: Option<std::path::PathBuf>,
}

#[derive(Clone)]
pub struct WireEvent {
    pub kind: String,
    pub payload: Value,
    /// native session/thread id if revealed by this event
    pub native_id: Option<String>,
    pub usage: Option<Value>,
    /// agent text output (for result extraction)
    pub text: Option<String>,
    /// structured error surfaced by the adapter
    pub error: Option<String>,
    /// structured agent result (fake adapter contract)
    pub result: Option<AgentResult>,
}

pub trait Adapter {
    fn id(&self) -> &'static str;
    fn build_spec(&self, ctx: &DispatchContext) -> Result<SpawnSpec, String>;
    fn parse_line(&mut self, line: &str) -> Vec<WireEvent>;
    /// Adapter-level verdict when the process ends. Default: parse trailing result.
    fn result_from_run(&self, state: &RunState, exit: &ExitOutcome) -> AgentResult;
}

#[derive(Default)]
pub struct RunState {
    pub native_id: Option<String>,
    pub last_text: Option<String>,
    pub usage: Option<Value>,
    pub errors: Vec<String>,
    pub saw_any_event: bool,
    pub fake_result: Option<AgentResult>,
}

pub struct RunOutcome {
    pub exit: ExitOutcome,
    pub state: RunState,
    /// wire events already emitted during the run
    pub emitted: usize,
}

/// Run one attempt end-to-end on the blocking side (call from spawn_blocking).
/// Cancel flag is polled by the runner stop; timeout enforces the task budget.
pub fn run_attempt(
    adapter: &mut dyn Adapter,
    ctx: &DispatchContext,
    cancel: Arc<AtomicBool>,
    on_event: &dyn Fn(&WireEvent),
) -> Result<RunOutcome, String> {
    let spec = adapter.build_spec(ctx)?;
    let child: RunningChild = crate::runner::launch(&spec)?;
    let pid = child.pid;
    let lines = child.take_lines().ok_or("stdout reader missing")?;
    let mut state = RunState::default();
    let mut emitted = 0usize;

    // reader side: parse lines as they arrive (non-blocking poll loop)
    let timeout = Duration::from_secs(ctx.timeout_secs.max(1));
    let started = std::time::Instant::now();
    let mut cancelled = false;
    let mut exit: Option<ExitOutcome> = None;

    loop {
        // drain pending lines
        while let Ok(line) = lines.try_recv() {
            for ev in adapter.parse_line(&line) {
                state.saw_any_event = true;
                if let Some(nid) = &ev.native_id {
                    state.native_id = Some(nid.clone());
                }
                if let Some(u) = &ev.usage {
                    state.usage = Some(u.clone());
                }
                if let Some(t) = &ev.text {
                    state.last_text = Some(t.clone());
                }
                if let Some(e) = &ev.error {
                    state.errors.push(e.clone());
                }
                if let Some(r) = &ev.result {
                    state.fake_result = Some(r.clone());
                }
                on_event(&ev);
                emitted += 1;
            }
        }
        if cancelled && exit.is_none() {
            // stop requested: wait for exit confirmation
            exit = Some(child.wait(Duration::from_secs(15)));
            break;
        }
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            child.stop();
            continue;
        }
        // do we have an exit already? peek by waiting a tick
        match child.poll_exit_short() {
            Some(outcome) => {
                exit = Some(outcome);
                break;
            }
            None => {
                if started.elapsed() > timeout {
                    child.stop();
                    cancelled = true;
                    exit = Some(child.wait(Duration::from_secs(15)));
                    break;
                }
                std::thread::sleep(Duration::from_millis(120));
            }
        }
    }

    // final drain of remaining lines
    while let Ok(line) = lines.try_recv() {
        for ev in adapter.parse_line(&line) {
            if let Some(nid) = &ev.native_id {
                state.native_id = Some(nid.clone());
            }
            if let Some(u) = &ev.usage {
                state.usage = Some(u.clone());
            }
            if let Some(t) = &ev.text {
                state.last_text = Some(t.clone());
            }
            if let Some(e) = &ev.error {
                state.errors.push(e.clone());
            }
            if let Some(r) = &ev.result {
                state.fake_result = Some(r.clone());
            }
            on_event(&ev);
            emitted += 1;
        }
    }

    let exit = exit.ok_or("no exit outcome")?;
    // record pid for evidence
    let _ = pid;
    let result = adapter.result_from_run(&state, &exit);
    Ok(RunOutcome {
        exit,
        state,
        emitted,
    })
}

/// Extract the wagglepost-result fenced block from the final agent text.
pub fn extract_agent_result(text: &str) -> Option<AgentResult> {
    let marker = "wagglepost-result";
    let mut best: Option<AgentResult> = None;
    let mut rest = text;
    while let Some(start) = rest.find(marker) {
        let after = &rest[start + marker.len()..];
        // skip to opening fence
        let brace = after.find('{')?;
        let depth_open = &after[brace..];
        // find matching closing brace
        let mut depth = 0i64;
        let mut end = None;
        for (i, c) in depth_open.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        let json_str = &depth_open[..=end];
        if let Ok(v) = serde_json::from_str::<Value>(json_str) {
            if let Ok(mut r) = serde_json::from_value::<AgentResult>(v.clone()) {
                // normalize options
                for opt in r.options.iter_mut() {
                    if opt.effect_en.is_empty() {
                        opt.effect_en = opt.effect_zh.clone();
                    }
                    if opt.effect_zh.is_empty() {
                        opt.effect_zh = opt.effect_en.clone();
                    }
                    if opt.label_en.is_empty() {
                        opt.label_en = opt.label_zh.clone();
                    }
                    if opt.label_zh.is_empty() {
                        opt.label_zh = opt.label_en.clone();
                    }
                }
                if ["done", "needs_decision", "blocked"].contains(&r.status.as_str()) {
                    best = Some(r);
                }
            }
        }
        rest = &after[end + 1..];
    }
    best
}

impl std::fmt::Debug for dyn Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Adapter({})", self.id())
    }
}
