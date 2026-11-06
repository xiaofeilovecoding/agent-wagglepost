//! WAGGLEPOST-001 review regressions WP01–WP08 (05:00 Codex review).
//! Each test reproduces the review counterexample offline and must FAIL on the
//! reviewed HEAD 7ab99d3 and PASS after the rework.
mod common;

use common::TestEnv;
use wagglepost::adapters::Adapter as _;
use serde_json::{json, Value};
use std::time::Duration;

// ---------- WP01: authentication on data endpoints ----------

#[test]
fn wp01_anonymous_and_wrong_cookie_rejected() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);
    // anonymous GET on a data endpoint must not leak task content
    let r = env.raw("GET", &format!("/v1/tasks/{task_id}"), None, &[]);
    assert!(
        r.is_err(),
        "anonymous GET task detail must be rejected, got {:?}",
        r.map(|v| v.to_string())
    );
    let r = env.raw("GET", "/v1/tasks", None, &[]);
    assert!(r.is_err(), "anonymous GET task list must be rejected");
    // wrong cookie NAME must not authenticate (substring parsing bug)
    let r = env.raw(
        "GET",
        &format!("/v1/tasks/{task_id}"),
        None,
        &[("Cookie", &format!("xwp_session={}", env.token))],
    );
    assert!(r.is_err(), "wrong cookie name must not authenticate");
    // right cookie name authenticates
    let r = env.raw(
        "GET",
        &format!("/v1/tasks/{task_id}"),
        None,
        &[("Cookie", &format!("wp_session={}", env.token))],
    );
    assert!(r.is_ok(), "exact session cookie must authenticate: {:?}", r.err().map(|e| e.1.to_string()));
    // SSE requires auth
    let r = env.raw("GET", "/v1/events", None, &[]);
    assert!(r.is_err(), "anonymous SSE must be rejected");
    // health stays public
    let r = env.raw("GET", "/v1/health", None, &[]);
    assert!(r.is_ok(), "health check stays public");
}

#[test]
fn wp01_startup_code_single_consumption() {
    let env = TestEnv::new();
    // burn the code once via the browser path
    let r = env.raw(
        "POST",
        "/v1/session",
        Some(&json!({"code": env.startup_code()})),
        &[("Origin", &env.base())],
    );
    assert!(r.is_ok(), "first exchange succeeds: {:?}", r.err().map(|e| e.1.to_string()));
    // second exchange with the same code fails
    let r = env.raw(
        "POST",
        "/v1/session",
        Some(&json!({"code": env.startup_code()})),
        &[("Origin", &env.base())],
    );
    assert!(r.is_err(), "startup code must be single-use");
    // concurrent exchange: exactly one success across threads
    let env2 = TestEnv::new();
    let code = env2.startup_code();
    let base = env2.base();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let base = base.clone();
        let code = code.clone();
        handles.push(std::thread::spawn(move || {
            let mut r = ureq::post(&format!("{base}/v1/session"))
                .timeout(Duration::from_secs(30))
                .set("Origin", &base)
                .set("Host", &base.trim_start_matches("http://"));
            let resp = r.send_json(json!({"code": code}));
            resp.is_ok()
        }));
    }
    let wins = handles.into_iter().map(|h| h.join().unwrap_or(false)).filter(|ok| *ok).count();
    assert_eq!(wins, 1, "exactly one concurrent exchange may win, got {wins}");
}

// ---------- WP02: git execution isolation ----------

#[test]
fn wp02_post_checkout_hook_not_executed() {
    let env = TestEnv::new();
    // plant a post-checkout hook in the user repo
    let hook = env.user_repo.join(".git").join("hooks").join("post-checkout");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    let marker = env.user_repo.join("HOOK-MARKER.txt");
    std::fs::write(&hook, format!("#!/bin/sh\necho ran > \"{}\"\nexit 0\n", marker.to_string_lossy().replace('\\', "/"))).unwrap();
    let body = json!({
        "goal": "append a line to lib.txt (offline simulated)",
        "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct",
        "allowed_adapters": ["fake"],
    });
    let resp = env.req("POST", "/v1/tasks", Some(&body), Some("wp02-1")).expect("submit with hook repo");
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
    env.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None).expect("plan");
    let hash = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap()["plan"]["summary_hash"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/plan/approve"), Some(&json!({"summary_hash": hash})), None).expect("approve");
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp02-d1")).expect("dispatch");
    let status = env.run_until(&task_id, &["needs_review", "interrupted"], Duration::from_secs(90));
    assert_eq!(status, "needs_review");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !marker.exists(),
        "post-checkout hook must not execute during managed worktree creation"
    );
}

#[test]
fn wp02_configured_filter_repo_blocked() {
    let env = TestEnv::new();
    // repo config defines an external clean filter → unsafe, must be blocked
    let gitconfig = env.user_repo.join(".git").join("config");
    let mut cfg = std::fs::read_to_string(&gitconfig).unwrap();
    cfg.push_str("[filter \"evil\"]\n\tclean = ./evil-clean\n\tsmudge = ./evil-smudge\n\trequired = true\n");
    std::fs::write(&gitconfig, cfg).unwrap();
    std::fs::write(env.user_repo.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
    let body = json!({
        "goal": "g", "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct", "allowed_adapters": ["fake"],
    });
    let r = env.req("POST", "/v1/tasks", Some(&body), Some("wp02-2"));
    assert!(r.is_err(), "repo with configured external filter must be blocked");
    let err = r.unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("repo_unsafe_git"));
}

// ---------- WP03: candidate scope authorization ----------

#[test]
fn wp03_out_of_scope_change_blocks_candidate() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task_scoped(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {
                "lib.txt": "baseline content\nin-scope edit (simulated)\n",
                "outside-scope.txt": "must not enter the candidate (simulated)\n"
            },
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
        vec!["lib.txt".to_string()],
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp03-1")).expect("dispatch");
    let status = env.run_until(&task_id, &["needs_review", "interrupted"], Duration::from_secs(90));
    assert_eq!(status, "interrupted", "out-of-scope change must block the candidate");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["candidates"].as_array().unwrap().len(), 0, "no acceptable candidate with out-of-scope files");
    // diagnostics name the offending path
    let timeline = serde_json::to_string(&snap["events"]).unwrap();
    assert!(timeline.contains("outside-scope.txt"), "diagnostics name the out-of-scope path");
    // the in-scope-only control still produces a candidate
    let (t2, _) = env.ready_task_scoped(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content\nscoped edit (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
        vec!["lib.txt".to_string()],
    );
    env.req("POST", &format!("/v1/tasks/{t2}/dispatch"), Some(&json!({})), Some("wp03-2")).expect("dispatch");
    let status2 = env.run_until(&t2, &["needs_review", "interrupted"], Duration::from_secs(90));
    assert_eq!(status2, "needs_review", "in-scope changes still produce a candidate");
}

// ---------- WP04: review gate on accept ----------

#[test]
fn wp04_accept_requires_pass_review_in_write_then_review() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content
writer edit (simulated)
"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp04-1")).expect("dispatch");
    // poll WITHOUT ticking: the scheduler must not run the reviewer first
    let start = std::time::Instant::now();
    loop {
        let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        if snap["task"]["status"] == json!("needs_review") {
            assert_eq!(snap["reviews"].as_array().unwrap().len(), 0, "no reviews before the reviewer runs");
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "writer never finished");
        std::thread::sleep(Duration::from_millis(150));
    }
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let cand = snap["candidates"].as_array().unwrap()[0]["candidate_hash"].as_str().unwrap().to_string();
    // accept with ZERO reviews must fail
    let err = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/accept"),
        Some(&json!({"candidate_hash": cand})),
        Some("wp04-acc0"),
    ).unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("review_required"), "accept before any review is refused");
    // an issues review also blocks acceptance
    env.set_fake_script(&task_id, "s2", &json!({
        "behaviors": [],
        "result": {"status": "done", "summary": "review found issues: x (simulated)"}
    }));
    let start = std::time::Instant::now();
    loop {
        let _ = env.req("POST", "/v1/__tick", Some(&json!({})), None);
        let s = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        if !s["reviews"].as_array().unwrap().is_empty() { break; }
        assert!(start.elapsed() < Duration::from_secs(60), "review never recorded");
        std::thread::sleep(Duration::from_millis(200));
    }
    let err2 = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/accept"),
        Some(&json!({"candidate_hash": cand})),
        Some("wp04-acc1"),
    ).unwrap_err();
    assert_eq!(err2.1["error"]["code"], json!("review_not_passed"), "issues verdict blocks acceptance");
}

#[test]
fn wp04_direct_recipe_keeps_manual_accept() {
    let env = TestEnv::new();
    // the review gate is write_then_review-only; direct runs keep manual acceptance
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"d.txt": "direct (simulated)
"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("da-1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let cand = snap["candidates"].as_array().unwrap()[0]["candidate_hash"].as_str().unwrap().to_string();
    let acc = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/accept"),
        Some(&json!({"candidate_hash": cand})),
        Some("da-acc"),
    ).expect("direct recipe accepts without review");
    assert_eq!(acc["accepted"], json!(true));
}

#[test]
fn wp04_reviewer_binds_launch_candidate() {
    // the review record names the candidate it was launched against (not whatever is
    // current at finish); A/B re-collection marks superseded reviews historical
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"x.txt": "X (simulated)
"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("rb-1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    let start = std::time::Instant::now();
    loop {
        let _ = env.req("POST", "/v1/__tick", Some(&json!({})), None);
        let s = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        if !s["reviews"].as_array().unwrap().is_empty() { break; }
        assert!(start.elapsed() < Duration::from_secs(60), "review never recorded");
        std::thread::sleep(Duration::from_millis(200));
    }
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let cand = snap["candidates"].as_array().unwrap()[0]["candidate_hash"].as_str().unwrap().to_string();
    let review = &snap["reviews"].as_array().unwrap()[0];
    assert_eq!(review["candidate_hash"], json!(cand), "review bound to the frozen candidate");
    assert_eq!(review["historical"], json!(false));
}

// ---------- WP05: submit idempotency ----------

#[test]
fn wp05_same_key_same_body_single_task() {
    let env = TestEnv::new();
    let body = json!({
        "goal": "idempotent submit (offline)",
        "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct",
        "allowed_adapters": ["fake"],
    });
    let first = env.req("POST", "/v1/tasks", Some(&body), Some("wp05-key")).expect("first submit");
    let second = env.req("POST", "/v1/tasks", Some(&body), Some("wp05-key")).expect("replay submit");
    let t1 = first["task"]["task_id"].as_str().unwrap();
    let t2 = second["task"]["task_id"].as_str().unwrap();
    assert_eq!(t1, t2, "same key + same body must return the SAME task");
    // the request record is completed and names the task
    let rec = env.req("GET", "/v1/requests/wp05-key", None, None).unwrap();
    assert_eq!(rec["request"]["status"], json!("completed"));
    assert_eq!(rec["request"]["task_id"], json!(t1));
    // conflicting body → 409
    let mut conflicting = body.as_object().unwrap().clone();
    conflicting.insert("goal".to_string(), json!("different"));
    let err = env.req(
        "POST",
        "/v1/tasks",
        Some(&Value::Object(conflicting)),
        Some("wp05-key"),
    ).unwrap_err();
    assert_eq!(err.0, 409);
    // concurrent same-key: exactly one task
    let env2 = TestEnv::new();
    let body2 = json!({
        "goal": "concurrent submit (offline)",
        "repo_path": env2.user_repo.to_string_lossy(),
        "recipe": "direct",
        "allowed_adapters": ["fake"],
    });
    let base = env2.base();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let base = base.clone();
        let body2 = body2.clone();
        handles.push(std::thread::spawn(move || {
            ureq::post(&format!("{base}/v1/tasks"))
                .timeout(Duration::from_secs(60))
                .set("Authorization", "Bearer test-token")
                .set("Idempotency-Key", "wp05-conc")
                .send_json(body2)
        }));
    }
    let mut tasks = std::collections::HashSet::new();
    for h in handles {
        let r = h.join().unwrap();
        if let Ok(resp) = r {
            let v: serde_json::Value = resp.into_json().unwrap();
            if let Some(t) = v["task"]["task_id"].as_str() {
                tasks.insert(t.to_string());
            }
        }
    }
    assert!(tasks.len() <= 1, "concurrent same-key submits created {} tasks", tasks.len());
}

// ---------- WP06: instance lock ----------

#[test]
fn wp06_second_instance_refused_before_session_writes() {
    let env = TestEnv::new();
    // serve a SECOND instance on the same managed root, different port
    let exe = env!("CARGO_BIN_EXE_wagglepost");
    assert!(std::path::Path::new(exe).exists(), "bin exists for spawn");
    let session_file = env.managed_root.join("cli-session.json");
    // the in-process server owns the root; a real serve run would have written this
    std::fs::write(
        &session_file,
        r#"{"port": 8790, "token": "first", "csrf": "first-csrf"}"#,
    )
    .unwrap();
    let before = std::fs::read_to_string(&session_file).unwrap();
    let out = std::process::Command::new(exe)
        .args(["serve", "--port", "8799"])
        .env("WAGGLEPOST_MANAGED_ROOT", env.managed_root.to_string_lossy().as_ref())
        .output()
        .expect("spawn second instance");
    let after = std::fs::read_to_string(&session_file).unwrap();
    assert!(
        !out.status.success(),
        "second instance on the same managed root must be refused"
    );
    assert_eq!(
        before, after,
        "second instance must not rewrite the first instance's cli-session.json"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("instance") || stderr.contains("already"),
        "refusal names the running instance: {stderr}"
    );
}

// ---------- WP07: whole-tree stop confirmation ----------

#[test]
fn wp07_collection_waits_for_child_process() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files", "spawn_child"],
            "files": {"t.txt": "tree test (simulated)\n"},
            "child_secs": 4,
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp07-1")).expect("dispatch");
    let start = std::time::Instant::now();
    let status = env.run_until(&task_id, &["needs_review", "interrupted"], Duration::from_secs(120));
    let elapsed = start.elapsed().as_secs();
    assert_eq!(status, "needs_review");
    assert!(
        elapsed >= 4,
        "collection must wait for the job tree to empty (child outlives root); took {elapsed}s"
    );
}

// ---------- WP08: UTF-8 safe truncation ----------

#[test]
fn wp08_multibyte_truncation_no_panic() {
    // direct unit probe of the claude adapter parse path with a multibyte input echo
    let mut adapter = wagglepost::adapters::claude::ClaudeAdapter::new("claude.exe".into(), "test".into());
    let big = "中".repeat(300); // 900 bytes — the 400-byte cut lands mid-char
    let line = json!({
        "type": "assistant",
        "message": {"content": [{"type": "tool_use", "name": "Read", "input": {"file_path": big}}]}
    });
    let events = adapter.parse_line(&line.to_string());
    assert!(!events.is_empty());
    let echo = events[0].payload["input"].as_str().unwrap().to_string();
    assert!(echo.as_bytes().len() <= 500, "echo bounded");
    assert!(echo.ends_with('…'), "multibyte cut keeps char boundary + ellipsis");
    // emoji (4-byte) boundary too
    let emoji_line = json!({
        "type": "assistant",
        "message": {"content": [{"type": "tool_use", "name": "Read", "input": {"file_path": "🦀".repeat(200)}}]}
    });
    let events2 = adapter.parse_line(&emoji_line.to_string());
    assert!(!events2.is_empty());
}
