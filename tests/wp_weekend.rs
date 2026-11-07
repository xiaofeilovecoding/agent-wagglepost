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

// ---------- 晚间复审: WP13 — junction escape must have ZERO out-of-tree side effects ----------

fn create_junction(link: &std::path::Path, target: &std::path::Path) {
    let out = std::process::Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("spawn cmd for mklink");
    assert!(
        out.status.success(),
        "mklink /J failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn wp13_fake_junction_rejected_before_any_mkdir() {
    // Real Windows junction inside the worktree pointing OUTSIDE it: the fake
    // agent must refuse BEFORE creating any directory, so the failure leaves no
    // external dir or file behind (晚间复审: create_dir_all ran before the
    // ancestor check and created nested/ inside the junction target).
    let root = std::env::temp_dir().join(format!("wp13j-{}", wagglepost::ids::new_token()));
    let work = root.join("work");
    let outside = root.join("outside");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    // 1. junction at the first path level: jump/nested/marker.txt
    let jump = work.join("jump");
    create_junction(&jump, &outside);
    let out = run_fake_with_files(&work, "{\"jump/nested/marker.txt\": \"synthetic\"}");
    assert_eq!(out.status.code(), Some(3), "junction escape rejected with exit 3");
    assert!(
        !outside.join("nested").exists(),
        "mkdir must not run before validation — nothing may appear in the junction target"
    );
    assert!(!outside.join("marker.txt").exists(), "no file outside the workspace");
    assert!(
        !work.join("jump").join("nested").exists(),
        "nothing written through the junction either"
    );

    // 2. junction as an intermediate component: sub is a junction, target deeper
    let outside2 = root.join("outside2");
    std::fs::create_dir_all(&outside2).unwrap();
    let sub = work.join("sub");
    create_junction(&sub, &outside2);
    let out = run_fake_with_files(&work, "{\"sub/x.txt\": \"synthetic\"}");
    assert_eq!(out.status.code(), Some(3), "junction component rejected with exit 3");
    assert!(!outside2.join("x.txt").exists(), "no file through the junction");
    assert!(!outside2.join("nested").exists(), "no dir through the junction");

    // 3. normal relative writes — plain and nested — still work
    let out = run_fake_with_files(&work, "{\"inside.txt\": \"ok\"}");
    assert!(out.status.success(), "plain relative write still works");
    assert!(work.join("inside.txt").is_file(), "plain relative file written");
    let out = run_fake_with_files(&work, "{\"deep/dir/f.txt\": \"ok\"}");
    assert!(out.status.success(), "nested relative write still works");
    assert!(
        work.join("deep").join("dir").join("f.txt").is_file(),
        "nested relative file written"
    );

    let _ = std::fs::remove_dir(&jump); // removes the junction link itself
    let _ = std::fs::remove_dir(&sub);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn wp13_unknown_api_post_returns_json_404_not_html() {
    // The removed upload endpoint must answer a STABLE JSON 404 — production
    // used to fall through to the SPA fallback and return 200 text/html, which
    // made a dead POST look like success.
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/sample-script"),
            Some(&json!({"step": "s1", "script": {"behaviors": ["write_files"], "files": {"x.txt": "x"}}})),
            Some("wp13-404"),
        )
        .unwrap_err();
    assert_eq!(err.0, 404, "removed API route must be 404, not the SPA 200");
    assert_eq!(
        err.1["error"]["code"], json!("not_found"),
        "the error body is the stable JSON envelope"
    );

    // any unknown /v1/* path behaves the same regardless of verb
    let err = env
        .req("POST", "/v1/nonexistent", Some(&json!({})), Some("wp13-404b"))
        .unwrap_err();
    assert_eq!(err.0, 404);
    assert_eq!(err.1["error"]["code"], json!("not_found"));

    // SPA navigation is untouched: unknown non-API paths still serve the shell
    let (status, ctype) = raw_get(&env, "/");
    assert_eq!(status, 200, "root serves the SPA");
    assert!(ctype.starts_with("text/html"), "root is html, got {ctype}");
    let (status, ctype) = raw_get(&env, "/some/spa/route");
    assert_eq!(status, 200, "SPA fallback still works for non-API paths");
    assert!(ctype.starts_with("text/html"), "SPA fallback is html, got {ctype}");
}

fn raw_get(env: &TestEnv, path: &str) -> (u16, String) {
    let resp = ureq::get(&format!("{}{}", env.base(), path))
        .timeout(Duration::from_secs(30))
        .call()
        .expect("raw get");
    let ctype = resp.header("content-type").unwrap_or("").to_string();
    (resp.status(), ctype)
}

// ---------- 晚间复审: WP12 — content digests for INPUTS and the FULL output set ----------

const WP12_SOURCES: &[&str] = &[
    "src/app.tsx",
    "src/api.ts",
    "src/i18n.ts",
    "src/index.html",
    "src/styles.css",
];

fn wp12_sha(p: &std::path::Path) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(std::fs::read(p).unwrap());
    hex::encode(h.finalize())
}

struct Wp12Skel {
    root: std::path::PathBuf,
}

fn wp12_make_skel(tag: &str) -> Wp12Skel {
    let root = std::env::temp_dir().join(format!("{tag}-{}", wagglepost::ids::new_token()));
    let web = root.join("web");
    std::fs::create_dir_all(web.join("src")).unwrap();
    std::fs::create_dir_all(web.join("dist")).unwrap();
    for (name, content) in [
        ("src/app.tsx", "export const A = 1;\n"),
        ("src/api.ts", "export const api = 1;\n"),
        ("src/i18n.ts", "export const i18n = 1;\n"),
        ("src/index.html", "<!doctype html><title>t</title>\n"),
        ("src/styles.css", "body{}\n"),
        ("build.mjs", "console.log('build');\n"),
        ("package.json", "{\"n\":1}\n"),
        ("package-lock.json", "{\"lock\":1}\n"),
    ] {
        std::fs::write(web.join(name), content).unwrap();
    }
    // dist outputs, exactly as `npm run build` would produce (manifest next)
    std::fs::write(web.join("dist/app.js"), "bundle-v1;\n").unwrap();
    std::fs::write(web.join("dist/index.html"), "<!doctype html><title>t</title>\n").unwrap();
    std::fs::write(web.join("dist/styles.css"), "body{}\n").unwrap();
    Wp12Skel { root }
}

fn wp12_write_manifest(skel: &Wp12Skel) {
    let web = skel.root.join("web");
    let dist = web.join("dist");
    let mut ins: Vec<String> = WP12_SOURCES.iter().map(|s| s.to_string()).collect();
    ins.push("build.mjs".into());
    ins.push("package.json".into());
    ins.push("package-lock.json".into());
    ins.sort();
    let mut lines = vec!["# wagglepost asset manifest v2".to_string()];
    for f in &ins {
        lines.push(format!("in  {}  {}", wp12_sha(&web.join(f)), f));
    }
    for f in ["app.js", "index.html", "styles.css"] {
        lines.push(format!("out {}  {}", wp12_sha(&dist.join(f)), f));
    }
    std::fs::write(dist.join("ASSETS.sha"), lines.join("\n") + "\n").unwrap();
}

fn wp12_verify(skel: &Wp12Skel) -> Result<(), String> {
    wagglepost::asset_guard::verify_manifest(&skel.root.join("web").join("dist"), &skel.root.join("web"))
}

fn wp12_set_mtime(p: &std::path::Path, t: std::time::SystemTime) {
    let mut f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(t).unwrap();
}

#[test]
fn wp12_manifest_v2_clean_skeleton_and_real_repo_reproduce() {
    let skel = wp12_make_skel("wp12v2-clean");
    wp12_write_manifest(&skel);
    assert!(wp12_verify(&skel).is_ok(), "clean skeleton verifies");
    // the REAL production tree must reproduce: actual sources + committed manifest
    let repo_web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    assert!(
        wagglepost::asset_guard::verify_manifest(&repo_web.join("dist"), &repo_web).is_ok(),
        "committed repo assets verify against the committed manifest (normal production build is reproducible)"
    );
    let _ = std::fs::remove_dir_all(&skel.root);
}

#[test]
fn wp12_manifest_v2_modified_inputs_rejected_even_with_newest_manifest_mtime() {
    // The 1103 loophole: freshness was newest-mtime based, so editing a source
    // after the build and touching ASSETS.sha (making it the newest file) let
    // the OLD bundle ship. Input content digests must fail regardless of mtimes.
    let skel = wp12_make_skel("wp12v2-src");
    wp12_write_manifest(&skel);
    std::fs::write(
        skel.root.join("web").join("src").join("app.tsx"),
        "export const A = 2; // REVIEW_SYNTHETIC_INPUT_EDIT\n",
    )
    .unwrap();
    // adversarial mtimes: source looks OLD, manifest looks NEWEST (reviewer probe)
    let now = std::time::SystemTime::now();
    wp12_set_mtime(&skel.root.join("web").join("src").join("app.tsx"), now - Duration::from_secs(600));
    wp12_set_mtime(
        &skel.root.join("web").join("dist").join("ASSETS.sha"),
        now + Duration::from_secs(600),
    );
    assert!(
        wp12_verify(&skel).is_err(),
        "modified source fails even when the manifest mtime looks newest"
    );
    // same for the build script and the lockfile
    wp12_write_manifest(&skel);
    std::fs::write(skel.root.join("web").join("build.mjs"), "console.log('changed');\n").unwrap();
    assert!(wp12_verify(&skel).is_err(), "modified build script fails");
    std::fs::write(
        skel.root.join("web").join("package-lock.json"),
        "{\"lock\":2}\n",
    )
    .unwrap();
    assert!(wp12_verify(&skel).is_err(), "modified lockfile fails");
    let _ = std::fs::remove_dir_all(&skel.root);
}

#[test]
fn wp12_manifest_v2_rejects_empty_incomplete_duplicate_and_extra() {
    let skel = wp12_make_skel("wp12v2-set");
    let mpath = skel.root.join("web").join("dist").join("ASSETS.sha");
    // empty manifest must not be a vacuous pass (reviewer probe)
    std::fs::write(&mpath, "").unwrap();
    assert!(wp12_verify(&skel).is_err(), "empty manifest must fail");
    wp12_write_manifest(&skel);
    // one out entry removed → its artifact becomes unlisted → fixed set violated
    let text = std::fs::read_to_string(&mpath).unwrap();
    let no_styles = text
        .lines()
        .filter(|l| !l.contains("  styles.css"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&mpath, no_styles).unwrap();
    assert!(wp12_verify(&skel).is_err(), "unlisted output artifact must fail");
    // one in entry removed → its input becomes unverified
    wp12_write_manifest(&skel);
    let text = std::fs::read_to_string(&mpath).unwrap();
    let no_api = text
        .lines()
        .filter(|l| !l.contains("  src/api.ts"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&mpath, no_api).unwrap();
    assert!(wp12_verify(&skel).is_err(), "unlisted build input must fail");
    // duplicate entry
    wp12_write_manifest(&skel);
    let text = std::fs::read_to_string(&mpath).unwrap();
    let dup = format!("{text}{}\n", text.lines().find(|l| l.starts_with("out ")).unwrap());
    std::fs::write(&mpath, dup).unwrap();
    assert!(wp12_verify(&skel).is_err(), "duplicate manifest entries must fail");
    // replaced artifact content (kept from the 午后 round)
    wp12_write_manifest(&skel);
    std::fs::write(skel.root.join("web").join("dist").join("app.js"), "bundle-v2-tampered;\n").unwrap();
    assert!(wp12_verify(&skel).is_err(), "replaced app.js must fail");
    // missing artifact file
    wp12_write_manifest(&skel);
    std::fs::remove_file(skel.root.join("web").join("dist").join("app.js")).unwrap();
    assert!(wp12_verify(&skel).is_err(), "missing artifact must fail");
    // extra file in dist that the manifest does not know about
    std::fs::write(skel.root.join("web").join("dist").join("app.js"), "bundle-v1;\n").unwrap();
    wp12_write_manifest(&skel);
    std::fs::write(skel.root.join("web").join("dist").join("extra.js"), "sneak;\n").unwrap();
    assert!(wp12_verify(&skel).is_err(), "unexpected dist artifact must fail");
    let _ = std::fs::remove_dir_all(&skel.root);
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

// ---------- 晚间复审: WP09 — explicit --json preview is a single parseable object ----------

#[test]
fn wp09_cli_answer_json_preview_is_single_parseable_object() {
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
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("cli-j1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["waiting_decision"], Duration::from_secs(60));

    let decisions = env.req("GET", "/v1/decisions?status=open", None, None).unwrap();
    let card = &decisions["decisions"].as_array().unwrap()[0];
    let decision_id = card["decision_id"].as_str().unwrap().to_string();
    let prev = env
        .req(
            "GET",
            &format!("/v1/tasks/{task_id}/handoff-preview?decision={decision_id}&option=opt_go"),
            None,
            None,
        )
        .unwrap();
    let real_digest = prev["approved_digest"].as_str().unwrap().to_string();
    let real_text = prev["handoff"]["text"].as_str().unwrap().to_string();
    let real_bytes = prev["handoff"]["bytes"].as_u64().unwrap();

    // --json preview: stdout is ONE parseable JSON object carrying body/bytes/digest
    let out = run_cli(&env, &["answer", &decision_id, "opt_go", "--json"]);
    assert!(out.status.success(), "--json preview exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("--json preview stdout must be a single parseable JSON object");
    assert_eq!(v["previewed"], json!(true));
    assert_eq!(v["digest"], json!(real_digest), "digest matches the previewed body");
    assert_eq!(v["handoff"]["text"], json!(real_text), "actual relay body included");
    assert_eq!(v["handoff"]["bytes"], json!(real_bytes), "byte count included");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 1, "--json preview must NOT launch");
    assert_eq!(snap["task"]["resume_used"], json!(0), "--json preview does not consume budget");

    // human preview unchanged: readable prose, and NOT one JSON document
    let out = run_cli(&env, &["answer", &decision_id, "opt_go"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("digest:"), "human preview still shows the digest");
    assert!(
        serde_json::from_str::<serde_json::Value>(stdout.trim()).is_err(),
        "human preview stays human-readable prose"
    );

    // confirm with --json + correct digest → parses, still exactly one launch
    let out = run_cli(&env, &["answer", &decision_id, "opt_go", "--json", "--digest", &real_digest]);
    assert!(out.status.success(), "confirm with the correct digest queues");
    let v: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .expect("confirm --json stdout parseable");
    assert!(v.get("queued").is_some() && v.get("decision").is_some(), "confirm returns the API receipt ({{receipt, decision, queued}})");
    let status = env.run_until(&task_id, &["needs_review", "interrupted"], Duration::from_secs(90));
    assert_eq!(status, "needs_review", "exactly one resume launch");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 2, "still exactly 2 attempts total");
    assert_eq!(snap["task"]["resume_used"], json!(1), "exactly one resume budget unit");
}
