use crate::model::*;
use serde::Serialize;

/// Stable reason codes with bilingual text; the UI can recompute rejections from the same inputs.
#[derive(Clone, Debug, Serialize)]
pub struct Reason {
    pub reason_code: &'static str,
    pub text_zh: String,
    pub text_en: String,
}

pub fn reason(code: &'static str) -> Reason {
    let (zh, en) = match code {
        "adapter_disabled" => (
            "该运行时未被用户允许",
            "Runtime not allowed by the user",
        ),
        "version_unavailable" => (
            "未找到可用版本或入口不可执行",
            "No usable version found or entry not executable",
        ),
        "permission_insufficient" => (
            "运行时无法满足该角色所需权限",
            "Runtime cannot provide the permission this role needs",
        ),
        "repo_untrusted" => (
            "仓库未确认信任，先在工作台确认",
            "Repository is not trusted yet; confirm it on the desk first",
        ),
        "no_free_slot" => (
            "执行槽已占用，等待当前运行结束",
            "Execution slot is busy; wait for the current run to finish",
        ),
        "repo_writer_busy" => (
            "同一仓库已有写入者运行",
            "Another writer is already running for this repository",
        ),
        "quota_blocked" => (
            "该运行时额度已阻塞（已知），等待恢复",
            "Runtime quota is known-blocked; wait for recovery",
        ),
        "budget_exhausted" => (
            "启动预算已用完；增加预算需要你显式确认",
            "Launch budget exhausted; increasing it needs your explicit approval",
        ),
        "plan_stale" => (
            "计划已过期（任务已修订），需要重新确认",
            "Plan is stale (task revised); confirm again",
        ),
        "role_write_not_supported" => (
            "该运行时未通过写入能力门槛，不能承担写入角色",
            "Runtime has not passed the write-capability gate for a writer role",
        ),
        "capability_unverified" => (
            "该能力在本机版本上尚未验证；跑一次真实任务后自动验证",
            "Capability not yet verified on this machine's version; it verifies after one real run",
        ),
        "capability_experimental" => (
            "该运行时无法原生限制此角色，仅作实验/检测，不进入自动路由",
            "Runtime cannot natively restrict this role; experimental/detection only, never auto-routed",
        ),
        "scope_conflict" => (
            "允许路径与禁止项冲突",
            "Allowed paths conflict with forbidden paths",
        ),
        "resume_unconfirmed" => (
            "无法确认同一原生会话或交接版本，改发完整精简包",
            "Same native session or handoff version cannot be confirmed; sending the full compact package instead",
        ),
        _ => ("未知原因", "Unknown reason"),
    };
    Reason {
        reason_code: code,
        text_zh: zh.to_string(),
        text_en: en.to_string(),
    }
}

/// One candidate adapter evaluation for a role.
#[derive(Clone, Debug, Serialize)]
pub struct RouteCandidate {
    pub adapter: String,
    pub role: Role,
    pub ok: bool,
    pub reasons: Vec<Reason>,
    /// same-session resume can send a delta instead of the full package
    pub resume_delta_possible: bool,
    pub simulated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct RouteEvaluation {
    pub task_id: String,
    pub revision: u64,
    pub recipe: Recipe,
    pub candidates: Vec<RouteCandidate>,
    /// hard filter inputs, so the UI can recompute
    pub inputs: serde_json::Value,
}

/// Adapter runtime state as seen by routing (from the H1 capability table + live locks).
pub struct AdapterRuntimeState {
    pub id: String,
    pub available: bool,
    pub version: Option<String>,
    pub write_ok: bool,
    pub readonly_ok: bool,
    pub resume_ok: bool,
    pub quota_blocked: bool,
    pub simulated: bool,
    /// role capability cannot be natively restricted (实验/仅检测)
    pub experimental: bool,
}

pub struct RouteInputs<'a> {
    pub task: &'a Task,
    pub role: Role,
    /// number of live attempts in any task (global slot = 1)
    pub live_attempts: usize,
    /// live writer attempts on the same repo
    pub repo_live_writers: usize,
    pub repo_trusted: bool,
    pub adapter_states: &'a [AdapterRuntimeState],
}

/// Hard filter order: user-allowed runtimes → version/permission → repo trust →
/// global slot & same-repo writer lock → quota/budget. Deterministic, no model calls.
pub fn evaluate(inputs: &RouteInputs) -> RouteEvaluation {
    let mut candidates = Vec::new();
    for st in inputs.adapter_states {
        let mut reasons = Vec::new();
        let allowed = inputs.task.allowed_adapters.iter().any(|a| a == &st.id);
        if !allowed {
            reasons.push(reason("adapter_disabled"));
        }
        if !st.available {
            reasons.push(reason("version_unavailable"));
        }
        let role_ok = match inputs.role {
            Role::Writer => st.write_ok,
            Role::Reviewer => st.readonly_ok,
        };
        if st.available && !role_ok {
            // H1: name WHY the role gate failed — unverified on this version vs a
            // runtime that cannot natively restrict the role at all
            reasons.push(reason(if st.experimental {
                "capability_experimental"
            } else {
                "capability_unverified"
            }));
        }
        if inputs.role == Role::Writer && !st.write_ok {
            reasons.push(reason("role_write_not_supported"));
        }
        if !inputs.repo_trusted {
            reasons.push(reason("repo_untrusted"));
        }
        if inputs.live_attempts >= 1 {
            reasons.push(reason("no_free_slot"));
        }
        if inputs.role == Role::Writer && inputs.repo_live_writers >= 1 {
            reasons.push(reason("repo_writer_busy"));
        }
        if st.quota_blocked {
            reasons.push(reason("quota_blocked"));
        }
        if inputs.task.budget_remaining() <= 0 {
            reasons.push(reason("budget_exhausted"));
        }
        // scope sanity
        let conflict = inputs
            .task
            .forbidden_paths
            .iter()
            .any(|f| inputs.task.allowed_paths.iter().any(|a| a == f));
        if conflict {
            reasons.push(reason("scope_conflict"));
        }
        candidates.push(RouteCandidate {
            adapter: st.id.clone(),
            role: inputs.role,
            ok: reasons.is_empty(),
            reasons,
            resume_delta_possible: st.resume_ok && st.available,
            simulated: st.simulated,
        });
    }
    // keep user priority order (task.allowed_adapters) first, then stable id
    candidates.sort_by_key(|c| {
        let prio = inputs
            .task
            .allowed_adapters
            .iter()
            .position(|a| a == &c.adapter)
            .unwrap_or(usize::MAX);
        (prio, c.adapter.clone())
    });
    RouteEvaluation {
        task_id: inputs.task.task_id.clone(),
        revision: inputs.task.revision,
        recipe: inputs.task.recipe,
        candidates,
        inputs: serde_json::json!({
            "allowed_adapters": inputs.task.allowed_adapters,
            "role": inputs.role.as_str(),
            "live_attempts": inputs.live_attempts,
            "repo_live_writers": inputs.repo_live_writers,
            "repo_trusted": inputs.repo_trusted,
            "budget_remaining": inputs.task.budget_remaining(),
        }),
    }
}

/// Next-action enum shared by Web and CLI receipts (contract: bounded, generated, never model text).
pub fn next_action_for(task: &Task, has_open_decision: bool) -> serde_json::Value {
    use serde_json::json;
    let (action, target) = match task.status {
        TaskStatus::AwaitingApproval => ("approve_plan", json!(task.task_id)),
        TaskStatus::Queued => ("dispatch", json!(task.task_id)),
        TaskStatus::Starting | TaskStatus::Running | TaskStatus::NeedsInput => {
            ("view_status", json!(task.task_id))
        }
        TaskStatus::Stopping => ("view_status", json!(task.task_id)),
        TaskStatus::WaitingDecision => {
            if has_open_decision {
                ("answer_decision", json!(task.task_id))
            } else {
                ("reconcile", json!(task.task_id))
            }
        }
        TaskStatus::NeedsReview => ("view_candidate", json!(task.task_id)),
        TaskStatus::Accepted => ("view_candidate", json!(task.task_id)),
        TaskStatus::QuotaBlocked | TaskStatus::AuthRequired | TaskStatus::Interrupted => {
            ("consider_reroute", json!(task.task_id))
        }
        TaskStatus::StopUnconfirmed => ("reconcile", json!(task.task_id)),
        TaskStatus::RerouteQueued => ("view_status", json!(task.task_id)),
        TaskStatus::Cancelled | TaskStatus::Failed => ("view_task", json!(task.task_id)),
    };
    json!({
        "action": action,
        "target": target,
        "label_zh": next_label_zh(action),
        "label_en": next_label_en(action),
    })
}

pub fn next_label_zh(a: &str) -> &'static str {
    match a {
        "approve_plan" => "确认计划",
        "dispatch" => "派发",
        "view_status" => "查看状态",
        "answer_decision" => "回答决策卡",
        "reconcile" => "检查原请求",
        "view_candidate" => "查看候选",
        "consider_reroute" => "查看换轨选项",
        "view_task" => "查看任务",
        _ => "查看状态",
    }
}

pub fn next_label_en(a: &str) -> &'static str {
    match a {
        "approve_plan" => "Approve plan",
        "dispatch" => "Dispatch",
        "view_status" => "View status",
        "answer_decision" => "Answer decision",
        "reconcile" => "Check original request",
        "view_candidate" => "View candidate",
        "consider_reroute" => "View reroute options",
        "view_task" => "View task",
        _ => "View status",
    }
}

/// The receipt shared by Web and CLI (contract §回执).
pub fn build_receipt(
    request_id: &str,
    task: &Task,
    attempt_id: Option<&str>,
    has_open_decision: bool,
) -> serde_json::Value {
    serde_json::json!({
        "request_id": request_id,
        "task_id": task.task_id,
        "revision": task.revision,
        "attempt_id": attempt_id,
        "status": task.status.as_str(),
        "next_action": next_action_for(task, has_open_decision),
        "candidate_hash": task.accepted_candidate_hash,
    })
}
