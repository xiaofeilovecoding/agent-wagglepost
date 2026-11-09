//! H1 capability table: state ladder, version-bound verification, routing gaps.
//! Fixtures only — no real CLI launches, no accounts.

use serde_json::{json, Value};
use wagglepost::capability;

mod common;
use common::TestEnv;

fn cap_json(env: &TestEnv, adapter: &str) -> Value {
    env.req("GET", "/v1/doctor", None, None).unwrap()["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["adapter"] == json!(adapter))
        .cloned()
        .unwrap_or(Value::Null)
}

fn adapter_json(env: &TestEnv, adapter: &str) -> Value {
    env.req("GET", "/v1/adapters", None, None).unwrap()["adapters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(adapter))
        .cloned()
        .unwrap_or(Value::Null)
}

/// WP27: an environment whose runtime states are EXPLICITLY injected — the test
/// never depends on which CLIs the host happens to have or on real keys.
/// codex: missing; dsh: present stub (unverified); opencode: missing; claude:
/// missing. fake stays in-process.
fn injected_env() -> std::sync::Arc<TestEnv> {
    TestEnv::new_with_cfg(|cfg| {
        let missing = PathBuf::from("Z:/nonexistent/rgnu/copqz.exe");
        cfg.codex_exe = missing.clone();
        cfg.codex_version = None;
        cfg.claude_exe = missing.clone();
        cfg.claude_version = None;
        cfg.opencode_exe = missing.clone();
        cfg.opencode_version = None;
        cfg.opencode_model_configured = false;
        let dsh_stub = cfg.managed_root.join("dsh-state-stub.cmd");
        std::fs::write(
            &dsh_stub,
            "@echo off
if /i \"%~1\"==\"--version\" (
  echo 0.2.1-rc.5
  exit /b 0
)
exit /b 1
",
        )
        .unwrap();
        cfg.dsh = wagglepost::adapters::dsh::DshAdapter::new(Some(dsh_stub), "0.2.1-rc.5".into());
    })
}

use std::path::PathBuf;

/// codex stub whose --version answers but real launches exit 73 (no model calls)
fn codex_stub_env() -> std::sync::Arc<TestEnv> {
    TestEnv::new_with_cfg(|cfg| {
        let stub = cfg.managed_root.join("codex-version-stub.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\nif /i \"%1\"==\"--version\" (\r\n  echo codex-cli 99.0.0-review-fixture\r\n  exit /b 0\r\n)\r\nexit /b 73\r\n",
        )
        .unwrap();
        cfg.codex_exe = stub;
        cfg.codex_version = Some("codex-cli 99.0.0-review-fixture".into());
    })
}

#[test]
fn cap_probe_states_are_honest_without_verification() {
    // WP27: every state is injected explicitly — no host CLI dependency
    let env = injected_env();
    // codex injected as missing → not_installed, never claim capabilities
    let codex = cap_json(&env, "codex");
    assert_eq!(codex["state"], json!("not_installed"), "missing runtime reports not_installed");
    assert_eq!(adapter_json(&env, "codex")["write_supported"], json!(false));
    // fake is the in-process simulated runtime: verified by contract, routable
    let fake = cap_json(&env, "fake");
    assert_eq!(fake["state"], json!("verified"));
    assert_eq!(adapter_json(&env, "fake")["write_supported"], json!(true));
    // a runtime that was never verified stays probed (旧 unknown 维持 unknown:
    // nothing promotes it without evidence)
    let dsh = cap_json(&env, "dsh");
    assert_eq!(dsh["state"], json!("probed"), "present runtime without verification stays probed");
    let wr = &dsh["roles"].as_array().unwrap().iter().find(|r| r["role"] == json!("writer")).cloned().unwrap();
    assert_eq!(wr["verified"], json!(false));
    assert_eq!(wr["experimental"], json!(true), "dsh has no native restriction → experimental");
    // opencode injected as missing AND unconfigured → not_installed (needs_config
    // only applies to a PRESENT runtime without a model channel)
    let oc = cap_json(&env, "opencode");
    assert_eq!(oc["state"], json!("not_installed"));
}

#[test]
fn cap_version_drift_degrades_verification_and_reports_binding() {
    let env = codex_stub_env();
    env.seed_capability("codex", "codex-cli 99.0.0-review-fixture", &["writer"]);
    let before = env.core.capability_records().into_iter().find(|r| r.adapter == "codex").unwrap();
    assert_eq!(before.state, capability::STATE_VERIFIED);
    assert!(before.routable("writer"));
    // the device version changes: the same evidence composed against a new probe
    let mut probe = env.core.probe_record("codex");
    probe.version = Some("codex-cli 2.0.0".into());
    let ver = capability::load_verified(&env.core.store, "codex");
    let after = capability::ProbeRecord::compose(probe, ver.as_ref());
    assert_eq!(after.state, capability::STATE_PROBED, "verified evidence bound to another version degrades to probed");
    assert!(!after.routable("writer"), "degraded capability must not stay routable");
    assert!(after.probe_error.unwrap_or_default().contains("re-verify"), "the record names the rebinding");
    // and the OLD version's record still exists (evidence trail is not deleted)
    let raw = env.core.store.meta_get(&capability::meta_key("codex")).unwrap();
    assert!(raw.contains("codex-cli 99.0.0-review-fixture"));
}

#[test]
fn cap_real_loop_writes_real_evidence() {
    let env = TestEnv::new();
    let resp = env
        .req(
            "POST",
            "/v1/tasks",
            Some(&json!({
                "goal": "capability real-loop probe (simulated)",
                "repo_path": env.user_repo.to_string_lossy(),
                "recipe": "direct",
                "allowed_adapters": ["fake"],
            })),
            Some("cap-loop"),
        )
        .expect("submit");
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
    let plan = env.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None).expect("plan");
    let hash = plan["plan"]["summary_hash"].as_str().unwrap().to_string();
    env.approve_current(&task_id, &hash).expect("approve");
    env.run_until(&task_id, &["needs_review"], std::time::Duration::from_secs(90));
    let fake = cap_json(&env, "fake");
    assert_eq!(fake["evidence"], json!("real_loop"), "a completed loop attempt is real evidence");
    assert_eq!(fake["verified_version"], fake["version"]);
}

#[test]
fn cap_route_preview_names_capability_gaps() {
    let env = injected_env(); // codex/dsh states injected, not host-dependent
    // route preview works on a submitted task; no plan/approve needed
    let resp = env
        .req(
            "POST",
            "/v1/tasks",
            Some(&json!({
                "goal": "route gap naming",
                "repo_path": env.user_repo.to_string_lossy(),
                "recipe": "direct",
                "allowed_adapters": ["codex", "dsh", "fake"],
            })),
            Some("cap-route"),
        )
        .expect("submit");
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
    let route = env.req("GET", &format!("/v1/tasks/{task_id}/route-preview?role=writer"), None, None).unwrap();
    let cands = route["candidates"].as_array().unwrap();
    let find = |id: &str| cands.iter().find(|c| c["adapter"] == json!(id)).cloned().unwrap();
    // codex: not installed → version_unavailable (never a silent pass)
    let codex = find("codex");
    assert_eq!(codex["ok"], json!(false));
    let codex_codes: Vec<&str> = codex["reasons"].as_array().unwrap().iter().map(|r| r["reason_code"].as_str().unwrap()).collect();
    assert!(codex_codes.contains(&"version_unavailable"), "not-installed runtime names version_unavailable");
    // dsh: present but unverified + experimental → the routing gap is named
    let dsh = find("dsh");
    assert_eq!(dsh["ok"], json!(false));
    let dsh_codes: Vec<&str> = dsh["reasons"].as_array().unwrap().iter().map(|r| r["reason_code"].as_str().unwrap()).collect();
    assert!(
        dsh_codes.contains(&"capability_experimental") || dsh_codes.contains(&"capability_unverified"),
        "dsh gap is named with a capability reason, got {dsh_codes:?}"
    );
    // fake: verified + boundary → routable
    assert_eq!(find("fake")["ok"], json!(true));
}

#[test]
fn cap_experimental_runtime_never_auto_routes_even_after_real_run() {
    // dsh completes a real loop? not in this fixture — but the gate is structural:
    // note_real_loop preserves the experimental mark from the probe record.
    let env = TestEnv::new();
    // simulate what a real dsh run would write for its version
    let rec = env.core.probe_record("dsh");
    let exe_sha = rec.executable.as_deref().and_then(|p| wagglepost::core::exe_sha256(std::path::Path::new(p)));
    capability::note_real_loop(&env.core.store, "dsh", "0.2.1-rc.5", rec.executable.as_deref(), exe_sha.as_deref(), "writer");
    let rec = env.core.capability_records().into_iter().find(|r| r.adapter == "dsh").unwrap();
    let w = rec.role("writer").unwrap();
    assert!(w.verified, "the run itself is verified transport evidence");
    assert!(!w.auto_route, "experimental roles stay non-routable — a successful run proves transport, not restrictability");
    assert_eq!(rec.state, capability::STATE_VERIFIED, "state reflects the evidence ladder honestly");
}

#[test]
fn cap_offline_contract_never_opens_writer() {
    // offline evidence verifies transport only; without a real loop the writer
    // stays closed even with a boundary declared
    let env = TestEnv::new_with_cfg(|cfg| {
        let stub = cfg.managed_root.join("opencode-stub.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\nif /i \"%1\"==\"--version\" (\r\n  echo opencode 2.2.6\r\n  exit /b 0\r\n)\r\nexit /b 73\r\n",
        )
        .unwrap();
        cfg.opencode_exe = stub;
        cfg.opencode_version = Some("opencode 2.2.6".into());
        cfg.opencode_model_configured = true; // injected: the state must not depend on the host key
    });
    env.seed_capability("opencode", "opencode 2.2.6", &[]);
    let rec = env.core.capability_records().into_iter().find(|r| r.adapter == "opencode").unwrap();
    // no verified roles → the record does not claim a verified state at all
    assert_eq!(rec.state, capability::STATE_PROBED, "an evidence record with zero verified roles claims nothing");
    assert!(!rec.routable("writer"), "offline evidence alone never opens the writer");
    assert!(!rec.routable("reviewer"), "no roles were verified");
}

#[test]
fn cap_adapters_and_capabilities_share_one_semantics() {
    // /v1/adapters and doctor.capabilities must agree (H1: 前端与 README 共享同一语义)
    let env = codex_stub_env();
    let adapters = env.req("GET", "/v1/adapters", None, None).unwrap()["adapters"]
        .as_array()
        .unwrap()
        .clone();
    let caps = env.req("GET", "/v1/doctor", None, None).unwrap()["capabilities"]
        .as_array()
        .unwrap()
        .clone();
    for a in &adapters {
        let id = a["id"].as_str().unwrap();
        let c = caps.iter().find(|r| r["adapter"] == json!(id)).unwrap();
        assert_eq!(a["state"], c["state"], "{id} state agrees between legacy rows and the capability table");
        assert_eq!(
            a["available"],
            json!(!c["version"].is_null()),
            "{id} availability == observable version"
        );
        // legacy write_supported == capability routable(writer)
        let w = c["roles"].as_array().unwrap().iter().find(|r| r["role"] == json!("writer")).unwrap();
        assert_eq!(
            a["write_supported"],
            w["verified"] == json!(true) && w["auto_route"] == json!(true) && w["experimental"] == json!(false),
            "{id} write gate derives from the same record"
        );
    }
}

#[test]
fn cap_bill_describes_the_actual_launched_body() {
    // U3: the handoff-preview bill (bytes/digest) must describe the EXACT body
    // that dispatch launches — attempt.handoff_digest == bill.sha256, and the
    // previewed byte count equals the launched body's.
    let env = TestEnv::new();
    let resp = env
        .req(
            "POST",
            "/v1/tasks",
            Some(&json!({
                "goal": "bill parity probe (simulated)",
                "repo_path": env.user_repo.to_string_lossy(),
                "recipe": "direct",
                "allowed_adapters": ["fake"],
            })),
            Some("cap-bill"),
        )
        .expect("submit");
    let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
    env.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
    let plan = env.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None).expect("plan");
    let hash = plan["plan"]["summary_hash"].as_str().unwrap().to_string();
    let prev = env.req("GET", &format!("/v1/tasks/{task_id}/handoff-preview?role=writer"), None, None).unwrap();
    let bill = &prev["bill"];
    let preview_digest = prev["approved_digest"].as_str().unwrap().to_string();
    assert_eq!(bill["sha256"], json!(preview_digest), "bill digest == previewed body digest");
    assert!(bill["bytes"].as_u64().unwrap() > 0);
    assert!(bill["comparison_bytes"].is_u64(), "the comparison baseline is reported (对照)");
    assert_eq!(bill["included"].as_array().unwrap().len() >= 3, true, "goal/acceptance/scope included");
    // approve with the previewed digest, dispatch, then bind
    env.approve_current(&task_id, &hash).expect("approve");
    let d = env.req("POST", &format!("/v1/tasks/{task_id}/dispatch"), Some(&json!({})), Some("cap-bill-d")).unwrap();
    let aid = d["receipt"]["attempt_id"].as_str().unwrap().to_string();
    let attempt = env.core.store.get_attempt(&aid).unwrap();
    assert_eq!(attempt.handoff_digest.as_deref(), Some(preview_digest.as_str()),
        "the launched body IS the billed body");
    // identity block binds the revision the user saw
    assert_eq!(prev["identity"]["task_revision"], json!(env.revision_of(&task_id)));
}
