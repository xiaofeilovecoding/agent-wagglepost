//! Fake agent body shared by the `wagglepost __fake-agent` subcommand and the
//! `wagglepost-fake-agent` helper binary used by integration tests.

use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

pub fn run(args: &[String]) -> i32 {
    let flag_value = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let script_path = match flag_value("--script") {
        Some(p) => p,
        None => {
            eprintln!("fake-agent: --script required");
            return 3;
        }
    };
    let capture_path = flag_value("--capture");
    // read the full handoff body from stdin and capture it (G7a evidence)
    let mut body = String::new();
    let _ = std::io::stdin().read_to_string(&mut body);
    if let Some(cap) = capture_path {
        if let Some(parent) = std::path::Path::new(&cap).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cap, &body);
    }
    let script: Value = match std::fs::read_to_string(&script_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => {
            eprintln!("fake-agent: bad script");
            return 3;
        }
    };
    let out = |v: Value| {
        println!("{}", serde_json::to_string(&v).unwrap_or_default());
        use std::io::Write;
        let _ = std::io::stdout().flush();
    };
    out(json!({"fake": "1", "type": "started", "session_id": format!("fake-session-{}", &crate::ids::new_token()[..8]), "simulated": true}));
    let behaviors = script
        .get("behaviors")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();
    let has = |name: &str| behaviors.iter().any(|b| b.as_str() == Some(name));
    if has("duplicate_events") {
        let ev = json!({"fake": "1", "type": "duplicate", "key": script.get("dup_key").cloned().unwrap_or(json!("probe"))});
        out(ev.clone());
        out(ev);
    }
    if has("tool") {
        out(json!({"fake": "1", "type": "tool", "name": "Read", "detail": "reading task inputs", "simulated": true}));
    }
    if has("write_files") {
        if let Some(files) = script.get("files").and_then(|f| f.as_object()) {
            for (path, content) in files {
                let content = content.as_str().unwrap_or("");
                let full = std::path::Path::new(path);
                if let Some(parent) = full.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let ok = std::fs::write(full, content).is_ok();
                out(json!({"fake": "1", "type": "file_write", "path": path, "ok": ok, "simulated": true}));
            }
        }
    }
    if has("hang") {
        let secs = script.get("hang_secs").and_then(|h| h.as_u64()).unwrap_or(600);
        std::thread::sleep(Duration::from_secs(secs));
    }
    if let Some(msgs) = script.get("messages").and_then(|m| m.as_array()) {
        for m in msgs {
            out(json!({"fake": "1", "type": "message", "text": m.as_str().unwrap_or(""), "simulated": true}));
        }
    }
    if let Some(result) = script.get("result") {
        out(json!({"fake": "1", "type": "result", "result": result, "usage": {"input_tokens": 0, "output_tokens": 0, "simulated": true}}));
    }
    let exit_code = script.get("exit_code").and_then(|e| e.as_i64()).unwrap_or(0);
    if has("quota_block") {
        // quota simulation: blocked result + nonzero exit
        out(json!({"fake": "1", "type": "result", "result": {"status": "blocked", "quota_blocked": true, "blocked_reason": "simulated quota exhausted"}, "simulated": true}));
        return 42;
    }
    exit_code.clamp(0, 255) as i32
}
