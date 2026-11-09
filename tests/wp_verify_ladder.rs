//! WP22 · verification ladder fail-closed tests: help flags alone never promote a
//! role; the OpenCode role requires the real debug-config rule probe; nonzero
//! exits, timeouts and injected candidate configs all refuse. No model calls —
//! the stub .cmd stands in for the vendor binary.

mod common;
use common::TestEnv;

fn opencode_env(extra: &str) -> std::sync::Arc<TestEnv> {
    // a stub that plays the contract: --version answers; `run --help` carries the
    // transport flags; `debug config --standalone` prints a configurable document
    // list built from OC_STUB_CONFIG_JSON (default: managed deny rules only)
    TestEnv::new_with_cfg(move |cfg| {
        let stub = cfg.managed_root.join("opencode-contract-stub.cmd");
        let body = format!(
            "@echo off\r\n\
if /i \"%~1\"==\"--version\" (\r\n  echo opencode 2.2.6\r\n  exit /b 0\r\n)\r\n\
if /i \"%~1\"==\"run\" (\r\n  if /i \"%~2\"==\"--help\" (\r\n    echo Usage: opencode run [flags]\r\n    echo   --standalone           private server\r\n    echo   --format choice        default or json\r\n    echo   --session, -s string   session id\r\n    exit /b 0\r\n  )\r\n)\r\n\
if /i \"%~1\"==\"debug\" (\r\n  if /i \"%~2\"==\"config\" (\r\n    echo [{{\"type\":\"document\",\"path\":\"%XDG_CONFIG_HOME:\\=\\\\%\\\\opencode\\\\opencode.json\",\"info\":{{\"permissions\":[{{\"action\":\"edit\",\"resource\":\"*\",\"effect\":\"deny\"}},{{\"action\":\"shell\",\"resource\":\"*\",\"effect\":\"deny\"}},{{\"action\":\"webfetch\",\"resource\":\"*\",\"effect\":\"deny\"}},{{\"action\":\"subagent\",\"resource\":\"*\",\"effect\":\"deny\"}},{{\"action\":\"external_directory\",\"resource\":\"*\",\"effect\":\"deny\"}}]}}}}{extra}]\r\n    exit /b 0\r\n  )\r\n)\r\n\
exit /b 1\r\n",
            extra = extra
        );
        std::fs::write(&stub, body).unwrap();
        cfg.opencode_exe = stub;
        cfg.opencode_version = Some("opencode 2.2.6".into());
    })
}

#[test]
fn wp22_verify_records_reviewer_when_boundary_probe_holds() {
    // the stub echoes the managed config dir the probe actually set
    // (via %XDG_CONFIG_HOME%) and loads no other document → the reviewer role
    // is recorded, bound to the exact version AND binary
    let env = opencode_env("");
    let rec = env.core.verify_adapter("opencode").expect("boundary probe holds");
    assert_eq!(rec.state, "verified");
    assert!(rec.routable("reviewer"), "the probe-backed record opens the reviewer");
    assert!(!rec.routable("writer"), "the writer stays experimental");
    let raw = env.core.store.meta_get("capability_verified:opencode").unwrap();
    let v: wagglepost::capability::VerificationRecord = serde_json::from_str(&raw).unwrap();
    assert_eq!(v.roles, vec!["reviewer".to_string()]);
    assert_eq!(v.version, "opencode 2.2.6");
    assert!(v.exe_path.unwrap().ends_with("opencode-contract-stub.cmd"));
}

#[test]
fn wp22_injected_candidate_config_fails_verification() {
    // the stub appends a SECOND document (a candidate allow list) → refuse
    let env = opencode_env(
        ",{\"type\":\"document\",\"path\":\"C:\\\\candidate\\\\repo\\\\opencode.json\",\"info\":{\"permissions\":[{\"action\":\"edit\",\"resource\":\"*\",\"effect\":\"allow\"}]}}",
    );
    let err = env.core.verify_adapter("opencode").err().expect("injected config must refuse");
    assert!(format!("{err:?}").contains("non-managed config") || format!("{err:?}").contains("verify_failed"),
        "refusal names the injection: {err:?}");
    // and nothing was recorded: no routable role
    let rec = env.core.capability_records().into_iter().find(|r| r.adapter == "opencode").unwrap();
    assert!(!rec.routable("reviewer"), "failed verification must not open the reviewer");
}

#[test]
fn wp22_nonzero_exit_refuses() {
    // a stub whose run --help answers fine but debug config exits 3
    let env = TestEnv::new_with_cfg(|cfg| {
        let stub = cfg.managed_root.join("opencode-fail-stub.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\n\
if /i \"%~1\"==\"--version\" (\r\n  echo opencode 2.2.6\r\n  exit /b 0\r\n)\r\n\
exit /b 3\r\n",
        )
        .unwrap();
        cfg.opencode_exe = stub;
        cfg.opencode_version = Some("opencode 2.2.6".into());
    });
    let err = env.core.verify_adapter("opencode").err().expect("nonzero probe must refuse");
    assert!(format!("{err:?}").contains("exit"), "refusal names the exit: {err:?}");
    let rec = env.core.capability_records().into_iter().find(|r| r.adapter == "opencode").unwrap();
    assert_eq!(rec.state, "probed", "state stays probed (probe-only evidence)");
    assert!(!rec.routable("reviewer"));
}

#[test]
fn wp22_help_flags_alone_cannot_open_roles_for_codex() {
    // codex-style stub: help has --sandbox/--json → transport probe passes, but
    // roles stay EMPTY because no permission-boundary probe exists for codex
    let env = TestEnv::new_with_cfg(|cfg| {
        let stub = cfg.managed_root.join("codex-contract-stub.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\nif /i \"%~1\"==\"--version\" (\r\n  echo codex-cli 1.2.3\r\n  exit /b 0\r\n)\r\nif /i \"%~1\"==\"exec\" (\r\n  if /i \"%~2\"==\"--help\" (\r\n    echo   --sandbox   sandbox mode\r\n    echo   --json      json output\r\n    exit /b 0\r\n  )\r\n)\r\nexit /b 1\r\n",
        )
        .unwrap();
        cfg.codex_exe = stub;
        cfg.codex_version = Some("codex-cli 1.2.3".into());
    });
    let rec = env.core.verify_adapter("codex").expect("transport probe passes");
    assert_eq!(rec.state, "probed", "help evidence keeps the state at probed");
    assert!(!rec.routable("writer"), "help flags never open the writer");
    assert!(!rec.routable("reviewer"), "help flags never open the reviewer");
    let raw = env.core.store.meta_get("capability_verified:codex").unwrap();
    let v: wagglepost::capability::VerificationRecord = serde_json::from_str(&raw).unwrap();
    assert!(v.roles.is_empty(), "codex offline evidence records no roles");
    assert!(v.exe_path.is_some(), "the record binds the binary path");
}

#[test]
fn wp22_version_or_binary_drift_degrades_even_after_verify() {
    let env = opencode_env(","); // trailing comma edge → JSON with one doc; still valid shape
    let _ = env.core.verify_adapter("opencode"); // may fail (foreign path) — either way
    // simulate a verified record bound to another version, then drift
    env.seed_capability("opencode", "opencode 1.0.0", &["reviewer"]);
    let mut probe = env.core.probe_record("opencode");
    probe.version = Some("opencode 9.9.9".into());
    let ver = wagglepost::capability::load_verified(&env.core.store, "opencode");
    let composed = wagglepost::capability::ProbeRecord::compose(probe, ver.as_ref());
    assert!(!composed.routable("reviewer"), "version drift degrades the role");
    // binary drift: same version, different exe
    env.seed_capability("opencode", "opencode 2.2.6", &["reviewer"]);
    let mut probe = env.core.probe_record("opencode");
    probe.executable = Some("D:\\elsewhere\\opencode.exe".into());
    let ver = wagglepost::capability::load_verified(&env.core.store, "opencode");
    let composed = wagglepost::capability::ProbeRecord::compose(probe, ver.as_ref());
    assert!(!composed.routable("reviewer"), "binary drift degrades the role");
    assert!(composed.probe_error.unwrap_or_default().contains("binary"), "the degradation names the binary binding");
}

#[test]
fn wp22_real_loop_requires_same_binary_and_version() {
    let env = TestEnv::new();
    // a real loop on (version A, path P, sha S) authorizes only that identity
    let rec0 = env.core.probe_record("opencode");
    let version = rec0.version.clone().unwrap_or_default();
    assert!(!version.is_empty(), "the fixture host must have opencode discoverable for this test");
    let sha = "a1b2c3d4e5f6a7b8a1b2c3d4e5f6a7b8a1b2c3d4e5f6a7b8a1b2c3d4e5f6a7b8";
    let path = rec0.executable.clone().unwrap_or_default();
    wagglepost::capability::note_real_loop(&env.core.store, "opencode", &version, Some(&path), Some(sha), "reviewer");
    // WP26 gate: same path but DIFFERENT content hash → refused + revoked
    let err = env
        .core
        .check_dispatch_authorization("opencode", wagglepost::model::Role::Reviewer, Some(path.as_str()))
        .err()
        .expect("the live binary at P hashes differently from the bound sha in this offline fixture");
    assert_eq!(err.code, "adapter_identity_drift", "the gate names content drift: {err:?}");
    let ver = wagglepost::capability::load_verified(&env.core.store, "opencode").unwrap();
    assert!(ver.roles.is_empty(), "identity failure revokes the role (no stale authorization)");
    // a record bound to ANOTHER path never authorizes this one (compose degrades
    // on path drift before the gate even reaches the content hash)
    wagglepost::capability::note_real_loop(&env.core.store, "opencode", &version, Some(r"D:\other\opencode.exe"), Some(sha), "reviewer");
    let err = env
        .core
        .check_dispatch_authorization("opencode", wagglepost::model::Role::Reviewer, Some(path.as_str()))
        .err()
        .expect("path drift refuses");
    assert_eq!(err.code, "capability_not_routable", "path-drifted records degrade before the hash gate");
    // (the drifted record keeps its roles but they are inert: compose degrades
    // them against the live binary, so they can never authorize a launch)
    // exact identity match authorizes
    let real_sha = wagglepost::core::exe_sha256(std::path::Path::new(
        env.core.probe_record("opencode").executable.as_deref().unwrap_or(""),
    ));
    let rec = env.core.probe_record("opencode");
    let live_path = rec.executable.clone().unwrap_or_default();
    let live_sha = real_sha.unwrap_or_default();
    wagglepost::capability::note_real_loop(&env.core.store, "opencode", &version, Some(&live_path), Some(&live_sha), "reviewer");
    env.core
        .check_dispatch_authorization("opencode", wagglepost::model::Role::Reviewer, Some(&live_path))
        .expect("matching version+path+sha authorizes");
}

#[test]
fn wp26_dispatch_gate_refuses_unverified_roles() {
    let env = TestEnv::new();
    // no verification record at all → the gate refuses (help strings never help)
    let err = env
        .core
        .check_dispatch_authorization("opencode", wagglepost::model::Role::Reviewer, None)
        .err()
        .expect("unverified role must refuse");
    assert_eq!(err.code, "capability_not_routable");
    // and the fake runtime (in-process) always passes its own gate
    env.core
        .check_dispatch_authorization("fake", wagglepost::model::Role::Writer, None)
        .expect("fake has no binary identity to bind");
}

#[test]
fn wp22_dsh_and_opencode_probe_records_never_claim_resume() {
    // opencode: per-attempt data isolation → resume not claimed
    let env = opencode_env(",");
    let rec = env.core.probe_record("opencode");
    assert_eq!(rec.same_session_resume, "none", "opencode resume is unproven (WP22)");
    assert_eq!(rec.state, "probed");
}

#[test]
fn wp26_verify_failure_revokes_previous_roles() {
    // a stub whose debug-config output carries an injected candidate document:
    // the verification FAILS and must revoke the previously granted role
    let dir = std::env::temp_dir().join(format!("wp26-revoke-{}", &wagglepost::ids::new_token()[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let env = TestEnv::new_with_cfg(|cfg| {
        let stub = cfg.managed_root.join("opencode-inject-stub.cmd");
        // raw strings keep the .cmd content readable; CRLF joined explicitly
        let lines: Vec<&str> = vec![
            r#"@echo off"#,
            r#"if /i "%~1"=="--version" ("#,
            r#"  echo opencode 2.2.6"#,
            r#"  exit /b 0"#,
            r#")"#,
            r#"if /i "%~1"=="debug" ("#,
            r#"  if /i "%~2"=="config" ("#,
            r#"    echo [{"type":"document","path":"%XDG_CONFIG_HOME:\=\\%\\opencode\\opencode.json","info":{"permissions":[{"action":"edit","resource":"*","effect":"deny"}]}}"#,
            r#"    echo ,{"type":"document","path":"C:\\candidate\\repo\\opencode.json","info":{"permissions":[{"action":"edit","resource":"*","effect":"allow"}]}}]"#,
            r#"    exit /b 0"#,
            r#"  )"#,
            r#")"#,
            r#"exit /b 1"#,
        ];
        std::fs::write(&stub, lines.join("\r\n") + "\r\n").unwrap();
        cfg.opencode_exe = stub;
        cfg.opencode_version = Some("opencode 2.2.6".into());
        cfg.opencode_model_configured = true;
    });
    // grant a role first (as a prior successful verify would have)
    env.seed_capability("opencode", "opencode 2.2.6", &["reviewer"]);
    let rec = env.core.capability_records().into_iter().find(|r| r.adapter == "opencode").unwrap();
    assert!(rec.routable("reviewer"), "precondition: seeded role routable");
    let err = env.core.verify_adapter("opencode").err();
    assert!(err.is_some(), "the injected-config stub fails verification");
    let rec = env.core.capability_records().into_iter().find(|r| r.adapter == "opencode").unwrap();
    assert!(!rec.routable("reviewer"), "a failed verification must revoke the previously granted role");
    std::fs::remove_dir_all(&dir).ok();
}
