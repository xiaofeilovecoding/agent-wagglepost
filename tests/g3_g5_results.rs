//! G3: reroute demo (simulated writer stopped → checkpoint → confirmed reroute);
//! honest capability blocking for unverified second writers.
//! G5: result attribution — reviews and accepts bind candidate hashes; A cannot inherit B.
mod common;

use common::TestEnv;
use serde_json::json;
use std::time::Duration;

#[test]
fn g3_quota_block_then_reroute_same_adapter_simulated() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files", "quota_block"],
            "files": {"partial.txt": "quota-blocked partial work (simulated)\n"},
        })),
    );
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("q-1"),
    )
    .unwrap();
    let status = env.run_until(&task_id, &["quota_blocked"], Duration::from_secs(60));
    assert_eq!(status, "quota_blocked");

    // old writer stopped evidence + checkpoint exist
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert!(
        snap["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == json!("process_exited") && e["payload"]["stop_confirmed"] == json!(true)),
        "old writer stop confirmed"
    );

    // rerouting to an adapter that has NOT passed the independent write gate must be refused
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/reroute"),
            Some(&json!({"target_adapter": "claude"})),
            Some("rr-claude"),
        )
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("write_gate_failed"),
        "reroute to unverified writer is refused, not silently allowed");
    assert_eq!(err.0, 409);

    // rerouting to a gate-passing (simulated) writer is allowed and labeled simulated
    let rr = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/reroute"),
            Some(&json!({"target_adapter": "fake"})),
            Some("rr-fake"),
        )
        .unwrap();
    assert_eq!(rr["receipt"]["request_id"], json!("rr-fake"));
    let snap2 = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert!(
        snap2["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == json!("reroute_started")),
        "reroute_started emitted for the flight layer"
    );

    // relay attempt completes from the checkpoint
    env.set_fake_script(
        &task_id,
        "s1",
        &json!({
            "behaviors": ["write_files"],
            "files": {"partial.txt": "quota-blocked partial work (simulated)\ncontinued (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        }),
    );
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(90));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let attempts = snap["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "one original + one relay launch");
    assert_eq!(attempts[1]["generation"], json!(2));
    // no repeated write of the original attempt: generation 2 attempt is the only writer after stop
    let writer_launches = attempts.iter().filter(|a| a["role"] == json!("writer")).count();
    assert_eq!(writer_launches, 2);
}

#[test]
fn g5_candidate_binding_review_and_accept() {
    let env = TestEnv::new();
    // candidate A: writer A done → reviewer flags issues → manual reroute builds candidate B
    let (task_id, _) = env.ready_task(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"a.txt": "candidate A (simulated)
"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    // reviewer flags issues (set before dispatch so the chained review uses it)
    env.set_fake_script(
        &task_id,
        "s2",
        &json!({
            "behaviors": [],
            "result": {"status": "done", "summary": "review found issues: edge case uncovered (simulated)"}
        }),
    );
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("a-1"),
    )
    .unwrap();
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
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
    let cand_a = snap["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["current"].as_bool() == Some(true))
        .unwrap()["candidate_hash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(snap["reviews"][0]["verdict"], json!("issues"));

    // reroute (manual, after failed review) → candidate B from the checkpoint
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/reroute"),
        Some(&json!({"target_adapter": "fake"})),
        Some("b-reroute"),
    )
    .expect("reroute after failed review is eligible");
    env.set_fake_script(
        &task_id,
        "s1",
        &json!({
            "behaviors": ["write_files"],
            "files": {"a.txt": "candidate A (simulated)
", "b.txt": "candidate B (simulated)
"},
            "result": {"status": "done", "summary": "done (simulated)"}
        }),
    );
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(90));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let cands = snap["candidates"].as_array().unwrap();
    assert_eq!(cands.len(), 2, "A and B both recorded");
    let cand_b = cands
        .iter()
        .find(|c| c["current"].as_bool() == Some(true))
        .unwrap()["candidate_hash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(cand_a, cand_b, "A and B are distinct candidates");
    let cand_a_row = cands.iter().find(|c| c["candidate_hash"] == json!(cand_a)).unwrap();
    assert_eq!(cand_a_row["current"], json!(false), "A superseded by B");
    // A's review is historical once a new candidate exists
    let a_reviews_historical = snap["reviews"].as_array().unwrap().iter()
        .filter(|r| r["candidate_hash"] == json!(cand_a))
        .all(|r| r["historical"].as_bool() == Some(true));
    assert!(a_reviews_historical, "old-candidate reviews marked historical");

    // accepting A (stale) must fail; accepting B works — attribution stays with the version
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/accept"),
            Some(&json!({"candidate_hash": cand_a})),
            Some("acc-a"),
        )
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("candidate_stale"));
    let ok = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/accept"),
            Some(&json!({"candidate_hash": cand_b})),
            Some("acc-b"),
        )
        .unwrap();
    assert_eq!(ok["candidate_hash"], json!(cand_b));

    // A cannot be revived after B is accepted
    let err2 = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/accept"),
            Some(&json!({"candidate_hash": cand_a})),
            Some("acc-a2"),
        )
        .unwrap_err();
    assert_eq!(err2.1["error"]["code"], json!("already_accepted"));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["task"]["accepted_candidate_hash"], json!(cand_b));
}

#[test]
fn g5_agent_claims_are_not_checks() {
    let env = TestEnv::new();
    // writer claims done but changes nothing → NOT collected as a candidate; human handling
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": [],
            "result": {"status": "done", "summary": "trust me, done (simulated)"}
        })),
    );
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("nc-1"),
    )
    .unwrap();
    let status = env.run_until(&task_id, &["interrupted"], Duration::from_secs(60));
    assert_eq!(status, "interrupted", "empty change set is not a candidate");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["candidates"].as_array().unwrap().len(), 0);
}
