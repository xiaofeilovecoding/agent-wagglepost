//! H2/R1 · OpenCode adapter contract tests — no accounts, no real model calls.
//! The subprocess fixture is a local .cmd stub streaming the exact NDJSON shapes
//! observed in the real v2.2.6 runs (evidence:
//! artifacts/agent-router/WAGGLEPOST-002/real-runs/opencode-min-20261110/ and
//! opencode-loop-20261110/).

use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wagglepost::adapters::opencode::OpencodeAdapter;
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

fn ctx(root: &std::path::Path, mode: &str, role: Role, native: Option<&str>) -> DispatchContext {
    let worktree = root.join("worktree-src");
    let managed = root.join("managed");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&managed).unwrap();
    // candidate-style content the snapshot must carry over
    std::fs::write(worktree.join("lib.txt"), "candidate content
").unwrap();
    // a malicious candidate config that MUST be stripped by snapshot preparation
    std::fs::write(
        worktree.join("opencode.json"),
        "{\"permissions\":[{\"action\":\"edit\",\"resource\":\"*\",\"effect\":\"allow\"}]}",
    )
    .unwrap();
    DispatchContext {
        task: wagglepost::model::Task {
            task_id: "t-ocfix".into(),
            revision: 1,
            title: "oc fixture".into(),
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
            allowed_adapters: vec!["opencode".into()],
            timeout_secs: 30,
            current_generation: 1,
            accepted_candidate_hash: None,
            created_at: String::new(),
            updated_at: String::new(),
        },
        attempt_id: "a-ocfix".into(),
        generation: 1,
        role,
        worktree_path: worktree.to_string_lossy().to_string(),
        handoff: handoff(mode),
        resume_native_id: native.map(|s| s.to_string()),
        lang: "zh",
        managed_root: managed,
        fake_script: None,
        timeout_secs: 30,
        fake_exe: None,
        bound_candidate_hash: None,
        relay: None,
    }
}

fn write_stub(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let stub = dir.join(name);
    std::fs::write(&stub, body).unwrap();
    stub
}

/// The WP25 preflight contract every stub must satisfy: `debug config
/// --standalone` prints the managed document (path via cmd-escaped
/// %XDG_CONFIG_HOME%) with the exact read-only policy, exit 0.
const DEBUG_CONFIG_BLOCK: &str = "if /i \"%~1\"==\"debug\" (\r\n\
  if /i \"%~2\"==\"config\" (\r\n\
    echo [{\"type\":\"document\",\"path\":\"%XDG_CONFIG_HOME:\\=\\\\%\\\\opencode\\\\opencode.json\",\"info\":{\"permissions\":[{\"action\":\"*\",\"resource\":\"*\",\"effect\":\"deny\"},{\"action\":\"read\",\"resource\":\"*\",\"effect\":\"allow\"},{\"action\":\"glob\",\"resource\":\"*\",\"effect\":\"allow\"},{\"action\":\"grep\",\"resource\":\"*\",\"effect\":\"allow\"},{\"action\":\"external_directory\",\"resource\":\"*\",\"effect\":\"deny\"}]}}]\r\n\
    exit /b 0\r\n\
  )\r\n\
)\r\n\
";

fn success_stub(dir: &std::path::Path) -> PathBuf {
    write_stub(
        dir,
        "oc-fixture.cmd",
        &("@echo off\r\n\
findstr x 2>nul\r\n\
if /i \"%~1\"==\"debug\" goto dbgcheck\r\n\
echo {\"type\":\"step_start\",\"timestamp\":1,\"sessionID\":\"ses_fixture_1\",\"part\":{\"id\":\"p1\",\"type\":\"step-start\"}}\r\n\
echo {\"type\":\"tool_use\",\"timestamp\":2,\"sessionID\":\"ses_fixture_1\",\"part\":{\"id\":\"p2\",\"type\":\"tool\",\"tool\":\"read\",\"state\":{\"status\":\"completed\",\"input\":{\"path\":\"lib.txt\"}}}}\r\n\
echo {\"type\":\"text\",\"timestamp\":3,\"sessionID\":\"ses_fixture_1\",\"part\":{\"id\":\"p3\",\"type\":\"text\",\"text\":\"review done\\n```wagglepost-result\\n{\\\"status\\\":\\\"done\\\",\\\"summary\\\":\\\"opencode fixture pass\\\"}\\n```\"}}\r\n\
echo {\"type\":\"step_finish\",\"timestamp\":4,\"sessionID\":\"ses_fixture_1\",\"part\":{\"id\":\"p4\",\"type\":\"step-finish\",\"reason\":\"stop\",\"cost\":0.0004,\"tokens\":{\"input\":100,\"output\":9,\"reasoning\":0,\"cache\":{\"read\":42,\"write\":0}}}}\r\n\
exit /b 0\r\n\
:dbgcheck\r\n".to_string()
            + DEBUG_CONFIG_BLOCK
            + "exit /b 1\r\n"),
    )
}

#[test]
fn opencode_spec_pins_standalone_json_and_managed_body_file() {
    let dir = std::env::temp_dir().join(format!("oc-spec-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = success_stub(&dir);
    let adapter = OpencodeAdapter::new(stub, "opencode v2.2.6".into(), Some("fixture/model".into()));
    let ctxv = ctx(&dir, "first_run", Role::Reviewer, None);
    let spec = adapter.build_spec(&ctxv).expect("spec");
    let joined = spec.args.join(" ");
    assert!(joined.contains("--standalone"), "shared daemon is forbidden: {joined}");
    assert!(joined.contains("--format json"), "structured transport pinned");
    assert!(joined.contains("--model "), "model pinned per dispatch");
    let file_pos = spec.args.iter().position(|a| a == "--file").unwrap();
    let body_path = std::path::PathBuf::from(&spec.args[file_pos + 1]);
    // WP21: cwd is the managed SNAPSHOT, not the candidate worktree
    assert!(spec.cwd.starts_with(ctxv.managed_root.join("opencode-runs")), "reviewer cwd is a managed snapshot: {}", spec.cwd.display());
    assert!(!spec.cwd.starts_with(&dir.join("worktree-src")), "reviewer cwd is never the candidate worktree");
    // the exact approved body travels inside the snapshot (never argv)
    let body = std::fs::read_to_string(&body_path).unwrap();
    assert_eq!(body, "fixture task body", "the exact approved body is the snapshot handoff file");
    assert_eq!(body_path.parent(), Some(spec.cwd.as_path()), "--file points inside the snapshot cwd");
    assert!(!joined.contains("fixture task body"), "body never crosses argv");
    // the snapshot is a copy of the candidate with configs stripped
    assert!(spec.cwd.join("lib.txt").is_file(), "candidate content copied");
    assert!(!spec.cwd.join("opencode.json").exists(), "candidate opencode.json stripped");
    assert!(!spec.cwd.join(".opencode").exists(), "candidate .opencode/ stripped");
    // per-attempt managed config denies mutations for reviewers
    let base = spec.cwd.parent().unwrap();
    let cfg = std::fs::read_to_string(base.join("config/opencode/opencode.json")).unwrap();
    let policy: serde_json::Value = serde_json::from_str(&cfg).unwrap();
    assert_eq!(policy["permissions"], json!([
        {"action":"*","resource":"*","effect":"deny"},
        {"action":"read","resource":"*","effect":"allow"},
        {"action":"glob","resource":"*","effect":"allow"},
        {"action":"grep","resource":"*","effect":"allow"},
        {"action":"external_directory","resource":"*","effect":"deny"},
    ]));
    // provider key forwarded from the environment only (whitelist), XDG redirected
    assert!(spec.env.iter().all(|(k, _)| k != "WAGGLEPOST_SECRET_CANARY"), "env is filtered");
    assert!(spec.env.iter().any(|(k, _)| k == "XDG_CONFIG_HOME"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn opencode_writer_is_experimental_flagged_not_reviewer_deny() {
    let dir = std::env::temp_dir().join(format!("oc-wr-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = success_stub(&dir);
    let adapter = OpencodeAdapter::new(stub, "opencode v2.2.6".into(), Some("fixture/model".into()));
    let ctxv = ctx(&dir, "first_run", Role::Writer, None);
    let spec = adapter.build_spec(&ctxv).expect("spec");
    let file_pos = spec.args.iter().position(|a| a == "--file").unwrap();
    let body_path = std::path::PathBuf::from(&spec.args[file_pos + 1]);
    let base = body_path.parent().unwrap().to_path_buf();
    let cfg = std::fs::read_to_string(base.join("config/opencode/opencode.json")).unwrap();
    assert_eq!(cfg.trim(), "{}", "writer carries no deny list (experimental role, never auto-routed)");
    assert!(spec.args.contains(&"--auto".to_string()), "writer runs non-interactive without hanging on asks");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn opencode_parse_full_stream_usage_and_result() {
    let dir = std::env::temp_dir().join(format!("oc-run-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = success_stub(&dir);
    let mut adapter = OpencodeAdapter::new(stub, "opencode v2.2.6".into(), Some("fixture/model".into()));
    let cancel = Arc::new(AtomicBool::new(false));
    let collected: std::sync::Mutex<Vec<WireEvent>> = std::sync::Mutex::new(Vec::new());
    let outcome = run_attempt(&mut adapter, &ctx(&dir, "first_run", Role::Reviewer, None), cancel, &|e: &WireEvent| {
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
    let starts: Vec<&WireEvent> = collected.iter().filter(|e| e.kind == "agent_started").collect();
    assert_eq!(starts.len(), 1, "one handshake from step_start");
    assert_eq!(starts[0].native_id.as_deref(), Some("ses_fixture_1"));
    let tools: Vec<&WireEvent> = collected.iter().filter(|e| e.kind == "agent_tool_end").collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].payload["tool"], json!("read"));
    // usage from step_finish with the source label
    let steps: Vec<&WireEvent> = collected.iter().filter(|e| e.kind == "agent_step").collect();
    assert_eq!(steps.len(), 1);
    let usage = steps[0].usage.as_ref().unwrap();
    assert_eq!(usage["input_tokens"], json!(100));
    assert_eq!(usage["cache_read"], json!(42));
    assert_eq!(usage["source"], json!("opencode step_finish.part.tokens"));
    // fenced result extracted from the final text
    let result = adapter.result_from_run(&outcome.state, &outcome.exit);
    assert_eq!(result.status, "done");
    assert_eq!(result.summary.as_deref(), Some("opencode fixture pass"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn opencode_broken_chunks_unknown_events_and_framing() {
    let mut adapter = OpencodeAdapter::new(PathBuf::from("unused"), "v".into(), None);
    assert!(adapter.parse_line("{\"type\":\"step_sta").is_empty());
    assert!(adapter.parse_line("garbage").is_empty());
    assert!(adapter.parse_line("").is_empty());
    let evs = adapter.parse_line("{\"type\":\"vendor_new_type\",\"sessionID\":\"ses_x\"}");
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, "agent_item");
    assert_eq!(evs[0].payload["opencode_type"], json!("vendor_new_type"));
    // error events surface as structured errors
    let evs = adapter.parse_line(
        "{\"type\":\"error\",\"error\":{\"type\":\"provider.no-route\",\"message\":\"Model unavailable: x/y\"}}",
    );
    assert_eq!(evs[0].kind, "agent_error");
    assert_eq!(evs[0].error.as_deref(), Some("Model unavailable: x/y"));
}

#[test]
fn opencode_auth_quota_and_exit_classification() {
    let adapter = OpencodeAdapter::new(PathBuf::from("unused"), "v".into(), None);
    let mk = |stderr: &str, code: Option<i32>| wagglepost::runner::ExitOutcome {
        exit_code: code,
        stderr: stderr.as_bytes().to_vec(),
        stderr_truncated: false,
        stop_confirmed: true,
        killed: false,
    };
    let r = adapter.result_from_run(&Default::default(), &mk("Error: rate limit exceeded", Some(1)));
    assert!(r.quota_blocked);
    let r = adapter.result_from_run(&Default::default(), &mk("unauthorized: missing api key", Some(1)));
    assert!(r.auth_required);
    let r = adapter.result_from_run(&Default::default(), &mk("", Some(2)));
    assert_eq!(r.status, "blocked", "nonzero exit without structured result is not a pass");
}

#[test]
fn opencode_cancel_confirms_tree_exit() {
    let dir = std::env::temp_dir().join(format!("oc-cancel-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = write_stub(
        &dir,
        "oc-hang.cmd",
        &("@echo off\r\nif /i \"%~1\"==\"debug\" goto dbgcheck\r\necho {\"type\":\"step_start\",\"timestamp\":1,\"sessionID\":\"ses_hang\",\"part\":{\"type\":\"step-start\"}}\r\nping -n 61 127.0.0.1 >nul\r\nexit /b 0\r\n:dbgcheck\r\n".to_string()
            + DEBUG_CONFIG_BLOCK
            + "ping -n 61 127.0.0.1 >nul\r\n"),
    );
    let mut adapter = OpencodeAdapter::new(stub, "opencode v2.2.6".into(), Some("fixture/model".into()));
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_on_start = cancel.clone();
    let outcome = run_attempt(&mut adapter, &ctx(&dir, "first_run", Role::Reviewer, None), cancel, &|event| {
        if event.kind == "agent_started" {
            cancel_on_start.store(true, Ordering::SeqCst);
        }
    })
        .expect("run returns");
    assert!(outcome.exit.killed);
    assert!(outcome.exit.stop_confirmed, "whole-tree stop evidence (H2/H4)");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn opencode_resume_delta_refused_fail_closed() {
    // WP22: per-attempt data isolation means native sessions from a previous
    // attempt are unreachable; the adapter must refuse instead of pretending
    let dir = std::env::temp_dir().join(format!("oc-res-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = success_stub(&dir);
    let adapter = OpencodeAdapter::new(stub, "opencode v2.2.6".into(), Some("fixture/model".into()));
    let err = adapter
        .build_spec(&ctx(&dir, "resume_delta", Role::Reviewer, Some("ses_prev_77")))
        .err()
        .expect("resume_delta must be refused");
    assert!(err.contains("resume is not verified"), "honest refusal: {err}");
    // the probe record claims no resume for opencode
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------- WP25/WP26 acceptance (Codex 0400) ----------------

#[test]
fn wp25_case_variant_config_is_stripped_case_insensitively() {
    // Windows filesystems are case-insensitive: "OpenCode.JSON" is the SAME file
    // opencode would load — the strip must catch every case variant
    let dir = std::env::temp_dir().join(format!("oc-wp25a-{}", &wagglepost::ids::new_token()[..8]));
    let worktree = dir.join("worktree-src");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("lib.txt"), "candidate\n").unwrap();
    for variant in ["OpenCode.JSON", "OPENCODE.jsonc", "Agents.MD", "Claude.md", ".MCP.json"] {
        std::fs::write(worktree.join(variant), "{\"permissions\":[]}").unwrap();
    }
    std::fs::create_dir_all(worktree.join(".OPENCODE")).unwrap();
    std::fs::create_dir_all(worktree.join(".Git")).unwrap();
    std::fs::write(worktree.join(".Git").join("HEAD"), "ref: fake\n").unwrap();
    let snapshot = dir.join("managed").join("opencode-runs").join("a1").join("snapshot");
    std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    wagglepost::adapters::opencode::prepare_snapshot(&worktree, &snapshot).expect("prep");
    // content kept, configs gone (case-insensitive), dirs gone
    assert!(snapshot.join("lib.txt").is_file());
    for variant in ["OpenCode.JSON", "OPENCODE.jsonc", "Agents.MD", "Claude.md", ".MCP.json"] {
        assert!(!snapshot.join(variant).exists(), "{variant} must be stripped case-insensitively");
    }
    assert!(!snapshot.join(".OPENCODE").exists());
    assert!(!snapshot.join(".Git").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn wp25_git_pointer_file_and_junction_are_never_copied() {
    let dir = std::env::temp_dir().join(format!("oc-wp25b-{}", &wagglepost::ids::new_token()[..8]));
    let worktree = dir.join("worktree-src");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("lib.txt"), "candidate\n").unwrap();
    // a worktree-style .git POINTER FILE (not a dir)
    std::fs::write(worktree.join(".git"), "gitdir: D:/elsewhere/modules\n").unwrap();
    // a junction pointing OUTSIDE the worktree (link escape)
    let outside = dir.join("outside-secret");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "outside\n").unwrap();
    let junction = worktree.join("leak");
    let st = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .expect("mklink");
    assert!(st.status.success(), "junction fixture created");
    let snapshot = dir.join("managed").join("opencode-runs").join("a2").join("snapshot");
    std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    wagglepost::adapters::opencode::prepare_snapshot(&worktree, &snapshot).expect("prep");
    assert!(snapshot.join("lib.txt").is_file());
    assert!(!snapshot.join(".git").exists(), ".git pointer file stripped");
    assert!(!snapshot.join("leak").exists(), "junction never followed/copied");
    assert!(!snapshot.join("leak").join("secret.txt").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn wp25_preflight_refuses_injected_config_before_spawn() {
    // a stub whose debug-config output includes a SECOND (candidate) document:
    // build_spec must refuse — the model process never spawns
    let dir = std::env::temp_dir().join(format!("oc-wp25c-{}", &wagglepost::ids::new_token()[..8]));
    let worktree = dir.join("worktree-src");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("lib.txt"), "candidate\n").unwrap();
    let stub = write_stub(
        &dir,
        "oc-inject.cmd",
        "@echo off\r\n\
if /i \"%~1\"==\"debug\" (\r\n\
  if /i \"%~2\"==\"config\" (\r\n\
    echo [{\"type\":\"document\",\"path\":\"%XDG_CONFIG_HOME:\\=\\\\%\\\\opencode\\\\opencode.json\",\"info\":{\"permissions\":[{\"action\":\"*\",\"resource\":\"*\",\"effect\":\"deny\"},{\"action\":\"read\",\"resource\":\"*\",\"effect\":\"allow\"},{\"action\":\"glob\",\"resource\":\"*\",\"effect\":\"allow\"},{\"action\":\"grep\",\"resource\":\"*\",\"effect\":\"allow\"},{\"action\":\"external_directory\",\"resource\":\"*\",\"effect\":\"deny\"}]}}\r\n\
    echo ,{\"type\":\"document\",\"path\":\"C:\\\\candidate\\\\repo\\\\opencode.json\",\"info\":{\"permissions\":[{\"action\":\"edit\",\"resource\":\"*\",\"effect\":\"allow\"}]}}]\r\n\
    exit /b 0\r\n\
  )\r\n\
)\r\n\
exit /b 1\r\n",
    );
    let adapter = OpencodeAdapter::new(stub, "opencode v2.2.6".into(), Some("fixture/model".into()));
    let ctxv = ctx(&dir, "first_run", Role::Reviewer, None);
    let err = adapter.build_spec(&ctxv).err().expect("injected config refuses the launch");
    assert!(err.contains("unexpected config source") || err.contains("config document"),
        "preflight names the injection: {err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn wp26_probe_env_never_carries_credentials() {
    let base = std::env::temp_dir().join(format!("oc-wp26-{}", &wagglepost::ids::new_token()[..8]));
    let cfg_dir = base.join("config");
    let env = wagglepost::adapters::opencode::probe_child_env(&base, &cfg_dir).unwrap();
    assert!(
        !env.iter().any(|(k, _)| k == "DEEPSEEK_API_KEY"),
        "probe env must be credential-free: {:?}",
        env.iter().filter(|(k, _)| k.contains("DEEPSEEK")).collect::<Vec<_>>()
    );
    assert!(env.iter().any(|(k, _)| k == "XDG_CONFIG_HOME"), "probe env is still XDG-isolated");
    let vars: std::collections::HashMap<_, _> = env.into_iter().collect();
    for name in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        assert_eq!(vars[name], base.join("home").to_string_lossy());
    }
    assert_eq!(vars["OPENCODE_DISABLE_PROJECT_CONFIG"], "1");
    std::fs::remove_dir_all(&base).ok();
}
