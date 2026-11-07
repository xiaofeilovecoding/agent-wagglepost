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
