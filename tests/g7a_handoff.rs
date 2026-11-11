//! G7a: context gates — captured payloads match the approved preview, budget bound,
//! role-specific assembly, resume/relay material minimality, dedupe + zero model calls.
mod common;

use common::TestEnv;
use serde_json::json;
use std::time::Duration;

fn capture_path(env: &TestEnv, attempt_id: &str) -> std::path::PathBuf {
    env.managed_root
        .join("fake-captures")
        .join(format!("{attempt_id}.stdin.txt"))
}

#[test]
fn g7a_first_run_payload_matches_preview_and_budget() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"x.txt": "x\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    // preview BEFORE dispatch: digest shown to the approver
    let preview = env
        .req("GET", &format!("/v1/tasks/{task_id}/handoff-preview"), None, None)
        .unwrap();
    let digest = preview["approved_digest"].as_str().unwrap().to_string();
    assert!(
        preview["handoff"]["bytes"].as_u64().unwrap() <= 8 * 1024,
        "default body budget is 8 KiB"
    );

    let resp = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&json!({})),
            Some("g7-1"),
        )
        .unwrap();
    let attempt_id = resp["receipt"]["attempt_id"].as_str().unwrap().to_string();
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));

    // the attempt binds the digest that was approved
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let attempt = &snap["attempts"].as_array().unwrap()[0];
    assert_eq!(attempt["handoff_digest"], json!(digest), "launched body == approved preview");

    // captured stdin == the assembled handoff (exact bytes)
    let captured = std::fs::read_to_string(capture_path(&env, &attempt_id)).unwrap();
    let sha = wagglepost::ids::sha256_hex(captured.as_bytes());
    assert_eq!(sha, digest, "captured payload identical to the approved preview");

    // role content: goal + acceptance + baseline present; scope present
    assert!(captured.contains("## 目标 / Goal"), "goal section");
    assert!(captured.contains("base_commit:"), "baseline section");
    assert!(captured.contains("wagglepost-result"), "reporting contract appended");
    // local routing: no model was called for the plan (fake usage is zero and no planner events)
    let planner_calls = snap["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"].as_str().unwrap_or("").contains("model"))
        .count();
    assert_eq!(planner_calls, 0, "zero extra model calls for routing/handoff");
}

#[test]
fn g7a_relay_package_is_minimal_and_bounded() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "write_then_review",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"p.txt": "partial (simulated)\n"},
            "result": {
                "status": "needs_decision",
                "question": "继续吗？",
                "options": [{"id": "opt_go", "label": "继续", "effect": "继续任务"}]
            }
        })),
    );
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("rp-1"),
    )
    .unwrap();
    let _ = env.run_until(&task_id, &["waiting_decision"], Duration::from_secs(60));
    let decisions = env.req("GET", "/v1/decisions?status=open", None, None).unwrap();
    let _card = &decisions["decisions"].as_array().unwrap()[0];
    env.answer_card(&task_id, "opt_go", "rp-ans");

    // relay package preview: contains progress + failure + next step; no chat history
    let preview = env
        .req("GET", &format!("/v1/tasks/{task_id}/handoff-preview"), None, None)
        .unwrap();
    let body = preview["handoff"]["text"].as_str().unwrap();
    assert_eq!(preview["handoff"]["mode"], json!("relay_package"));
    assert!(body.contains("已确认进度 / Confirmed progress"), "confirmed progress section");
    assert!(body.contains("用户已回答决策卡"), "the decision answer rides with the relay");
    assert!(!body.contains("对话历史"), "no chat history in the package");
    assert!(!body.contains("chat history"), "no chat history (en)");
    assert!(preview["handoff"]["bytes"].as_u64().unwrap() <= 8 * 1024, "bounded");
    // evidence references bind candidate identity
    assert!(
        !preview["handoff"]["evidence_refs"].as_array().unwrap().is_empty()
            || body.contains("evidence://"),
        "evidence index present"
    );

    // resume dispatch → captured body matches
    let resp = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&json!({})),
            Some("rp-2"),
        )
        .unwrap();
    let attempt_id = resp["receipt"]["attempt_id"].as_str().unwrap().to_string();
    env.set_fake_script(
        &task_id,
        "s1",
        &json!({
            "behaviors": ["write_files"],
            "files": {"p.txt": "partial (simulated)\nmore (simulated)\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        }),
    );
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(90));
    let captured = std::fs::read_to_string(capture_path(&env, &attempt_id)).unwrap();
    assert!(
        captured.contains("evidence://task/"),
        "relay hands over evidence references"
    );
    // reviewer handoff starts from the candidate diff, not the conversation
    let review_preview = env
        .req(
            "GET",
            &format!("/v1/tasks/{task_id}/handoff-preview?role=reviewer"),
            None,
            None,
        )
        .unwrap();
    let rbody = review_preview["handoff"]["text"].as_str().unwrap();
    assert_eq!(review_preview["handoff"]["mode"], json!("review"));
    assert!(rbody.contains("完整哈希 / full hash:"), "reviewer sees candidate identity");
    assert!(rbody.contains("关键变更路径 / Key change paths"), "reviewer starts from the change list");
}

#[test]
fn g7a_overlong_must_keep_blocks_launch_with_required_and_limit() {
    let env = TestEnv::new();
    // a goal larger than the 8 KiB budget is must-keep content: it is never
    // trimmed silently — preview and approval return a blocking error carrying
    // required/limit, and no attempt is ever launched (超额零启动)
    let huge_goal = "x".repeat(20 * 1024);
    let body = json!({
        "goal": huge_goal, "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct", "allowed_adapters": ["fake"],
    });
    let resp = env.req("POST", "/v1/tasks", Some(&body), Some("big-1")).unwrap();
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None)
        .unwrap();
    env.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None)
        .unwrap();
    // preview surfaces the blocking error with required/limit
    let preview = env
        .req("GET", &format!("/v1/tasks/{task_id}/handoff-preview"), None, None)
        .unwrap_err();
    assert_eq!(preview.0, 400, "preview blocked");
    assert_eq!(preview.1["error"]["code"], json!("handoff_over_budget"));
    let required = preview.1["error"]["details"]["required"].as_u64().unwrap();
    let limit = preview.1["error"]["details"]["limit"].as_u64().unwrap();
    assert_eq!(limit, 8 * 1024, "default dispatch budget");
    assert!(required > limit, "required {required} exceeds limit {limit}");
    assert!(
        required > 20 * 1024,
        "required covers the full goal ({required} bytes), not a trimmed fragment"
    );
    // approval cannot bind an over-budget body (assembly fails before digest checks)
    let hash = env.plan_hash(&task_id);
    let rev = env.revision_of(&task_id);
    let approve = env.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{task_id}/plan/approve"),
        Some(&json!({"summary_hash": hash, "handoff_digest": "0".repeat(64)})),
        Some("big-approve"),
        rev,
    )
    .unwrap_err();
    assert_eq!(approve.0, 400, "approval blocked");
    assert_eq!(approve.1["error"]["code"], json!("handoff_over_budget"));
    // dispatch never launches
    let _ = env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("big-2"),
    )
    .unwrap_err();
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert!(
        snap["attempts"].as_array().unwrap().is_empty(),
        "zero launch for over-budget must-keep content"
    );
}
