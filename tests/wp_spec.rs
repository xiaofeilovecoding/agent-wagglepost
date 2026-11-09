//! 详设 1.5 对照复审 regressions: WP14 reroute contract, WP15 first approval,
//! WP16 agent-output redaction. All offline; no real model calls.
mod common;

use common::TestEnv;
use serde_json::{json, Value};
use std::time::Duration;

// ---------- WP14: reroute target / revision / launch agreement ----------

/// A task whose writer blocks AFTER writing (interrupted + checkpoint), the
/// reroute-eligible shape used by every WP14 case.
fn blocked_task(env: &TestEnv, allowed: Value) -> String {
    let body = json!({
        "goal": "blocked writer for reroute contract (offline simulated)",
        "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct",
        // pin the first writer to the simulated fixture even when the task also
        // allows a real runtime: the reroute target is what must change
        "writer_adapter": "fake",
        "allowed_adapters": allowed,
    });
    let resp = env
        .req("POST", "/v1/tasks", Some(&body), Some(&format!("sub-{}", &wagglepost::ids::new_token()[..8])))
        .expect("submit");
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
    env.set_fake_script(
        &task_id,
        "s1",
        &json!({
            "behaviors": ["write_files"],
            "files": {"partial.txt": "fixture partial\n"},
            "result": {"status": "blocked", "blocked_reason": "simulated quota exhausted"}
        }),
    );
    let plan = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/plan"),
            Some(&json!({"writer_adapter": "fake"})),
            None,
        )
        .expect("plan");
    env.approve_current(&task_id, plan["plan"]["summary_hash"].as_str().unwrap()).expect("approve");
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some(&format!("d-{}", &wagglepost::ids::new_token()[..6]))).expect("dispatch");
    let status = env.run_until(&task_id, &["quota_blocked", "interrupted"], Duration::from_secs(60));
    assert_eq!(status, "interrupted", "writer blocked with a saved checkpoint");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert!(
        !snap["checkpoints"].as_array().unwrap().is_empty(),
        "the task snapshot must expose the saved checkpoint"
    );
    task_id
}

#[test]
fn wp14_reroute_refuses_stale_revision_and_disallowed_target() {
    let env = TestEnv::new();
    let task_id = blocked_task(&env, json!(["fake"]));
    let digest = env.relay_digest(&task_id, None, None);
    let rev = env.revision_of(&task_id);
    let budget_before = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap()["task"]["resume_used"]
        .as_u64()
        .unwrap();

    // stale If-Match: the request must be refused, nothing queued, no event
    let err = env
        .req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/reroute"),
            Some(&json!({"target_adapter": "fake", "handoff_digest": digest})),
            Some("wp14-stale"),
            rev + 5,
        )
        .unwrap_err();
    assert_eq!(err.0, 409, "stale revision must be refused");
    assert_eq!(err.1["error"]["code"], json!("revision_mismatch"));

    // disallowed target: codex is runtime-available but NOT in allowed_adapters
    let err = env
        .req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/reroute"),
            Some(&json!({"target_adapter": "codex", "handoff_digest": digest})),
            Some("wp14-notallowed"),
            rev,
        )
        .unwrap_err();
    assert_eq!(err.0, 409, "target outside allowed_adapters must be refused");
    assert_eq!(err.1["error"]["code"], json!("target_not_allowed"));

    // nothing moved: no queued intent, no reroute_started event, no budget spent
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["task"]["status"], json!("interrupted"), "task stays stopped");
    assert_eq!(snap["task"]["resume_used"], json!(budget_before), "no budget consumed");
    assert!(
        !snap["events"].as_array().unwrap().iter().any(|e| e["kind"] == json!("reroute_started")),
        "no reroute event may be published before launch evidence"
    );
    // replay of the refused request keeps refusing (idempotent refusal)
    let err = env
        .req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/reroute"),
            Some(&json!({"target_adapter": "codex", "handoff_digest": digest})),
            Some("wp14-notallowed"),
            rev,
        )
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("target_not_allowed"));
}

#[test]
fn wp14_reroute_target_override_launches_the_approved_runtime() {
    // Two distinguishable offline runtime fixtures: "fake" (the real fake agent)
    // and "codex" (a version stub that exits 73 on any real launch). The stub is
    // registered via the version probe only — no network, no real service.
    let env = TestEnv::new_with_cfg(|cfg| {
        let stub = cfg.managed_root.join("codex-version-stub.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\nif /i \"%1\"==\"--version\" (\r\n  echo codex-cli 99.0.0-review-fixture\r\n  exit /b 0\r\n)\r\nexit /b 73\r\n",
        )
        .unwrap();
        cfg.codex_exe = stub;
        cfg.codex_version = Some("codex-cli 99.0.0-review-fixture".into());
    });
    // H1: the stub's transport is verified by this fixture itself — record the
    // same role-scoped, version-bound evidence a real loop would produce
    env.seed_capability("codex", "codex-cli 99.0.0-review-fixture", &["writer", "reviewer"]);
    // allowed includes BOTH runtimes, so target=codex is permitted — and the
    // launch must actually be the codex fixture, never silently fake
    let task_id = blocked_task(&env, json!(["fake", "codex"]));
    let digest = env.relay_digest(&task_id, None, None);
    let rev = env.revision_of(&task_id);

    env.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{task_id}/reroute"),
        Some(&json!({"target_adapter": "codex", "handoff_digest": digest})),
        Some("wp14-codex"),
        rev,
    )
    .expect("reroute to the allowed codex target queues");

    // queued state publishes NO reroute event yet (nothing has launched)
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert!(
        !snap["events"].as_array().unwrap().iter().any(|e| e["kind"] == json!("reroute_started")),
        "queueing must not publish the reroute event (no launch evidence yet)"
    );

    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp14-codex-d")).expect("dispatch");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let snap = loop {
        let s = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        let a = s["attempts"].as_array().unwrap();
        if a.len() >= 2 && a[1]["ended_at"].is_string() {
            break s;
        }
        assert!(std::time::Instant::now() < deadline, "relay attempt never finished");
        std::thread::sleep(Duration::from_millis(120));
    };
    let second = &snap["attempts"].as_array().unwrap()[1];
    // the launched runtime IS the approved codex fixture (exit 73 from the stub —
    // unmistakably not the fake adapter, which would have completed)
    assert_eq!(second["adapter"], json!("codex"), "the relay must launch the approved target");
    assert_eq!(second["simulated"], json!(false), "the codex fixture is not the simulated fake adapter");
    assert_eq!(second["status"], json!("failed"), "the version stub refuses real launches with 73");
    assert_eq!(second["exit_code"], json!(73), "the stub's exit code proves the stub itself ran");

    // launch failed → no reroute_started may have been published (queue/launch
    // failure never broadcasts "already flying"), and the request id is never
    // used as an attempt id anywhere
    let rid = "wp14-codex";
    for e in snap["events"].as_array().unwrap() {
        if e["kind"] == json!("reroute_started") {
            assert_ne!(
                e["attempt_id"], json!(rid),
                "no event may carry the request id as an attempt id"
            );
        }
    }
    assert!(
        !snap["events"].as_array().unwrap().iter().any(|e| e["kind"] == json!("reroute_started")),
        "a failed launch must not broadcast the reroute event"
    );
    let failed_aid = second["attempt_id"].as_str().unwrap();
    assert!(
        !snap["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == json!("attempt_started") && e["attempt_id"] == json!(failed_aid)),
        "a failed relay launch must not broadcast a normal departure either"
    );
    assert!(
        snap["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == json!("attempt_prepared") && e["attempt_id"] == json!(failed_aid)),
        "the failed relay still records its prepared intent"
    );

    // control: the SAME two-fixture task rerouted to fake launches fake and
    // publishes exactly ONE reroute_started with the REAL attempt id + target
    let env2 = TestEnv::new_with_cfg(|cfg| {
        let stub = cfg.managed_root.join("codex-version-stub.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\nif /i \"%1\"==\"--version\" (\r\n  echo codex-cli 99.0.0-review-fixture\r\n  exit /b 0\r\n)\r\nexit /b 73\r\n",
        )
        .unwrap();
        cfg.codex_exe = stub;
        cfg.codex_version = Some("codex-cli 99.0.0-review-fixture".into());
    });
    env2.seed_capability("codex", "codex-cli 99.0.0-review-fixture", &["writer", "reviewer"]);
    let t2 = blocked_task(&env2, json!(["fake", "codex"]));
    let digest2 = env2.relay_digest(&t2, None, None);
    let rev2 = env2.revision_of(&t2);
    env2.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{t2}/reroute"),
        Some(&json!({"target_adapter": "fake", "handoff_digest": digest2})),
        Some("wp14-fake"),
        rev2,
    )
    .expect("reroute to fake queues");
    env2.req("POST", &format!("/v1/tasks/{t2}/dispatch"), Some(&json!({})), Some("wp14-fake-d")).expect("dispatch");
    let status = env2.run_until(&t2, &["needs_review"], Duration::from_secs(90));
    assert_eq!(status, "needs_review", "relay to the fake runtime completes the writer step");
    let snap2 = env2.req("GET", &format!("/v1/tasks/{t2}"), None, None).unwrap();
    let second2 = &snap2["attempts"].as_array().unwrap()[1];
    assert_eq!(second2["adapter"], json!("fake"));
    let reroutes: Vec<&Value> = snap2["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == json!("reroute_started"))
        .collect();
    assert_eq!(reroutes.len(), 1, "exactly one reroute event, published after launch evidence");
    assert_eq!(reroutes[0]["attempt_id"], second2["attempt_id"], "event carries the REAL attempt id");
    assert_eq!(reroutes[0]["payload"]["target"], json!("fake"), "event names the actually launched target");
    let relay_aid = second2["attempt_id"].as_str().unwrap();
    assert!(
        !snap2["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == json!("attempt_started") && e["attempt_id"] == json!(relay_aid)),
        "a relay launch must not first play the normal dispatch direction"
    );
    let first_aid = snap2["attempts"].as_array().unwrap()[0]["attempt_id"].as_str().unwrap();
    assert!(
        snap2["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == json!("attempt_started") && e["attempt_id"] == json!(first_aid)),
        "the original normal launch still has its departure"
    );
    let _ = (&env, &env2);
}

#[test]
fn wp14b_no_handshake_never_announces_departure() {
    // The approved codex target answers --version normally but its real launch
    // exits 73 without emitting agent_started. The persisted launch intent must
    // not be mistaken for a running attempt or a departure animation.
    let env = TestEnv::new_with_cfg(|cfg| {
        let stub = cfg.managed_root.join("codex-no-handshake.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\nif /i \"%1\"==\"--version\" (\r\n  echo codex-cli 99.0.0-review-fixture\r\n  exit /b 0\r\n)\r\nexit /b 73\r\n",
        )
        .unwrap();
        cfg.codex_exe = stub;
        cfg.codex_version = Some("codex-cli 99.0.0-review-fixture".into());
    });
    env.seed_capability("codex", "codex-cli 99.0.0-review-fixture", &["writer", "reviewer"]);
    let body = json!({
        "goal": "no-handshake codex fixture (offline simulated)",
        "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct",
        "allowed_adapters": ["codex"],
        "writer_adapter": "codex",
    });
    let resp = env.req("POST", "/v1/tasks", Some(&body), Some("wp14b-sub")).expect("submit");
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
    let plan = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/plan"),
            Some(&json!({"writer_adapter": "codex"})),
            None,
        )
        .expect("plan");
    env.approve_current(&task_id, plan["plan"]["summary_hash"].as_str().unwrap()).expect("approve");
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp14b-d")).expect("dispatch");
    let status = env.run_until(&task_id, &["interrupted", "failed"], Duration::from_secs(60));
    assert_eq!(status, "interrupted");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let attempt = &snap["attempts"].as_array().unwrap()[0];
    let aid = attempt["attempt_id"].as_str().unwrap();
    assert_eq!(attempt["status"], json!("failed"));
    assert_eq!(attempt["exit_code"], json!(73));
    assert_eq!(snap["task"]["launches_used"], json!(1), "failed real launch still counts");
    let events = snap["events"].as_array().unwrap();
    assert!(
        events.iter().any(|e| e["kind"] == json!("attempt_prepared") && e["attempt_id"] == json!(aid)),
        "the prepared intent is visible as its own event"
    );
    assert!(
        !events.iter().any(|e| e["kind"] == json!("agent_started")),
        "the stub never emitted a handshake"
    );
    assert!(
        !events.iter().any(|e| e["kind"] == json!("attempt_started")),
        "no departure without runtime evidence"
    );
    assert!(
        !events.iter().any(|e| e["kind"] == json!("reroute_started")),
        "no reroute departure without runtime evidence"
    );
}

#[test]
fn wp14b_normal_success_has_exactly_one_departure() {
    let env = TestEnv::new();
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"ok.txt": "ok\n"},
            "result": {"status": "done", "summary": "done (simulated)"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp14b-ok"))
        .expect("dispatch");
    let status = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    assert_eq!(status, "needs_review");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let events = snap["events"].as_array().unwrap();
    assert_eq!(
        events.iter().filter(|e| e["kind"] == json!("attempt_started")).count(),
        1,
        "normal success publishes exactly one departure"
    );
    assert_eq!(
        events.iter().filter(|e| e["kind"] == json!("reroute_started")).count(),
        0,
        "normal success has no reroute direction"
    );
    assert!(
        events.iter().any(|e| e["kind"] == json!("attempt_prepared")),
        "prepared intent is recorded before the handshake"
    );
}

#[test]
fn wp14_checkpoint_drift_is_refused_before_launch() {
    let env = TestEnv::new();
    let task_id = blocked_task(&env, json!(["fake"]));
    let digest = env.relay_digest(&task_id, None, None);
    let rev = env.revision_of(&task_id);
    env.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{task_id}/reroute"),
        Some(&json!({"target_adapter": "fake", "handoff_digest": digest})),
        Some("wp14-cp"),
        rev,
    )
    .expect("reroute queues");
    let gen = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap()["task"]
        ["current_generation"]
        .as_u64()
        .unwrap()
        + 1;
    // a different checkpoint appears after approval (late event / restart)
    env.core
        .store
        .meta_set(&format!("reroute_checkpoint:{task_id}:{gen}"), "cp-not-the-approved-one");
    let err = env
        .req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp14-cp-d"))
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("checkpoint_drift"));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 1, "no second launch");
}

#[test]
fn wp14_busy_refusal_preserves_the_relay_intent() {
    // write_then_review: writer completes, reviewer reports issues → manual
    // reroute queues the WRITER relay (generation 2). A post-intent gate
    // (fail-closed git filter) then refuses dispatch — the relay intent must
    // survive, so the next dispatch still relays the WRITER instead of falling
    // back to "no dispatchable step".
    let env = TestEnv::new();
    let body = json!({
        "goal": "intent preservation probe (offline simulated)",
        "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "write_then_review",
        "allowed_adapters": ["fake"],
        "resume_budget": 3,
    });
    let resp = env.req("POST", "/v1/tasks", Some(&body), Some("wp14i-sub")).expect("submit");
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
    env.set_fake_script(
        &task_id,
        "s1",
        &json!({"behaviors": ["write_files"], "files": {"a.txt": "A\n"}, "result": {"status": "done", "summary": "writer done (simulated)"}}),
    );
    env.set_fake_script(
        &task_id,
        "s2",
        &json!({
            "behaviors": [],
            "result": {"status": "done", "summary": "review found issues (simulated)"},
            "review": {"verdict": "issues"}
        }),
    );
    let plan = env.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None).expect("plan");
    env.approve_current(&task_id, plan["plan"]["summary_hash"].as_str().unwrap()).expect("approve");
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp14i-d1")).expect("writer");
    // the scheduler launches the reviewer from the frozen candidate; wait for
    // the persisted verdict instead of hand-dispatching the review step
    let start = std::time::Instant::now();
    loop {
        let _ = env.req("POST", "/v1/__tick", Some(&json!({})), None);
        let s = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
        if s["reviews"].as_array().unwrap().iter().any(|r| r["verdict"] == json!("issues")) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "reviewer never recorded issues");
        std::thread::sleep(Duration::from_millis(120));
    }
    let status = env.run_until(&task_id, &["needs_review"], Duration::from_secs(60));
    assert_eq!(status, "needs_review", "reviewer found issues");

    let digest = env.relay_digest(&task_id, None, None);
    let rev = env.revision_of(&task_id);
    env.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{task_id}/reroute"),
        Some(&json!({"target_adapter": "fake", "handoff_digest": digest})),
        Some("wp14-intent"),
        rev,
    )
    .expect("reroute queued the writer relay");
    // sabotage a gate that sits AFTER the intent is recorded
    let out = std::process::Command::new("git")
        .args(["-C"])
        .arg(env.user_repo.as_os_str())
        .args(["config", "filter.waggleprobe.process", "true-synthetic"])
        .output()
        .expect("git config");
    assert!(out.status.success());
    let err = env
        .req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp14-intent-d1"))
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("repo_unsafe_git"), "dispatch refuses fail-closed");
    // remove the sabotage → the relay intent must STILL be set
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(env.user_repo.as_os_str())
        .args(["config", "--unset", "filter.waggleprobe.process"])
        .output()
        .expect("git config unset");
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp14-intent-d2"))
        .expect("dispatch after the gate clears — intent was preserved");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let attempts = snap["attempts"].as_array().unwrap();
    assert!(attempts.len() >= 3, "the writer relay launched (not a no_dispatchable_step fallback)");
    let relay = attempts.last().unwrap();
    assert_eq!(relay["role"], json!("writer"), "the relay intent still names the writer step");
    assert_eq!(
        relay["generation"],
        json!(3),
        "writer gen1 + reviewer gen2, so the relay is gen3 from the checkpoint"
    );
}

// ---------- WP15: first approval binds the previewed body + revision ----------

#[test]
fn wp15_approve_requires_digest_and_current_revision() {
    let env = TestEnv::new();
    // submit + trust + plan, no approve (caller supplies the material)
    let body = json!({
        "goal": "approve contract probe (offline simulated)",
        "repo_path": env.user_repo.to_string_lossy(),
        "recipe": "direct",
        "allowed_adapters": ["fake"],
    });
    let resp = env.req("POST", "/v1/tasks", Some(&body), Some("wp15-sub")).expect("submit");
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
    let plan = env.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None).expect("plan");
    let hash = plan["plan"]["summary_hash"].as_str().unwrap().to_string();
    let rev = env.revision_of(&task_id);

    // 1. no digest at all → refused, nothing queued, no attempt
    let err = env
        .req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": hash})),
            Some("wp15-nodigest"),
            rev,
        )
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("handoff_digest_required"), "approval must name the previewed body");

    // 2. wrong digest → refused
    let err = env
        .req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": hash, "handoff_digest": "0".repeat(64)})),
            Some("wp15-wrongdigest"),
            rev,
        )
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("handoff_digest_mismatch"));

    // 3. correct digest but STALE revision → refused (the reviewer's probe sent
    //    If-Match 99999 and the server launched anyway)
    let digest = env.relay_digest(&task_id, None, None);
    let err = env
        .req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": hash, "handoff_digest": digest})),
            Some("wp15-stale"),
            rev + 42,
        )
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("revision_mismatch"));

    // 4. missing If-Match header entirely → refused (the old server accepted)
    let err = env
        .req(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": hash, "handoff_digest": digest})),
            Some("wp15-norev"),
        )
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("revision_required"));

    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["task"]["status"], json!("awaiting_approval"), "nothing was queued");
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 0, "no launch happened");

    // 5. correct digest + current revision → approved; dispatch launches once
    env.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{task_id}/plan/approve"),
        Some(&json!({"summary_hash": hash, "handoff_digest": digest})),
        Some("wp15-ok"),
        rev,
    )
    .expect("approve with previewed body + current revision");

    // 6. same-key replay returns the original receipt without a second event;
    //    a fresh key cannot re-approve and move a queued task backwards
    let replay = env
        .req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": hash, "handoff_digest": digest})),
            Some("wp15-ok"),
            rev,
        )
        .expect("same request id replays");
    assert_eq!(replay["task"]["status"], json!("queued"));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let approved_events = snap["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == json!("plan_approved"))
        .count();
    assert_eq!(approved_events, 1, "replay must not emit a second plan_approved");
    let err = env
        .req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": hash, "handoff_digest": digest})),
            Some("wp15-dup"),
            rev,
        )
        .unwrap_err();
    assert_eq!(err.1["error"]["code"], json!("invalid_state"), "a new key cannot re-approve");

    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp15-d")).expect("dispatch");
    let status = env.run_until(&task_id, &["needs_review"], Duration::from_secs(90));
    assert_eq!(status, "needs_review");
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["attempts"].as_array().unwrap().len(), 1, "exactly one launch");
}

#[test]
fn wp15_demo_preset_keeps_one_click_simulated_approval() {
    // the built-in all-fake demo keeps its one-click path: the server-side
    // fixed preset self-binds the digest under an explicit simulated route
    let env = TestEnv::new();
    let body = json!({
        "goal": "demo one-click (offline simulated)",
        "repo_path": "sample:demo",
        "recipe": "direct",
        "allowed_adapters": ["fake"],
        "demo": "decision",
    });
    let resp = env.req("POST", "/v1/tasks", Some(&body), Some("wp15-demo")).expect("demo submit");
    let tid = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{tid}/trust"), Some(&json!({})), None).expect("trust");
    let plan = env.req("POST", &format!("/v1/tasks/{tid}/plan"), Some(&json!({})), None).expect("plan");
    let rev = env.revision_of(&tid);
    // no handoff_digest — the demo path self-binds (simulated)
    env.req_with_if_match(
        "POST",
        &format!("/v1/tasks/{tid}/plan/approve"),
        Some(&json!({"summary_hash": plan["plan"]["summary_hash"]})),
        Some("wp15-demo-approve"),
        rev,
    )
    .expect("demo approval stays one-click");
    env.req("POST", &format!("/v1/tasks/{tid}/dispatch"), Some(&json!({})), Some("wp15-demo-d")).expect("dispatch");
    let status = env.run_until(&tid, &["waiting_decision"], Duration::from_secs(60));
    assert_eq!(status, "waiting_decision", "the demo preset ran");
    let snap = env.req("GET", &format!("/v1/tasks/{tid}"), None, None).unwrap();
    let att = &snap["attempts"].as_array().unwrap()[0];
    assert_eq!(att["simulated"], json!(true), "the demo path is explicitly simulated");
}

// ---------- WP16: agent output is redacted before persistence ----------

#[test]
fn wp16_agent_output_redacted_before_persistence() {
    let env = TestEnv::new();
    // synthetic credential-shaped marker; its VALUE is never printed by this test
    let marker = format!("ghp_{}{}0123456789", "SyntheticCanary", "AbCdEfGhIjKlMnOp");
    let (task_id, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {"partial.txt": "fixture partial\n"},
            "result": {"status": "blocked", "blocked_reason": format!("Authorization: Bearer {marker} during sync")}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("wp16-d1")).expect("dispatch");
    let _ = env.run_until(&task_id, &["quota_blocked", "interrupted"], Duration::from_secs(60));

    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    let events_json = snap["events"].to_string();
    let inbox = env.req("GET", "/v1/messages?after=0", None, None).unwrap().to_string();
    let preview = env.req("GET", &format!("/v1/tasks/{task_id}/handoff-preview?role=writer"), None, None).unwrap().to_string();
    let mut sqlite_leak = false;
    for q in std::fs::read_dir(env.managed_root.join("")).unwrap().flatten() {
        let name = q.file_name().to_string_lossy().to_string();
        if name.starts_with("wagglepost.db") {
            if let Ok(bytes) = std::fs::read(q.path()) {
                if find(bytes.as_slice(), marker.as_bytes()) {
                    sqlite_leak = true;
                }
            }
        }
    }
    assert!(!events_json.contains(&marker), "canary leaked into task events (value suppressed)");
    assert!(!inbox.contains(&marker), "canary leaked into the inbox");
    assert!(!preview.contains(&marker), "canary leaked into the handoff preview");
    assert!(!sqlite_leak, "canary leaked into sqlite");
    // diagnostics keep their type and shape
    let events = snap["events"].as_array().unwrap();
    assert!(
        events.iter().any(|e| e["payload"]["reason"].as_str().map(|s| s.contains("[REDACTED]")).unwrap_or(false)),
        "the blocked reason stays present in redacted form"
    );

    // stderr vector: the fake agent's refusal diagnostic carries the marker in
    // a file path; the raw stderr log must be redacted but keep the error class
    let (task2, _) = env.ready_task(
        "direct",
        Some("fake"),
        Some(json!({
            "behaviors": ["write_files"],
            "files": {format!("../Authorization_Bearer_{marker}"): "blocked safely"}
        })),
    );
    env.req("POST", &format!("/v1/tasks/{task2}/dispatch"), Some(&json!({})), Some("wp16-d2")).expect("dispatch");
    let _ = env.run_until(&task2, &["interrupted", "failed", "needs_review"], Duration::from_secs(60));
    let snap2 = env.req("GET", &format!("/v1/tasks/{task2}"), None, None).unwrap();
    let attempt_id = snap2["attempts"].as_array().unwrap()[0]["attempt_id"].as_str().unwrap().to_string();
    let log_path = env.managed_root.join("logs").join(format!("{attempt_id}.stderr.log"));
    assert!(log_path.is_file(), "stderr log exists");
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(!log.contains(&marker), "canary leaked into the raw stderr log (value suppressed)");
    assert!(
        log.contains("[REDACTED]") && log.contains("parent traversal"),
        "the stderr diagnostic keeps its error class and location shape"
    );
}

/// byte needle search without printing the needle
fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
