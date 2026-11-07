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
    pub csrf: String,
    pub startup_code_value: String,
    pub core: Arc<Core>,
    pub managed_root: PathBuf,
    pub user_repo: PathBuf,
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
        .unwrap_or_else(|_| PathBuf::from("D:\\dev\\wagglepost-test-data"));
    let dir = base.join(format!("it-{}", &new_token()[..12]));
    std::fs::create_dir_all(&dir).expect("create test root");
    dir
}

impl TestEnv {
    pub fn new() -> Arc<TestEnv> {
        let root = unique_root();
        let managed = root.join("managed");
        let user_repo = root.join("user-repo");
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
                    session_token: "test-token".into(),
                    csrf_token: "test-csrf".into(),
                    port,
                });
                let _ = axum::serve(listener, router(state)).await;
            });
        });
        let SC = format!("code-{}", &new_token()[..8]);
        core.store.meta_set("startup_code", &SC);
        // the in-process server IS a real instance: hold the managed-root lock (WP06)
        let lock = wagglepost::instance_lock::acquire(&managed).expect("instance lock");
        Arc::new(TestEnv {
            port,
            token: "test-token".into(),
            csrf: "test-csrf".into(),
            startup_code_value: SC.clone(),
            core,
            managed_root: managed,
            user_repo,
            _lock: Some(lock),
            _guard: Some(tempdir_guard::Guard(root)),
        })
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn req(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        idem: Option<&str>,
    ) -> Result<Value, (u16, Value)> {
        let mut r = ureq_request(method, &format!("{}{}", self.base(), path))
            .timeout(Duration::from_secs(60))
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
            Err(e) => panic!("request failed: {e}"),
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
        let mut r = ureq_request(method, &format!("{}{}", self.base(), path))
            .timeout(Duration::from_secs(60));
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
            Err(e) => panic!("request failed: {e}"),
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
        self.req(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": hash})),
            None,
        )
        .expect("approve");
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
        self.req(
            "POST",
            &format!("/v1/tasks/{task_id}/reroute"),
            Some(&json!({"target_adapter": target, "handoff_digest": digest})),
            Some(idem),
        )
        .expect("reroute with previewed digest")
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
            self.req("POST", &format!("/v1/tasks/{task_id}/plan/approve"), Some(&json!({"summary_hash": hash})), None).expect("approve");
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
        self.req("POST", &format!("/v1/tasks/{task_id}/plan/approve"), Some(&json!({"summary_hash": hash})), None).expect("approve");
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

fn ureq_request(method: &str, url: &str) -> ureq::Request {
    match method {
        "POST" => ureq::post(url),
        "PUT" => ureq::put(url),
        "DELETE" => ureq::delete(url),
        _ => ureq::get(url),
    }
}

pub fn git(cwd: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("D:\\dev\\Git\\cmd\\git.exe")
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
