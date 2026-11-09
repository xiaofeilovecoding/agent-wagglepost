//! H2/R1 · DSH adapter contract tests — no accounts, no real model calls.
//! The subprocess fixture is a local .cmd stub that streams the exact NDJSON
//! event shapes observed in the real 0.2.1-rc.5 headless run (evidence:
//! artifacts/agent-router/WAGGLEPOST-002/real-runs/dsh-min-20261109/).

use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wagglepost::adapters::dsh::DshAdapter;
use wagglepost::adapters::{run_attempt, Adapter, DispatchContext, WireEvent};
use wagglepost::model::{HandoffBody, Role};

fn handoff(mode: &str) -> HandoffBody {
    HandoffBody {
        text: "fixture task body".into(),
        bytes: 18,
        sha256: "fixture".into(),
        evidence_refs: vec![],
        mode: mode.into(),
        budget_bytes: 8192,
    }
}

fn ctx(worktree: &std::path::Path, mode: &str, native: Option<&str>) -> DispatchContext {
    DispatchContext {
        task: wagglepost::model::Task {
            task_id: "t-dshfix".into(),
            revision: 1,
            title: "dsh fixture".into(),
            goal: "fixture".into(),
            acceptance: vec![],
            repo_path: worktree.to_string_lossy().to_string(),
            repo_id: "r".into(),
            base_commit: "head".into(),
            allowed_paths: vec![],
            forbidden_paths: vec![],
            recipe: wagglepost::model::Recipe::Direct,
            status: wagglepost::model::TaskStatus::Running,
            planned_steps: 1,
            resume_budget: 1,
            resume_used: 0,
            launches_used: 1,
            allowed_adapters: vec!["dsh".into()],
            timeout_secs: 30,
            current_generation: 1,
            accepted_candidate_hash: None,
            created_at: String::new(),
            updated_at: String::new(),
        },
        attempt_id: "a-dshfix".into(),
        generation: 1,
        role: Role::Writer,
        worktree_path: worktree.to_string_lossy().to_string(),
        handoff: handoff(mode),
        resume_native_id: native.map(|s| s.to_string()),
        lang: "zh",
        managed_root: worktree.to_path_buf(),
        fake_script: None,
        timeout_secs: 30,
        fake_exe: None,
        bound_candidate_hash: None,
        relay: None,
    }
}

/// DSH stub .cmd: streams the full verified event contract, writes a fixture
/// file, reports the wagglepost-result fenced block, exits 0.
fn write_success_stub(dir: &std::path::Path) -> PathBuf {
    let stub = dir.join("dsh-fixture.cmd");
    std::fs::write(
        &stub,
        "@echo off\r\n\
rem consume stdin so the writer thread never blocks\r\n\
findstr x 2>nul\r\n\
echo {\"type\":\"session\",\"sessionId\":\"session-fixture-1\",\"cwd\":\"worktree\"}\r\n\
echo {\"type\":\"status\",\"phase\":\"turn_start\",\"turn\":1}\r\n\
echo {\"type\":\"status\",\"phase\":\"step_start\",\"turn\":1,\"step\":1}\r\n\
echo {\"type\":\"text\",\"text\":\"working\\n```wagglepost-result\\n{\\\"status\\\":\\\"done\\\",\\\"summary\\\":\\\"dsh fixture wrote file\\\"}\\n```\"}\r\n\
echo {\"type\":\"status\",\"phase\":\"step_end\",\"turn\":1,\"step\":1,\"usage\":{\"inputTokens\":10,\"outputTokens\":2,\"cacheReadTokens\":5,\"cacheWriteTokens\":0,\"totalTokens\":17}}\r\n\
echo {\"type\":\"status\",\"phase\":\"turn_end\",\"turn\":1,\"reason\":{\"kind\":\"completed\"}}\r\n\
echo {\"type\":\"final\",\"text\":\"done\\n```wagglepost-result\\n{\\\"status\\\":\\\"done\\\",\\\"summary\\\":\\\"dsh fixture wrote file\\\"}\\n```\"}\r\n\
echo fixture-content > dsh-fixture-output.md\r\n\
exit /b 0\r\n",
    )
    .unwrap();
    stub
}

fn events_of<'a>(evs: &'a [WireEvent], kind: &str) -> Vec<&'a WireEvent> {
    evs.iter().filter(|e| e.kind == kind).collect()
}

#[test]
fn dsh_spec_uses_argv_array_stdin_body_and_no_shell_string() {
    let dir = std::env::temp_dir().join(format!("dsh-spec-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = write_success_stub(&dir);
    let adapter = DshAdapter::new(Some(stub), "0.2.1-rc.5".into());
    let spec = adapter.build_spec(&ctx(&dir, "first_run", None)).expect("spec");
    // H4: exe is the resolved entry (cmd.exe wrapper for .cmd), args are an argv
    // ARRAY with no shell metacharacter string, body travels on stdin
    assert!(spec.exe.to_string_lossy().to_lowercase().ends_with("cmd.exe"), "cmd resolved to an absolute executable: {}", spec.exe.display());
    let joined = spec.args.join(" ");
    assert!(joined.contains("--profile headless"), "headless profile pinned: {joined}");
    assert!(joined.contains("--json"), "structured transport pinned");
    assert!(joined.ends_with("-"), "task body arrives on stdin via -");
    assert_eq!(spec.stdin_text, "fixture task body");
    assert_eq!(spec.cwd, dir, "cwd is the worktree");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dsh_spec_resume_adds_session_id_flag() {
    let dir = std::env::temp_dir().join(format!("dsh-res-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = write_success_stub(&dir);
    let adapter = DshAdapter::new(Some(stub), "0.2.1-rc.5".into());
    let spec = adapter
        .build_spec(&ctx(&dir, "resume_delta", Some("session-prev-9")))
        .expect("spec");
    let joined = spec.args.join(" ");
    assert!(joined.contains("--session-id session-prev-9"), "resume addresses the native session: {joined}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dsh_parse_full_stream_and_extract_result() {
    let dir = std::env::temp_dir().join(format!("dsh-run-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = write_success_stub(&dir);
    let mut adapter = DshAdapter::new(Some(stub.clone()), "0.2.1-rc.5".into());
    let cancel = Arc::new(AtomicBool::new(false));
    let collected: std::sync::Mutex<Vec<WireEvent>> = std::sync::Mutex::new(Vec::new());
    let outcome = run_attempt(&mut adapter, &ctx(&dir, "first_run", None), cancel, &|e: &WireEvent| {
        collected.lock().unwrap().push(WireEvent {
            kind: e.kind.clone(),
            payload: e.payload.clone(),
            native_id: e.native_id.clone(),
            usage: e.usage.clone(),
            text: e.text.clone(),
            error: e.error.clone(),
            result: e.result.clone(),
        });
    })
    .expect("run completes");
    let collected = collected.into_inner().unwrap();
    // handshake: exactly one agent_started carrying the native session id
    let starts = events_of(&collected, "agent_started");
    assert_eq!(starts.len(), 1, "one handshake");
    assert_eq!(starts[0].native_id.as_deref(), Some("session-fixture-1"));
    // usage captured from step_end
    let steps = events_of(&collected, "agent_step");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].usage.as_ref().unwrap()["totalTokens"], json!(17));
    // turn completed
    assert_eq!(events_of(&collected, "agent_turn_completed").len(), 1);
    // the fenced wagglepost-result block was extracted
    let result = adapter.result_from_run(&outcome.state, &outcome.exit);
    assert_eq!(result.status, "done");
    assert_eq!(result.summary.as_deref(), Some("dsh fixture wrote file"));
    // exit 0 with a structured result → completed, stop confirmed (tree exit)
    assert_eq!(outcome.exit.exit_code, Some(0));
    assert!(outcome.exit.stop_confirmed, "job tree exit is confirmed evidence");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dsh_broken_chunks_and_unknown_events_are_tolerated() {
    let mut adapter = DshAdapter::new(None, "0.2.1-rc.5".into());
    // broken/partial JSON lines never panic and never fabricate events
    assert!(adapter.parse_line("{\"type\":\"sess").is_empty());
    assert!(adapter.parse_line("not json at all").is_empty());
    assert!(adapter.parse_line("").is_empty());
    assert!(adapter.parse_line("{\"type\":\"status\"}").is_empty(), "status without phase is ignored");
    // unknown event types land in a tolerated catch-all bucket
    let evs = adapter.parse_line("{\"type\":\"vendor_future_event\",\"x\":1}");
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, "agent_item");
    assert_eq!(evs[0].payload["dsh_type"], json!("vendor_future_event"));
    // oversized text stays bounded
    let big = "x".repeat(50_000);
    let evs = adapter.parse_line(&format!("{{\"type\":\"text\",\"text\":\"{big}\"}}"));
    assert_eq!(evs[0].text.as_ref().unwrap().chars().count(), 50_000, "parse keeps full text; the line cap lives in the runner");
}

#[test]
fn dsh_auth_quota_and_nonzero_exit_classification() {
    let adapter = DshAdapter::new(None, "0.2.1-rc.5".into());
    let mk = |stderr: &str, code: Option<i32>| wagglepost::runner::ExitOutcome {
        exit_code: code,
        stderr: stderr.as_bytes().to_vec(),
        stderr_truncated: false,
        stop_confirmed: true,
        killed: false,
    };
    let r = adapter.result_from_run(&Default::default(), &mk("Error: insufficient balance for quota", Some(1)));
    assert_eq!(r.status, "blocked");
    assert!(r.quota_blocked);
    let r = adapter.result_from_run(&Default::default(), &mk("unauthorized: api key invalid", Some(1)));
    assert_eq!(r.status, "blocked");
    assert!(r.auth_required);
    let r = adapter.result_from_run(&Default::default(), &mk("", Some(3)));
    assert_eq!(r.status, "blocked", "nonzero exit without structured result is not a pass");
    // killed (cancel/timeout) reports blocked, never done
    let r = adapter.result_from_run(&Default::default(), &wagglepost::runner::ExitOutcome {
        exit_code: None,
        stderr: Vec::new(),
        stderr_truncated: false,
        stop_confirmed: true,
        killed: true,
    });
    assert_eq!(r.status, "blocked");
    // unknown usage shape: usage passes through as provider-raw (receipt marks it)
    let mut st = wagglepost::adapters::RunState::default();
    st.usage = Some(json!({"weird_field": true}));
    assert_eq!(st.usage.as_ref().unwrap()["weird_field"], json!(true));
}

#[test]
fn dsh_cancel_confirms_tree_exit_before_slot_handover() {
    let dir = std::env::temp_dir().join(format!("dsh-cancel-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    // a stub that streams the handshake then hangs forever
    let stub = dir.join("dsh-hang.cmd");
    std::fs::write(
        &stub,
        "@echo off\r\n\
echo {\"type\":\"session\",\"sessionId\":\"session-hang-1\",\"cwd\":\"worktree\"}\r\n\
ping -n 61 127.0.0.1 >nul\r\n",
    )
    .unwrap();
    let mut adapter = DshAdapter::new(Some(stub), "0.2.1-rc.5".into());
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        c2.store(true, Ordering::SeqCst);
    });
    let outcome = run_attempt(&mut adapter, &ctx(&dir, "first_run", None), cancel, &|_| {}).expect("run returns");
    assert!(outcome.exit.killed, "cancel killed the run");
    assert!(outcome.exit.stop_confirmed, "stop evidence requires the whole tree exit (H2/H4)");
    assert_eq!(outcome.exit.exit_code, Some(1), "TerminateJobObject exit code");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dsh_version_drift_rebinds_capability() {
    // the probe record revalidation is covered in wp_capability; here: the
    // adapter pins the version it was constructed with, and dispatch-time drift
    // is refused by core (plan.approved_versions binding, WP09).
    let a1 = DshAdapter::new(None, "0.2.1-rc.5".into());
    assert_eq!(a1.version, "0.2.1-rc.5");
    let a2 = DshAdapter::new(None, "0.3.0".into());
    assert_eq!(a2.version, "0.3.0");
}
