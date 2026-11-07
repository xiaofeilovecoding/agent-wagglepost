//! G8: decision cards, slot release, answer idempotency, stale/expired answers,
//! duplicate events, budget enforcement.
mod common;

use common::TestEnv;
use serde_json::json;
use std::time::Duration;

fn needs_decision_script() -> serde_json::Value {
    json!({
        "behaviors": ["write_files"],
        "files": {"partial.txt": "partial work kept at decision point (simulated)\n"},
        "result": {
            "status": "needs_decision",
            "question": "取消后保留旧缓存吗？",
            "options": [
                {"id": "opt_keep", "label": "保留旧缓存", "label_en": "Keep old cache", "effect_zh": "只阻止本次写入", "effect_en": "Block this write only"},
                {"id": "opt_clear", "label": "清除旧缓存", "label_en": "Clear old cache", "effect_zh": "清除后再阻止", "effect_en": "Clear then block"}
            ]
        }
    })
}

#[test]
fn g8_decision_flow_slot_release_and_resume() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("direct", Some("fake"), Some(needs_decision_script()));
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("dec-1"),
    )
    .unwrap();
    let status = env.run_until(&task_id, &["waiting_decision"], Duration::from_secs(60));
    assert_eq!(status, "waiting_decision");

    // process exit confirmed BEFORE entering waiting_decision
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let exited = snap["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == json!("process_exited"))
        .expect("process exit evidence")
        .clone();
    assert_eq!(exited["payload"]["stop_confirmed"], json!(true));
    // checkpoint saved and bound to the generation
    let cps: Vec<&serde_json::Value> = snap["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == json!("checkpoint_saved"))
        .collect();
    assert!(!cps.is_empty(), "checkpoint persisted before slot release");
    // slot is free (no live attempts)
    assert!(
        env.core.store.list_live_attempts().is_empty(),
        "waiting_decision implies the process tree exited and the slot is released"
    );
    // partial work kept in the writer worktree
    let worktrees = snap["worktrees"].as_array().unwrap();
    let wt = worktrees
        .iter()
        .find(|w| w["generation"] == json!(1))
        .unwrap();
    let partial = std::path::Path::new(wt["path"].as_str().unwrap()).join("partial.txt");
    assert!(partial.exists(), "checkpoint content preserved on disk");

    // decision card open with 2 options
    let decisions = env
        .req("GET", "/v1/decisions?status=open", None, None)
        .unwrap();
    let cards = decisions["decisions"].as_array().unwrap();
    assert_eq!(cards.len(), 1, "one deduped card");
    let card = &cards[0];
    let decision_id = card["decision_id"].as_str().unwrap().to_string();
    let revision = card["revision"].as_u64().unwrap();
    assert_eq!(card["options"].as_array().unwrap().len(), 2);

    // stale revision rejected
    let err = env
        .req(
            "POST",
            &format!("/v1/decisions/{decision_id}/answer"),
            Some(&json!({"option_id": "opt_keep", "revision": revision + 5})),
            Some("ans-stale"),
        )
        .unwrap_err();
    assert_eq!(err.0, 409, "stale answer rejected");

    // correct answer → queued resume
    let ans = env.answer_card(&task_id, "opt_keep", "ans-1");
    assert_eq!(ans["queued"], json!(true));

    // duplicate answer → same receipt, no second launch
    // duplicate answer replays the original receipt (answered check precedes the
    // digest verification, so a duplicate never triggers a second launch)
    let ans2 = env.req(
        "POST",
        &format!("/v1/decisions/{decision_id}/answer"),
        Some(&json!({"option_id": "opt_clear", "revision": revision})),
        Some("ans-2"),
    ).unwrap();
    assert_eq!(ans2["decision"]["status"], json!("answered"));
    assert_eq!(
        ans2["decision"]["resolution"],
        json!("opt_keep"),
        "first answer wins; duplicates reuse the receipt"
    );

    // resume runs generation 2 with a relay handoff containing the decision answer
    let status2 = env.run_until(&task_id, &["needs_review"], Duration::from_secs(90));
    assert_eq!(status2, "needs_review");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let attempts = snap["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "exactly one resume launch");
    assert_eq!(attempts[1]["generation"], json!(2));
    // only one answer triggered one resume: launches_used = 2
    assert_eq!(snap["task"]["launches_used"], json!(2));
    // the resumed worktree carries the checkpoint patch
    let worktrees = snap["worktrees"].as_array().unwrap();
    let g2 = worktrees
        .iter()
        .find(|w| w["generation"] == json!(2))
        .expect("generation-2 worktree");
    let partial = std::path::Path::new(g2["path"].as_str().unwrap()).join("partial.txt");
    assert!(
        partial.exists(),
        "checkpoint patch applied into the relay worktree"
    );
}

#[test]
fn g8_duplicate_events_deduped() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["duplicate_events", "write_files"],
            "files": {"d.txt": "dup\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("dup-1"),
    )
    .unwrap();
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let events = snap["events"].as_array().unwrap();
    // the fake emits the same message twice; core records one + a dedupe marker
    let dup_msgs = events
        .iter()
        .filter(|e| e["kind"] == json!("agent_message") && e["payload"]["dedupe_probe"].is_string())
        .count();
    assert_eq!(dup_msgs, 1, "duplicate wire event collapsed to one");
    assert!(
        events.iter().any(|e| e["kind"] == json!("event_deduped")),
        "dedupe recorded as an event for the timeline"
    );
}

#[test]
fn g8_budget_exhaustion_blocks_extra_launch() {
    let env = TestEnv::new();
    // direct recipe: planned 1 + resume 1 = 2 launches. Force 2 blocked launches to drain.
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"b1.txt": "1\n"},
            "result": {"status": "blocked", "blocked_reason": "simulated failure 1"}
        })),
    );
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("b-1"),
    )
    .unwrap();
    let _ = env.run_until(&task_id, &["interrupted"], Duration::from_secs(60));
    // the relay launch will also end blocked (script is one-shot per run)
    env.set_fake_script(
        &task_id,
        "s1",
        &json!({
            "behaviors": ["write_files"],
            "files": {"b2.txt": "2
"},
            "result": {"status": "blocked", "blocked_reason": "simulated failure 2"}
        }),
    );
    // resume after blocked → reroute path consumes resume budget
    env.reroute_queued(&task_id, "fake", "r-1");
    let _ = env.run_until(&task_id, &["interrupted"], Duration::from_secs(90));
    // budget now exhausted (2 launches used) → further relaunch refused with stable reason
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/reroute"),
            Some(&json!({"target_adapter": "fake", "handoff_digest": "dummy"})),
            Some("r-2"),
        )
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("budget_exhausted"));
}

#[test]
fn g8_timeline_replays_state_changes() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);
    env.req(
        "POST",
        &format!("/v1/tasks/{task_id}/dispatch"),
        Some(&json!({})),
        Some("tl-1"),
    )
    .unwrap();
    let _ = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let events = snap["events"].as_array().unwrap();
    let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    let mut sorted = seqs.clone();
    sorted.sort();
    assert_eq!(seqs, sorted, "events have a monotonic local seq");
    // every state change is reconstructable: created → plan → attempt_started → exited → collected
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    for expected in ["task_created", "plan_created", "plan_approved", "attempt_started", "process_exited", "result_collected"] {
        assert!(kinds.contains(&expected), "timeline has {expected}: {kinds:?}");
    }
}
