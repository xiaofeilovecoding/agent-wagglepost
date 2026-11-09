//! Shared integration harness: in-process axum server on an ephemeral loopback port,
//! temp managed root, temp user repo, bearer-authenticated ureq client.
#![allow(dead_code)]
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use wagglepost::core::{Core, CoreConfig};
use wagglepost::http::{router, AppState};
use wagglepost::ids::new_token;
use wagglepost::store::Store;

pub struct TestEnv {
    pub port: u16,
    pub token: String,
    pub browser_token: String,
    pub csrf: String,
    pub startup_code_value: String,
    pub core: Arc<Core>,
    pub managed_root: PathBuf,
    pub user_repo: PathBuf,
    /// Dedicated agent with connection pooling disabled. Reusing an idle HTTP/1.1
    /// socket across a blocking scheduler tick produced transient Windows 10053
    /// status-line aborts on CI; a fresh connection per request is deterministic.
    pub agent: ureq::Agent,
    _guard: Option<tempdir_guard::Guard>,    _lock: Option<wagglepost::instance_lock::InstanceGuard>,
}

mod tempdir_guard {
    use std::path::PathBuf;
    pub struct Guard(pub PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn unique_root() -> PathBuf {
    let base = std::env::var("WAGGLEPOST_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("wagglepost-tests"));
    let dir = base.join(format!("it-{}", &new_token()[..12]));
    std::fs::create_dir_all(&dir).expect("create test root");
    dir
}

impl TestEnv {
    pub fn new() -> Arc<TestEnv> {
        Self::new_with_cfg(|_| {})
    }

    /// Variant with direct CoreConfig overrides (e.g. the offline codex version
    /// stub used by the WP14 two-fixture reroute test).
    pub fn new_with_cfg(f: impl FnOnce(&mut CoreConfig)) -> Arc<TestEnv> {
        Self::new_with_cfg_repos(f, Vec::new())
    }

    /// CLI repository-scope test fixture: the browser token stays separate.
    pub fn new_with_cli_repos(repos: Vec<String>) -> Arc<TestEnv> {
        Self::new_with_cfg_repos(|_| {}, repos)
    }

    /// Scope the CLI channel to this fixture's user repository.
    pub fn new_with_cli_scope() -> Arc<TestEnv> {
        Self::new_inner(|_| {}, Vec::new(), true)
    }

    pub fn new_with_cfg_repos(
        f: impl FnOnce(&mut CoreConfig),
        cli_allowed_repos: Vec<String>,
    ) -> Arc<TestEnv> {
        Self::new_inner(f, cli_allowed_repos, false)
    }

    fn new_inner(
        f: impl FnOnce(&mut CoreConfig),
        mut cli_allowed_repos: Vec<String>,
        scope_to_user_repo: bool,
    ) -> Arc<TestEnv> {
        let root = unique_root();
        let managed = root.join("managed");
        let user_repo = root.join("user-repo");
        if scope_to_user_repo {
            cli_allowed_repos.push(user_repo.to_string_lossy().to_string());
        }
        std::fs::create_dir_all(&user_repo).unwrap();
        git(&user_repo, &["init", "-q", "-b", "main"]);
        std::fs::write(user_repo.join("lib.txt"), "baseline content\n").unwrap();
        git(&user_repo, &["add", "-A"]);
        git(
            &user_repo,
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"],
        );

        std::fs::create_dir_all(&managed).unwrap();
        let db_path = managed.join("test.db");
        let store = Store::open(&db_path).expect("store");
        let mut cfg = CoreConfig::probe();
        cfg.managed_root = managed.clone();
        cfg.git.managed_root = managed.clone();
        cfg.fake_exe = Some(PathBuf::from(env!("CARGO_BIN_EXE_wagglepost")));
        // tests are independent of the host disk reserve; the disk_low case opts in
        cfg.min_free_bytes = 0;
        f(&mut cfg);
        let core = Core::new(store, cfg);
        let core2 = core.clone();

        // bind synchronously so the port is owned by THIS env only
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = std_listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                std_listener.set_nonblocking(true).unwrap();
                let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
                let state = Arc::new(AppState {
                    core: core2,
                    session_token: "test-browser-token".into(),
                    cli_token: "test-token".into(),
                    cli_allowed_repos: cli_allowed_repos.clone(),
                    csrf_token: "test-csrf".into(),
                    port,
                });
                // mirror production exactly: same fallback (晚间复审 WP13 — the
                // old test env served a bare 404 where production returned the
                // SPA page with 200, hiding the defect)
                let app = router(state).fallback(wagglepost::http::api_or_static_fallback);
                let _ = axum::serve(listener, app).await;
            });
        });
        let startup_code = format!("code-{}", &new_token()[..8]);
        core.store.meta_set("startup_code", &startup_code);
        // the in-process server IS a real instance: hold the managed-root lock (WP06)
        let lock = wagglepost::instance_lock::acquire(&managed).expect("instance lock");
        let agent = ureq::AgentBuilder::new()
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .timeout(Duration::from_secs(60))
            .build();
        Arc::new(TestEnv {
            port,
            token: "test-token".into(),
            browser_token: "test-browser-token".into(),
            csrf: "test-csrf".into(),
            startup_code_value: startup_code.clone(),
            core,
            managed_root: managed,
            user_repo,
            agent,
            _lock: Some(lock),
            _guard: Some(tempdir_guard::Guard(root)),
        })
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// H1: seed a capability verification record for a fixture runtime whose
    /// transport the test itself stands in for (e.g. the offline codex version
    /// stub). Verification is role-scoped and version-bound, exactly like a
    /// real-loop record.
    pub fn seed_capability(&self, adapter: &str, version: &str, roles: &[&str]) {
        // bind the CURRENT probe executable — a fixture stands in for that binary
        let rec = self.core.probe_record(adapter);
        let exe_path = rec.executable.clone();
        let exe_sha256 = exe_path.as_deref().and_then(|p| wagglepost::core::exe_sha256(std::path::Path::new(p)));
        let ver = wagglepost::capability::VerificationRecord {
            version: version.to_string(),
            verified_at: "fixture".into(),
            evidence: wagglepost::capability::EVID_OFFLINE_CONTRACT.into(),
            roles: roles.iter().map(|s| s.to_string()).collect(),
            exe_path,
            exe_sha256,
        };
        wagglepost::capability::save_verified(&self.core.store, adapter, &ver);
    }

    pub fn req(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        idem: Option<&str>,
    ) -> Result<Value, (u16, Value)> {
        let mut r = agent_request(&self.agent, method, &format!("{}{}", self.base(), path))
            .timeout(Duration::from_secs(60))
            .set("Connection", "close")
            .set("Authorization", "Bearer test-token");
        if method != "GET" {
            r = r
                .set("X-CSRF-Token", "test-csrf")
                .set("Origin", &self.base());
        }
        if let Some(k) = idem {
            r = r.set("Idempotency-Key", k);
        }
        let resp = match body {
            Some(b) => r.send_json(b.clone()),
            None => r.call(),
        };
        match resp {
            Ok(r) => {
                let v: Value = r.into_json().unwrap_or(Value::Null);
                Ok(v)
            }
            Err(ureq::Error::Status(code, r)) => {
                let v: Value = r.into_json().unwrap_or(Value::Null);
                Err((code, v))
            }
            Err(e) => {
                if std::env::var_os("WAGGLEPOST_TEST_HTTP_DIAG").is_some() {
                    // Test-only, no bodies/tokens: enough to tell stale connection
                    // (method/path/transport) from a server-side semantic failure.
                    eprintln!("[test-http] {method} {path}: {e}");
                }
                panic!("request failed: {e}")
            }
        }
    }

    /// raw request with explicit headers (for host/origin/csrf security tests)
    pub fn raw(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Result<Value, (u16, Value)> {
        let mut r = agent_request(&self.agent, method, &format!("{}{}", self.base(), path))
            .timeout(Duration::from_secs(60))
            .set("Connection", "close");
        for (k, v) in headers {
            r = r.set(k, v);
        }
        let resp = match body {
            Some(b) => r.send_json(b.clone()),
            None => r.call(),
        };
        match resp {
            Ok(r) => {
                let v: Value = r.into_json().unwrap_or(Value::Null);
                Ok(v)
            }
            Err(ureq::Error::Status(code, r)) => {
                let v: Value = r.into_json().unwrap_or(Value::Null);
                Err((code, v))
            }
            Err(e) => {
                if std::env::var_os("WAGGLEPOST_TEST_HTTP_DIAG").is_some() {
                    // Test-only, no bodies/tokens: enough to tell stale connection
                    // (method/path/transport) from a server-side semantic failure.
                    eprintln!("[test-http] {method} {path}: {e}");
                }
                panic!("request failed: {e}")
            }
        }
    }

    /// full setup: submit + trust + plan + approve; returns task_id and plan hash
    pub fn ready_task(
        &self,
        recipe: &str,
        writer: Option<&str>,
        script: Option<Value>,
    ) -> (String, String) {
        let mut body = json!({
            "goal": "append one demo line to lib.txt (offline simulated)",
            "repo_path": self.user_repo.to_string_lossy(),
            "recipe": recipe,
            "allowed_adapters": ["fake"],
        });
        if let Some(w) = writer {
            body["writer_adapter"] = json!(w);
        }
        let resp = self
            .req(
                "POST",
                "/v1/tasks",
                Some(&body),
                Some(&format!("sub-{}", &new_token()[..8])),
            )
            .expect("submit");
        let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
        self.req(
            "POST",
            &format!("/v1/tasks/{task_id}/trust"),
            Some(&json!({})),
            None,
        )
        .expect("trust");
        let mut plan_body = json!({});
        if let Some(w) = writer {
            plan_body["writer_adapter"] = json!(w);
        }
        let plan = self
            .req(
                "POST",
                &format!("/v1/tasks/{task_id}/plan"),
                Some(&plan_body),
                None,
            )
            .expect("plan");
        if let Some(s) = &script {
            self.core
                .store
                .meta_set(&format!("fake_script:{}:s1", task_id), &s.to_string());
        }
        let hash = plan["plan"]["summary_hash"].as_str().unwrap().to_string();
        self.approve_current(&task_id, &hash).expect("approve");
        (task_id, hash)
    }

    pub fn startup_code(&self) -> String {
        self.startup_code_value.clone()
    }

    /// WP09: fetch the relay-body digest the server will send for the next generation.
    /// When a decision card is being answered, pass its id + chosen option so the
    /// preview includes the exact answer line.
    pub fn relay_digest(&self, task_id: &str, decision: Option<&str>, option: Option<&str>) -> String {
        let mut path = format!("/v1/tasks/{task_id}/handoff-preview?role=writer");
        if let (Some(d), Some(o)) = (decision, option) {
            path.push_str(&format!("&decision={d}&option={o}"));
        }
        self.req("GET", &path, None, None).unwrap()["approved_digest"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Flip the last hex digit so a "wrong digest" is guaranteed different even
    /// when the real digest happens to end in the digit the probe would append.
    pub fn tamper_digest(real: &str) -> String {
        let mut s = real.to_string();
        let last = if s.ends_with('0') { '1' } else { '0' };
        s.pop();
        s.push(last);
        s
    }

    /// WP09: answer a decision card with the previewed body digest (save + queue).
    pub fn answer_card(&self, task_id: &str, option_id: &str, idem: &str) -> Value {
        let decisions = self.req("GET", "/v1/decisions?status=open", None, None).unwrap();
        let card = decisions["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["task_id"] == json!(task_id))
            .cloned()
            .expect("open card for task");
        let digest = self.relay_digest(task_id, Some(card["decision_id"].as_str().unwrap()), Some(option_id));
        self.req(
            "POST",
            &format!("/v1/decisions/{}/answer", card["decision_id"].as_str().unwrap()),
            Some(&json!({
                "option_id": option_id,
                "revision": card["revision"].as_u64().unwrap(),
                "handoff_digest": digest,
            })),
            Some(idem),
        )
        .expect("answer with previewed digest")
    }

    /// WP09: reroute with the previewed relay body digest.
    pub fn reroute_queued(&self, task_id: &str, target: &str, idem: &str) -> Value {
        let digest = self.relay_digest(task_id, None, None);
        let rev = self.revision_of(task_id);
        self.req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/reroute"),
            Some(&json!({"target_adapter": target, "handoff_digest": digest})),
            Some(idem),
            rev,
        )
        .expect("reroute with previewed digest and current revision")
    }


    /// WP14/WP15: the current task revision, for If-Match bindings.
    pub fn revision_of(&self, task_id: &str) -> u64 {
        self.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap()["task"]["revision"]
            .as_u64()
            .unwrap()
    }

    /// WP15: approve like a real client — send the CURRENT revision (If-Match)
    /// and the previewed writer-body digest.
    pub fn approve_current(&self, task_id: &str, hash: &str) -> Result<Value, (u16, Value)> {
        let rev = self.revision_of(task_id);
        let digest = self.relay_digest(task_id, None, None);
        self.req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": hash, "handoff_digest": digest})),
            Some(&format!("test-approve-{}", &new_token()[..12])),
            rev,
        )
    }

    pub fn req_with_if_match(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        idem: Option<&str>,
        revision: u64,
    ) -> Result<Value, (u16, Value)> {
        let mut r = agent_request(&self.agent, method, &format!("{}{}", self.base(), path))
            .timeout(Duration::from_secs(60))
            .set("Connection", "close")
            .set("Authorization", "Bearer test-token")
            .set("If-Match", &format!("\"{revision}\""));
        if method != "GET" {
            r = r
                .set("X-CSRF-Token", "test-csrf")
                .set("Origin", &self.base());
        }
        if let Some(k) = idem {
            r = r.set("Idempotency-Key", k);
        }
        let resp = match body {
            Some(b) => r.send_json(b.clone()),
            None => r.call(),
        };
        match resp {
            Ok(r) => r
                .into_json::<Value>()
                .map_err(|e| (500, json!({"error": {"code": "bad_json", "message": e.to_string()}}))),
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                let v = serde_json::from_str(&body).unwrap_or(Value::Null);
                Err((code, v))
            }
            Err(e) => Err((0, json!({"error": {"code": "transport", "message": e.to_string()}}))),
        }
    }

    pub fn plan_hash(&self, task_id: &str) -> String {
        self.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap()["plan"]["summary_hash"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// submit + trust + plan but NOT approve (caller approves with explicit material)
    pub fn ready_task_unapproved(
        &self,
        recipe: &str,
        writer: Option<&str>,
        script: Option<Value>,
    ) -> (String, String) {
        let (task_id, _) = self.ready_task_with_budget_inner(recipe, writer, script, None, false);
        (task_id, String::new())
    }

    pub fn ready_task_with_budget(
        &self,
        recipe: &str,
        writer: Option<&str>,
        script: Option<Value>,
        resume_budget: u64,
    ) -> (String, String) {
        self.ready_task_with_budget_inner(recipe, writer, script, Some(resume_budget), true)
    }

    pub fn ready_task_with_budget_inner(
        &self,
        recipe: &str,
        writer: Option<&str>,
        script: Option<Value>,
        resume_budget: Option<u64>,
        approve: bool,
    ) -> (String, String) {
        let mut body = json!({
            "goal": "append one demo line to lib.txt (offline simulated)",
            "repo_path": self.user_repo.to_string_lossy(),
            "recipe": recipe,
            "allowed_adapters": ["fake"],
        });
        if let Some(w) = writer {
            body["writer_adapter"] = json!(w);
        }
        if let Some(rb) = resume_budget {
            body["resume_budget"] = json!(rb);
        }
        let resp = self
            .req("POST", "/v1/tasks", Some(&body), Some(&format!("sub-{}", &new_token()[..8])))
            .expect("submit");
        let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
        self.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
        let plan = self.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None).expect("plan");
        if let Some(s) = &script {
            self.core.store.meta_set(&format!("fake_script:{}:s1", task_id), &s.to_string());
        }
        let hash = plan["plan"]["summary_hash"].as_str().unwrap().to_string();
        if approve {
            self.approve_current(&task_id, &hash).expect("approve");
        }
        (task_id, hash)
    }

    pub fn ready_task_scoped(
        &self,
        recipe: &str,
        writer: Option<&str>,
        script: Option<Value>,
        allowed_paths: Vec<String>,
    ) -> (String, String) {
        let mut body = json!({
            "goal": "append one demo line to lib.txt (offline simulated)",
            "repo_path": self.user_repo.to_string_lossy(),
            "recipe": recipe,
            "allowed_adapters": ["fake"],
            "allowed_paths": allowed_paths,
        });
        if let Some(w) = writer {
            body["writer_adapter"] = json!(w);
        }
        let resp = self
            .req("POST", "/v1/tasks", Some(&body), Some(&format!("sub-{}", &new_token()[..8])))
            .expect("submit");
        let task_id = resp["task"]["task_id"].as_str().unwrap().to_string();
        self.req("POST", &format!("/v1/tasks/{task_id}/trust"), Some(&json!({})), None).expect("trust");
        let plan = self.req("POST", &format!("/v1/tasks/{task_id}/plan"), Some(&json!({})), None).expect("plan");
        if let Some(s) = &script {
            self.core.store.meta_set(&format!("fake_script:{}:s1", task_id), &s.to_string());
        }
        let hash = plan["plan"]["summary_hash"].as_str().unwrap().to_string();
        self.approve_current(&task_id, &hash).expect("approve");
        (task_id, hash)
    }

    pub fn set_fake_script(&self, task_id: &str, step: &str, script: &Value) {
        self.core
            .store
            .meta_set(&format!("fake_script:{}:{}", task_id, step), &script.to_string());
    }

    /// dispatch next step and wait until one of the statuses
    pub fn run_until(&self, task_id: &str, statuses: &[&str], timeout: Duration) -> String {
        let start = std::time::Instant::now();
        loop {
            // scheduler-driven: tick via the server runtime between polls
            let _ = self.req("POST", "/v1/__tick", Some(&json!({})), None);
            let snap = self
                .req("GET", &format!("/v1/tasks/{task_id}"), None, None)
                .expect("task snapshot");
            let status = snap["task"]["status"].as_str().unwrap_or("").to_string();
            if statuses.contains(&status.as_str()) {
                return status;
            }
            if start.elapsed() > timeout {
                panic!(
                    "timeout waiting for {statuses:?}; last status {status}; snapshot: {}",
                    serde_json::to_string(&snap).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

fn agent_request(agent: &ureq::Agent, method: &str, url: &str) -> ureq::Request {
    match method {
        "POST" => agent.post(url),
        "PUT" => agent.put(url),
        "DELETE" => agent.delete(url),
        _ => agent.get(url),
    }
}

pub fn git(cwd: &Path, args: &[&str]) -> String {
    let git_exe = wagglepost::worktree::find_in_path("git").unwrap_or_else(|| PathBuf::from("git"));
    let out = std::process::Command::new(git_exe)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git run");
    if !out.status.success() {
        panic!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).to_string()
}
