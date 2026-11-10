#![cfg(windows)]

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use wagglepost::adapters::opencode::{assert_boundary_holds, write_reviewer_config};
use wagglepost::runner::{bounded_probe, filter_env, LINE_CAP};

#[test]
fn reviewer_preflight_rejects_unknown_sources_and_policy_changes() {
    let root = std::env::temp_dir().join(wagglepost::ids::new_token());
    let file = write_reviewer_config(&root).unwrap();
    let info = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let source = json!({"type":"document","path":file,"info":info});
    let directory = json!({"type":"directory","path":file.parent().unwrap()});
    let path = file.to_str().unwrap();
    assert!(assert_boundary_holds(&json!([source, directory]).to_string(), path).is_ok());

    let mut extra = source.clone();
    extra["path"] = json!(root.with_extension("other").join("opencode/opencode.json"));
    assert!(assert_boundary_holds(&json!([source, extra]).to_string(), path).is_err());
    assert!(assert_boundary_holds(&json!([source, {"type":"directory","path":root}]).to_string(), path).is_err());
    assert!(assert_boundary_holds(&json!([source, {"type":"plugin","path":file}]).to_string(), path).is_err());

    let mut narrowed = source.clone();
    narrowed["info"]["permissions"][0]["resource"] = json!("some-directory/*");
    assert!(assert_boundary_holds(&json!([narrowed]).to_string(), path).is_err());
    let mut reordered = source.clone();
    reordered["info"]["permissions"].as_array_mut().unwrap().swap(0, 1);
    assert!(assert_boundary_holds(&json!([reordered]).to_string(), path).is_err());
    let mut plugin = source;
    plugin["info"]["plugins"] = json!(["unexpected"]);
    assert!(assert_boundary_holds(&json!([plugin]).to_string(), path).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

const FIXTURE: &str = "WAGGLEPOST_PROBE_FIXTURE_MODE";
const CANARY: &str = "WAGGLEPOST_PROBE_FIXTURE_CANARY";
const CHILD_ARGS: &[&str] = &["--exact", "probe_process_fixture", "--nocapture"];

#[test]
fn probe_process_fixture() {
    match std::env::var(FIXTURE).as_deref() {
        Ok("oversize") => {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            out.write_all(&vec![b'x'; LINE_CAP + 1]).unwrap();
            writeln!(out, "\nvalid-looking-output").unwrap();
        }
        Ok("spawn-child") => {
            let _child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(CHILD_ARGS)
                .env(FIXTURE, "child")
                .spawn()
                .unwrap();
        }
        Ok("child") => {
            std::thread::sleep(Duration::from_millis(1400));
            std::fs::write(PathBuf::from(std::env::var_os(CANARY).unwrap()), "survived").unwrap();
        }
        _ => {}
    }
}

#[test]
fn probe_rejects_oversized_frame_and_terminates_descendants() {
    let exe = std::env::current_exe().unwrap();
    let oversized = filter_env(&[(FIXTURE.into(), "oversize".into())]);
    let err = bounded_probe(&exe, CHILD_ARGS, None, &oversized, Duration::from_secs(5), |s| {
        s.contains("valid-looking-output")
    }).unwrap_err();
    assert!(err.contains("cap"), "{err}");

    let path = std::env::temp_dir().join(wagglepost::ids::new_token());
    let env = filter_env(&[(FIXTURE.into(), "spawn-child".into()), (CANARY.into(), path.to_string_lossy().into_owned())]);
    let err = bounded_probe(&exe, CHILD_ARGS, None, &env, Duration::from_millis(600), |_| true).unwrap_err();
    assert!(err.contains("process tree was terminated"), "{err}");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(!path.exists(), "grandchild escaped the probe job");
}

#[test]
fn snapshot_retries_a_transient_windows_share_violation() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = std::env::temp_dir().join(wagglepost::ids::new_token());
    let source = root.join("source");
    let snapshot = root.join("snapshot");
    std::fs::create_dir_all(&source).unwrap();
    let path = source.join("lib.txt");
    std::fs::write(&path, "candidate contents").unwrap();
    let exclusive = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(65));
        drop(exclusive);
    });
    wagglepost::adapters::opencode::prepare_snapshot(&source, &snapshot).unwrap();
    release.join().unwrap();
    assert_eq!(std::fs::read_to_string(snapshot.join("lib.txt")).unwrap(), "candidate contents");
    std::fs::remove_dir_all(root).unwrap();
}
