//! G4: security gates — host/origin/CSRF, subprocess containment (job object stop
//! confirmation), env canary filtering, same-repo concurrent launch.
mod common;

use common::TestEnv;
use serde_json::json;
use std::time::Duration;

#[test]
fn g4_bad_host_rejected() {
    let env = TestEnv::new();
    let err = env
        .raw(
            "GET",
            "/v1/health",
            None,
            &[("Authorization", "Bearer test-token"), ("Host", "evil.example.com")],
        )
        .unwrap_err();
    assert_eq!(err.0, 403, "DNS-rebinding host rejected");
}

#[test]
fn g4_bad_origin_rejected() {
    let env = TestEnv::new();
    let err = env
        .raw(
            "POST",
            "/v1/tasks",
            Some(&json!({"goal": "x", "repo_path": "C:\\nope"})),
            &[
                ("Authorization", "Bearer test-token"),
                ("X-CSRF-Token", "test-csrf"),
                ("Origin", "http://evil.example.com"),
            ],
        )
        .unwrap_err();
    assert_eq!(err.0, 403, "cross-origin mutation rejected");
}

#[test]
fn g4_missing_csrf_with_origin_rejected() {
    let env = TestEnv::new();
    let err = env
        .raw(
            "POST",
            "/v1/tasks",
            Some(&json!({"goal": "x", "repo_path": "C:\\nope"})),
            &[
                ("Authorization", "Bearer test-token"),
                ("Origin", &env.base()),
            ],
        )
        .unwrap_err();
    assert_eq!(err.0, 403, "cookie/origin caller without CSRF token rejected");
}

#[test]
fn g4_env_canary_not_inherited_by_agents() {
    // the runner env filter drops credential-looking and unrelated vars
    std::env::set_var("WAGGLEPOST_TEST_CANARY", "secret-canary-value");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "canary");
    std::env::set_var("GITHUB_TOKEN", "canary");
    let filtered = wagglepost::runner::filter_env(&[]);
    let joined = format!("{filtered:?}");
    assert!(!joined.contains("secret-canary-value"), "canary must be dropped");
    assert!(!joined.contains("AWS_SECRET_ACCESS_KEY"));
    assert!(!joined.contains("GITHUB_TOKEN"));
    let _ = std::env::remove_var("WAGGLEPOST_TEST_CANARY");
    let _ = std::env::remove_var("AWS_SECRET_ACCESS_KEY");
    let _ = std::env::remove_var("GITHUB_TOKEN");
}

#[test]
fn g4_hang_cancel_confirms_process_tree_exit() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["hang"],
            "hang_secs": 600,
        })),
    );
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("hang-1"),
    )
    .unwrap();
    // wait until running
    let start = std::time::Instant::now();
    let attempt_id = loop {
        let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        let attempts = snap["attempts"].as_array().unwrap();
        if attempts.len() == 1 && attempts[0]["status"] == json!("running") {
            break attempts[0]["attempt_id"].as_str().unwrap().to_string();
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("attempt never started running");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // cancel → stopping → cancelled with confirmed exit
    env.req(
        "POST",
        &format!("/v1/attempts/{attempt_id}/cancel"),
        Some(&json!({})),
        Some("cancel-1"),
    )
    .unwrap();
    let status = env.run_until(&task_id, &["cancelled"], Duration::from_secs(30));
    assert_eq!(status, "cancelled");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let exited = snap["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == json!("process_exited") || e["kind"] == json!("attempt_cancelled"))
        .expect("process exit evidence event")
        .clone();
    assert_eq!(
        exited["payload"]["stop_confirmed"],
        json!(true),
        "stop evidence: process tree exit confirmed through the job handle"
    );
    // slot freed: a second task can dispatch right away
    let (task_id2, _) = env.ready_task("direct", Some("fake"), None);
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id2}/dispatch"),
        Some(&json!({})),
        Some("after-cancel"),
    )
    .expect("slot is free after confirmed cancel");
    let _ = env.run_until(&task_id2, &["needs_review"], Duration::from_secs(60));
}

#[test]
fn g4_same_repo_second_writer_rejected() {
    let env = TestEnv::new();
    // task 1 running long (hang)
    let (t1, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({"behaviors": ["hang"], "hang_secs": 300})),
    );
    env.req(
        "POST",
        &format!("/v1/tasks/{t1}/dispatch"),
        Some(&json!({})),
        Some("w1"),
    )
    .unwrap();
    // task 2 on the SAME repo must be rejected while writer 1 is live
    let (t2, _) = env.ready_task("direct", Some("fake"), None);
    // wait until t1 is running
    let start = std::time::Instant::now();
    loop {
        let snap = env.req("GET", &format!("/v1/tasks/{t1}"), None, None).unwrap();
        let attempts = snap["attempts"].as_array().unwrap();
        if attempts.len() == 1 && attempts[0]["status"] == json!("running") {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("t1 never started");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{t2}/dispatch"),
            Some(&json!({})),
            Some("w2"),
        )
        .unwrap_err();
    assert_eq!(err.0, 429, "global slot is occupied");
    // clean up: cancel t1
    let snap = env.req("GET", &format!("/v1/tasks/{t1}"), None, None).unwrap();
    let aid = snap["attempts"][0]["attempt_id"].as_str().unwrap().to_string();
    env.req(
        "POST",
        &format!("/v1/attempts/{aid}/cancel"),
        Some(&json!({})),
        Some("c1"),
    )
    .unwrap();
    let _ = env.run_until(&t1, &["cancelled"], Duration::from_secs(30));
}

#[test]
fn g4_unconfirmed_repo_dirty_baseline_rejected() {
    let env = TestEnv::new();
    // dirty the user repo
    std::fs::write(env.user_repo.join("dirty.txt"), "uncommitted\n").unwrap();
    let body = json!({
        "goal": "g", "repo_path": env.user_repo.to_string_lossy(),
        "allowed_adapters": ["fake"],
    });
    let err = env.req("POST", "/v1/tasks", Some(&body), Some("dirty-1")).unwrap_err();
    assert_eq!(err.0, 409, "dirty repo without explicit base_commit is rejected");
    assert_eq!(err.1["error"]["code"], json!("repo_dirty"));
    // explicit base_commit is accepted (worktree pins the committed baseline)
    let head = common::git(&env.user_repo, &["rev-parse", "HEAD"]);
    let body2 = json!({
        "goal": "g", "repo_path": env.user_repo.to_string_lossy(),
        "base_commit": head.trim(), "allowed_adapters": ["fake"],
    });
    let ok = env.req("POST", "/v1/tasks", Some(&body2), Some("dirty-2")).unwrap();
    assert!(ok["task"]["task_id"].is_string());
}
