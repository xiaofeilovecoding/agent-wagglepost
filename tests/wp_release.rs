//! Release boundary regressions: separate CLI/browser tokens and CLI repository scope.
mod common;

use common::TestEnv;
use serde_json::json;
use wagglepost::model::{Attempt, AttemptStatus, Role, TaskStatus};

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
