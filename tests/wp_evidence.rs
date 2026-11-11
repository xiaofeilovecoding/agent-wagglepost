//! W3 evidence: one full fake journey — quota block → confirmed relay →
//! review → accept — with bilingual must-keep material, a unique ending
//! sentence, and long optional evidence. Publishes the bill comparison:
//! must-keep fields arrive complete, delivered bytes stay under budget and
//! under the naive full re-brief (对照). Everything simulated and synthetic.

mod common;

use common::TestEnv;
use serde_json::json;
use std::time::Duration;

fn capture_path(env: &TestEnv, attempt_id: &str) -> std::path::PathBuf {
    env.managed_root
        .join("fake-captures")
        .join(format!("{attempt_id}.stdin.txt"))
}

/// 60 tracked evidence files keep the optional change-path list long while
/// must-keep fields stay comfortably inside the 8 KiB budget.
fn evidence_files() -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    for i in 0..60 {
        m.insert(
            format!("tracked-evidence-{i:02}.txt"),
            json!("synthetic evidence line, updated by the writer (simulated)\n"),
        );
    }
    m
}

/// Commit the tracked evidence files so the fake writer's edits surface as
/// changed_files in the checkpoint (change paths for the relay package).
fn seed_tracked_files(repo: &std::path::Path) {
    for i in 0..60 {
        std::fs::write(
            repo.join(format!("tracked-evidence-{i:02}.txt")),
            "baseline (tracked)\n",
        )
        .unwrap();
    }
    for args in [
        vec!["add", "-A"],
        vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "seed evidence files"],
    ] {
        let st = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git available in test env");
        assert!(st.success(), "git seed step failed");
    }
}

#[test]
fn w3_quota_block_confirm_relay_review_accept_with_bill_evidence() {
    let env = TestEnv::new();
    seed_tracked_files(&env.user_repo);
    let goal = "在配额阻塞后从检查点换班继续，保持必保留内容完整。\n\
                Resume from the checkpoint after the quota block with must-keep content intact.\n\
                唯一结尾句：W3 证据链在此结束 ⏹ unique-ending-marker-9f2c";
    let acceptance = "- 必保留字段完整送达（目标/验收/范围/进度/失败位置/下一步）\n\
                      - Must-keep fields arrive complete (goal/acceptance/scope/progress/failure/next)\n\
                      - 交付字节小于对照载荷 / delivered bytes smaller than the comparison payload";
    // submit the bilingual must-keep material up front so approval binds exactly
    // the body that will be re-assembled at dispatch
    let resp = env
        .req(
            "POST",
            "/v1/tasks",
            Some(&json!({
                "goal": goal,
                "acceptance": acceptance.lines().collect::<Vec<_>>(),
                "repo_path": env.user_repo.to_string_lossy(),
                "recipe": "write_then_review",
                "allowed_adapters": ["fake"],
            })),
            Some("w3-submit"),
        )
        .unwrap();
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).unwrap();
    env.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None).unwrap();
    env.core.store.meta_set(
        &format!("fake_script:{task_id}:s1"),
        &json!({
            "behaviors": ["write_files", "quota_block"],
            "files": evidence_files(),
        })
        .to_string(),
    );
    let hash = env.plan_hash(&task_id);
    env.approve_current(&task_id, &hash).unwrap();

    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("w3-1"))
        .unwrap();
    let status = env.run_until(&task_id, &["quota_blocked"], Duration::from_secs(60));
    assert_eq!(status, "quota_blocked", "writer quota-blocked with partial work saved");

    // ---- confirmed relay: preview the package, approve by digest, queue ----
    let preview = env
        .req("GET", &format!("/v1/tasks/{task_id}/handoff-preview"), None, None)
        .unwrap();
    let body = preview["handoff"]["text"].as_str().unwrap();
    let bill = &preview["bill"];
    assert_eq!(preview["handoff"]["mode"], json!("relay_package"));

    // must-keep fields complete — including the unique ending sentence
    for marker in [
        "## 目标 / Goal\n",
        "## 验收条件 / Acceptance\n",
        "## 范围与约束 / Scope\n",
        "## 已确认进度 / Confirmed progress\n",
        "## 失败位置 / Failure location\n",
        "## 下一步 / Next step\n",
    ] {
        assert!(body.contains(marker), "relay must-keep section present: {marker}");
    }
    assert!(body.contains("唯一结尾句：W3 证据链在此结束 ⏹ unique-ending-marker-9f2c"));
    assert!(body.contains("- 必保留字段完整送达（目标/验收/范围/进度/失败位置/下一步）"));
    // the long optional change paths ride along when they fit
    assert!(body.contains("tracked-evidence-59.txt"), "optional change paths present");
    // bill: must-send marked, bytes under budget AND under the naive re-brief
    assert_eq!(bill["omitted_count"], json!(0), "everything fits for this synthetic input");
    let must_sections: Vec<&str> = bill["included"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["must"].as_bool() == Some(true))
        .map(|s| s["title"].as_str().unwrap())
        .collect();
    assert_eq!(must_sections.len(), 6, "six must-keep sections on the bill");
    let delivered = preview["handoff"]["bytes"].as_u64().unwrap();
    let comparison = bill["comparison_bytes"].as_u64().unwrap();
    assert!(
        comparison > delivered,
        "delivered {delivered} bytes < full naive re-brief {comparison} bytes (对照)"
    );
    assert!(delivered <= 8 * 1024, "delivered bytes within the 8 KiB budget");
    eprintln!(
        "W3-EVIDENCE delivered_bytes={delivered} comparison_bytes={comparison} required_bytes={} omitted={}",
        bill["required_bytes"], bill["omitted_count"]
    );

    // transfer == preview: the relay attempt receives the exact approved body;
    // the relay writer completes the goal instead of blocking again
    env.set_fake_script(
        &task_id,
        "s1",
        &json!({
            "behaviors": ["write_files"],
            "files": evidence_files(),
            "result": {"status": "done", "summary": "relay completed (simulated)"}
        }),
    );
    env.reroute_queued(&task_id, "fake", "w3-relay");
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(90));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let attempts = snap["attempts"].as_array().unwrap();
    let writer_attempts: Vec<&serde_json::Value> =
        attempts.iter().filter(|a| a["role"] == json!("writer")).collect();
    assert_eq!(writer_attempts.len(), 2, "original + relay writer");
    let relay_digest = &writer_attempts[1]["handoff_digest"];
    assert_eq!(
        relay_digest,
        &preview["approved_digest"],
        "launched relay body binds the previewed digest"
    );
    let captured = std::fs::read_to_string(capture_path(
        &env,
        writer_attempts[1]["attempt_id"].as_str().unwrap(),
    ))
    .expect("relay capture exists");
    assert_eq!(
        wagglepost::ids::sha256_hex(captured.as_bytes()),
        preview["approved_digest"],
        "captured relay bytes identical to the preview (传输与预览一致)"
    );

    // ---- review: read-only gates not degraded, then accept ----
    let review_preview = env
        .req("GET", &format!("/v1/tasks/{task_id}/handoff-preview?role=reviewer"), None, None)
        .unwrap();
    let rbody = review_preview["handoff"]["text"].as_str().unwrap();
    assert!(rbody.contains("## 复核问题 / Review questions\n"), "reviewer sees the questions");
    assert!(rbody.contains("候选身份 / Candidate identity\n"), "reviewer sees candidate identity");
    assert!(
        review_preview["bill"]["included"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["must"].as_bool() == Some(true)),
        "review bill marks must-keep sections"
    );
    let cand_hash = snap["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["current"].as_bool() == Some(true))
        .map(|c| c["candidate_hash"].as_str().unwrap().to_string())
        .expect("current candidate");
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
    assert_eq!(reviews[0]["candidate_hash"], json!(cand_hash), "review binds the frozen candidate");
    assert_eq!(reviews[0]["verdict"], json!("pass"));

    let acc = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/accept"),
            Some(&json!({"candidate_hash": cand_hash})),
            Some("w3-accept"),
        )
        .unwrap();
    assert_eq!(acc["accepted"], json!(true), "accept binds the version");
    assert_eq!(acc["candidate_hash"], json!(cand_hash));
    let final_snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(final_snap["task"]["status"], json!("accepted"), "journey complete");
    assert_eq!(final_snap["task"]["accepted_candidate_hash"], json!(cand_hash));
}
