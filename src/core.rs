//! Core orchestration: tasks, plans, dispatch, attempts, checkpoints, decisions,
//! reroute, accept, requests (idempotency), recovery. Local rules only — zero model
//! calls for routing and handoff assembly.

use crate::adapters::{self, Adapter, DispatchContext, WireEvent};
use crate::events::EventBus;
use crate::handoff::{self, HandoffMode, HandoffParts};
use crate::ids::{new_id, sha256_hex};
use crate::model::*;
use crate::routing::{self, AdapterRuntimeState, RouteInputs};
use crate::store::{timestamp, Store};
use crate::worktree::{self, GitConfig};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct CoreConfig {
    pub git: GitConfig,
    /// executable used to spawn the fake agent (tests pin this to the test binary host)
    pub fake_exe: Option<PathBuf>,
    pub codex_exe: PathBuf,
    pub claude_exe: PathBuf,
    pub codex_version: Option<String>,
    pub claude_version: Option<String>,
    pub dsh: adapters::dsh::DshAdapter,
    pub managed_root: PathBuf,
    pub default_body_budget: usize,
}

impl CoreConfig {
    pub fn probe() -> Self {
        let codex_exe = discover_codex();
        let codex_version = codex_exe
            .as_ref()
            .and_then(|p| probe_version(p, &["exec", "--version"]));
        let claude_exe = discover_claude();
        let claude_version = claude_exe.as_ref().and_then(|p| probe_version(p, &["--version"]));
        CoreConfig {
            git: GitConfig::default(),
            fake_exe: None,
            codex_exe: codex_exe.unwrap_or_default(),
            claude_exe: claude_exe.unwrap_or_default(),
            codex_version,
            claude_version,
            dsh: adapters::dsh::probe(),
            managed_root: worktree::default_managed_root(),
            default_body_budget: handoff::DEFAULT_BODY_BUDGET,
        }
    }
}

fn discover_codex() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("WAGGLEPOST_CODEX_EXE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let p = PathBuf::from(r"C:\Users\ExampleUser\.codex\plugins\.plugin-appserver\codex.exe");
    if p.is_file() {
        return Some(p);
    }
    None
}

fn discover_claude() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("WAGGLEPOST_CLAUDE_EXE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    // npm shim dir → real native binary
    let candidates = [
        PathBuf::from(r"D:\dev\nvm\installs\v24.15.0\node_modules\@anthropic-ai\claude-code\bin\claude.exe"),
    ];
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

fn probe_version(exe: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(exe)
        .args(args)
        .env_clear()
        .envs(crate::runner::filter_env(&[]))
        .output()
        .ok()?;
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v = s.lines().next().unwrap_or("").trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

pub struct Core {
    pub store: Store,
    pub bus: Arc<EventBus>,
    pub cfg: CoreConfig,
    pub cancel_flags: std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>,
}

#[derive(Debug)]
pub struct CoreError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub reason: Option<routing::Reason>,
}

impl CoreError {
    pub fn bad(code: &str, msg: impl Into<String>) -> Self {
        CoreError {
            status: 400,
            code: code.into(),
            message: msg.into(),
            reason: None,
        }
    }
    pub fn conflict(code: &str, msg: impl Into<String>) -> Self {
        CoreError {
            status: 409,
            code: code.into(),
            message: msg.into(),
            reason: None,
        }
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        CoreError {
            status: 404,
            code: "not_found".into(),
            message: msg.into(),
            reason: None,
        }
    }
    pub fn busy(code: &str, msg: impl Into<String>) -> Self {
        CoreError {
            status: 429,
            code: code.into(),
            message: msg.into(),
            reason: None,
        }
    }
}

impl From<String> for CoreError {
    fn from(e: String) -> Self {
        CoreError {
            status: 500,
            code: "internal".into(),
            message: e,
            reason: None,
        }
    }
}

pub struct SubmitInput {
    pub title: String,
    /// explicitly approved resume budget at submission time (WP10), default 1
    pub resume_budget: Option<u64>,
    pub goal: String,
    pub acceptance: Vec<String>,
    pub repo_path: String,
    pub base_commit: Option<String>,
    pub allowed_paths: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub recipe: Recipe,
    pub allowed_adapters: Vec<String>,
    pub timeout_secs: Option<u64>,
}

impl Core {
    pub fn new(store: Store, cfg: CoreConfig) -> Arc<Self> {
        let bus = EventBus::new(Store::open(&store.db_path).expect("second store handle"));
        Arc::new(Core {
            store,
            bus,
            cfg,
            cancel_flags: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    pub fn managed_root(&self) -> PathBuf {
        self.cfg.managed_root.clone()
    }

    /// Managed demo repository for the 试样例 entry (offline, fake runtime only).
    fn ensure_sample_repo(&self) -> Result<PathBuf, CoreError> {
        let dir = self.cfg.managed_root.join("samples").join("demo-repo");
        std::fs::create_dir_all(&dir).map_err(|e| CoreError::bad("managed_root_unwritable", e.to_string()))?;
        if !dir.join(".git").exists() {
            std::fs::write(dir.join("README.md"), "# demo

Sample repository for the offline wagglepost tour.
").ok();
            // freshly created directories can transiently fail CreateProcess cwd
            // validation on Windows; retry a few times before giving up
            let mut last: Option<String> = None;
            for _ in 0..4 {
                let init = worktree::run_git(&self.cfg.git, &dir, &["init", "-q", "-b", "main"], None);
                match init {
                    Ok(_) => {
                        last = None;
                        break;
                    }
                    Err(e) => {
                        last = Some(e);
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                }
            }
            if let Some(e) = last {
                return Err(CoreError::bad("sample_repo_failed", e));
            }
            worktree::run_git(&self.cfg.git, &dir, &["add", "-A"], None)
                .map_err(|e| CoreError::bad("sample_repo_failed", e))?;
            worktree::run_git(
                &self.cfg.git,
                &dir,
                &["-c", "user.name=wagglepost", "-c", "user.email=wagglepost@local", "commit", "-q", "-m", "demo baseline"],
                None,
            )
            .map_err(|e| CoreError::bad("sample_repo_failed", e))?;
        }
        Ok(dir)
    }

    fn ensure_managed_dirs(&self) -> Result<(), CoreError> {
        std::fs::create_dir_all(self.cfg.managed_root.join("worktrees")).map_err(|e| {
            CoreError::bad("managed_root_unwritable", format!("managed root: {e}"))
        })?;
        std::fs::create_dir_all(self.cfg.managed_root.join("patches")).ok();
        std::fs::create_dir_all(self.cfg.managed_root.join("exports")).ok();
        std::fs::create_dir_all(self.cfg.managed_root.join("fake-scripts")).ok();
        std::fs::create_dir_all(self.cfg.managed_root.join("fake-captures")).ok();
        Ok(())
    }

    // ================= doctor =================

    pub fn doctor(&self) -> Value {
        let _codex_ok = self.cfg.codex_exe.is_file();
        let _claude_ok = self.cfg.claude_exe.is_file();
        let managed_ok = self.cfg.managed_root.is_dir()
            || std::fs::create_dir_all(self.cfg.managed_root.join("worktrees")).is_ok();
        let git_version = worktree::run_git(&self.cfg.git, std::path::Path::new("."), &["--version"], None)
            .ok()
            .map(|s| s.trim().to_string());
        json!({
            "adapters": [
                codex_info(&self.cfg),
                claude_info(&self.cfg),
                adapters::dsh::info(&self.cfg.dsh),
                fake_info(),
            ],
            "git": {"exe": self.cfg.git.git_exe.to_string_lossy(), "version": git_version},
            "managed_root": self.cfg.managed_root.to_string_lossy(),
            "managed_root_ok": managed_ok,
            "platform": std::env::consts::OS,
        })
    }

    pub fn adapter_states(&self) -> Vec<AdapterRuntimeState> {
        vec![
            AdapterRuntimeState {
                id: "codex".into(),
                available: self.cfg.codex_exe.is_file() && self.cfg.codex_version.is_some(),
                version: self.cfg.codex_version.clone(),
                // write gate: verified on-device (P0 probe) — sandboxed exec with JSON events
                write_ok: self.cfg.codex_exe.is_file() && self.cfg.codex_version.is_some(),
                readonly_ok: self.cfg.codex_exe.is_file() && self.cfg.codex_version.is_some(),
                resume_ok: self.cfg.codex_exe.is_file() && self.cfg.codex_version.is_some(),
                quota_blocked: false,
                simulated: false,
            },
            AdapterRuntimeState {
                id: "claude".into(),
                available: self.cfg.claude_exe.is_file() && self.cfg.claude_version.is_some(),
                version: self.cfg.claude_version.clone(),
                // read-only reviewer gate: verified on-device (P0 probe).
                // write gate NOT verified in this batch → claude cannot be a writer candidate.
                write_ok: false,
                readonly_ok: self.cfg.claude_exe.is_file() && self.cfg.claude_version.is_some(),
                resume_ok: self.cfg.claude_exe.is_file() && self.cfg.claude_version.is_some(),
                quota_blocked: false,
                simulated: false,
            },
            AdapterRuntimeState {
                id: "dsh".into(),
                available: self.cfg.dsh.version.is_some(),
                version: self.cfg.dsh.version.clone(),
                // capability gates not passed in 0.1 → candidate stays blocked
                write_ok: false,
                readonly_ok: false,
                resume_ok: false,
                quota_blocked: false,
                simulated: false,
            },
            AdapterRuntimeState {
                id: "fake".into(),
                available: true,
                version: Some("0.1.0".into()),
                write_ok: true,
                readonly_ok: true,
                resume_ok: true,
                quota_blocked: false,
                simulated: true,
            },
        ]
    }

    pub fn repo_trusted(&self, repo_path: &str) -> bool {
        self.store.meta_get(&format!("trust:{}", normalize_repo(repo_path)))
            == Some("1".to_string())
    }

    pub fn trust_repo(&self, repo_path: &str) -> Result<(), CoreError> {
        let p = PathBuf::from(repo_path);
        if !p.is_dir() {
            return Err(CoreError::bad("repo_missing", "repository path does not exist"));
        }
        worktree::resolve_head(&self.cfg.git, repo_path)
            .map_err(|e| CoreError::bad("repo_not_git", format!("not a git repository: {e}")))?;
        self.store
            .meta_set(&format!("trust:{}", normalize_repo(repo_path)), "1");
        Ok(())
    }

    // ================= tasks & plans =================

    pub fn submit(&self, input: SubmitInput) -> Result<Task, CoreError> {
        self.ensure_managed_dirs()?;
        if input.goal.trim().is_empty() {
            return Err(CoreError::bad("goal_required", "goal is required"));
        }
        // "sample:demo" resolves to a managed demo repo (created on demand)
        let resolved_repo = if input.repo_path.trim() == "sample:demo" {
            let demo = self.ensure_sample_repo()?;
            demo.to_string_lossy().to_string()
        } else {
            input.repo_path.clone()
        };
        let repo_path = normalize_repo(&resolved_repo);
        let base = match &input.base_commit {
            Some(c) if !c.is_empty() => {
                worktree::resolve_head(&self.cfg.git, &repo_path)
                    .map_err(|e| CoreError::bad("base_commit_invalid", e))?;
                c.clone()
            }
            _ => {
                if !worktree::is_clean(&self.cfg.git, &repo_path)
                    .map_err(|e| CoreError::bad("repo_check_failed", e))?
                {
                    return Err(CoreError::conflict(
                        "repo_dirty",
                        "user repository has uncommitted changes; commit first or pass an explicit base_commit",
                    ));
                }
                worktree::resolve_head(&self.cfg.git, &repo_path)
                    .map_err(|e| CoreError::bad("repo_not_git", e))?
            }
        };
        let now = timestamp();
        let task = Task {
            task_id: new_id("t"),
            revision: 1,
            title: if input.title.trim().is_empty() {
                input.goal.chars().take(40).collect()
            } else {
                input.title.trim().to_string()
            },
            goal: input.goal.trim().to_string(),
            acceptance: input.acceptance,
            repo_path: repo_path.clone(),
            repo_id: worktree::repo_id_for(&repo_path),
            base_commit: base,
            allowed_paths: input.allowed_paths,
            forbidden_paths: input.forbidden_paths,
            recipe: input.recipe,
            status: TaskStatus::AwaitingApproval,
            planned_steps: if input.recipe == Recipe::WriteThenReview { 2 } else { 1 },
            resume_budget: input.resume_budget.unwrap_or(1).clamp(1, 8),
            resume_used: 0,
            launches_used: 0,
            allowed_adapters: if input.allowed_adapters.is_empty() {
                vec!["codex".into(), "claude".into(), "dsh".into(), "fake".into()]
            } else {
                input.allowed_adapters
            },
            timeout_secs: input.timeout_secs.unwrap_or(20 * 60),
            current_generation: 0,
            accepted_candidate_hash: None,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.insert_task(&task)?;
        self.bus.emit(
            Some(&task.task_id),
            None,
            0,
            "task_created",
            json!({"status": task.status.as_str(), "title": task.title}),
        );
        self.upsert_inbox(
            &task.task_id,
            "approval_needed",
            json!({"task_id": task.task_id, "title": task.title}),
            format!("approval:{}", task.task_id),
        );
        Ok(task)
    }

    /// Local-rule plan builder: recipe → ordered steps with permissions. Zero model calls.
    pub fn create_plan(&self, task_id: &str, writer_adapter: Option<String>) -> Result<Plan, CoreError> {
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| CoreError::not_found("task not found"))?;
        if task.status.terminal() {
            return Err(CoreError::conflict("task_terminal", "task already finished"));
        }
        let states = self.adapter_states();
        // pick writer: user priority order, first ok for the role
        let pick = |role: Role| -> Result<String, CoreError> {
            for cand in states.iter().filter(|s| s.available) {
                if !task.allowed_adapters.iter().any(|a| a == &cand.id) {
                    continue;
                }
                if cand.id == "dsh" {
                    continue; // not enabled for dispatch in 0.1
                }
                if cand.simulated && task.allowed_adapters.len() > 1 && states.iter().any(|s| !s.simulated && s.available && task.allowed_adapters.iter().any(|a| a == &s.id)) {
                    // skip fake unless it is the only real option or explicitly first
                    continue;
                }
                let ok = match role {
                    Role::Writer => cand.write_ok,
                    Role::Reviewer => cand.readonly_ok,
                };
                if ok {
                    return Ok(cand.id.clone());
                }
            }
            Err(CoreError::bad("no_candidate", "no adapter passed the gate for this role"))
        };
        let writer = match writer_adapter {
            Some(a) if !a.is_empty() => {
                let st = states.iter().find(|s| s.id == a)
                    .ok_or_else(|| CoreError::bad("unknown_adapter", format!("unknown adapter {a}")))?;
                if !st.write_ok {
                    return Err(CoreError::bad("role_write_not_supported", format!("{a} has not passed the write gate")));
                }
                a
            }
            _ => pick(Role::Writer)?,
        };
        // independent review prefers a SECOND runtime: exclude the writer adapter
        // when any other gate-passing runtime exists (cross-tool review is the point)
        let reviewer = (Role::Reviewer, {
            let writer = writer.clone();
            move |s: &AdapterRuntimeState| s.id != writer
        });
        let reviewer = {
            let (role, exclude) = reviewer;
            let mut chosen: Option<String> = None;
            for cand in states.iter().filter(|s| s.available && exclude(s)) {
                if !task.allowed_adapters.iter().any(|a| a == &cand.id) {
                    continue;
                }
                if cand.id == "dsh" {
                    continue;
                }
                if cand.simulated
                    && task.allowed_adapters.len() > 1
                    && states.iter().any(|s| !s.simulated && s.available && exclude(s) && task.allowed_adapters.iter().any(|a| a == &s.id))
                {
                    continue;
                }
                let ok = match role {
                    Role::Writer => cand.write_ok,
                    Role::Reviewer => cand.readonly_ok,
                };
                if ok {
                    chosen = Some(cand.id.clone());
                    break;
                }
            }
            match chosen {
                Some(c) => c,
                None => pick(Role::Reviewer)?,
            }
        };
        let now = timestamp();
        let mut steps = vec![PlanStep {
            step_id: "s1".into(),
            role: Role::Writer,
            adapter: writer.clone(),
            permission: "workspace-write".into(),
            depends_on: vec![],
            handoff: None,
        }];
        if task.recipe == Recipe::WriteThenReview {
            steps.push(PlanStep {
                step_id: "s2".into(),
                role: Role::Reviewer,
                adapter: reviewer.clone(),
                permission: "read-only".into(),
                depends_on: vec!["s1".into()],
                handoff: None,
            });
        }
        let plan = Plan {
            plan_id: new_id("p"),
            task_id: task.task_id.clone(),
            task_revision: task.revision,
            recipe: task.recipe,
            summary_hash: {
                let canonical = json!({
                    "task_id": task.task_id, "revision": task.revision,
                    "recipe": task.recipe.as_str(),
                    "steps": steps.iter().map(|s| json!({"step": s.step_id, "role": s.role.as_str(), "adapter": s.adapter, "perm": s.permission})).collect::<Vec<_>>(),
                    "base": task.base_commit,
                });
                sha256_hex(canonical.to_string().as_bytes())
            },
            steps,
            created_at: now.clone(),
            approved_at: None,
            approved_handoff_digest: None,
            approved_versions: None,
        };
        // attach route evaluation for the receipt (reasons why)
        self.store.insert_plan(&plan)?;
        self.bus.emit(
            Some(&task.task_id),
            None,
            0,
            "plan_created",
            json!({"plan_id": plan.plan_id, "summary_hash": plan.summary_hash, "recipe": plan.recipe.as_str()}),
        );
        Ok(plan)
    }

    pub fn route_preview(&self, task_id: &str, role: Role) -> Result<routing::RouteEvaluation, CoreError> {
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| CoreError::not_found("task not found"))?;
        let live = self.store.list_live_attempts();
        let repo_live_writers = live
            .iter()
            .filter(|a| {
                a.role == Role::Writer
                    && self
                        .store
                        .get_task(&a.task_id)
                        .map(|t| normalize_repo(&t.repo_path) == normalize_repo(&task.repo_path))
                        .unwrap_or(false)
            })
            .count();
        let ev = routing::evaluate(&RouteInputs {
            task: &task,
            role,
            live_attempts: live.len(),
            repo_live_writers,
            repo_trusted: self.repo_trusted(&task.repo_path),
            adapter_states: &self.adapter_states(),
        });
        Ok(ev)
    }

    pub fn approve_plan(
        &self,
        task_id: &str,
        summary_hash: &str,
        handoff_digest: Option<&str>,
    ) -> Result<Task, CoreError> {
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| CoreError::not_found("task not found"))?;
        let plan = self
            .store
            .latest_plan(task_id)
            .ok_or_else(|| CoreError::not_found("plan not found"))?;
        if plan.summary_hash != summary_hash {
            return Err(CoreError::conflict(
                "plan_stale",
                "plan hash mismatch — the plan changed; fetch it again and re-confirm",
            ));
        }
        if plan.task_revision != task.revision {
            return Err(CoreError::conflict("plan_stale", "plan was built for another task revision"));
        }
        if !self.repo_trusted(&task.repo_path) {
            return Err(CoreError::conflict(
                "repo_untrusted",
                "confirm the repository before approving dispatch",
            ));
        }
        // WP09: approval binds the EXACT writer first-run body (previewed by the user)
        // and the adapter versions that will run. The digest is assembled server-side
        // so the approved value is always the one dispatch will verify against.
        let writer_step = plan
            .steps
            .iter()
            .find(|s| s.role == Role::Writer)
            .ok_or_else(|| CoreError::bad("no_candidate", "plan has no writer step"))?;
        let approved_body = self.assemble_handoff(&task, writer_step, 1)?;
        if let Some(d) = handoff_digest {
            if d != approved_body.sha256 {
                return Err(CoreError::conflict(
                    "handoff_digest_mismatch",
                    "the digest you approved does not match the body that would be sent; refresh the preview",
                ));
            }
        }
        let mut versions = serde_json::Map::new();
        for st in &plan.steps {
            versions.insert(st.adapter.clone(), Value::String(self.version_of(&st.adapter)));
        }
        let versions = Value::Object(versions);
        self.store
            .bind_plan_approval(&plan.plan_id, &approved_body.sha256, &versions)?;
        self.store.approve_plan(&plan.plan_id, &timestamp())?;
        let mut t = task;
        t.status = TaskStatus::Queued;
        t.updated_at = timestamp();
        self.store.update_task(&t)?;
        self.store.resolve_message_by_task(task_id, "approval_needed");
        self.bus.emit(
            Some(task_id),
            None,
            0,
            "plan_approved",
            json!({"plan_id": plan.plan_id}),
        );
        Ok(t)
    }

    // ================= requests (idempotency) =================

    /// Begin a request: replay-safe. Returns Err(conflict) on digest mismatch.
    pub fn begin_request(
        &self,
        request_id: &str,
        kind: &str,
        task_id: Option<&str>,
        body_digest: String,
    ) -> Result<RequestRecord, CoreError> {
        if let Some(existing) = self.store.get_request(request_id) {
            if existing.digest == body_digest {
                if existing.status == "processing" {
                    // a same-key concurrent call may not double-execute; retry later
                    return Err(CoreError::conflict(
                        "request_in_flight",
                        "this request id is still being processed; query it instead of re-sending",
                    ));
                }
                return Ok(existing); // replay
            }
            return Err(CoreError::conflict(
                "request_id_conflict",
                "request id already used with a different body",
            ));
        }
        let now = timestamp();
        let rec = RequestRecord {
            request_id: request_id.to_string(),
            kind: kind.to_string(),
            digest: body_digest,
            task_id: task_id.map(|s| s.to_string()),
            attempt_id: None,
            status: "processing".into(),
            replayed: false,
            result: Value::Null,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.insert_request(&rec)?;
        Ok(rec)
    }

    pub fn finish_request(&self, mut rec: RequestRecord, result: Value, attempt_id: Option<String>) {
        rec.status = "completed".into();
        rec.result = result;
        rec.attempt_id = attempt_id;
        rec.updated_at = timestamp();
        let _ = self.store.update_request(&rec);
    }

    pub fn fail_request(&self, mut rec: RequestRecord, result: Value) {
        rec.status = "completed".into();
        rec.result = result;
        rec.updated_at = timestamp();
        let _ = self.store.update_request(&rec);
    }

    // ================= dispatch =================

    /// Dispatch the next runnable step of a queued task. Idempotent via request record.
    pub fn dispatch(self: &Arc<Self>, task_id: &str, request: &mut RequestRecord) -> Result<Task, CoreError> {
        self.ensure_managed_dirs()?;
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| CoreError::not_found("task not found"))?;
        if task.status.terminal() {
            return Err(CoreError::conflict("task_terminal", "task already finished"));
        }
        if task.status == TaskStatus::Stopping || task.status == TaskStatus::StopUnconfirmed {
            return Err(CoreError::conflict("stopping", "a stop is in progress; reconcile first"));
        }
        let plan = self
            .store
            .latest_plan(task_id)
            .filter(|p| p.approved_at.is_some() && p.task_revision == task.revision)
            .ok_or_else(|| CoreError::conflict("plan_missing", "no approved plan for this revision"))?;
        // which step runs next?
        let attempts = self.store.list_attempts(task_id);
        let done_steps: Vec<&str> = attempts
            .iter()
            .filter(|a| a.status == AttemptStatus::Completed)
            .map(|a| a.step_id.as_str())
            .collect();
        // explicit relay/resume intent (set by reroute/answer) names the step to re-run;
        // otherwise pick the first fresh step (never attempted, deps done)
        let next_step_id = self.store.meta_get(&format!("next_step:{}", task_id));
        let intent_was_set = next_step_id.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        let next_step = match next_step_id {
            Some(sid) if !sid.is_empty() => {
                self.store.meta_set(&format!("next_step:{}", task_id), "");
                plan.steps.iter().find(|s| s.step_id == sid).cloned()
            }
            _ => plan
                .steps
                .iter()
                .find(|s| {
                    !attempts.iter().any(|a| a.step_id == s.step_id)
                        && s.depends_on.iter().all(|d| done_steps.contains(&d.as_str()))
                })
                .cloned(),
        }
        .filter(|s| {
            // even an intent-named step cannot run twice in the same generation
            let generation = task.current_generation + 1;
            !attempts.iter().any(|a| {
                a.step_id == s.step_id && (a.status.live() || a.generation >= generation)
            })
        })
        .ok_or_else(|| {
                if attempts.iter().any(|a| a.status.live()) {
                    CoreError::busy("slot_busy", "an attempt is already running")
                } else {
                    CoreError::conflict("no_dispatchable_step", "no runnable step (writer done? review done?)")
                }
            })?;

        // hard routing gate at dispatch time
        let live = self.store.list_live_attempts();
        if !live.is_empty() {
            return Err(CoreError::busy("no_free_slot", "execution slot is busy"));
        }
        if next_step.role == Role::Writer {
            let same_repo_writers = live
                .iter()
                .filter(|a| {
                    a.role == Role::Writer
                        && self
                            .store
                            .get_task(&a.task_id)
                            .map(|t| normalize_repo(&t.repo_path) == normalize_repo(&task.repo_path))
                            .unwrap_or(false)
                })
                .count();
            if same_repo_writers >= 1 {
                return Err(CoreError::busy("repo_writer_busy", "same-repo writer lock held"));
            }
        }
        if !self.repo_trusted(&task.repo_path) {
            return Err(CoreError::conflict("repo_untrusted", "repository is not trusted"));
        }
        if task.budget_remaining() <= 0 {
            // WP10: the budget is strict — reroute discloses its full cost upfront
            return Err(CoreError::conflict(
                "budget_exhausted",
                "launch budget exhausted; increasing needs explicit approval",
            ));
        }
        let _ = intent_was_set;

        // assemble handoff (local, deterministic)
        let generation = task.current_generation + 1;
        let handoff = self.assemble_handoff(&task, &next_step, generation)?;
        let _body_digest = sha256_hex(handoff.text.as_bytes());
        if handoff.bytes > handoff.budget_bytes {
            return Err(CoreError::bad(
                "handoff_over_budget",
                format!("handoff {} bytes exceeds budget {}", handoff.bytes, handoff.budget_bytes),
            ));
        }

        // WP09: the launched body must be the approved body (writer first run) and the
        // runtime versions must still match what approval bound.
        if let Some(approved_digest) = plan.approved_handoff_digest.as_deref() {
            if next_step.role == Role::Writer && generation == 1 && approved_digest != handoff.sha256 {
                return Err(CoreError::conflict(
                    "handoff_not_approved",
                    "the assembled body differs from the approved digest; refresh the preview and re-approve",
                ));
            }
        }
        if let Some(approved_versions) = &plan.approved_versions {
            let bound = approved_versions.get(&next_step.adapter).and_then(|v| v.as_str());
            if let Some(bound) = bound {
                if bound != self.version_of(&next_step.adapter) {
                    return Err(CoreError::conflict(
                        "adapter_version_drift",
                        format!(
                            "{} runtime changed since approval (approved {}, now {}); re-approve the plan",
                            next_step.adapter, bound, self.version_of(&next_step.adapter)
                        ),
                    ));
                }
            }
        }

        // create worktree BEFORE starting (workspace ownership persisted first)
        let wt = if next_step.role == Role::Reviewer {
            let cand = self
                .store
                .current_candidate(task_id)
                .ok_or_else(|| CoreError::conflict("candidate_missing", "no frozen candidate to review"))?;
            worktree::create_review_worktree(&self.store, &self.cfg.git, &task, &cand)?
        } else if generation > 1 {
            let cp = self
                .store
                .latest_checkpoint(task_id)
                .ok_or_else(|| CoreError::conflict("checkpoint_missing", "no checkpoint for relay"))?;
            worktree::apply_checkpoint(&self.store, &self.cfg.git, &task, generation, &cp)?
        } else {
            worktree::create_worktree(&self.store, &self.cfg.git, &task, generation)?
        };

        let attempt = Attempt {
            attempt_id: new_id("a"),
            task_id: task.task_id.clone(),
            task_revision: task.revision,
            step_id: next_step.step_id.clone(),
            adapter: next_step.adapter.clone(),
            adapter_version: self.version_of(&next_step.adapter),
            role: next_step.role,
            worktree_id: Some(wt.worktree_id.clone()),
            generation,
            native_id: None,
            request_id: Some(request.request_id.clone()),
            status: AttemptStatus::Starting,
            launch_no: self.store.next_launch_no(task_id),
            exit_code: None,
            started_at: Some(timestamp()),
            ended_at: None,
            simulated: next_step.adapter == "fake",
            handoff_digest: Some(handoff.sha256.clone()),
            pid: None,
        };
        self.store.insert_attempt(&attempt)?;

        // intent + workspace ownership persisted; now flip task to starting
        let mut t = task.clone();
        t.status = TaskStatus::Starting;
        t.current_generation = generation;
        t.launches_used += 1;
        t.updated_at = timestamp();
        self.store.update_task(&t)?;
        self.store.resolve_message_by_task(task_id, "blocked");
        self.bus.emit(
            Some(task_id),
            Some(&attempt.attempt_id),
            generation,
            "attempt_started",
            json!({
                "attempt_id": attempt.attempt_id,
                "adapter": attempt.adapter,
                "role": attempt.role.as_str(),
                "generation": generation,
                "simulated": attempt.simulated,
                "launch_no": attempt.launch_no,
                "worktree": wt.worktree_id,
            }),
        );

        // launch in background
        let core = self.clone();
        let attempt_id = attempt.attempt_id.clone();
        let task_id2 = task_id.to_string();
        let ctx = DispatchContext {
            task: t.clone(),
            attempt_id: attempt_id.clone(),
            generation,
            role: next_step.role,
            worktree_path: wt.path.clone(),
            handoff,
            resume_native_id: None,
            lang: "zh",
            managed_root: self.cfg.managed_root.clone(),
            fake_script: self.fake_script_for(&task, &next_step, generation),
            timeout_secs: task.timeout_secs,
            fake_exe: self.cfg.fake_exe.clone(),
            bound_candidate_hash: if next_step.role == Role::Reviewer {
                self.store.current_candidate(task_id).map(|c| c.candidate_hash)
            } else {
                None
            },
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_flags
            .lock()
            .unwrap()
            .insert(attempt_id.clone(), cancel.clone());
        request.attempt_id = Some(attempt_id.clone());
        request.updated_at = timestamp();
        let _ = self.store.update_request(request);

        tokio::task::spawn_blocking(move || {
            core.run_attempt_blocking(&task_id2, &attempt_id, ctx, cancel);
        });

        Ok(t)
    }

    fn version_of(&self, adapter: &str) -> String {
        match adapter {
            "codex" => self.cfg.codex_version.clone().unwrap_or_default(),
            "claude" => self.cfg.claude_version.clone().unwrap_or_default(),
            "dsh" => self.cfg.dsh.version.clone().unwrap_or_default(),
            "fake" => "0.1.0".into(),
            _ => String::new(),
        }
    }

    fn fake_script_for(&self, task: &Task, step: &PlanStep, generation: u64) -> Option<Value> {
        if step.adapter != "fake" {
            return None;
        }
        // default fake script: writer writes a small file, reviewer passes.
        // tests can override via meta key fake_script:<task_id>:<step>
        let script_key = format!("fake_script:{}:{}", task.task_id, step.step_id);
        if let Some(s) = self.store.meta_get(&script_key) {
            if !s.is_empty() {
                // one-shot: tests pin a script per run; consumed on use so later
                // generations fall back to the default scripted behavior
                self.store.meta_set(&script_key, "");
                return serde_json::from_str(&s).ok();
            }
        }
        if step.role == Role::Reviewer {
            Some(json!({
                "behaviors": ["review"],
                "result": {"status": "done", "summary": "复核完成 pass：候选与检查证据一致（模拟）"},
                "review": {"verdict": "pass"}
            }))
        } else if generation > 1 {
            Some(json!({
                "behaviors": ["write_files"],
                "files": {"RELAY-NOTES.md": "continued from checkpoint (simulated)\n"},
                "result": {"status": "done", "summary": "接力完成：检查点改动已保留并补齐（模拟）"}
            }))
        } else {
            Some(json!({
                "behaviors": ["write_files"],
                "files": {"wagglepost-demo.md": "demo change from fake writer (simulated)\n"},
                "result": {"status": "done", "summary": "写入完成（模拟）"}
            }))
        }
    }

    /// Assemble the role-specific handoff (G7a contract).
    pub fn assemble_handoff(&self, task: &Task, step: &PlanStep, generation: u64) -> Result<HandoffBody, CoreError> {
        let lang = self
            .store
            .meta_get("ui_lang")
            .unwrap_or_else(|| "zh".into());
        let lang: &'static str = if lang == "en" { "en" } else { "zh" };
        let acceptance = if task.acceptance.is_empty() {
            "- 完成目标且不破坏现有行为 / complete the goal without breaking existing behavior".to_string()
        } else {
            task.acceptance
                .iter()
                .map(|a| format!("- {a}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let scope = format!(
            "{}\n{}",
            if task.allowed_paths.is_empty() {
                "允许路径 / allowed: 整个工作区（worktree） / the whole worktree".to_string()
            } else {
                format!(
                    "允许路径 / allowed paths:\n{}",
                    task.allowed_paths.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n")
                )
            },
            if task.forbidden_paths.is_empty() {
                String::new()
            } else {
                format!(
                    "禁止路径 / forbidden paths:\n{}",
                    task.forbidden_paths.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n")
                )
            }
        );
        let baseline = format!("base_commit: {}\nrepo: {}（你在隔离 worktree 中工作，不要触碰用户原始目录）", task.base_commit, task.repo_path);

        let parts = match step.role {
            Role::Writer => {
                if generation > 1 {
                    // relay package
                    let cp = self.store.latest_checkpoint(&task.task_id);
                    HandoffParts {
                        goal: task.goal.clone(),
                        acceptance: acceptance.clone(),
                        scope: scope.clone(),
                        baseline: format!(
                            "base_commit: {}\ncheckpoint: {}\n候选哈希: {}",
                            task.base_commit,
                            cp.as_ref().map(|c| c.checkpoint_id.clone()).unwrap_or_default(),
                            cp.as_ref().map(|c| c.candidate_hash.clone()).unwrap_or_default(),
                        ),
                        confirmed_progress: Some({
                            let base = cp
                                .as_ref()
                                .map(|c| {
                                    format!(
                                        "上一写者已保存的改动文件：{}
未跟踪文件：{}",
                                        c.changed_files.join(", "),
                                        c.untracked_files.join(", ")
                                    )
                                })
                                .unwrap_or_else(|| "无已确认进度 / none".into());
                            let answered = self
                                .store
                                .list_decisions(false)
                                .into_iter()
                                .filter(|d| d.task_id == task.task_id && d.status == "answered")
                                .max_by_key(|d| d.created_at.clone());
                            match answered {
                                Some(d) => format!(
                                    "{}
用户已回答决策卡 {}：选择 {}（问题：{}）",
                                    base,
                                    d.decision_id,
                                    d.resolution.clone().unwrap_or_default(),
                                    d.question
                                ),
                                None => base,
                            }
                        }),
failure_location: cp
                            .as_ref()
                            .and_then(|c| c.failure_summary.clone())
                            .or(Some("失败位置见检查点 / see checkpoint".into())),
                        key_changes: cp.as_ref().map(|c| c.changed_files.join("\n")),
                        evidence: cp.as_ref().map(|c| {
                            format!(
                                "evidence://task/{}/checkpoint/{} (generation {}, candidate {})",
                                task.task_id, c.checkpoint_id, c.attempt_id, c.candidate_hash
                            )
                        }),
                        next_step: Some(
                            "从检查点继续：先复核已改动内容，再补齐目标 / continue from the checkpoint: review the kept changes, then finish the goal"
                                .into(),
                        ),
                        ..Default::default()
                    }
                } else {
                    HandoffParts {
                        goal: task.goal.clone(),
                        acceptance,
                        scope,
                        baseline,
                        next_step: Some("按验收条件实现改动 / implement the change to the acceptance criteria".into()),
                        ..Default::default()
                    }
                }
            }
            Role::Reviewer => {
                let cand = self
                    .store
                    .current_candidate(&task.task_id)
                    .ok_or_else(|| CoreError::conflict("candidate_missing", "no frozen candidate"))?;
                let review_meta = self.store.meta_get(&format!("review_questions:{}", task.task_id));
                HandoffParts {
                    goal: task.goal.clone(),
                    acceptance,
                    scope: format!("你只有只读权限 / you have read-only access only\n{}", scope),
                    baseline: format!(
                        "candidate_hash: {}\nfrozen_commit: {}\nbase_commit: {}\n生成代次: generation {}",
                        cand.candidate_hash,
                        cand.frozen_commit.clone().unwrap_or_default(),
                        cand.base_commit,
                        cand.generation
                    ),
                    candidate_identity: format!(
                        "候选短 ID / candidate short id: {}\n完整哈希 / full hash: {}",
                        &cand.candidate_hash[..8],
                        cand.candidate_hash
                    ),
                    key_changes: Some(format!(
                        "关键变更文件 / key changed files:\n{}",
                        cand.changed_files.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n")
                    )),
                    review_questions: review_meta.unwrap_or_else(|| {
                        "1. 变更是否实现目标且满足验收条件？\n2. 是否引入回归或越界改动？\n3. 给出结论：pass 或 issues（附证据）".to_string()
                    }),
                    evidence_refs: vec![format!(
                        "evidence://task/{}/candidate/{}",
                        task.task_id, cand.candidate_hash
                    )],
                    ..Default::default()
                }
            }
        };
        let mode = match (step.role, generation) {
            (Role::Writer, g) if g > 1 => HandoffMode::RelayPackage,
            (Role::Writer, _) => HandoffMode::FirstRun,
            (Role::Reviewer, _) => HandoffMode::Review,
        };
        handoff::assemble(mode, task, parts, lang, self.cfg.default_body_budget)
            .map_err(|e| CoreError::bad("handoff_error", e))
    }

    // ================= attempt run loop (blocking) =================

    pub fn run_attempt_blocking(
        self: &Arc<Self>,
        task_id: &str,
        attempt_id: &str,
        ctx: DispatchContext,
        cancel: Arc<AtomicBool>,
    ) {
        let adapter_id = {
            let a = match self.store.get_attempt(attempt_id) {
                Some(a) => a,
                None => return,
            };
            a.adapter.clone()
        };
        let mut adapter_box: Box<dyn Adapter + Send> = match adapter_id.as_str() {
            "codex" => Box::new(adapters::codex::CodexAdapter::new(
                self.cfg.codex_exe.clone(),
                self.cfg.codex_version.clone().unwrap_or_default(),
            )),
            "claude" => Box::new(adapters::claude::ClaudeAdapter::new(
                self.cfg.claude_exe.to_string_lossy().to_string(),
                self.cfg.claude_version.clone().unwrap_or_default(),
            )),
            "fake" => Box::new(adapters::fake::FakeAdapter::new()),
            _ => {
                self.mark_attempt_failed(attempt_id, "unknown adapter");
                return;
            }
        };

        let attempt = match self.store.get_attempt(attempt_id) {
            Some(a) => a,
            None => return,
        };
        let mut a = attempt;
        a.status = AttemptStatus::Running;
        let _ = self.store.update_attempt(&a);
        let mut t = match self.store.get_task(task_id) {
            Some(t) => t,
            None => return,
        };
        if t.status == TaskStatus::Starting {
            t.status = TaskStatus::Running;
            t.updated_at = timestamp();
            let _ = self.store.update_task(&t);
        }

        let generation = ctx.generation;
        let attempt_id2 = attempt_id.to_string();
        let bus = self.bus.clone();
        let seen_keys: Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
            Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        let on_event = move |ev: &WireEvent| {
            // dedupe identical wire events (fake duplicate probe)
            let key = format!("{}|{}|{}", ev.kind, ev.payload, ev.text.clone().unwrap_or_default());
            {
                let mut seen = seen_keys.lock().unwrap();
                if !seen.insert(key) {
                    // record that a duplicate arrived but only count it
                    let payload = json!({"deduped": true});
                    bus.emit(Some(task_id), Some(&attempt_id2), generation, "event_deduped", payload);
                    return;
                }
            }
            bus.emit(
                Some(task_id),
                Some(&attempt_id2),
                generation,
                &ev.kind,
                ev.payload.clone(),
            );
        };

        let outcome = adapters::run_attempt(adapter_box.as_mut(), &ctx, cancel, &on_event);

        match outcome {
            Ok(res) => {
                self.finish_attempt(task_id, attempt_id, &ctx, &res);
            }
            Err(e) => {
                self.mark_attempt_failed(attempt_id, &e);
            }
        }
    }

    fn mark_attempt_failed(self: &Arc<Self>, attempt_id: &str, err: &str) {
        if let Some(mut a) = self.store.get_attempt(attempt_id) {
            a.status = AttemptStatus::Failed;
            a.ended_at = Some(timestamp());
            let _ = self.store.update_attempt(&a);
            self.bus.emit(
                Some(&a.task_id),
                Some(attempt_id),
                a.generation,
                "attempt_failed",
                json!({"error": err}),
            );
            if let Some(mut t) = self.store.get_task(&a.task_id) {
                t.status = TaskStatus::Interrupted;
                t.updated_at = timestamp();
                let _ = self.store.update_task(&t);
                self.upsert_inbox(
                    &t.task_id,
                    "blocked",
                    json!({"task_id": t.task_id, "reason": err}),
                    format!("blocked:{}:{}", t.task_id, a.generation),
                );
            }
        }
    }

    /// Post-exit classification + checkpoint + review chain. (blocking context)
    fn finish_attempt(
        self: &Arc<Self>,
        task_id: &str,
        attempt_id: &str,
        ctx: &DispatchContext,
        res: &adapters::RunOutcome,
    ) {
        let Some(mut attempt) = self.store.get_attempt(attempt_id) else {
            return;
        };
        attempt.exit_code = res.exit.exit_code.map(|c| c as i64);
        attempt.ended_at = Some(timestamp());
        if attempt.native_id.is_none() {
            attempt.native_id = res.state.native_id.clone();
        }
        // a requested cancel ends as cancelled regardless of the agent's last words
        if attempt.status == AttemptStatus::Stopping || res.exit.killed {
            attempt.status = AttemptStatus::Cancelled;
            let _ = self.store.update_attempt(&attempt);
            self.save_checkpoint(task_id, attempt_id, Some("cancelled by user; changes kept"), CheckpointSource::LocalObserved);
            if let Some(mut t) = self.store.get_task(task_id) {
                t.status = TaskStatus::Cancelled;
                t.updated_at = timestamp();
                let _ = self.store.update_task(&t);
            }
            self.cancel_flags.lock().unwrap().remove(attempt_id);
            self.bus.emit(
                Some(task_id),
                Some(attempt_id),
                attempt.generation,
                "attempt_cancelled",
                json!({"stop_confirmed": res.exit.stop_confirmed}),
            );
            return;
        }
        attempt.status = AttemptStatus::Completed;
        let _ = self.store.update_attempt(&attempt);

        // persist stderr as attempt log (bounded already)
        let log_path = self
            .cfg
            .managed_root
            .join("logs")
            .join(format!("{attempt_id}.stderr.log"));
        let _ = std::fs::create_dir_all(log_path.parent().unwrap());
        let _ = std::fs::write(&log_path, &res.exit.stderr);

        let result = adapters::extract_agent_result(res.state.last_text.as_deref().unwrap_or(""))
            .or(Some(match &res.state.fake_result {
                Some(r) => r.clone(),
                None => adapter_result_fallback(&res.state, &res.exit),
            }));

        let Some(mut task) = self.store.get_task(task_id) else {
            return;
        };

        // stop confirmation evidence: job wait returned → stop_confirmed true
        self.bus.emit(
            Some(task_id),
            Some(attempt_id),
            attempt.generation,
            "process_exited",
            json!({
                "exit_code": res.exit.exit_code,
                "stop_confirmed": res.exit.stop_confirmed,
                "killed": res.exit.killed,
                "native_id": attempt.native_id,
                "usage": res.state.usage,
            }),
        );

        let Some(result) = result else {
            // invalid result → human handling, changes kept
            task.status = TaskStatus::Interrupted;
            task.updated_at = timestamp();
            self.store.update_task(&task).ok();
            self.save_checkpoint(task_id, attempt_id, Some("invalid agent result"), CheckpointSource::LocalObserved);
            self.upsert_inbox(task_id, "blocked", json!({"task_id": task_id, "reason": "invalid agent result"}), format!("blocked:{}:{}", task_id, attempt.generation));
            return;
        };

        if attempt.role == Role::Writer {
            self.finish_writer(task_id, attempt_id, ctx, &task, &result, &res.state, &res.exit);
        } else {
            self.finish_reviewer(task_id, attempt_id, ctx, &task, &result, &res.state);
        }
    }

    fn finish_writer(
        self: &Arc<Self>,
        task_id: &str,
        attempt_id: &str,
        _ctx: &DispatchContext,
        task: &Task,
        result: &AgentResult,
        state: &adapters::RunState,
        exit: &crate::runner::ExitOutcome,
    ) {
        let mut t = task.clone();
        match result.status.as_str() {
            "needs_decision" => {
                // process already exited (confirmed); persist checkpoint then decision card
                let cp = self.save_checkpoint(task_id, attempt_id, None, CheckpointSource::AgentReported);
                let d = Decision {
                    decision_id: new_id("d"),
                    task_id: task_id.to_string(),
                    revision: t.revision,
                    attempt_id: attempt_id.to_string(),
                    checkpoint_id: cp.as_ref().map(|c| c.checkpoint_id.clone()),
                    question: result.question.clone().unwrap_or_else(|| "Agent 请求决策".into()),
                    options: if result.options.is_empty() {
                        vec![
                            DecisionOption {
                                id: "opt_continue".into(),
                                label_zh: "按建议继续".into(),
                                label_en: "Continue as suggested".into(),
                                effect_zh: "用剩余额度继续该任务".into(),
                                effect_en: "Resume the task with the remaining budget".into(),
                            },
                            DecisionOption {
                                id: "opt_stop".into(),
                                label_zh: "停止并保留改动".into(),
                                label_en: "Stop and keep changes".into(),
                                effect_zh: "停止任务，已保存改动可用补丁导出".into(),
                                effect_en: "Stop the task; kept changes stay exportable".into(),
                            },
                        ]
                    } else {
                        result.options.clone()
                    },
                    status: "open".into(),
                    dedupe_key: format!("decision:{}:{}", task_id, t.current_generation),
                    merge_count: 1,
                    resolution: None,
                    resume_request_id: None,
                    created_at: timestamp(),
                    answered_at: None,
                };
                if self.store.find_open_decision_by_dedupe(&d.dedupe_key).is_none() {
                    let _ = self.store.insert_decision(&d);
                }
                t.status = TaskStatus::WaitingDecision;
                t.updated_at = timestamp();
                let _ = self.store.update_task(&t);
                self.upsert_inbox(task_id, "decision_opened", json!({"task_id": task_id}), format!("decision:{}", task_id));
                self.bus.emit(Some(task_id), Some(attempt_id), attempt_generation(self, attempt_id), "decision_opened", json!({"question": result.question}));
            }
            "blocked" if result.quota_blocked => {
                let cp = self.save_checkpoint(task_id, attempt_id, result.blocked_reason.as_deref(), CheckpointSource::LocalObserved);
                t.status = TaskStatus::QuotaBlocked;
                t.updated_at = timestamp();
                let _ = self.store.update_task(&t);
                self.upsert_inbox(task_id, "blocked", json!({"task_id": task_id, "reason_code": "quota_blocked", "checkpoint": cp.map(|c| c.checkpoint_id)}), format!("blocked:{}:{}", task_id, t.current_generation));
                self.bus.emit(Some(task_id), Some(attempt_id), 0, "task_blocked", json!({"reason_code": "quota_blocked"}));
            }
            "blocked" if result.auth_required => {
                let _cp = self.save_checkpoint(task_id, attempt_id, result.blocked_reason.as_deref(), CheckpointSource::LocalObserved);
                t.status = TaskStatus::AuthRequired;
                t.updated_at = timestamp();
                let _ = self.store.update_task(&t);
                self.upsert_inbox(task_id, "blocked", json!({"task_id": task_id, "reason_code": "auth_required"}), format!("blocked:{}:{}", task_id, t.current_generation));
                self.bus.emit(Some(task_id), Some(attempt_id), 0, "task_blocked", json!({"reason_code": "auth_required"}));
            }
            "blocked" => {
                let cp = self.save_checkpoint(task_id, attempt_id, result.blocked_reason.as_deref(), CheckpointSource::AgentReported);
                t.status = TaskStatus::Interrupted;
                t.updated_at = timestamp();
                let _ = self.store.update_task(&t);
                self.upsert_inbox(task_id, "blocked", json!({"task_id": task_id, "reason": result.blocked_reason, "checkpoint": cp.map(|c| c.checkpoint_id)}), format!("blocked:{}:{}", task_id, t.current_generation));
                self.bus.emit(Some(task_id), Some(attempt_id), 0, "task_blocked", json!({"reason": result.blocked_reason}));
            }
            "done" => {
                // collect candidate from the worktree (local observation, not agent claims)
                let wt = self
                    .store
                    .list_task_worktrees(task_id)
                    .into_iter()
                    .find(|w| w.generation == t.current_generation);
                let collected = wt
                    .as_ref()
                    .map(|w| worktree::observe_worktree(&self.cfg.git, &w.path))
                    .transpose()
                    .ok()
                    .flatten();
                let has_changes = collected
                    .as_ref()
                    .map(|o| !o.changed_files.is_empty() || !o.untracked_files.is_empty())
                    .unwrap_or(false);
                let mut notes = String::new();
                if !has_changes {
                    notes = "writer reported done but no file changes observed in worktree".into();
                }
                let _ = state;
                let _ = exit;
                if t.recipe == Recipe::WriteThenReview && has_changes {
                    // freeze candidate then chain the reviewer
                    match wt.as_ref().map(|_w| worktree::freeze_candidate(&self.store, &self.cfg.git, &t, attempt_id, t.current_generation)) {
                        Some(Ok(_cand)) => {
                            // a new candidate supersedes old-candidate reviews (kept as history)
                            self.store.mark_reviews_historical(task_id);
                            // re-run the reviewer for the new candidate (same-generation
                            // guard still prevents double dispatch within one generation)
                            let reviewer_step = self
                                .store
                                .latest_plan(task_id)
                                .and_then(|p| {
                                    p.steps
                                        .iter()
                                        .find(|s| s.role == Role::Reviewer)
                                        .map(|s| s.step_id.clone())
                                });
                            if let Some(step) = reviewer_step {
                                self.store.meta_set(&format!("next_step:{}", task_id), &step);
                            }
                            t.status = TaskStatus::NeedsReview;
                            t.updated_at = timestamp();
                            let _ = self.store.update_task(&t);
                            self.store.resolve_message_by_task(task_id, "blocked");
                            self.bus.emit(
                                Some(task_id),
                                Some(attempt_id),
                                t.current_generation,
                                "result_collected",
                                json!({"candidate_pending": true, "generation": t.current_generation}),
                            );
                            // reviewer dispatch happens through the scheduler (slot/budget gates)
                        }
                        Some(Err(e)) => {
                            t.status = TaskStatus::Interrupted;
                            t.updated_at = timestamp();
                            let _ = self.store.update_task(&t);
                            self.upsert_inbox(task_id, "blocked", json!({"task_id": task_id, "reason": format!("freeze failed: {e}")}), format!("blocked:{}:{}", task_id, t.current_generation));
                            self.bus.emit(
                                Some(task_id),
                                Some(attempt_id),
                                t.current_generation,
                                "task_blocked",
                                json!({"reason": e}),
                            );
                        }
                        None => {
                            t.status = TaskStatus::Interrupted;
                            let _ = self.store.update_task(&t);
                        }
                    }
                } else {
                    // direct recipe: candidate recorded; awaiting accept
                    if has_changes {
                        let frozen = wt
                            .as_ref()
                            .map(|_w| worktree::freeze_candidate(&self.store, &self.cfg.git, &t, attempt_id, t.current_generation));
                        if let Some(Err(e)) = frozen {
                            t.status = TaskStatus::Interrupted;
                            t.updated_at = timestamp();
                            let _ = self.store.update_task(&t);
                            self.upsert_inbox(task_id, "blocked", json!({"task_id": task_id, "reason": format!("freeze failed: {e}")}), format!("blocked:{}:{}", task_id, t.current_generation));
                            self.bus.emit(
                                Some(task_id),
                                Some(attempt_id),
                                t.current_generation,
                                "task_blocked",
                                json!({"reason": e}),
                            );
                            return;
                        }
                        t.status = TaskStatus::NeedsReview;
                        t.updated_at = timestamp();
                        let _ = self.store.update_task(&t);
                        self.upsert_inbox(task_id, "result_ready", json!({"task_id": task_id, "note": notes}), format!("result:{}", task_id));
                        self.bus.emit(
                            Some(task_id),
                            Some(attempt_id),
                            t.current_generation,
                            "result_collected",
                            json!({"generation": t.current_generation}),
                        );
                    } else {
                        t.status = TaskStatus::Interrupted;
                        t.updated_at = timestamp();
                        let _ = self.store.update_task(&t);
                        self.upsert_inbox(task_id, "blocked", json!({"task_id": task_id, "reason": notes}), format!("blocked:{}:{}", task_id, t.current_generation));
                    }
                }
            }
            _ => {
                t.status = TaskStatus::Interrupted;
                t.updated_at = timestamp();
                let _ = self.store.update_task(&t);
            }
        }
    }

    fn finish_reviewer(
        self: &Arc<Self>,
        task_id: &str,
        attempt_id: &str,
        ctx: &DispatchContext,
        task: &Task,
        result: &AgentResult,
        state: &adapters::RunState,
    ) {
        // the reviewed candidate is the one bound at reviewer launch (WP04)
        let bound = ctx
            .bound_candidate_hash
            .clone()
            .or_else(|| self.store.current_candidate(task_id).map(|c| c.candidate_hash));
        let cand = bound.and_then(|h| self.store.get_candidate(task_id, &h));
        let verdict = review_verdict_from(result, state);
        let mut t = task.clone();
        if let Some(cand) = &cand {
            // candidate must still be current (G5: review binds the hash it saw)
            let still_current = self
                .store
                .current_candidate(task_id)
                .map(|c| c.candidate_hash == cand.candidate_hash)
                .unwrap_or(false);
            let review = ReviewRecord {
                review_id: new_id("r"),
                task_id: task_id.to_string(),
                revision: t.revision,
                attempt_id: attempt_id.to_string(),
                candidate_hash: cand.candidate_hash.clone(),
                reviewer_adapter: self
                    .store
                    .get_attempt(attempt_id)
                    .map(|a| a.adapter)
                    .unwrap_or_default(),
                verdict: Some(verdict.clone()),
                summary: result.summary.clone(),
                issues: serde_json::json!({"raw": result.summary}),
                historical: !still_current,
                created_at: timestamp(),
            };
            self.store.insert_review(&review).ok();
            t.status = TaskStatus::NeedsReview;
            t.updated_at = timestamp();
            let _ = self.store.update_task(&t);
            self.upsert_inbox(
                task_id,
                "review_ready",
                json!({"task_id": task_id, "verdict": verdict, "candidate": &cand.candidate_hash[..8]}),
                format!("review:{}:{}", task_id, cand.candidate_hash),
            );
            self.bus.emit(
                Some(task_id),
                Some(attempt_id),
                attempt_generation(self, attempt_id),
                "result_collected",
                json!({"review": verdict, "candidate": &cand.candidate_hash[..8]}),
            );
        } else {
            t.status = TaskStatus::Interrupted;
            let _ = self.store.update_task(&t);
        }
    }

    /// Save a checkpoint from the current writer worktree (local observation).
    pub fn save_checkpoint(
        &self,
        task_id: &str,
        attempt_id: &str,
        failure_summary: Option<&str>,
        source: CheckpointSource,
    ) -> Option<Checkpoint> {
        let task = self.store.get_task(task_id)?;
        // the writer attempt's own worktree carries the work (review generations 1000+ don't)
        let wt = self
            .store
            .get_attempt(attempt_id)
            .and_then(|a| a.worktree_id)
            .and_then(|wid| self.store.get_worktree(&wid));
        let wt = match wt {
            Some(w) => w,
            None => self
                .store
                .list_task_worktrees(task_id)
                .into_iter()
                .find(|w| w.generation == task.current_generation)?,
        };
        let obs = worktree::observe_worktree(&self.cfg.git, &wt.path).ok()?;
        let chash = worktree::candidate_hash(&self.cfg.git, &wt.path).ok()?;
        // patch = committed diff vs base + uncommitted diff (frozen candidates diff base..HEAD)
        let committed = worktree::run_git(
            &self.cfg.git,
            std::path::Path::new(&wt.path),
            &["diff", "--binary", &task.base_commit, "HEAD"],
            None,
        )
        .unwrap_or_default();
        let patch_text = format!("{committed}{}", obs.diff_text);
        let patch_rel = format!("patches/{}.g{}.checkpoint.patch", task_id, task.current_generation);
        let patch_path = self.cfg.managed_root.join(&patch_rel);
        let _ = std::fs::create_dir_all(patch_path.parent().unwrap());
        let _ = std::fs::write(&patch_path, &patch_text);
        // untracked file contents are checkpointed into a bundle (diffs cannot carry them)
        let bundle_dir = self
            .cfg
            .managed_root
            .join("checkpoints")
            .join(format!("{}.g{}.checkpoint", task_id, task.current_generation));
        if !obs.untracked_files.is_empty() {
            let _ = std::fs::create_dir_all(&bundle_dir);
            for f in &obs.untracked_files {
                let src = std::path::Path::new(&wt.path).join(f);
                let dst = bundle_dir.join(f);
                if let Some(parent) = dst.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(&src, &dst);
            }
        }
        let cp = Checkpoint {
            checkpoint_id: new_id("cp"),
            task_id: task_id.to_string(),
            revision: task.revision,
            attempt_id: attempt_id.to_string(),
            base_commit: task.base_commit.clone(),
            candidate_hash: chash,
            changed_files: obs.changed_files,
            untracked_files: obs.untracked_files,
            patch_path: Some(patch_path.to_string_lossy().to_string()),
            failure_summary: failure_summary.map(|s| s.to_string()),
            source,
            created_at: timestamp(),
        };
        self.store.insert_checkpoint(&cp).ok()?;
        self.bus.emit(
            Some(task_id),
            Some(attempt_id),
            task.current_generation,
            "checkpoint_saved",
            json!({"checkpoint_id": cp.checkpoint_id, "candidate": &cp.candidate_hash[..8], "files": cp.changed_files.len()}),
        );
        Some(cp)
    }

    // ================= cancel =================

    pub fn request_cancel(self: &Arc<Self>, attempt_id: &str) -> Result<(), CoreError> {
        let attempt = self
            .store
            .get_attempt(attempt_id)
            .ok_or_else(|| CoreError::not_found("attempt not found"))?;
        if !attempt.status.live() {
            return Err(CoreError::conflict("not_running", "attempt is not live"));
        }
        let mut a = attempt.clone();
        a.status = AttemptStatus::Stopping;
        let _ = self.store.update_attempt(&a);
        let mut t = self
            .store
            .get_task(&attempt.task_id)
            .ok_or_else(|| CoreError::not_found("task not found"))?;
        t.status = TaskStatus::Stopping;
        t.updated_at = timestamp();
        let _ = self.store.update_task(&t);
        if let Some(flag) = self.cancel_flags.lock().unwrap().get(attempt_id) {
            flag.store(true, Ordering::SeqCst);
        } else {
            // no live runner in this process (e.g. after restart) — record stop unconfirmed
            a.status = AttemptStatus::Unknown;
            let _ = self.store.update_attempt(&a);
            t.status = TaskStatus::StopUnconfirmed;
            t.updated_at = timestamp();
            let _ = self.store.update_task(&t);
        }
        self.bus.emit(
            Some(&attempt.task_id),
            Some(attempt_id),
            attempt.generation,
            "cancel_requested",
            json!({}),
        );
        Ok(())
    }

    // ================= decisions =================

    pub fn answer_decision(
        self: &Arc<Self>,
        decision_id: &str,
        option_id: &str,
        task_revision: u64,
        request: &mut RequestRecord,
    ) -> Result<(Decision, Task), CoreError> {
        let d = self
            .store
            .get_decision(decision_id)
            .ok_or_else(|| CoreError::not_found("decision not found"))?;
        let mut task = self
            .store
            .get_task(&d.task_id)
            .ok_or_else(|| CoreError::not_found("task not found"))?;
        // duplicate/expired answers never trigger a second launch
        if d.status == "answered" {
            return Ok((d, task)); // idempotent: same receipt, no new launch
        }
        if d.status != "open" {
            return Err(CoreError::conflict("decision_closed", "decision card is no longer open; refresh"));
        }
        if task_revision != task.revision {
            return Err(CoreError::conflict("revision_stale", "task has been revised; refresh the card"));
        }
        if !d.options.iter().any(|o| o.id == option_id) {
            return Err(CoreError::bad("unknown_option", "option id not on this card"));
        }
        if task.budget_remaining() <= 0 {
            return Err(CoreError::conflict("budget_exhausted", "resume budget exhausted"));
        }
        // atomic: mark answered + bind resume request id
        let mut d2 = d.clone();
        d2.status = "answered".into();
        d2.resolution = Some(option_id.to_string());
        d2.resume_request_id = Some(request.request_id.clone());
        d2.answered_at = Some(timestamp());
        self.store.update_decision(&d2)?;
        self.store.resolve_message_by_task(&d.task_id, "decision_opened");
        // queue resume: task back to queued; scheduler dispatches when slot free
        let writer_step = self
            .store
            .latest_plan(&d.task_id)
            .and_then(|p| p.steps.iter().find(|s| s.role == Role::Writer).map(|s| s.step_id.clone()))
            .unwrap_or_else(|| "s1".into());
        self.store.meta_set(&format!("next_step:{}", d.task_id), &writer_step);
        task.status = TaskStatus::Queued;
        task.resume_used += 1;
        task.updated_at = timestamp();
        self.store.update_task(&task)?;
        self.bus.emit(
            Some(&d.task_id),
            Some(&d.attempt_id),
            0,
            "decision_answered",
            json!({"decision_id": decision_id, "option": option_id, "queued": true}),
        );
        Ok((d2, task))
    }

    // ================= reroute =================

    pub fn reroute(
        self: &Arc<Self>,
        task_id: &str,
        target_adapter: &str,
        request: &mut RequestRecord,
    ) -> Result<Task, CoreError> {
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| CoreError::not_found("task not found"))?;
        // reroute eligible from stopped states with confirmed old writer exit + checkpoint,
        // or from needs_review when the current candidate's review found issues (manual reroute)
        let review_failed = task.status == TaskStatus::NeedsReview
            && self
                .store
                .current_candidate(task_id)
                .and_then(|c| self.store.latest_review_for(task_id, &c.candidate_hash))
                .map(|r| r.verdict.as_deref() == Some("issues"))
                .unwrap_or(false);
        let eligible = matches!(
            task.status,
            TaskStatus::Interrupted | TaskStatus::QuotaBlocked | TaskStatus::AuthRequired | TaskStatus::Cancelled
        ) || review_failed;
        if !eligible {
            return Err(CoreError::conflict(
                "reroute_not_eligible",
                "reroute needs a confirmed-stopped writer and saved checkpoint (or a failed review)",
            ));
        }
        let cp = match self.store.latest_checkpoint(task_id) {
            Some(cp) => Some(cp),
            None => {
                if review_failed {
                    let writer_attempt = self
                        .store
                        .current_candidate(task_id)
                        .map(|c| c.attempt_id)
                        .unwrap_or_default();
                    self.save_checkpoint(task_id, &writer_attempt, Some("review found issues; manual reroute"), CheckpointSource::LocalObserved)
                } else {
                    None
                }
            }
        }
        .ok_or_else(|| CoreError::conflict("checkpoint_missing", "no checkpoint to relay"))?;
        // old writer stop evidence: last writer attempt must not be live
        let old_writer_live = self
            .store
            .list_attempts(task_id)
            .iter()
            .any(|a| a.role == Role::Writer && a.status.live());
        if old_writer_live {
            return Err(CoreError::conflict("writer_still_running", "old writer has not confirmed exit"));
        }
        // target gate: only adapters that independently passed the write gate
        let states = self.adapter_states();
        let target = states
            .iter()
            .find(|s| s.id == target_adapter)
            .ok_or_else(|| CoreError::bad("unknown_adapter", format!("unknown adapter {target_adapter}")))?;
        if !target.write_ok || !target.available {
            return Err(CoreError::conflict(
                "write_gate_failed",
                format!("{target_adapter} has not passed the independent write-capability gate; export the patch and hand off manually"),
            ));
        }
        // WP10: disclose the FULL relay cost before any launch — the relay writer
        // plus, for write_then_review, its paired reviewer re-run.
        let needs_reviewer = self
            .store
            .latest_plan(task_id)
            .map(|p| p.recipe == Recipe::WriteThenReview)
            .unwrap_or(false);
        let needed = 1 + u64::from(needs_reviewer);
        let remaining = task.budget_remaining();
        if remaining < needed as i64 {
            return Err(CoreError::conflict(
                "budget_exhausted",
                format!(
                    "reroute needs {needed} launch(es) (writer{}), but only {remaining} remain in the approved budget; increase the budget explicitly and try again",
                    if needs_reviewer { " + review re-run" } else { "" }
                ),
            ));
        }
        if !self.repo_trusted(&task.repo_path) {
            return Err(CoreError::conflict("repo_untrusted", "repository is not trusted"));
        }
        let live = self.store.list_live_attempts();
        if !live.is_empty() {
            return Err(CoreError::busy("no_free_slot", "execution slot is busy"));
        }
        // queue the relay: the scheduler dispatches generation+1 from the checkpoint
        let writer_step = self
            .store
            .latest_plan(task_id)
            .and_then(|p| p.steps.iter().find(|s| s.role == Role::Writer).map(|s| s.step_id.clone()))
            .unwrap_or_else(|| "s1".into());
        self.store.meta_set(&format!("next_step:{}", task_id), &writer_step);
        let mut t = task;
        t.status = TaskStatus::Queued;
        t.resume_used += 1;
        t.updated_at = timestamp();
        self.store.update_task(&t)?;
        self.store.resolve_message_by_task(task_id, "blocked");
        self.bus.emit(
            Some(task_id),
            Some(&request.request_id),
            t.current_generation + 1,
            "reroute_started",
            json!({"target": target_adapter, "checkpoint": cp.checkpoint_id, "generation": t.current_generation + 1}),
        );
        Ok(t)
    }

    // ================= accept =================

    pub fn accept(self: &Arc<Self>, task_id: &str, candidate_hash: &str, _request: &mut RequestRecord) -> Result<(Task, Value), CoreError> {
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| CoreError::not_found("task not found"))?;
        if task.status == TaskStatus::Accepted {
            // idempotent: same candidate → same receipt
            if task.accepted_candidate_hash.as_deref() == Some(candidate_hash) {
                return Ok((task, json!({"accepted": true, "replay": true})));
            }
            return Err(CoreError::conflict("already_accepted", "another candidate is already accepted"));
        }
        if !matches!(task.status, TaskStatus::NeedsReview) {
            return Err(CoreError::conflict("not_ready", "task is not in needs_review state"));
        }
        let cand = self
            .store
            .get_candidate(task_id, candidate_hash)
            .ok_or_else(|| CoreError::not_found("candidate not found"))?;
        if !cand.current {
            return Err(CoreError::conflict("candidate_stale", "candidate is historical; re-collect first"));
        }
        // WP04: write_then_review acceptance requires a valid PASS review bound to
        // this exact candidate (same revision, same generation, not historical).
        // The direct recipe keeps manual acceptance.
        if task.recipe == Recipe::WriteThenReview {
            let review = self.store.latest_review_for(task_id, candidate_hash);
            match review {
                None => {
                    return Err(CoreError::conflict(
                        "review_required",
                        "write_then_review candidates need a review of this candidate before acceptance",
                    ));
                }
                Some(r) if r.historical => {
                    return Err(CoreError::conflict(
                        "candidate_stale",
                        "the review for this candidate is historical; re-collect first",
                    ));
                }
                Some(r) if r.verdict.as_deref() != Some("pass") => {
                    return Err(CoreError::conflict(
                        "review_not_passed",
                        format!("review verdict is {:?}; only pass candidates are acceptable", r.verdict),
                    ));
                }
                Some(_) => {}
            }
        }
        // WP04 (0700): acceptance must deliver a real patch — an export failure is a
        // stable error and the task stays receivable; nothing is marked accepted.
        let exported = worktree::export_patch(&self.store, &self.cfg.git, &task, &cand)
            .map_err(|e| {
                CoreError::conflict(
                    "patch_export_failed",
                    format!("could not write the acceptance patch: {e}; fix the path and accept again"),
                )
            })?
            .to_string_lossy()
            .to_string();
        let mut t = task;
        t.status = TaskStatus::Accepted;
        t.accepted_candidate_hash = Some(candidate_hash.to_string());
        t.updated_at = timestamp();
        self.store.update_task(&t)?;
        self.store.resolve_message_by_task(task_id, "result_ready");
        self.store.resolve_message_by_task(task_id, "review_ready");
        self.bus.emit(
            Some(task_id),
            None,
            t.current_generation,
            "candidate_accepted",
            json!({"candidate": &candidate_hash[..8.min(candidate_hash.len())], "revision": t.revision}),
        );
        let receipt_json = json!({
            "accepted": true,
            "candidate_hash": candidate_hash,
            "revision": t.revision,
            "patch": exported,
        });
        Ok((t, receipt_json))
    }

    // ================= recovery =================

    /// On boot: processes died with us (kill-on-close). Mark live attempts honestly.
    pub fn recover(&self) -> Vec<String> {
        let mut notes = Vec::new();
        for a in self.store.list_live_attempts() {
            let mut a2 = a.clone();
            // job object kill-on-close means children are dead, but we cannot observe it
            a2.status = AttemptStatus::Unknown;
            let _ = self.store.update_attempt(&a2);
            if let Some(mut t) = self.store.get_task(&a.task_id) {
                if t.status.active() || t.status == TaskStatus::Stopping {
                    t.status = TaskStatus::StopUnconfirmed;
                    t.updated_at = timestamp();
                    let _ = self.store.update_task(&t);
                    self.upsert_inbox(
                        &t.task_id,
                        "blocked",
                        json!({"task_id": t.task_id, "reason_code": "stop_unconfirmed"}),
                        format!("blocked:{}:{}", t.task_id, a.generation),
                    );
                    notes.push(format!("attempt {} → stop_unconfirmed", a.attempt_id));
                }
            }
        }
        // requests left processing: completed with recovery note (no auto-resend)
        // (walked lazily via GET /v1/requests)
        notes
    }

    /// Scheduler tick: start queued tasks when the slot is free. Runs in background.
    pub fn scheduler_tick(self: &Arc<Self>) {
        let live = self.store.list_live_attempts();
        if !live.is_empty() {
            return;
        }
        for task in self.store.list_tasks() {
            if task.status != TaskStatus::Queued && task.status != TaskStatus::NeedsReview {
                continue;
            }
            let Some(_plan) = self
                .store
                .latest_plan(&task.task_id)
                .filter(|p| p.approved_at.is_some() && p.task_revision == task.revision)
            else {
                continue;
            };
            if task.budget_remaining() <= 0 {
                continue;
            }
            // attempt dispatch with a synthetic scheduler request (idempotent per task+generation)
            let request_id = format!("sched:{}:{}:{}", task.task_id, task.current_generation, task.launches_used);
            let digest = sha256_hex(format!("schedule:{request_id}").as_bytes());
            let mut req = match self.begin_request(&request_id, "dispatch", Some(&task.task_id), digest) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if req.status == "completed" {
                continue;
            }
            match self.dispatch(&task.task_id, &mut req) {
                Ok(_) => {
                    let aid = req.attempt_id.clone();
                    self.finish_request(req, json!({"scheduled": true, "task_id": task.task_id}), aid);
                    return; // one per tick
                }
                Err(_) => {
                    // busy / no runnable step / transient gate → drop the synthetic
                    // request so a later tick retries when state changes
                    self.store.delete_request(&request_id);
                    continue;
                }
            }
        }
    }

    pub fn upsert_inbox(&self, task_id: &str, kind: &str, payload: Value, dedupe_key: String) {
        let m = Message {
            message_id: new_id("m"),
            task_id: task_id.to_string(),
            kind: kind.to_string(),
            payload,
            dedupe_key,
            merge_count: 1,
            resolved: false,
            created_at: timestamp(),
            resolved_at: None,
        };
        let _ = self.store.upsert_message(&m);
        self.bus.emit(
            Some(task_id),
            None,
            0,
            "inbox_updated",
            json!({"kind": kind}),
        );
    }

    // ================= handoff resume (same native session) =================

    /// Dispatch a resume delta to the SAME native session when confirmable; otherwise
    /// the caller must preview + approve a full compact package (mode stays first_run).
    pub fn resume_same_session_possible(&self, task_id: &str) -> (bool, Option<String>) {
        let task = match self.store.get_task(task_id) {
            Some(t) => t,
            None => return (false, None),
        };
        let attempts = self.store.list_attempts(task_id);
        let last_writer = attempts
            .iter()
            .filter(|a| a.role == Role::Writer)
            .max_by_key(|a| a.launch_no);
        let Some(a) = last_writer else {
            return (false, None);
        };
        if a.adapter == "fake" || a.adapter == "dsh" {
            return (false, a.native_id.clone());
        }
        // confirmable when we hold the native id and the adapter supports resume
        let ok = a.native_id.is_some()
            && matches!(task.status, TaskStatus::WaitingDecision)
            && a.status == AttemptStatus::Completed;
        (ok, a.native_id.clone())
    }

    pub fn inbox(&self) -> Vec<Message> {
        self.store.list_messages(true)
    }

    pub fn task_snapshot(&self, task_id: &str) -> Option<Value> {
        let task = self.store.get_task(task_id)?;
        let attempts = self.store.list_attempts(task_id);
        let plan = self.store.latest_plan(task_id);
        let events = self.store.list_task_events(task_id, 400);
        let decisions = self
            .store
            .list_decisions(false)
            .into_iter()
            .filter(|d| d.task_id == task_id)
            .collect::<Vec<_>>();
        let reviews = self.store.list_reviews(task_id);
        let candidates = self.store.list_candidates(task_id);
        let worktrees = self.store.list_task_worktrees(task_id);
        let checkpoints: Vec<Value> = Vec::new();
        let has_open_decision = decisions.iter().any(|d| d.status == "open");
        let receipt = routing::build_receipt(
            &self
                .store
                .meta_get(&format!("last_request:{task_id}"))
                .unwrap_or_default(),
            &task,
            attempts.last().map(|a| a.attempt_id.as_str()),
            has_open_decision,
        );
        Some(json!({
            "task": task,
            "plan": plan,
            "attempts": attempts,
            "events": events,
            "decisions": decisions,
            "reviews": reviews,
            "candidates": candidates,
            "worktrees": worktrees,
            "checkpoints": checkpoints,
            "receipt": receipt,
        }))
    }
}

fn attempt_generation(core: &Core, attempt_id: &str) -> u64 {
    core.store
        .get_attempt(attempt_id)
        .map(|a| a.generation)
        .unwrap_or(0)
}

fn adapter_result_fallback(state: &adapters::RunState, exit: &crate::runner::ExitOutcome) -> AgentResult {
    if let Some(t) = &state.last_text {
        if let Some(r) = adapters::extract_agent_result(t) {
            return r;
        }
    }
    if exit.killed {
        return AgentResult {
            status: "blocked".into(),
            blocked_reason: Some("stopped before reporting".into()),
            ..Default::default()
        };
    }
    if exit.exit_code != Some(0) {
        return AgentResult {
            status: "blocked".into(),
            blocked_reason: Some(format!("exit code {:?}", exit.exit_code)),
            ..Default::default()
        };
    }
    AgentResult {
        status: "done".into(),
        summary: state.last_text.clone().map(|t| t.chars().take(400).collect()),
        ..Default::default()
    }
}

fn review_verdict_from(result: &AgentResult, state: &adapters::RunState) -> String {
    if let Some(r) = &state.fake_result {
        if let Some(v) = r.summary.as_deref() {
            if v.contains("fail") {
                return "issues".into();
            }
        }
    }
    let text = format!(
        "{} {}",
        result.summary.clone().unwrap_or_default(),
        result.question.clone().unwrap_or_default()
    )
    .to_lowercase();
    if text.contains("pass") && !text.contains("issues") {
        "pass".into()
    } else if text.contains("issues") || text.contains("问题") || text.contains("bug") {
        "issues".into()
    } else if result.status == "blocked" {
        "blocked".into()
    } else {
        "unknown".into()
    }
}

fn normalize_repo(p: &str) -> String {
    p.trim_end_matches(['/', '\\']).replace('/', "\\")
}

fn fake_info() -> Value {
    json!({
        "id": "fake",
        "version": "0.1.0",
        "available": true,
        "simulated": true,
        "write_supported": true,
        "read_only_supported": true,
        "resume_supported": true,
        "cancel_supported": true,
        "probe_error": Value::Null,
    })
}

fn codex_info(cfg: &CoreConfig) -> Value {
    let ok = cfg.codex_exe.is_file() && cfg.codex_version.is_some();
    json!({
        "id": "codex",
        "version": cfg.codex_version,
        "available": ok,
        "simulated": false,
        "write_supported": ok,
        "read_only_supported": ok,
        "resume_supported": ok,
        "cancel_supported": ok,
        "probe_error": if ok { Value::Null } else { Value::String("codex CLI not found".into()) },
    })
}

fn claude_info(cfg: &CoreConfig) -> Value {
    let ok = cfg.claude_exe.is_file() && cfg.claude_version.is_some();
    json!({
        "id": "claude",
        "version": cfg.claude_version,
        "available": ok,
        "simulated": false,
        "write_supported": false,
        "read_only_supported": ok,
        "resume_supported": ok,
        "cancel_supported": ok,
        "probe_error": if ok { Value::Null } else { Value::String("claude CLI not found".into()) },
    })
}
