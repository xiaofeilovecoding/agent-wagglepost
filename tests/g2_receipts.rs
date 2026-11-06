//! G2: decision-explainable routing + request identity/reconciliation.
mod common;

use common::TestEnv;
use serde_json::json;
use std::time::Duration;

#[test]
fn g2_idempotent_dispatch_same_key_replays() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);

    let first = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&json!({})),
            Some("same-key"),
        )
        .expect("first dispatch");
    let aid = first["receipt"]["attempt_id"].as_str().unwrap().to_string();

    // busy: the attempt is running → replay of the ORIGINAL result, not a second launch
    let second = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&json!({})),
            Some("same-key"),
        )
        .expect("replay");
    assert_eq!(second["replayed"], json!(true));
    assert_eq!(
        second["receipt"]["attempt_id"], json!(aid),
        "same request id maps to the original attempt"
    );
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 1, "no second launch");
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
}

#[test]
fn g2_conflicting_key_same_id_different_body() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("k1"),
    )
    .unwrap();
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&json!({"note": "different body"})),
            Some("k1"),
        )
        .unwrap_err();
    assert_eq!(err.0, 409, "same id + different digest → conflict");
}

#[test]
fn g2_route_reasons_recomputable() {
    let env = TestEnv::new();
    // untrusted repo → route preview shows repo_untrusted with bilingual text
    let body = json!({
        "goal": "g", "repo_path": env.user_repo.to_string_lossy(),
        "allowed_adapters": ["fake"],
    });
    let resp = env
        .req("POST", "/v1/tasks", Some(&body), Some("untrust-1"))
        .unwrap();
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None)
        .unwrap();
    let route = env
        .req("GET", &format!("/v1/tasks/{task_id}/route-preview"), None, None)
        .unwrap();
    let fake = route["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["adapter"] == "fake")
        .unwrap()
        .clone();
    assert_eq!(fake["ok"], json!(false));
    let codes: Vec<String> = fake["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reason_code"].as_str().unwrap().to_string())
        .collect();
    assert!(codes.contains(&"repo_untrusted".to_string()), "untrusted repo rejected with stable code: {codes:?}");
    assert!(
        route["inputs"]["repo_trusted"].as_bool() == Some(false),
        "inputs exposed so the UI can recompute"
    );

    // budget exhausted reason: submit with resume budget drained is hard to fake directly;
    // instead verify adapter_disabled via user allowlist
    let body2 = json!({
        "goal": "g2", "repo_path": env.user_repo.to_string_lossy(),
        "allowed_adapters": ["dsh"],
    });
    let resp2 = env.req("POST", "/v1/tasks", Some(&body2), Some("dsh-only")).unwrap();
    let t2 = resp2["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{t2}/trust"), Some(&json!({})), None)
        .unwrap();
    // planning with only dsh allowed fails closed (dsh not enabled in 0.1)
    let plan_err = env
        .req("POST", &format!("/v1/tasks/{t2}/plan"), Some(&json!({})), None)
        .unwrap_err();
    assert_eq!(plan_err.1["error"]["code"], json!("no_candidate"));
    let route2 = env
        .req("GET", &format!("/v1/tasks/{t2}/route-preview"), None, None)
        .unwrap();
    let codex = route2["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["adapter"] == "codex")
        .unwrap()
        .clone();
    let codes: Vec<String> = codex["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reason_code"].as_str().unwrap().to_string())
        .collect();
    assert!(codes.contains(&"adapter_disabled".to_string()));
}

#[test]
fn g2_request_reconciliation_honest() {
    let env = TestEnv::new();
    // unknown request id → 404 with honest guidance (absence is not proof of not-started)
    let err = env.req("GET", "/v1/requests/req-unknown", None, None).unwrap_err();
    assert_eq!(err.0, 404);
    // completed request replays its stored result
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);
    let first = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&json!({})),
            Some("rec-1"),
        )
        .unwrap();
    let rec = env.req("GET", "/v1/requests/rec-1", None, None).unwrap();
    assert_eq!(rec["reconciliation"], json!("completed"));
    assert_eq!(rec["request"]["status"], json!("completed"));
    assert_eq!(
        rec["request"]["attempt_id"],
        first["receipt"]["attempt_id"],
        "request bound to its attempt for reconciliation"
    );
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
}

#[test]
fn g2_busy_second_dispatch_rejected_not_queued() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("busy-1"),
    )
    .unwrap();
    // different id while slot busy → 429 retryable, no dangling pending job
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&json!({})),
            Some("busy-2"),
        )
        .unwrap_err();
    assert_eq!(err.0, 429, "busy is a retryable rejection");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 1);
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
}

#[test]
fn g2_untrusted_repo_cannot_dispatch() {
    let env = TestEnv::new();
    let body = json!({
        "goal": "g", "repo_path": env.user_repo.to_string_lossy(),
        "allowed_adapters": ["fake"],
    });
    let resp = env.req("POST", "/v1/tasks", Some(&body), Some("u1")).unwrap();
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None)
        .unwrap();
    let hash = env
        .req("GET", &format!("/v1/tasks/{task_id}"), None, None)
        .unwrap()["plan"]["summary_hash"]
        .as_str()
        .unwrap()
        .to_string();
    // approve without trust → rejected
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": hash})),
            None,
        )
        .unwrap_err();
    assert_eq!(err.0, 409);
    assert_eq!(err.1["error"]["code"], json!("repo_untrusted"));
}
