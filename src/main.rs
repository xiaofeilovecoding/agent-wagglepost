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
        "submit" | "status" | "inbox" | "request" | "answer" | "why" | "dispatch" | "accept" | "cancel" | "plan" | "approve" | "verify" => cmd_client(cmd, rest),
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
        "wagglepost 0.2 — 蜂递：本地多 Agent 路由 / local multi-agent router\n\
         \n\
         USAGE:\n\
           wagglepost serve [--port N] [--managed-root DIR] [--cli-repo DIR]...\n\
           wagglepost doctor [--json]                            runtime capability check\n\
           wagglepost verify <adapter>                           offline no-account capability contract\n\
           wagglepost submit --file draft.json --json            submit a task draft\n\
           wagglepost status <task_id> --json                    task snapshot\n\
           wagglepost inbox [--after SEQ] --json                 inbox messages\n\
           wagglepost request show <request_id> --json           request reconciliation\n\
           wagglepost answer <decision_id> <option_id> [--json]  preview the relay body (nothing sent)\n\
           wagglepost answer <decision_id> <option_id> --digest <d>\n\
                                                                 confirm with the reviewed digest\n\
           wagglepost why <task_id> --json                       route reasons + events\n\
           wagglepost dispatch <task_id> --json                  dispatch next step\n\
           wagglepost approve <task_id> <plan_hash> [--json]     preview the first-run body\n\
           wagglepost approve <task_id> <plan_hash> --digest <d> confirm revision + digest\n\
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
    // CLI write scope. `--cli-repo DIR` may be repeated; the environment form is
    // a path-list separated by ';'. An empty list keeps the legacy local CLI
    // unrestricted, but the two tokens are always distinct.
    let mut cli_allowed_repos: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--cli-repo" {
            if let Some(v) = args.get(i + 1) {
                cli_allowed_repos.push(v.clone());
            }
            i += 2;
            continue;
        }
        if let Some(v) = args[i].strip_prefix("--cli-repo=") {
            cli_allowed_repos.push(v.to_string());
        }
        i += 1;
    }
    if let Ok(env_repos) = std::env::var("WAGGLEPOST_CLI_ALLOWED_REPOS") {
        cli_allowed_repos.extend(
            env_repos
                .split(';')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
        );
    }
    cli_allowed_repos.sort();
    cli_allowed_repos.dedup();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async move {
        let managed_root = wagglepost::worktree::default_managed_root();
        std::fs::create_dir_all(&managed_root).ok();
        // WP06: the instance lock is acquired BEFORE Store::open / recover / any
        // authentication material (startup code, cli-session) is created or replaced.
        let _guard = match wagglepost::instance_lock::acquire(&managed_root) {
            Ok(g) => g,
            Err(msg) => {
                eprintln!("refusing to start: {msg}");
                return 3;
            }
        };
        let db_path = managed_root.join("wagglepost.db");
        let store = match Store::open(&db_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("failed to open store: {e}");
                return 3;
            }
        };
        let cfg = CoreConfig::probe();
        let core = Core::new(store, cfg);
        let removed_logs = core.cleanup_old_logs();
        if removed_logs > 0 {
            eprintln!("[logs] removed {removed_logs} expired attempt log(s)");
        }
        let notes = core.recover();
        for n in notes {
            eprintln!("[recover] {n}");
        }
        // bind BEFORE handing out credentials: a failed bind must not mint a new session
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("failed to bind {addr}: {e}");
                return 3;
            }
        };
        let session_token = new_token();
        let cli_token = new_token();
        let csrf = new_token();
        let startup_code = wagglepost::ids::new_token()[..8].to_string();
        core.store.meta_set("startup_code", &startup_code);
        let cli_file = managed_root.join("cli-session.json");
        let _ = std::fs::write(
            &cli_file,
            json!({
                "port": port,
                "token": cli_token,
                "csrf": csrf,
                "allowed_repos": cli_allowed_repos.clone(),
            })
            .to_string(),
        );
        let state = Arc::new(AppState {
            core: core.clone(),
            session_token,
            cli_token,
            cli_allowed_repos: cli_allowed_repos.clone(),
            csrf_token: csrf,
            port,
        });
        let ui_router = router(state).fallback(wagglepost::http::api_or_static_fallback);
        eprintln!("蜂递 WagglePost serving on http://127.0.0.1:{port}");
        eprintln!("startup code: {startup_code}");
        eprintln!("managed root: {}", managed_root.display());
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
            let state = a["state_label_zh"].as_str().unwrap_or("");
            let exp = if a["writer_experimental"].as_bool().unwrap_or(false)
                || a["reviewer_experimental"].as_bool().unwrap_or(false)
            {
                " 实验/仅检测"
            } else {
                ""
            };
            println!(
                "  {id:<8} {} {} {} [{state}{exp}]",
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
        println!(
            "  logs     {} files / {} bytes / retention {}d",
            doc["logs"]["files"].as_u64().unwrap_or(0),
            doc["logs"]["bytes"].as_u64().unwrap_or(0),
            doc["logs"]["retention_days"].as_u64().unwrap_or(0)
        );
        if let Some(free) = doc["free_bytes"].as_u64() {
            println!(
                "  disk     {} free / {} reserve",
                free,
                doc["min_free_bytes"].as_u64().unwrap_or(0)
            );
        }
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
    http_req_rev(port, token, csrf, method, path, body, idem, None)
}

/// Same request with an explicit optimistic-concurrency binding. Used by the
/// first-approval command so the revision the user saw travels with the digest.
fn http_req_rev(
    port: u16,
    token: &str,
    csrf: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    idem: Option<&str>,
    if_match: Option<u64>,
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
    if let Some(rev) = if_match {
        req = req.set("If-Match", &format!("\"{rev}\""));
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
            let open = http_req(port, &token, &csrf, "GET", "/v1/decisions?status=open", None, None)
                .ok()
                .and_then(|v| {
                    v["decisions"]
                        .as_array()?
                        .iter()
                        .find(|d| d["decision_id"].as_str() == Some(id.as_str()))
                        .cloned()
                });
            let Some(open) = open else {
                eprintln!("no open decision card found for this id");
                return 1;
            };
            let task_id = open["task_id"].as_str().unwrap_or("").to_string();
            let rev = open["revision"].as_u64().unwrap_or(0);
            // WP09 CLI: preview the exact relay body (with this choice applied) and
            // print it. NOTHING is submitted without the user passing the digest.
            let prev = match http_req(
                port,
                &token,
                &csrf,
                "GET",
                &format!("/v1/tasks/{task_id}/handoff-preview?decision={id}&option={opt}"),
                None,
                None,
            ) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("preview failed: {e}");
                    return 1;
                }
            };
            let digest = prev["approved_digest"].as_str().unwrap_or("").to_string();
            let body_text = prev["handoff"]["text"].as_str().unwrap_or("");
            let body_bytes = prev["handoff"]["bytes"].as_u64().unwrap_or(0);
            if let Some(user_digest) = flag_value(&args, "--digest") {
                // confirm step: submit with the USER-PASSED digest (never auto-fetched)
                let rid = format!("cli-{}", wagglepost::ids::new_token()[..12].to_string());
                http_req(
                    port,
                    &token,
                    &csrf,
                    "POST",
                    &format!("/v1/decisions/{id}/answer"),
                    Some(&json!({"option_id": opt, "revision": rev, "handoff_digest": user_digest})),
                    Some(&rid),
                )
            } else if args.iter().any(|a| a == "--json") {
                // 晚间复审 WP09: machine mode prints ONE parseable object (the
                // prose preview used to precede it, breaking JSON parsers);
                // nothing is submitted here either.
                Ok(json!({
                    "previewed": true,
                    "decision_id": id,
                    "option_id": opt,
                    "revision": rev,
                    "task_id": task_id,
                    "digest": digest,
                    "handoff": {"text": body_text, "bytes": body_bytes},
                }))
            } else {
                println!("=== relay body ({} bytes) ===", body_bytes);
                println!("{}", body_text);
                println!("=== digest: {digest} ===");
                println!("re-run with --digest {digest} to approve and queue");
                Ok(json!({"previewed": true, "digest": digest}))
            }
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
        "verify" => {
            let id = args.first().cloned().unwrap_or_default();
            if id.is_empty() {
                eprintln!("verify needs an adapter id (opencode | dsh | codex | claude | fake)");
                return 2;
            }
            http_req(port, &token, &csrf, "POST", &format!("/v1/adapters/{id}/verify"), Some(&json!({})), None)
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
            // WP15: bind the current task revision + the previewed body digest.
            let snap = match http_req(port, &token, &csrf, "GET", &format!("/v1/tasks/{id}"), None, None) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{e}");
                    return 1;
                }
            };
            let rev = snap["task"]["revision"].as_u64().unwrap_or(0);
            let preview = match http_req(
                port,
                &token,
                &csrf,
                "GET",
                &format!("/v1/tasks/{id}/handoff-preview?role=writer"),
                None,
                None,
            ) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{e}");
                    return 1;
                }
            };
            let digest = preview["approved_digest"].as_str().unwrap_or("").to_string();
            if let Some(user_digest) = flag_value(&args, "--digest") {
                let rid = format!("cli-{}", wagglepost::ids::new_token()[..12].to_string());
                http_req_rev(
                    port,
                    &token,
                    &csrf,
                    "POST",
                    &format!("/v1/tasks/{id}/plan/approve"),
                    Some(&json!({"summary_hash": hash, "handoff_digest": user_digest})),
                    Some(&rid),
                    Some(rev),
                )
            } else {
                // nothing is submitted: show the body, revision and digest first
                Ok(json!({
                    "previewed": true,
                    "task_id": id,
                    "revision": rev,
                    "digest": digest,
                    "handoff": {
                        "text": preview["handoff"]["text"],
                        "bytes": preview["handoff"]["bytes"],
                    },
                    "next": format!("re-run approve {id} {hash} --digest {digest}"),
                }))
            }
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
