//! Release boundary regressions: separate CLI/browser tokens and CLI repository scope.
mod common;

use common::TestEnv;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::time::Duration;
use wagglepost::model::{Attempt, AttemptStatus, Decision, DecisionOption, Role, TaskStatus};

#[test]
fn wp19_sse_mixed_window_resyncs_without_leaking_b() {
    let env = TestEnv::new_with_cli_scope();
    let (a_id, _) = env.ready_task("direct", Some("fake"), None);
    let outside = env.managed_root.parent().unwrap().join("sse-b-repo");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("x.txt"), "b\n").unwrap();
    common::git(&outside, &["init", "-q", "-b", "main"]);
    common::git(&outside, &["add", "-A"]);
    common::git(
        &outside,
        &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"],
    );
    let b_body = json!({
        "goal": "SSE B task",
        "repo_path": outside.to_string_lossy(),
        "allowed_adapters": ["fake"],
    });
    let b_task = env
        .raw(
            "POST",
            "/v1/tasks",
            Some(&b_body),
            &[
                ("Cookie", &format!("wp_session={}", env.browser_token)),
                ("Origin", &env.base()),
                ("X-CSRF-Token", &env.csrf),
            ],
        )
        .expect("browser creates B task");
    let b_id = b_task["task"]["task_id"].as_str().unwrap().to_string();

    let before = env.core.bus.current_seq();
    env.core.bus.emit(Some(&a_id), None, 0, "synthetic", json!({"n": "a1"}));
    for n in 0..500 {
        env.core.bus.emit(Some(&b_id), None, 0, "synthetic", json!({"n": n}));
    }
    env.core.bus.emit(Some(&a_id), None, 0, "synthetic", json!({"n": "a2"}));

    let resp = ureq::get(&format!("{}/v1/events", env.base()))
        .timeout(Duration::from_secs(5))
        .set("Authorization", "Bearer test-token")
        .set("Last-Event-ID", &before.to_string())
        .call()
        .expect("SSE call");
    let mut reader = BufReader::new(resp.into_reader());
    let mut lines = Vec::new();
    // resync (event+data+blank) + the oldest authorized A (event+data+blank).
    // The second A is the 502nd raw event outside the 501-row replay read; the
    // resync tells the client to refetch the snapshot instead of silently losing it.
    for _ in 0..6 {
        let mut line = String::new();
        reader.read_line(&mut line).expect("SSE line");
        lines.push(line);
    }
    let visible = lines.join("");
    assert!(visible.contains("event: resync"), "raw 502-event window must resync: {visible}");
    assert!(visible.contains(&a_id), "authorized A events survive: {visible}");
    assert!(!visible.contains(&b_id), "B events must not leak: {visible}");
}

fn running_attempt(task_id: &str, attempt_id: &str, revision: u64) -> Attempt {
    Attempt {
        attempt_id: attempt_id.into(),
        task_id: task_id.into(),
        task_revision: revision,
        step_id: "s1".into(),
        adapter: "fake".into(),
        adapter_version: "0.1.0".into(),
        role: Role::Writer,
        worktree_id: None,
        generation: 1,
        native_id: None,
        request_id: None,
        status: AttemptStatus::Running,
        launch_no: 1,
        exit_code: None,
        started_at: Some(wagglepost::store::timestamp()),
        ended_at: None,
        simulated: true,
        handoff_digest: None,
        pid: None,
    }
}

fn open_decision(task_id: &str, decision_id: &str, attempt_id: &str, revision: u64) -> Decision {
    Decision {
        decision_id: decision_id.into(),
        task_id: task_id.into(),
        revision,
        attempt_id: attempt_id.into(),
        checkpoint_id: None,
        question: "continue? (synthetic)".into(),
        options: vec![DecisionOption {
            id: "continue".into(),
            label_zh: "继续".into(),
            label_en: "Continue".into(),
            effect_zh: "继续".into(),
            effect_en: "Continue".into(),
        }],
        status: "open".into(),
        dedupe_key: format!("decision:{task_id}:{decision_id}"),
        merge_count: 1,
        resolution: None,
        resume_request_id: None,
        created_at: wagglepost::store::timestamp(),
        answered_at: None,
    }
}

#[test]
fn wp18_cancel_and_answer_receipts_bind_task_scope() {
    let env = TestEnv::new_with_cli_scope();
    let (a_id, _) = env.ready_task("direct", Some("fake"), None);
    let outside = env.managed_root.parent().unwrap().join("receipt-b-repo");
    std::fs::create_dir_all(&outside).unwrap();
    common::git(&outside, &["init", "-q", "-b", "main"]);
    std::fs::write(outside.join("x.txt"), "b\n").unwrap();
    common::git(&outside, &["add", "-A"]);
    common::git(
        &outside,
        &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"],
    );
    let b_task = env
        .raw(
            "POST",
            "/v1/tasks",
            Some(&json!({
                "goal": "receipt B task",
                "repo_path": outside.to_string_lossy(),
                "allowed_adapters": ["fake"],
            })),
            &[
                ("Cookie", &format!("wp_session={}", env.browser_token)),
                ("Origin", &env.base()),
                ("X-CSRF-Token", &env.csrf),
            ],
        )
        .expect("browser creates B task");
    let b_id = b_task["task"]["task_id"].as_str().unwrap().to_string();
    let b_rev = env.core.store.get_task(&b_id).unwrap().revision;
    let a_rev = env.core.store.get_task(&a_id).unwrap().revision;

    env.core
        .store
        .insert_attempt(&running_attempt(&a_id, "a-receipt-a", a_rev))
        .unwrap();
    env.core
        .store
        .insert_attempt(&running_attempt(&b_id, "a-receipt-b", b_rev))
        .unwrap();
    env.core
        .store
        .insert_decision(&open_decision(&a_id, "d-receipt-a", "a-receipt-a", a_rev))
        .unwrap();
    env.core
        .store
        .insert_decision(&open_decision(&b_id, "d-receipt-b", "a-receipt-b", b_rev))
        .unwrap();

    let cookie = format!("wp_session={}", env.browser_token);
    let origin = env.base();
    let csrf = env.csrf.clone();
    let browser_headers = [
        ("Cookie", cookie.as_str()),
        ("Origin", origin.as_str()),
        ("X-CSRF-Token", csrf.as_str()),
    ];
    let cancel_headers = [
        browser_headers[0],
        browser_headers[1],
        browser_headers[2],
        ("Idempotency-Key", "browser-cancel-b"),
    ];
    let answer_headers = [
        browser_headers[0],
        browser_headers[1],
        browser_headers[2],
        ("Idempotency-Key", "browser-answer-b"),
    ];
    // Browser cancels/answers the out-of-scope B task; the CLI must not read the
    // resulting request receipts by knowing their idempotency keys.
    env.raw(
        "POST",
        "/v1/attempts/a-receipt-b/cancel",
        Some(&json!({})),
        &cancel_headers,
    )
    .expect("browser cancels B");
    env.raw(
        "POST",
        "/v1/decisions/d-receipt-b/answer",
        Some(&json!({"option_id": "continue", "revision": b_rev})),
        &answer_headers,
    )
    .ok();
    for key in ["browser-cancel-b", "browser-answer-b"] {
        let err = env.req("GET", &format!("/v1/requests/{key}"), None, None).unwrap_err();
        assert_eq!(err.0, 403, "{key} must not leak B attempt: {err:?}");
        assert_eq!(err.1["error"]["code"], json!("repo_not_authorized"));
    }

    // Same operations on the authorized A repository remain replayable to the CLI.
    env.req(
        "POST",
        "/v1/attempts/a-receipt-a/cancel",
        Some(&json!({})),
        Some("cli-cancel-a"),
    )
    .expect("CLI cancels A");
    let rec = env.req("GET", "/v1/requests/cli-cancel-a", None, None).expect("CLI reads A cancel");
    assert_eq!(rec["request"]["task_id"], json!(a_id));
    env.req(
        "POST",
        "/v1/decisions/d-receipt-a/answer",
        Some(&json!({"option_id": "continue", "revision": a_rev})),
        Some("cli-answer-a"),
    )
    .ok();
    let rec = env.req("GET", "/v1/requests/cli-answer-a", None, None).expect("CLI reads A answer");
    assert_eq!(rec["request"]["task_id"], json!(a_id));
}

#[test]
fn wp12_log_retention_removes_only_expired_logs() {
    let env = TestEnv::new();
    let logs = env.managed_root.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let old = logs.join("old.log");
    let keep = logs.join("keep.log");
    std::fs::write(&old, "old\n").unwrap();
    std::fs::write(&keep, "keep\n").unwrap();
    let eight_days = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(eight_days)
        .unwrap();
    assert_eq!(env.core.cleanup_old_logs(), 1);
    assert!(!old.exists(), "expired log removed");
    assert!(keep.exists(), "recent log kept");
}

#[test]
fn wp12_recovery_marks_live_attempt_unknown_without_auto_resend() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);
    let mut task = env.core.store.get_task(&task_id).unwrap();
    task.status = TaskStatus::Running;
    task.updated_at = wagglepost::store::timestamp();
    env.core.store.update_task(&task).unwrap();
    let attempt = Attempt {
        attempt_id: "a-recovery-probe".into(),
        task_id: task_id.clone(),
        task_revision: task.revision,
        step_id: "s1".into(),
        adapter: "fake".into(),
        adapter_version: "0.1.0".into(),
        role: Role::Writer,
        worktree_id: None,
        generation: 1,
        native_id: None,
        request_id: Some("req-recovery-probe".into()),
        status: AttemptStatus::Running,
        launch_no: 1,
        exit_code: None,
        started_at: Some(wagglepost::store::timestamp()),
        ended_at: None,
        simulated: true,
        handoff_digest: None,
        pid: None,
    };
    env.core.store.insert_attempt(&attempt).unwrap();
    let notes = env.core.recover();
    assert!(!notes.is_empty(), "recovery reports the live attempt");
    let a = env.core.store.get_attempt(&attempt.attempt_id).unwrap();
    assert_eq!(a.status, AttemptStatus::Unknown);
    let t = env.core.store.get_task(&task_id).unwrap();
    assert_eq!(t.status, TaskStatus::StopUnconfirmed);
    let inbox = env.core.inbox();
    assert!(
        inbox.iter().any(|m| m.task_id == task_id
            && m.payload.get("reason_code").and_then(|v| v.as_str()) == Some("stop_unconfirmed")),
        "stop_unconfirmed is surfaced without a resend action"
    );
}

#[cfg(windows)]
#[test]
fn wp12_disk_low_refuses_before_worktree_or_launch() {
    let env = TestEnv::new_with_cfg(|cfg| cfg.min_free_bytes = u64::MAX);
    let (task_id, _) = env.ready_task("direct", Some("fake"), None);
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&json!({})),
            Some("disk-low-d"),
        )
        .unwrap_err();
    assert_eq!(err.0, 409);
    assert_eq!(err.1["error"]["code"], json!("disk_low"));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 0, "no attempt");
    assert_eq!(snap["task"]["launches_used"], json!(0), "no budget consumed");
}

#[test]
fn wp12_cli_and_browser_tokens_are_separate_and_repo_scoped() {
    let env = TestEnv::new_with_cli_scope();
    let outside = env.managed_root.parent().unwrap().join("outside-repo");
    std::fs::create_dir_all(&outside).unwrap();
    std::process::Command::new("git")
        .args(["-C", outside.to_str().unwrap(), "init", "-q", "-b", "main"])
        .status()
        .unwrap();
    std::fs::write(outside.join("x.txt"), "outside\n").unwrap();
    std::process::Command::new("git")
        .args(["-C", outside.to_str().unwrap(), "-c", "core.autocrlf=false", "add", "-A"])
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["-C", outside.to_str().unwrap(), "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"])
        .status()
        .unwrap();

    // The CLI channel is scoped to env.user_repo; a different repo is refused
    // before any task row or worker intent exists.
    let err = env
        .req(
            "POST",
            "/v1/tasks",
            Some(&json!({
                "goal": "outside scope",
                "repo_path": outside.to_string_lossy(),
                "allowed_adapters": ["fake"]
            })),
            Some("scope-outside"),
        )
        .unwrap_err();
    assert_eq!(err.0, 403);
    assert_eq!(err.1["error"]["code"], json!("repo_not_authorized"));

    // In-scope submit is accepted.
    let ok = env
        .req(
            "POST",
            "/v1/tasks",
            Some(&json!({
                "goal": "inside scope",
                "repo_path": env.user_repo.to_string_lossy(),
                "allowed_adapters": ["fake"]
            })),
            Some("scope-inside"),
        )
        .expect("in-scope submit");
    let inside_id = ok["task"]["task_id"].as_str().unwrap().to_string();

    // Browser cookie has its own token and can create an out-of-scope task.
    let browser = env.raw(
        "POST",
        "/v1/tasks",
        Some(&json!({
            "goal": "browser outside scope",
            "repo_path": outside.to_string_lossy(),
            "allowed_adapters": ["fake"]
        })),
        &[
            ("Cookie", &format!("wp_session={}", env.browser_token)),
            ("Origin", &env.base()),
            ("X-CSRF-Token", &env.csrf),
        ],
    );
    let outside_task = browser.expect("browser channel is not CLI-scoped");
    let outside_id = outside_task["task"]["task_id"].as_str().unwrap().to_string();

    // CLI bearer can read its own repo task...
    env.req("GET", &format!("/v1/tasks/{inside_id}"), None, None)
        .expect("CLI reads in-scope task");
    // ...but cannot read the out-of-scope task or start it.
    let err = env
        .req("GET", &format!("/v1/tasks/{outside_id}"), None, None)
        .unwrap_err();
    assert_eq!(err.0, 403);
    assert_eq!(err.1["error"]["code"], json!("repo_not_authorized"));
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{outside_id}/dispatch"),
            Some(&json!({})),
            Some("scope-outside-dispatch"),
        )
        .unwrap_err();
    assert_eq!(err.0, 403);
    assert_eq!(err.1["error"]["code"], json!("repo_not_authorized"));
}
