use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------- status enums ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// submitted draft, awaiting plan approval
    AwaitingApproval,
    /// plan approved, waiting for dispatch
    Queued,
    /// dispatch intent persisted, process starting
    Starting,
    Running,
    /// agent reported needs_decision/blocked; process exited; checkpoint persisted
    WaitingDecision,
    /// writer finished, candidate frozen, review pending or running
    NeedsReview,
    /// human accepted candidate (version-bound)
    Accepted,
    QuotaBlocked,
    AuthRequired,
    /// running agent requested input via structured result, process still owned
    NeedsInput,
    /// cancel requested, waiting for process tree exit
    Stopping,
    Cancelled,
    /// process crashed or was killed; changes kept
    Interrupted,
    /// cancel issued but process exit could not be confirmed; slot kept
    StopUnconfirmed,
    /// reroute approved and new attempt starting on checkpoint
    RerouteQueued,
    /// terminal failure (adapter launch failure etc.), changes kept
    Failed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::AwaitingApproval => "awaiting_approval",
            TaskStatus::Queued => "queued",
            TaskStatus::Starting => "starting",
            TaskStatus::Running => "running",
            TaskStatus::WaitingDecision => "waiting_decision",
            TaskStatus::NeedsReview => "needs_review",
            TaskStatus::Accepted => "accepted",
            TaskStatus::QuotaBlocked => "quota_blocked",
            TaskStatus::AuthRequired => "auth_required",
            TaskStatus::NeedsInput => "needs_input",
            TaskStatus::Stopping => "stopping",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Interrupted => "interrupted",
            TaskStatus::StopUnconfirmed => "stop_unconfirmed",
            TaskStatus::RerouteQueued => "reroute_queued",
            TaskStatus::Failed => "failed",
        }
    }
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Accepted | TaskStatus::Cancelled | TaskStatus::Failed
        )
    }
    pub fn active(&self) -> bool {
        matches!(
            self,
            TaskStatus::Starting
                | TaskStatus::Running
                | TaskStatus::NeedsInput
                | TaskStatus::Stopping
                | TaskStatus::NeedsReview
        )
    }
}

/// review runs inside NeedsReview; kept as attempt-level state
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collecting {
    Collecting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Pending,
    Starting,
    Running,
    WaitingInput,
    Stopping,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

impl AttemptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AttemptStatus::Pending => "pending",
            AttemptStatus::Starting => "starting",
            AttemptStatus::Running => "running",
            AttemptStatus::WaitingInput => "waiting_input",
            AttemptStatus::Stopping => "stopping",
            AttemptStatus::Completed => "completed",
            AttemptStatus::Failed => "failed",
            AttemptStatus::Cancelled => "cancelled",
            AttemptStatus::Unknown => "unknown",
        }
    }
    pub fn live(&self) -> bool {
        matches!(
            self,
            AttemptStatus::Pending
                | AttemptStatus::Starting
                | AttemptStatus::Running
                | AttemptStatus::WaitingInput
                | AttemptStatus::Stopping
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Writer,
    Reviewer,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Writer => "writer",
            Role::Reviewer => "reviewer",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recipe {
    #[default]
    Direct,
    WriteThenReview,
}

impl Recipe {
    pub fn as_str(&self) -> &'static str {
        match self {
            Recipe::Direct => "direct",
            Recipe::WriteThenReview => "write_then_review",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointSource {
    AgentReported,
    LocalObserved,
    Independent,
}

// ---------- entities ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub task_id: String,
    pub revision: u64,
    pub title: String,
    pub goal: String,
    pub acceptance: Vec<String>,
    /// user repo root (absolute). worktrees are created in managed root, never in user repo.
    pub repo_path: String,
    pub repo_id: String,
    pub base_commit: String,
    pub allowed_paths: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub recipe: Recipe,
    pub status: TaskStatus,
    /// launch budget: planned steps each get 1 launch; resume_budget shared by decision-resume and reroute
    pub planned_steps: u64,
    pub resume_budget: u64,
    pub resume_used: u64,
    pub launches_used: u64,
    /// runtimes allowed by the user, priority order
    pub allowed_adapters: Vec<String>,
    /// timeout per attempt, seconds
    pub timeout_secs: u64,
    pub current_generation: u64,
    pub accepted_candidate_hash: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Task {
    /// soft budget = planned steps + resume budget; launches may exceed on failure replays? No: every real launch counts, failures too.
    pub fn budget_total(&self) -> u64 {
        self.planned_steps + self.resume_budget
    }
    pub fn budget_remaining(&self) -> i64 {
        self.budget_total() as i64 - self.launches_used as i64
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanStep {
    pub step_id: String,
    pub role: Role,
    pub adapter: String,
    /// sandbox/permission requirement, e.g. workspace-write / read-only
    pub permission: String,
    pub depends_on: Vec<String>,
    /// handoff body for this step (assembled locally, 0 model calls)
    pub handoff: Option<HandoffBody>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Plan {
    pub plan_id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub recipe: Recipe,
    pub steps: Vec<PlanStep>,
    /// sha256 of the canonical plan content; approvals bind this
    pub summary_hash: String,
    pub created_at: String,
    pub approved_at: Option<String>,
    /// WP09: sha256 of the exact writer first-run body bound at approval time
    pub approved_handoff_digest: Option<String>,
    /// WP09: adapter → version bound at approval time; dispatch refuses drift
    pub approved_versions: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub attempt_id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub step_id: String,
    pub adapter: String,
    pub adapter_version: String,
    pub role: Role,
    pub worktree_id: Option<String>,
    pub generation: u64,
    /// native session/thread id (codex thread_id, claude session_id)
    pub native_id: Option<String>,
    /// request id of the dispatch that created this attempt (idempotency)
    pub request_id: Option<String>,
    pub status: AttemptStatus,
    /// launch order number within task
    pub launch_no: u64,
    pub exit_code: Option<i64>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    /// simulated attempts never drive production actions
    pub simulated: bool,
    /// handoff body digest bound at launch approval
    pub handoff_digest: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventRow {
    pub seq: u64,
    pub task_id: Option<String>,
    pub attempt_id: Option<String>,
    pub generation: u64,
    pub kind: String,
    pub payload: Value,
    pub ts: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub checkpoint_id: String,
    pub task_id: String,
    pub revision: u64,
    pub attempt_id: String,
    pub base_commit: String,
    /// hash of the candidate tree captured at checkpoint time
    pub candidate_hash: String,
    /// relative paths of changed tracked files
    pub changed_files: Vec<String>,
    /// relative paths of untracked files
    pub untracked_files: Vec<String>,
    /// path (managed root relative) of the patch file, if any
    pub patch_path: Option<String>,
    pub failure_summary: Option<String>,
    pub source: CheckpointSource,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DecisionOption {
    pub id: String,
    pub label_zh: String,
    pub label_en: String,
    pub effect_zh: String,
    pub effect_en: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Decision {
    pub decision_id: String,
    pub task_id: String,
    pub revision: u64,
    pub attempt_id: String,
    pub checkpoint_id: Option<String>,
    pub question: String,
    pub options: Vec<DecisionOption>,
    pub status: String, // open | answered | expired | superseded
    pub dedupe_key: String,
    pub merge_count: u64,
    pub resolution: Option<String>,
    pub resume_request_id: Option<String>,
    pub created_at: String,
    pub answered_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestRecord {
    pub request_id: String,
    pub kind: String, // plan|dispatch|cancel|reroute|answer|accept
    pub digest: String,
    pub task_id: Option<String>,
    pub attempt_id: Option<String>,
    /// completed | processing | unknown
    pub status: String,
    pub replayed: bool,
    pub result: Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub message_id: String,
    pub task_id: String,
    /// approval_needed | blocked | result_ready | decision_opened | review_ready
    pub kind: String,
    pub payload: Value,
    pub dedupe_key: String,
    pub merge_count: u64,
    pub resolved: bool,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeRecord {
    pub worktree_id: String,
    pub task_id: String,
    pub generation: u64,
    pub repo_path: String,
    pub path: String,
    pub base_commit: String,
    pub created_at: String,
    pub removed_at: Option<String>,
    pub keep_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub review_id: String,
    pub task_id: String,
    pub revision: u64,
    pub attempt_id: String,
    pub candidate_hash: String,
    pub reviewer_adapter: String,
    pub verdict: Option<String>, // pass | issues | blocked | unknown
    pub summary: Option<String>,
    pub issues: Value,
    pub historical: bool,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub candidate_hash: String,
    pub task_id: String,
    pub generation: u64,
    pub attempt_id: String,
    pub base_commit: String,
    pub frozen_commit: Option<String>,
    pub changed_files: Vec<String>,
    pub untracked_files: Vec<String>,
    pub patch_path: Option<String>,
    pub label: String, // A | B | ...
    pub current: bool,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandoffBody {
    pub text: String,
    pub bytes: usize,
    pub sha256: String,
    /// which facts were included, for the receipt / preview parity
    pub evidence_refs: Vec<String>,
    pub mode: String, // first_run | resume_delta | relay_package | review
    pub budget_bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdapterInfo {
    pub id: String,
    pub version: Option<String>,
    pub available: bool,
    pub simulated: bool,
    pub write_supported: bool,
    pub read_only_supported: bool,
    pub resume_supported: bool,
    pub cancel_supported: bool,
    pub probe_error: Option<String>,
}

// ---------- structured agent result ----------

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentResult {
    pub status: String, // done | needs_decision | blocked
    pub summary: Option<String>,
    pub question: Option<String>,
    pub options: Vec<DecisionOption>,
    pub blocked_reason: Option<String>,
    pub quota_blocked: bool,
    pub auth_required: bool,
}
