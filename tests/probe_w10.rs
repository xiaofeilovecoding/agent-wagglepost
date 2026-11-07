mod common;
use common::TestEnv;
use serde_json::json;
use std::time::Duration;

#[test]
fn probe_w10() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("write_then_review", Some("fake"), Some(json!({
        "behaviors": ["write_files"], "files": {"lib.txt": "baseline content\nA\n"},
        "result": {"status": "done", "summary": "done"}
    })));
    env.set_fake_script(&task_id, "s2", &json!({
        "behaviors": [], "result": {"status": "done", "summary": "review found issues: reroute probe (simulated)"}
    }));
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("d1")).unwrap();
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    let start = std::time::Instant::now();
    loop {
        let _ = env.req("POST", "/v1/__tick", Some(&json!({})), None);
        let s = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        if !s["reviews"].as_array().unwrap().is_empty() { break; }
        assert!(start.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(200));
    }
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    println!("STATUS: {}", snap["task"]["status"]);
    println!("REVIEW: verdict={} hist={} cand={}", snap["reviews"][0]["verdict"], snap["reviews"][0]["historical"], &snap["reviews"][0]["candidate_hash"].as_str().unwrap()[..8]);
    let cur = snap["candidates"].as_array().unwrap().iter().find(|c| c["current"].as_bool() == Some(true)).map(|c| c["candidate_hash"].as_str().unwrap()[..8].to_string());
    println!("CURRENT CAND: {:?}", cur);
    let rr = env.req("POST", &format!("/v1/tasks/{task_id}/reroute"), Some(&json!({"target_adapter": "fake"})), Some("rr"));
    println!("REROUTE: {:?}", rr.map(|v| v["receipt"]["status"].clone()).map_err(|e| e.1["error"]["code"].clone()));
}
