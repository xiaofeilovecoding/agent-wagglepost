//! wagglepost — CLI + server entry. Same binary serves the loopback Web UI and the CLI.
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use wagglepost::core::{Core, CoreConfig};
use wagglepost::http::{router, AppState};
use wagglepost::ids::new_token;
use wagglepost::store::Store;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = real_main(args);
    std::process::exit(code);
}

fn real_main(args: Vec<String>) -> i32 {
    let Some(cmd) = args.first().cloned() else {
        print_usage();
        return 2;
    };
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    match cmd.as_str() {
        "serve" => cmd_serve(rest),
        "doctor" => cmd_doctor(rest),
        "__fake-agent" => cmd_fake_agent(rest),
        // CLI subcommands that talk to a running server
        "submit" | "status" | "inbox" | "request" | "answer" | "why" | "dispatch" | "accept" | "cancel" | "plan" | "approve" => cmd_client(cmd, rest),
        "--help" | "-h" | "help" => {
            print_usage();
            0
        }
        _ => {
            eprintln!("unknown command: {cmd}");
            print_usage();
            2
        }
    }
}

fn print_usage() {
    eprintln!(
        "wagglepost 0.1 — 蜂递：本地多 Agent 路由 / local multi-agent router\n\
         \n\
         USAGE:\n\
           wagglepost serve [--port N] [--managed-root DIR]     start the loopback server + web UI\n\
           wagglepost doctor [--json]                            runtime capability check\n\
           wagglepost submit --file draft.json --json            submit a task draft\n\
           wagglepost status <task_id> --json                    task snapshot\n\
           wagglepost inbox [--after SEQ] --json                 inbox messages\n\
           wagglepost request show <request_id> --json           request reconciliation\n\
           wagglepost answer <decision_id> <option_id> --json    answer a decision card\n\
           wagglepost why <task_id> --json                       route reasons + events\n\
           wagglepost dispatch <task_id> --json                  dispatch next step\n\
           wagglepost accept <task_id> <candidate_hash> --json   accept a candidate\n\
           wagglepost cancel <attempt_id> --json                 cancel an attempt\n\
         \n\
         The server prints a one-time startup code for the Web session."
    );
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

// ---------------- serve ----------------

fn cmd_serve(args: Vec<String>) -> i32 {
    let port: u16 = flag_value(&args, "--port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(8742);
    if let Some(root) = flag_value(&args, "--managed-root") {
        std::env::set_var("WAGGLEPOST_MANAGED_ROOT", root);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async move {
        let db_path = wagglepost::worktree::default_managed_root().join("wagglepost.db");
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let store = match Store::open(&db_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("failed to open store: {e}");
                return 3;
            }
        };
        let cfg = CoreConfig::probe();
        let core = Core::new(store, cfg);
        let notes = core.recover();
        for n in notes {
            eprintln!("[recover] {n}");
        }
        // startup code for the web session (one-time)
        let session_token = new_token();
        let csrf = new_token();
        let startup_code = wagglepost::ids::new_token()[..8].to_string();
        core.store.meta_set("startup_code", &startup_code);
        // CLI channel: token file inside managed root (local user only)
        let cli_file = core.managed_root().join("cli-session.json");
        let _ = std::fs::write(
            &cli_file,
            json!({"port": port, "token": session_token, "csrf": csrf}).to_string(),
        );
        let state = Arc::new(AppState {
            core: core.clone(),
            session_token,
            csrf_token: csrf,
            port,
        });
        // static UI
        let ui_router = router(state).fallback(wagglepost::ui::static_handler);
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("failed to bind {addr}: {e}");
                return 3;
            }
        };
        eprintln!("蜂递 WagglePost serving on http://127.0.0.1:{port}");
        eprintln!("startup code: {startup_code}");
        eprintln!("managed root: {}", core.managed_root().display());
        eprintln!("cli session: {}", cli_file.display());
        // scheduler loop
        let sched_core = core.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                sched_core.scheduler_tick();
            }
        });
        match axum::serve(listener, ui_router).await {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("server error: {e}");
                3
            }
        }
    })
}

// ---------------- doctor ----------------

fn cmd_doctor(args: Vec<String>) -> i32 {
    let cfg = CoreConfig::probe();
    let json_out = has_flag(&args, "--json");
    let store = Store::open_in_memory().expect("memory store");
    let core = Core::new(store, cfg);
    let doc = core.doctor();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&doc).unwrap());
    } else {
        println!("wagglepost doctor");
        for a in doc["adapters"].as_array().unwrap_or(&vec![]) {
            let id = a["id"].as_str().unwrap_or("?");
            let v = a["version"].as_str().unwrap_or("-");
            let ok = a["available"].as_bool().unwrap_or(false);
            let sim = a["simulated"].as_bool().unwrap_or(false);
            println!(
                "  {id:<8} {} {} {}",
                if ok { "OK " } else { "MISSING" },
                v,
                if sim { "(simulated)" } else { "" }
            );
            if let Some(err) = a["probe_error"].as_str() {
                println!("           error: {err}");
            }
        }
        println!(
            "  git      {}",
            doc["git"]["version"].as_str().unwrap_or("-")
        );
        println!(
            "  managed  {} {}",
            doc["managed_root"].as_str().unwrap_or("-"),
            if doc["managed_root_ok"].as_bool().unwrap_or(false) { "OK" } else { "FAIL" }
        );
    }
    0
}

// ---------------- fake agent subcommand ----------------

fn cmd_fake_agent(args: Vec<String>) -> i32 {
    wagglepost::fake_agent::run(&args)
}

// ---------------- client (talks to the local server) ----------------

fn client_target() -> Result<(u16, String, String), String> {
    let root = wagglepost::worktree::default_managed_root();
    let file = root.join("cli-session.json");
    let raw = std::fs::read_to_string(&file).map_err(|e| {
        format!(
            "no cli session at {} (start `wagglepost serve` first): {e}",
            file.display()
        )
    })?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let port = v["port"].as_u64().ok_or("bad cli session")? as u16;
    let token = v["token"].as_str().ok_or("bad token")?.to_string();
    let csrf = v["csrf"].as_str().unwrap_or("").to_string();
    Ok((port, token, csrf))
}

fn http_req(
    port: u16,
    token: &str,
    csrf: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    idem: Option<&str>,
) -> Result<Value, String> {
    let mut req = ureq_request(method, &format!("http://127.0.0.1:{port}{path}"));
    req = req.set("Authorization", &format!("Bearer {token}"));
    if !csrf.is_empty() && method != "GET" {
        req = req.set("X-CSRF-Token", csrf);
        req = req.set("Origin", &format!("http://127.0.0.1:{port}"));
    }
    if let Some(k) = idem {
        req = req.set("Idempotency-Key", k);
    }
    let resp = match body {
        Some(b) => req.send_json(b.clone()),
        None => req.call(),
    };
    match resp {
        Ok(r) => r
            .into_json::<Value>()
            .map_err(|e| format!("bad json response: {e}")),
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            Err(format!("HTTP {code}: {body}"))
        }
        Err(e) => Err(format!("request failed: {e}")),
    }
}

fn ureq_request(method: &str, url: &str) -> ureq::Request {
    match method {
        "POST" => ureq::post(url),
        "PUT" => ureq::put(url),
        "DELETE" => ureq::delete(url),
        _ => ureq::get(url),
    }
}

fn cmd_client(cmd: String, args: Vec<String>) -> i32 {
    let json_out = has_flag(&args, "--json") || true; // CLI always speaks JSON per contract
    let (port, token, csrf) = match client_target() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return 3;
        }
    };
    let result: Result<Value, String> = match cmd.as_str() {
        "submit" => {
            let file = flag_value(&args, "--file");
            let body: Value = match file {
                Some(f) => match std::fs::read_to_string(&f)
                    .ok()
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                {
                    Some(v) => v,
                    None => {
                        eprintln!("cannot read draft {f}");
                        return 2;
                    }
                },
                None => {
                    // inline: goal + repo
                    let goal = args.iter().find(|a| a.starts_with("--goal=")).map(|a| a[7..].to_string());
                    let repo = args.iter().find(|a| a.starts_with("--repo=")).map(|a| a[7..].to_string());
                    match (goal, repo) {
                        (Some(goal), Some(repo)) => json!({"goal": goal, "repo_path": repo}),
                        _ => {
                            eprintln!("submit needs --file draft.json or --goal=... --repo=...");
                            return 2;
                        }
                    }
                }
            };
            let rid = format!("cli-{}", wagglepost::ids::new_token()[..12].to_string());
            http_req(port, &token, &csrf, "POST", "/v1/tasks", Some(&body), Some(&rid))
        }
        "status" => {
            let id = args.first().cloned().unwrap_or_default();
            http_req(port, &token, &csrf, "GET", &format!("/v1/tasks/{id}"), None, None)
        }
        "inbox" => {
            let after = flag_value(&args, "--after").unwrap_or_default();
            http_req(port, &token, &csrf, "GET", &format!("/v1/messages?after={after}"), None, None)
        }
        "request" => {
            let id = args.get(1).cloned().unwrap_or_default();
            http_req(port, &token, &csrf, "GET", &format!("/v1/requests/{id}"), None, None)
        }
        "answer" => {
            let id = args.first().cloned().unwrap_or_default();
            let opt = args.get(1).cloned().unwrap_or_default();
            // revision read from the card first
            let rev = http_req(port, &token, &csrf, "GET", &format!("/v1/decisions?status=open"), None, None)
                .ok()
                .and_then(|v| {
                    v["decisions"]
                        .as_array()?
                        .iter()
                        .find(|d| d["decision_id"].as_str() == Some(id.as_str()))
                        .and_then(|d| d["revision"].as_u64())
                })
                .unwrap_or(0);
            let rid = format!("cli-{}", wagglepost::ids::new_token()[..12].to_string());
            http_req(
                port,
                &token,
                &csrf,
                "POST",
                &format!("/v1/decisions/{id}/answer"),
                Some(&json!({"option_id": opt, "revision": rev})),
                Some(&rid),
            )
        }
        "why" => {
            let id = args.first().cloned().unwrap_or_default();
            let route = http_req(port, &token, &csrf, "GET", &format!("/v1/tasks/{id}/route-preview"), None, None);
            let events = http_req(port, &token, &csrf, "GET", &format!("/v1/tasks/{id}"), None, None);
            Ok(json!({"route": route.unwrap_or(Value::Null), "task": events.unwrap_or(Value::Null)}))
        }
        "dispatch" => {
            let id = args.first().cloned().unwrap_or_default();
            let rid = format!("cli-{}", wagglepost::ids::new_token()[..12].to_string());
            http_req(port, &token, &csrf, "POST", &format!("/v1/tasks/{id}/dispatch"), Some(&json!({})), Some(&rid))
        }
        "accept" => {
            let id = args.first().cloned().unwrap_or_default();
            let cand = args.get(1).cloned().unwrap_or_default();
            let rid = format!("cli-{}", wagglepost::ids::new_token()[..12].to_string());
            http_req(
                port,
                &token,
                &csrf,
                "POST",
                &format!("/v1/tasks/{id}/accept"),
                Some(&json!({"candidate_hash": cand})),
                Some(&rid),
            )
        }
        "cancel" => {
            let id = args.first().cloned().unwrap_or_default();
            let rid = format!("cli-{}", wagglepost::ids::new_token()[..12].to_string());
            http_req(
                port,
                &token,
                &csrf,
                "POST",
                &format!("/v1/attempts/{id}/cancel"),
                Some(&json!({})),
                Some(&rid),
            )
        }
        "plan" => {
            let id = args.first().cloned().unwrap_or_default();
            http_req(port, &token, &csrf, "POST", &format!("/v1/tasks/{id}/plan"), Some(&json!({})), None)
        }
        "approve" => {
            let id = args.first().cloned().unwrap_or_default();
            let hash = args.get(1).cloned().unwrap_or_default();
            http_req(
                port,
                &token,
                &csrf,
                "POST",
                &format!("/v1/tasks/{id}/plan/approve"),
                Some(&json!({"summary_hash": hash})),
                None,
            )
        }
        _ => Err("unknown client command".into()),
    };
    match result {
        Ok(v) => {
            if json_out {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}
