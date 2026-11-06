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
    pub core: Arc<Core>,
    pub managed_root: PathBuf,
    pub user_repo: PathBuf,
    _guard: Option<tempdir_guard::Guard>,
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
        Arc::new(TestEnv {
            port,
            token: "test-token".into(),
            csrf: "test-csrf".into(),
            core,
            managed_root: managed,
            user_repo,
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
