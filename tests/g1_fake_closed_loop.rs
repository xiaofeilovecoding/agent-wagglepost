//! G1 (offline core) + G7a: the full journey with the fake adapter —
//! submit → plan → approve → dispatch → writer candidate → reviewer → accept.
//! Everything simulated is labeled simulated; production receipts come from real events.
mod common;

use common::TestEnv;
use serde_json::json;
use std::time::Duration;

#[test]
fn g1_fake_write_then_review_accept_loop() {
    let env = TestEnv::new();
    let (task_id, _hash) = env.ready_task(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"lib.txt": "baseline content\nappended by fake writer (simulated)\n"},
            "result": {"status": "done", "summary": "写入完成（模拟）"}
        })),
    );

    // dispatch the writer
    let resp = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&json!({})),
            Some("disp-writer-1"),
        )
        .expect("dispatch writer");
    let receipt = resp["receipt"].clone();
    assert_eq!(receipt["request_id"], "disp-writer-1");
    assert_eq!(receipt["task_id"], task_id);
    assert!(
        receipt["attempt_id"].is_string(),
        "receipt carries the attempt id"
    );
    assert!(
        receipt["next_action"]["action"].is_string(),
        "next action is a bounded enum, not model text"
    );

    // writer runs → candidate frozen → needs_review
    let status = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    assert_eq!(status, "needs_review");

    // reviewer runs automatically (scheduler) → review recorded on the same candidate
    let snap = env
        .req("GET", &format!("/v1/tasks/{task_id}"), None, None)
        .unwrap();
    let cand = &snap["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["current"].as_bool() == Some(true))
        .expect("current candidate")
        .clone();
    let cand_hash = cand["candidate_hash"].as_str().unwrap().to_string();
    assert!(
        !cand_hash.is_empty(),
        "candidate hash recorded at freeze time"
    );
    // wait for the review record (status stays needs_review across the review run)
    let start = std::time::Instant::now();
    let snap = loop {
        let _ = env.req("POST", "/v1/__tick", Some(&json!({})), None);
        let s = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        if !s["reviews"].as_array().unwrap().is_empty() {
            break s;
        }
        if start.elapsed() > Duration::from_secs(90) {
            panic!("review never recorded");
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let reviews = snap["reviews"].as_array().unwrap();
    assert!(!reviews.is_empty(), "review recorded");
    assert_eq!(reviews[0]["candidate_hash"], json!(cand_hash), "review binds the candidate hash it saw");
    assert_eq!(reviews[0]["verdict"], json!("pass"));

    // two attempts: writer + reviewer, one launch each (budget 2 = planned steps)
    let attempts = snap["attempts"].as_array().unwrap();
    if attempts.len() != 2 {
        eprintln!("DEBUG attempts: {}", serde_json::to_string_pretty(&attempts).unwrap_or_default());
        eprintln!("DEBUG status: {}", snap["task"]["status"]);
    }
    assert_eq!(attempts.len(), 2, "writer + reviewer attempts only");
    assert_eq!(attempts[0]["role"], "writer");
    assert_eq!(attempts[1]["role"], "reviewer");
    assert_eq!(attempts[1]["adapter"], "fake");
    for a in attempts {
        assert_eq!(a["simulated"], json!(true), "fake attempts marked simulated");
    }

    // events: the production flight kinds fired from real events only
    let kinds: Vec<String> = snap["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(kinds.contains(&"attempt_started".to_string()), "attempt_started emitted");
    assert!(kinds.contains(&"result_collected".to_string()), "result_collected emitted");

    // accept binds the candidate hash
    let acc = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/accept"),
            Some(&json!({"candidate_hash": cand_hash})),
            Some("accept-1"),
        )
        .expect("accept");
    assert_eq!(acc["accepted"], json!(true));
    assert_eq!(acc["candidate_hash"], json!(cand_hash));
    assert!(
        acc["patch"].as_str().map(|p| !p.is_empty()).unwrap_or(false),
        "patch export path present for the user to take away"
    );

    // user repo untouched by the whole journey
    let git_status = common::git(&env.user_repo, &["status", "--porcelain"]);
    assert!(git_status.trim().is_empty(), "user repo stays clean");

    // closing browser / restarting the client does not affect the task: snapshot is the same
    let snap2 = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap2["task"]["status"], json!("accepted"));
    assert_eq!(snap2["task"]["accepted_candidate_hash"], json!(cand_hash));
}

#[test]
fn g1_direct_recipe_single_step() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"demo.txt": "direct demo (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("d-1"),
    )
    .expect("dispatch");
    let status = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    assert_eq!(status, "needs_review");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let attempts = snap["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1, "direct recipe: writer only");
    let cand = snap["candidates"].as_array().unwrap();
    assert_eq!(cand.len(), 1, "candidate recorded for accept");
}
