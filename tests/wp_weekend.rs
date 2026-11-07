//! Weekend-review regressions: WP02 textconv + WP09 dispatch authorization.
mod common;

use common::TestEnv;
use serde_json::json;
use std::time::Duration;

// ---------- WP02 weekend: external textconv must not execute in collection ----------

#[test]
fn wp02c_textconv_disabled_in_collection_and_preview() {
    let env = TestEnv::new();
    let marker = env.managed_root.parent().unwrap().join("textconv-marker");
    let _ = std::fs::remove_file(&marker);
    let exe = env!("CARGO_BIN_EXE_wagglepost").replace('\\', "/");
    // diff.<driver>.textconv writes the marker and emits the raw content
    let conv_cmd = format!(
        "echo executed > \"{}\"; cat",
        marker.to_string_lossy().replace('\\', "/")
    );
    common::git(&env.user_repo, &["config", "diff.waggletext.textconv", &conv_cmd]);
    std::fs::write(env.user_repo.join(".gitattributes"), "*.txt diff=waggletext\n").unwrap();
    common::git(&env.user_repo, &["add", "-A"]);
    common::git(&env.user_repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "attrs"]);
    let _ = std::fs::remove_file(&marker);

    let (task_id, _) = env.ready_task(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content\nin-scope edit (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("tc-1")).expect("dispatch");
    let status = env.run_until(&task_id, &["needs_review"], Duration::from_secs(90));
    assert_eq!(status, "needs_review");
    std::thread::sleep(Duration::from_millis(400));
    assert!(!marker.exists(), "textconv must not execute during collection/patch/preview");

    // the candidate patch still carries the real text change (not replaced by a
    // textconv-transformed or binary-only view)
    let diff = env.req("GET", &format!("/v1/tasks/{task_id}/diff"), None, None).unwrap();
    let patch = diff["diff"].as_str().unwrap();
    assert!(patch.contains("in-scope edit (simulated)"), "normal text diff preserved:\n{patch}");
    // handoff preview must not trigger it either
    env.req("GET", &format!("/v1/tasks/{task_id}/handoff-preview?role=reviewer"), None, None).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "textconv must not execute in previews");
}

// ---------- WP09 weekend: dispatch authorization for resume/reroute/reviewer ----------

#[test]
fn wp09b_answer_without_digest_saves_but_never_queues() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"p.txt": "partial (simulated)\n"},
            "result": {
                "status": "needs_decision",
                "question": "go?",
                "options": [{"id": "opt_go", "label": "go", "effect": "go"}]
            }
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("d1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["waiting_decision"], Duration::from_secs(60));
    let decisions = env.req("GET", "/v1/decisions?status=open", None, None).unwrap();
    let card = &decisions["decisions"].as_array().unwrap()[0];
    // answer WITHOUT the body digest: the choice is saved, but nothing queues
    let ans = env.req(
        "POST",
        &format!("/v1/decisions/{}/answer", card["decision_id"].as_str().unwrap()),
        Some(&json!({"option_id": "opt_go", "revision": card["revision"].as_u64().unwrap()})),
        Some("wp09b-ans"),
    ).unwrap_err();
    assert_eq!(ans.1["error"]["code"], json!("handoff_digest_required"), "unconfirmed body must not queue");
    // duplicate submit with the same key: still refused (no digest), no launch
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 1, "no second launch without an approved body");
    assert_eq!(snap["task"]["status"], json!("waiting_decision"));
}

#[test]
fn wp09c_wrong_resume_digest_refused_without_cost() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"p.txt": "partial (simulated)\n"},
            "result": {
                "status": "needs_decision",
                "question": "go?",
                "options": [{"id": "opt_go", "label": "go", "effect": "go"}]
            }
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("d1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["waiting_decision"], Duration::from_secs(60));
    let decisions = env.req("GET", "/v1/decisions?status=open", None, None).unwrap();
    let card = &decisions["decisions"].as_array().unwrap()[0];
    // preview the relay body the server will actually send (with this choice applied)
    let did = card["decision_id"].as_str().unwrap().to_string();
    let prev = env.req(
        "GET",
        &format!("/v1/tasks/{task_id}/handoff-preview?decision={did}&option=opt_go"),
        None,
        None,
    ).unwrap();
    let real_digest = prev["approved_digest"].as_str().unwrap().to_string();
    // wrong digest → refused, no queue, no launch, budget untouched
    let tampered = format!("{}0", &real_digest[..63]);
    let err = env.req(
        "POST",
        &format!("/v1/decisions/{}/answer", card["decision_id"].as_str().unwrap()),
        Some(&json!({"option_id": "opt_go", "revision": card["revision"].as_u64().unwrap(), "handoff_digest": tampered})),
        Some("wp09c-ans"),
    ).unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("handoff_digest_mismatch"));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 1, "no launch on wrong digest");
    assert_eq!(snap["task"]["resume_used"], json!(0), "refused resume does not consume budget");
    // correct digest → queued, and the launched attempt binds the approved digest
    let ans = env.req(
        "POST",
        &format!("/v1/decisions/{}/answer", card["decision_id"].as_str().unwrap()),
        Some(&json!({"option_id": "opt_go", "revision": card["revision"].as_u64().unwrap(), "handoff_digest": real_digest})),
        Some("wp09c-ans2"),
    ).expect("answer with the previewed digest");
    assert_eq!(ans["queued"], json!(true));
    let status = env.run_until(&task_id, &["needs_review", "interrupted"], Duration::from_secs(90));
    assert_eq!(status, "needs_review");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let attempts = snap["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "resume launched exactly once");
    assert_eq!(snap["task"]["resume_used"], json!(1));
}

#[test]
fn wp09d_reroute_requires_and_verifies_digest() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content\npartial (simulated)\n"},
            "result": {"status": "blocked", "blocked_reason": "simulated quota probe"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("rr-d1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["interrupted"], Duration::from_secs(60));
    // reroute WITHOUT digest → refused, nothing queued
    let err = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/reroute"),
        Some(&json!({"target_adapter": "fake"})),
        Some("rr-nd"),
    ).unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("handoff_digest_required"), "unconfirmed relay body must not queue");
    // wrong digest → refused
    let prev = env.req("GET", &format!("/v1/tasks/{task_id}/handoff-preview"), None, None).unwrap();
    let real_digest = prev["approved_digest"].as_str().unwrap().to_string();
    let err2 = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/reroute"),
        Some(&json!({"target_adapter": "fake", "handoff_digest": format!("{}0", &real_digest[..63])})),
        Some("rr-wd"),
    ).unwrap_err();
    assert_eq!(err2.1["error"]["code"], json!("handoff_digest_mismatch"));
    // correct digest → queued; the relay attempt binds the approved digest
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/reroute"),
        Some(&json!({"target_adapter": "fake", "handoff_digest": real_digest})),
        Some("rr-ok"),
    ).expect("reroute with the previewed digest");
    let _ = env.run_until(&task_id, &["needs_review", "interrupted"], Duration::from_secs(90));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let attempts = snap["attempts"].as_array().unwrap();
    let relay = attempts.iter().find(|a| a["generation"].as_u64() == Some(2)).expect("relay attempt");
    assert_eq!(relay["handoff_digest"], json!(real_digest), "relay body bound to the approved digest");
}

#[test]
fn wp09e_reviewer_body_bound_at_freeze() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content\nreview binding probe (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("rb-1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    // the reviewer body was assembled and bound when the candidate froze; the launched
    // reviewer attempt must carry exactly that digest
    let start = std::time::Instant::now();
    loop {
        let _ = env.req("POST", "/v1/__tick", Some(&json!({})), None);
        let s = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        if !s["reviews"].as_array().unwrap().is_empty() { break; }
        assert!(start.elapsed() < Duration::from_secs(60), "review never recorded");
        std::thread::sleep(Duration::from_millis(200));
    }
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let reviewer_attempt = snap["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["role"] == json!("reviewer"))
        .expect("reviewer attempt");
    let bound = env.core.store.meta_get(&format!("approved_reviewer_digest:{}", task_id));
    assert_eq!(
        reviewer_attempt["handoff_digest"].as_str(),
        bound.as_deref(),
        "reviewer launched with the frozen approved body"
    );
}
// ---------- 午后复审: WP13 — demo presets server-side, fake write boundary ----------

#[test]
fn wp13_sample_script_endpoint_removed_and_demo_presets_server_side() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);
    // the script-upload endpoint is GONE (404/405) — arbitrary scripts cannot cross HTTP
    let r = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/sample-script"),
        Some(&json!({"step": "s1", "script": {"behaviors": ["write_files"], "files": {"x.txt": "x"}}})),
        Some("wp13-pin"),
    );
    assert!(
        r.is_err(),
        "the sample-script upload endpoint must not exist"
    );
    let code = r.unwrap_err().0;
    assert!(code == 404 || code == 405, "gone or method-not-allowed, got {code}");
    // demo presets are server-side: submitting with demo="decision" runs the built-in
    // decision preset WITHOUT any script content crossing HTTP
    let body = json!({
        "goal": "demo decision preset probe",
        "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct",
        "allowed_adapters": ["fake"],
        "demo": "decision",
    });
    let resp = env.req("POST", "/v1/tasks", Some(&body), Some("wp13-demo")).expect("demo submit");
    let tid = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{tid}/trust"), Some(&json!({})), None).expect("trust");
    let plan = env.req("POST", &format!("/v1/tasks/{tid}/plan"), Some(&json!({})), None).expect("plan");
    env.req("POST", &format!("/v1/tasks/{tid}/plan/approve"), Some(&json!({"summary_hash": plan["plan"]["summary_hash"]})), None).expect("approve");
    env.req("POST", &format!("/v1/tasks/{tid}/dispatch"), Some(&json!({})), Some("wp13-d1")).expect("dispatch");
    let status = env.run_until(&tid, &["waiting_decision"], Duration::from_secs(60));
    assert_eq!(status, "waiting_decision", "the built-in decision preset ran");
}

#[test]
fn wp13_demo_preset_requires_all_fake_adapters() {
    let env = TestEnv::new();
    let body = json!({
        "goal": "g", "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct", "allowed_adapters": ["fake", "codex"], "demo": "decision",
    });
    let err = env.req("POST", "/v1/tasks", Some(&body), Some("wp13-mixed")).unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("demo_requires_fake"), "demo presets are fake-only");
}

// black-box: the REAL fake-agent binary rejects out-of-workspace write paths
fn run_fake_with_files(cwd: &std::path::Path, files_json: &str) -> std::process::Output {
    let exe = env!("CARGO_BIN_EXE_wagglepost");
    let script_path = cwd.join("_wp13_script.json");
    std::fs::write(
        &script_path,
        format!("{{\"behaviors\": [\"write_files\"], \"files\": {files_json}}}"),
    )
    .unwrap();
    let capture = cwd.join("_wp13_capture.txt");
    let out = std::process::Command::new(exe)
        .args([
            "__fake-agent",
            "--script",
            script_path.to_string_lossy().as_ref(),
            "--capture",
            capture.to_string_lossy().as_ref(),
        ])
        .current_dir(cwd)
        .output()
        .expect("spawn fake agent");
    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_file(&capture);
    out
}

#[test]
fn wp13_fake_rejects_absolute_paths() {
    let env = TestEnv::new();
    let outside = env.managed_root.parent().unwrap().join("wp13-outside-marker.txt");
    let _ = std::fs::remove_file(&outside);
    let outside_str = outside.to_string_lossy().replace('\\', "/");
    let out = run_fake_with_files(
        &env.user_repo,
        &format!("{{\"{outside_str}\": \"marker content\"}}"),
    );
    assert!(
        !out.status.success(),
        "fake agent must fail on absolute write paths"
    );
    assert!(
        !outside.exists(),
        "marker outside the workspace must not be written"
    );
}

#[test]
fn wp13_fake_rejects_traversal_paths() {
    let env = TestEnv::new();
    let outside = env
        .managed_root
        .parent()
        .unwrap()
        .join("wp13-traversal-marker.txt");
    let _ = std::fs::remove_file(&outside);
    let outside_str = outside.to_string_lossy().replace('\\', "/");
    // traversal from the worktree: enough .. to reach above the managed root
    let out = run_fake_with_files(
        &env.user_repo,
        &format!("{{\"../../../../../{outside_str}\": \"marker content\"}}"),
    );
    assert!(!out.status.success(), "fake agent must fail on parent traversal");
    assert!(
        !outside.exists(),
        "marker above the workspace must not be written"
    );
}

#[test]
fn wp13_fake_rejects_drive_relative_paths() {
    let env = TestEnv::new();
    let marker = env.managed_root.parent().unwrap().join("wp13-drive-marker.txt");
    let _ = std::fs::remove_file(&marker);
    // drive-relative path "D:wp13-drive-marker.txt" resolves against the D: cwd
    let out = run_fake_with_files(
        &env.user_repo,
        &format!("{{\"D:{}\": \"marker content\"}}", "wp13-drive-marker.txt"),
    );
    assert!(!out.status.success(), "fake agent must fail on drive-relative paths");
    assert!(
        !marker.exists(),
        "drive-relative marker must not be written"
    );
}

// ---------- 午后复审: WP12 — asset content digest verification ----------

#[test]
fn wp12_manifest_detects_content_change_and_missing_files() {
    let dir = std::env::temp_dir().join(format!("wp12-{}", wagglepost::ids::new_token()));
    std::fs::create_dir_all(&dir).unwrap();
    // valid assets + manifest
    std::fs::write(dir.join("app.js"), "console.log(1);").unwrap();
    let digest = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(std::fs::read(dir.join("app.js")).unwrap());
        hex::encode(h.finalize())
    };
    std::fs::write(dir.join("ASSETS.sha"), format!("{digest}  app.js\n")).unwrap();
    // matching manifest passes
    assert!(wagglepost::asset_guard::verify_manifest(&dir).is_ok());
    // content change → mismatch
    std::fs::write(dir.join("app.js"), "console.log(2);").unwrap();
    assert!(
        wagglepost::asset_guard::verify_manifest(&dir).is_err(),
        "replaced app.js must fail the manifest check"
    );
    // missing file → fail
    std::fs::remove_file(dir.join("app.js")).unwrap();
    assert!(wagglepost::asset_guard::verify_manifest(&dir).is_err(), "missing asset fails");
    // missing manifest → fail
    std::fs::remove_file(dir.join("ASSETS.sha")).unwrap();
    std::fs::write(dir.join("app.js"), "x").unwrap();
    assert!(wagglepost::asset_guard::verify_manifest(&dir).is_err(), "missing manifest fails");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wp12_touching_manifest_time_cannot_mask_content() {
    // the 0700 gap: touching ASSETS.sha's mtime alone must not mask a stale bundle.
    // The content digest check is mtime-independent: after touching the manifest, a
    // replaced app.js STILL fails the content verification.
    let dir = std::env::temp_dir().join(format!("wp12t-{}", wagglepost::ids::new_token()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.js"), "original").unwrap();
    let digest = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(std::fs::read(dir.join("app.js")).unwrap());
        hex::encode(h.finalize())
    };
    std::fs::write(dir.join("ASSETS.sha"), format!("{digest}  app.js\n")).unwrap();
    // touch the manifest (bump mtime)
    let md = std::fs::metadata(dir.join("ASSETS.sha")).unwrap();
    let _ = md;
    std::fs::write(dir.join("ASSETS.sha"), format!("{digest}  app.js\n")).unwrap();
    // replace app.js WITHOUT updating the manifest → content mismatch detected
    std::fs::write(dir.join("app.js"), "tampered").unwrap();
    assert!(
        wagglepost::asset_guard::verify_manifest(&dir).is_err(),
        "touching the manifest time cannot mask a replaced app.js"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------- 午后复审: WP09 CLI — explicit preview/confirm, black-box ----------

fn write_cli_session(env: &TestEnv) {
    let session = json!({"port": env.port, "token": env.token, "csrf": env.csrf});
    std::fs::write(
        env.managed_root.join("cli-session.json"),
        session.to_string(),
    )
    .unwrap();
}

fn run_cli(env: &TestEnv, args: &[&str]) -> std::process::Output {
    let exe = env!("CARGO_BIN_EXE_wagglepost");
    std::process::Command::new(exe)
        .args(args)
        .env("WAGGLEPOST_MANAGED_ROOT", env.managed_root.to_string_lossy().as_ref())
        .output()
        .expect("spawn cli")
}

#[test]
fn wp09_cli_answer_preview_then_confirm_black_box() {
    let env = TestEnv::new();
    write_cli_session(&env);
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"p.txt": "partial (simulated)\n"},
            "result": {
                "status": "needs_decision",
                "question": "go?",
                "options": [{"id": "opt_go", "label": "go", "effect": "g"}]
            }
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("cli-d1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["waiting_decision"], Duration::from_secs(60));
    let exe = env!("CARGO_BIN_EXE_wagglepost");

    // find the open decision
    let decisions = env.req("GET", "/v1/decisions?status=open", None, None).unwrap();
    let card = &decisions["decisions"].as_array().unwrap()[0];
    let decision_id = card["decision_id"].as_str().unwrap().to_string();
    // preview the body via HTTP (what the CLI prints)
    let prev = env.req(
        "GET",
        &format!("/v1/tasks/{task_id}/handoff-preview?decision={decision_id}&option=opt_go"),
        None,
        None,
    ).unwrap();
    let real_digest = prev["approved_digest"].as_str().unwrap().to_string();

    // step 1 (black-box): the REAL CLI binary without --digest → preview only,
    // exit 0, NOTHING changes (no attempt, no budget, no queue)
    let out = std::process::Command::new(exe)
        .args(["answer", &decision_id, "opt_go"])
        .env("WAGGLEPOST_MANAGED_ROOT", env.managed_root.to_string_lossy().as_ref())
        .output()
        .expect("spawn cli preview");
    assert!(out.status.success(), "preview exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("relay body"), "preview prints the body");
    assert!(stdout.contains("digest:"), "preview prints the digest");
    assert!(stdout.contains("re-run with --digest"), "preview instructs the confirm step");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 1, "preview does NOT launch");
    assert_eq!(snap["task"]["resume_used"], json!(0), "preview does not consume budget");

    // step 2 (black-box): the REAL CLI binary with --digest <wrong> → refused
    let out = std::process::Command::new(exe)
        .args(["answer", &decision_id, "opt_go", "--digest", "wrong-digest"])
        .env("WAGGLEPOST_MANAGED_ROOT", env.managed_root.to_string_lossy().as_ref())
        .output()
        .expect("spawn cli");
    assert!(
        !out.status.success(),
        "wrong digest must be refused by the CLI"
    );
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 1, "no launch on wrong digest");
    assert_eq!(snap["task"]["resume_used"], json!(0), "no budget consumed");

    // step 3 (black-box): the REAL CLI binary with --digest <correct> → queues once
    let out = std::process::Command::new(exe)
        .args(["answer", &decision_id, "opt_go", "--digest", &real_digest])
        .env("WAGGLEPOST_MANAGED_ROOT", env.managed_root.to_string_lossy().as_ref())
        .output()
        .expect("spawn cli");
    assert!(out.status.success(), "confirm with the correct digest queues");
    let status = env.run_until(&task_id, &["needs_review", "interrupted"], Duration::from_secs(90));
    assert_eq!(status, "needs_review", "exactly one resume launch");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 2, "exactly 2 attempts total");
    assert_eq!(snap["task"]["resume_used"], json!(1));
}
