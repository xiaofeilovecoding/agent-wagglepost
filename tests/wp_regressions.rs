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
        &[("Cookie", &format!("xwp_session={}", env.browser_token))],
    );
    assert!(r.is_err(), "wrong cookie name must not authenticate");
    // the CLI bearer token must not become a browser cookie
    let r = env.raw(
        "GET",
        &format!("/v1/tasks/{task_id}"),
        None,
        &[("Cookie", &format!("wp_session={}", env.token))],
    );
    assert!(r.is_err(), "CLI token must not authenticate as a browser session");
    // right cookie name authenticates
    let r = env.raw(
        "GET",
        &format!("/v1/tasks/{task_id}"),
        None,
        &[("Cookie", &format!("wp_session={}", env.browser_token))],
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
            let r = ureq::post(&format!("{base}/v1/session"))
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
    env.approve_current(&task_id, &hash).expect("approve");
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
fn wp02_configured_filter_neutralized_marker_absent() {
    let env = TestEnv::new();
    // repo config defines an external clean/smudge filter → neutralized at runtime:
    // the attacker command never runs and the journey completes on raw content
    let gitconfig = env.user_repo.join(".git").join("config");
    let mut cfg = std::fs::read_to_string(&gitconfig).unwrap();
    cfg.push_str("[filter \"evil\"]
	clean = ./evil-clean
	smudge = ./evil-smudge
	required = false
");
    std::fs::write(&gitconfig, cfg).unwrap();
    let marker = env.managed_root.parent().unwrap().join("evil-filter-marker");
    let _ = std::fs::remove_file(&marker);
    let cmd = format!("echo executed > \"{}\"; cat", marker.to_string_lossy().replace("\\", "/"));
    common::git(&env.user_repo, &["config", "filter.evil.clean", &cmd]);
    common::git(&env.user_repo, &["config", "filter.evil.smudge", &cmd]);
    std::fs::write(env.user_repo.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
    // hostile filter is active: stage through isolated git
    common::git(&env.user_repo, &["-c", "filter.evil.clean=exit 1", "-c", "filter.evil.smudge=exit 1", "add", "-A"]);
    common::git(&env.user_repo, &["-c", "filter.evil.clean=exit 1", "-c", "filter.evil.smudge=exit 1", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "attrs"]);
    let _ = std::fs::remove_file(&marker);
    let body = json!({
        "goal": "g", "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct", "allowed_adapters": ["fake"],
    });
    let r = env.req("POST", "/v1/tasks", Some(&body), Some("wp02-2"));
    assert!(r.is_err(), "repo with an external filter must be refused");
    assert_eq!(r.unwrap_err().1["error"]["code"], json!("repo_unsafe_git"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "evil filter command must never execute (refused at submit)");
}
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

// ---------- 0700: WP02 reopened — include.path filter neutralized via git semantics ----------

#[test]
fn wp02b_included_filter_repo_refused_fail_closed() {
    let env = TestEnv::new();
    let marker = env.managed_root.parent().unwrap().join("include-filter-marker");
    let _ = std::fs::remove_file(&marker);
    let include_cfg = env.user_repo.join("external-filter.conf");
    let exe = env!("CARGO_BIN_EXE_wagglepost").replace("\\", "/");
    // PROCESS driver (the 08:20 counterexample) via include.path
    let process_cmd = format!(
        "{exe} --verify WAGGLEPOST_NO_SUCH_REF_0700; echo executed > \"{}\"; cat",
        marker.to_string_lossy().replace("\\", "/")
    );
    common::git(
        &env.user_repo,
        &["config", "-f", include_cfg.to_string_lossy().as_ref(), "filter.waggleproof.process", &process_cmd],
    );
    common::git(
        &env.user_repo,
        &["config", "-f", include_cfg.to_string_lossy().as_ref(), "filter.waggleproof.clean", &process_cmd],
    );
    common::git(&env.user_repo, &["config", "--local", "include.path", include_cfg.to_string_lossy().as_ref()]);
    std::fs::write(env.user_repo.join(".gitattributes"), "*.txt filter=waggleproof
").unwrap();
    // NOTE: no staging here — the fail-closed refusal fires BEFORE the dirty check,
    // so no git operation that could trigger the process filter is ever reached.
    let _ = std::fs::remove_file(&marker);

    let body = json!({
        "goal": "g", "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct", "allowed_adapters": ["fake"],
    });
    let r = env.req("POST", "/v1/tasks", Some(&body), Some("wp02b-1"));
    assert!(r.is_err(), "include.path filter (process) must be refused at submit");
    assert_eq!(r.unwrap_err().1["error"]["code"], json!("repo_unsafe_git"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "process filter must never execute");

    // post-approval config change: a clean repo approved, then a filter appears → dispatch refuses
    // clear the refused repo's include first so t2's clean repo can be planned
    common::git(&env.user_repo, &["config", "--local", "--unset-all", "include.path"]);
    // the uncommitted .gitattributes + late config make the repo dirty → explicit base
    let head = common::git(&env.user_repo, &["rev-parse", "HEAD"]);
    let (_t2, head_hash) = (String::new(), head.trim().to_string());
    let body2 = json!({
        "goal": "append one demo line to lib.txt (offline simulated)",
        "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct",
        "allowed_adapters": ["fake"],
        "base_commit": head_hash,
    });
    let resp2 = env.req("POST", "/v1/tasks", Some(&body2), Some("wp02b-t2")).expect("submit t2");
    let t2 = resp2["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{t2}/trust"), Some(&json!({})), None).expect("trust");
    let plan2 = env.req("POST", &format!("/v1/tasks/{t2}/plan"), Some(&json!({})), None).expect("plan");
    let hash2 = plan2["plan"]["summary_hash"].as_str().unwrap().to_string();
    env.approve_current(&t2, &hash2).expect("approve");
    let _ = body2;
    let _ = (&t2, &head_hash);
    // the late filter appears AFTER approval → dispatch refuses (fail closed)
    let marker2 = env.managed_root.parent().unwrap().join("late-filter-marker");
    let _ = std::fs::remove_file(&marker2);
    let late_inc = env.user_repo.join("late-filter.conf");
    let late_cmd = format!(
        "echo executed > \"{}\"; cat",
        marker2.to_string_lossy().replace("\\", "/")
    );
    common::git(&env.user_repo, &["config", "-f", late_inc.to_string_lossy().as_ref(), "filter.late.process", &late_cmd]);
    common::git(&env.user_repo, &["config", "--local", "include.path", late_inc.to_string_lossy().as_ref()]);
    let err = env.req(
        "POST",
        &format!("/v1/tasks/{t2}/dispatch"),
        Some(&json!({})),
        Some("wp02b-2"),
    ).unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("repo_unsafe_git"), "post-approval filter additions are refused at dispatch");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker2.exists(), "late filter must never execute");
}

// ---------- 0700: WP04 reopened — export failure must not fake acceptance ----------

#[test]
fn wp04b_export_failure_rejects_accept_and_recovers() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content\nexport probe (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp04b-1")).expect("dispatch");
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
    // pre-create the export target as a DIRECTORY so the patch write fails
    let export_dir = env
        .managed_root
        .join("exports")
        .join(format!("{}-{}.patch", task_id, &cand[..8]));
    std::fs::create_dir_all(&export_dir).unwrap();
    let err = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/accept"),
        Some(&json!({"candidate_hash": cand})),
        Some("wp04b-acc1"),
    ).unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("patch_export_failed"), "export failure must not fake acceptance");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["task"]["status"], json!("needs_review"), "task stays needs_review after failed export");
    // remove the obstruction → accept succeeds with a real patch
    std::fs::remove_dir_all(&export_dir).unwrap();
    let ok = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/accept"),
        Some(&json!({"candidate_hash": cand})),
        Some("wp04b-acc2"),
    ).expect("accept succeeds after the path is fixed");
    assert_eq!(ok["accepted"], json!(true));
    assert!(ok["patch"].as_str().map(|p| !p.is_empty()).unwrap_or(false), "patch path present");
    assert!(std::path::Path::new(ok["patch"].as_str().unwrap()).exists(), "patch file really written");
}

// ---------- 0700: WP09 — approval binds handoff digest + adapter versions ----------

#[test]
fn wp09_tampered_digest_rejected_and_dispatch_verifies_binding() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task_unapproved(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content\nbinding probe (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    let prev = env.req("GET", &format!("/v1/tasks/{task_id}/handoff-preview"), None, None).unwrap();
    let real_digest = prev["approved_digest"].as_str().unwrap().to_string();
    // tampered digest → approve refused
    let tampered = TestEnv::tamper_digest(&real_digest);
    let err = env.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{task_id}/plan/approve"),
        Some(&json!({"summary_hash": env.plan_hash(&task_id), "handoff_digest": tampered})),
        Some("wp09-tamper"),
        env.revision_of(&task_id),
    ).unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("handoff_digest_mismatch"), "tampered digest must not approve");
    // correct digest → approved and STORED
    env.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{task_id}/plan/approve"),
        Some(&json!({"summary_hash": env.plan_hash(&task_id), "handoff_digest": real_digest})),
        Some("wp09-ok"),
        env.revision_of(&task_id),
    ).expect("approve with the previewed digest");
    let plan = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap()["plan"].clone();
    assert_eq!(plan["approved_handoff_digest"], json!(real_digest), "approval stores the body digest");
    // dispatch verifies the binding: corrupt the stored digest → dispatch refuses, no process
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    env.core.store.set_plan_approved_digest(&plan_id, "corrupted-digest");
    let err2 = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("wp09-disp"),
    ).unwrap_err();
    assert_eq!(err2.1["error"]["code"], json!("handoff_not_approved"), "dispatch refuses a body that differs from the approved digest");
    // restore → dispatch proceeds and the attempt binds the approved digest
    env.core.store.set_plan_approved_digest(&plan_id, &real_digest);
    let disp = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("wp09-disp2"),
    ).expect("dispatch with the approved digest");
    let attempt_id = disp["receipt"]["attempt_id"].as_str().unwrap().to_string();
    let attempt = env.req("GET", &format!("/v1/attempts/{attempt_id}"), None, None).unwrap();
    assert_eq!(attempt["attempt"]["handoff_digest"], json!(real_digest));
    let plan = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap()["plan"].clone();
    assert!(plan["approved_versions"].is_object(), "adapter versions bound at approval");
}

// ---------- 0700: WP10 — reroute discloses and validates review budget ----------

#[test]
fn wp10_reroute_budget_disclosure_and_no_extra_launch() {
    let env = TestEnv::new();
    // default 2+1 budget: write_then_review used 2 launches; reroute needs writer+review (2)
    let (task_id, _) = env.ready_task(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content\nA (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    // the review must find issues so the reroute is ELIGIBLE (then the budget check fires)
    env.set_fake_script(&task_id, "s2", &json!({
        "behaviors": [],
        "result": {"status": "done", "summary": "review found issues: budget probe (simulated)"}
    }));
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp10-1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    let start = std::time::Instant::now();
    loop {
        let _ = env.req("POST", "/v1/__tick", Some(&json!({})), None);
        let s = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        if !s["reviews"].as_array().unwrap().is_empty() { break; }
        assert!(start.elapsed() < Duration::from_secs(60), "review never recorded");
        std::thread::sleep(Duration::from_millis(200));
    }
    let dbg = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(dbg["reviews"][0]["verdict"], json!("issues"), "issues verdict needed for eligibility");
    println!("[w10] status={} launches={}", dbg["task"]["status"], dbg["task"]["launches_used"]);
    let digest = env.relay_digest(&task_id, None, None);
    let err = env.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{task_id}/reroute"),
        Some(&json!({"target_adapter": "fake", "handoff_digest": digest})),
        Some("wp10-rr"),
        env.revision_of(&task_id),
    ).unwrap_err();
    println!("[w10] reroute error: {}", err.1["error"]);
    assert_eq!(err.1["error"]["code"], json!("budget_exhausted"));
    let msg = err.1["error"]["message"].as_str().unwrap().to_string();
    assert!(msg.contains('2'), "disclosure names the required launch count: {msg}");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 2, "no process launched after the refused reroute");
    // with an explicitly larger planned budget the reroute completes within budget
    let (t2, _) = env.ready_task_with_budget(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content\nA2 (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
        4,
    );
    env.set_fake_script(&t2, "s2", &json!({
        "behaviors": [],
        "result": {"status": "done", "summary": "review found issues: budget probe 2 (simulated)"}
    }));
    env.req("POST", &format!("/v1/tasks/{t2}/dispatch"), Some(&json!({})), Some("wp10-2")).expect("dispatch");
    let _ = env.run_until(&t2, &["needs_review"], Duration::from_secs(60));
    let start = std::time::Instant::now();
    loop {
        let _ = env.req("POST", "/v1/__tick", Some(&json!({})), None);
        let s = env.req("GET", &format!("/v1/tasks/{t2}"), None, None).unwrap();
        if !s["reviews"].as_array().unwrap().is_empty() { break; }
        assert!(start.elapsed() < Duration::from_secs(60), "review never recorded");
        std::thread::sleep(Duration::from_millis(200));
    }
    env.reroute_queued(&t2, "fake", "wp10-rr2");
    env.set_fake_script(&t2, "s1", &json!({
        "behaviors": ["write_files"],
        "files": {"lib.txt": "baseline content\nA2 (simulated)\nB2 (simulated)\n"},
        "result": {"status": "done", "summary": "done (simulated)"}
    }));
    let _ = env.run_until(&t2, &["needs_review"], Duration::from_secs(90));
    let snap = env.req("GET", &format!("/v1/tasks/{t2}"), None, None).unwrap();
    let launches = snap["task"]["launches_used"].as_u64().unwrap();
    assert!(launches <= 5, "launches stay within the approved budget: {launches}");
}
