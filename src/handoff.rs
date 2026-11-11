use crate::ids::sha256_hex;
use crate::model::*;
use serde_json::json;

pub const DEFAULT_BODY_BUDGET: usize = 8 * 1024;
/// Notice prepended when optional material had to be omitted to fit the budget.
pub const TRUNCATED_NOTICE_ZH: &str =
    "【蜂递】正文超出预算，部分可选材料被整段省略；完整变更清单保存在受管工作区。";
pub const TRUNCATED_NOTICE_EN: &str =
    "[wagglepost] Body exceeded the budget; some optional material was omitted whole. The full change list stays in the managed workspace.";

/// Role-specific structured-result footer appended to every dispatch body.
/// Tells the agent how to report done / needs_decision / blocked.
pub fn result_contract_footer(lang: &'static str) -> String {
    if lang == "en" {
        "## Reporting contract (required)\n\
         End your run with exactly one fenced JSON block tagged `wagglepost-result`:\n\
         ```wagglepost-result\n\
         {\"status\":\"done\",\"summary\":\"...\"}\n\
         ```\n\
         Use {\"status\":\"needs_decision\",\"question\":\"...\",\"options\":[{\"id\":\"opt_keep\",\"label\":\"...\",\"effect\":\"...\"}]} to ask the user exactly one decision (2-4 options). Use {\"status\":\"blocked\",\"blocked_reason\":\"...\"} when you cannot continue (quota/auth/missing permission). Never include secrets in the block.\n".to_string()
    } else {
        "## 汇报契约（必须遵守）\n\
         结束时输出且只输出一个标记为 wagglepost-result 的围栏 JSON 块：\n\
         ```wagglepost-result\n\
         {\"status\":\"done\",\"summary\":\"...\"}\n\
         ```\n\
         需要用户决策时用 {\"status\":\"needs_decision\",\"question\":\"...\",\"options\":[{\"id\":\"opt_keep\",\"label\":\"...\",\"effect\":\"...\"}]}，只问一个决策，2–4 个选项。无法继续（额度/认证/权限缺失）时用 {\"status\":\"blocked\",\"blocked_reason\":\"...\"}。块内不得包含任何密钥。\n".to_string()
    }
}

/// Stable section identity. Titles are presentation only; selection and
/// priority are decided by these IDs and the handoff mode, never by title text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionId {
    Goal,
    Acceptance,
    Scope,
    Baseline,
    NextStep,
    ConfirmedProgress,
    FailureLocation,
    Answer,
    ChangedFields,
    ReviewQuestions,
    CandidateIdentity,
    Unresolved,
    KeyChanges,
    CheckResults,
    Evidence,
    /// Evidence references handed to the adapter separately from the body text.
    EvidenceRefs,
}

impl SectionId {
    fn title(self) -> String {
        // headers are bilingual in the source strings; parity across languages
        match self {
            SectionId::Goal => "目标 / Goal",
            SectionId::Acceptance => "验收条件 / Acceptance",
            SectionId::Scope => "范围与约束 / Scope",
            SectionId::Baseline => "基线 / Baseline",
            SectionId::NextStep => "下一步 / Next step",
            SectionId::ConfirmedProgress => "已确认进度 / Confirmed progress",
            SectionId::FailureLocation => "失败位置 / Failure location",
            SectionId::Answer => "本次回答 / Your answer",
            SectionId::ChangedFields => "变化字段 / Changed fields",
            SectionId::ReviewQuestions => "复核问题 / Review questions",
            SectionId::CandidateIdentity => "候选身份 / Candidate identity",
            SectionId::Unresolved => "未解决项 / Unresolved items",
            SectionId::KeyChanges => "关键变更路径 / Key change paths",
            SectionId::CheckResults => "检查结果 / Check results",
            SectionId::Evidence => "证据引用 / Evidence",
            SectionId::EvidenceRefs => "证据索引 / Evidence index",
        }
        .to_string()
    }

    /// Selection tier. Must-keep content is never trimmed: it either fully fits
    /// or the assembly blocks with required/limit. Remaining space first carries
    /// change paths and check results, then evidence descriptions and references.
    fn tier(self, mode: HandoffMode) -> Tier {
        match mode {
            HandoffMode::FirstRun => match self {
                SectionId::Goal
                | SectionId::Acceptance
                | SectionId::Scope
                | SectionId::Baseline
                | SectionId::NextStep => Tier::MustKeep,
                SectionId::KeyChanges | SectionId::CheckResults => Tier::First,
                _ => Tier::Then,
            },
            HandoffMode::RelayPackage => match self {
                SectionId::Goal
                | SectionId::Acceptance
                | SectionId::Scope
                | SectionId::ConfirmedProgress
                | SectionId::FailureLocation
                | SectionId::NextStep => Tier::MustKeep,
                SectionId::KeyChanges | SectionId::CheckResults => Tier::First,
                _ => Tier::Then,
            },
            HandoffMode::ResumeDelta => match self {
                SectionId::Answer | SectionId::ChangedFields => Tier::MustKeep,
                SectionId::KeyChanges | SectionId::CheckResults => Tier::First,
                _ => Tier::Then,
            },
            HandoffMode::Review => match self {
                SectionId::ReviewQuestions
                | SectionId::Acceptance
                | SectionId::CandidateIdentity
                | SectionId::Unresolved => Tier::MustKeep,
                SectionId::KeyChanges | SectionId::CheckResults => Tier::First,
                _ => Tier::Then,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    MustKeep,
    /// remaining space goes here first: change paths, check results
    First,
    /// then evidence descriptions and references
    Then,
}

/// Why an assembly cannot proceed. Must-keep content is bounded below by the
/// reporting contract, titles and separators; if that floor exceeds the budget
/// nothing is launched and the caller receives required/limit for the UI hint.
#[derive(Clone, Debug)]
pub enum HandoffError {
    OverBudget { required: usize, limit: usize },
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandoffError::OverBudget { required, limit } => write!(
                f,
                "handoff must-keep content requires {required} bytes but the dispatch budget is {limit}; reduce must-keep material or raise the limit"
            ),
        }
    }
}

/// One assembled handoff: the exact delivered body plus the U3 交接账单 (bill:
/// what must be sent, what was omitted this time, byte counts, digest binding).
#[derive(Clone, Debug)]
pub struct Assembled {
    pub body: HandoffBody,
    pub bill: HandoffBill,
}

/// Exact render of a section selection: `## title\n{body}\n` blocks separated
/// by one newline, closed by one final newline. Byte counts below come from
/// rendering, so titles, numbering, CJK and emoji all count their real UTF-8
/// bytes and no boundary can split a character.
fn render_text(all: &[(SectionId, String)]) -> String {
    let mut text = String::new();
    for (id, body) in all {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("## {}\n{}\n", id.title(), body));
    }
    text.push('\n');
    text
}

/// Deterministic local assembly for a role: zero model calls. Sections are
/// selected whole — content boundaries inside a section body cannot be
/// determined reliably (prose, code fences, lists), so a section is either
/// kept entire or omitted entire; evidence references have definite item
/// boundaries and are fitted one by one. Fixed input, language and budget
/// always produce byte-identical output.
pub fn assemble(
    mode: HandoffMode,
    _task: &Task,
    parts: HandoffParts,
    lang: &'static str,
    budget: usize,
) -> Result<Assembled, HandoffError> {
    // ---- 1. collect sections in the mode's fixed display order ----
    let mut sections: Vec<(SectionId, String)> = Vec::new();
    let mut push = |id: SectionId, body: String| {
        // empty old fields are skipped here; required-ness is enforced by the
        // existing business validation outside the assembler
        if !body.trim().is_empty() {
            sections.push((id, body));
        }
    };
    match mode {
        HandoffMode::FirstRun => {
            push(SectionId::Goal, parts.goal);
            push(SectionId::Acceptance, parts.acceptance);
            push(SectionId::Scope, parts.scope);
            push(SectionId::Baseline, parts.baseline);
            if let Some(next) = parts.next_step {
                push(SectionId::NextStep, next);
            }
            if let Some(kc) = parts.key_changes {
                push(SectionId::KeyChanges, kc);
            }
            if let Some(cr) = parts.check_results {
                push(SectionId::CheckResults, cr);
            }
            if let Some(ev) = parts.evidence {
                push(SectionId::Evidence, ev);
            }
        }
        HandoffMode::ResumeDelta => {
            // same native session confirmed: send only this round's answer + changed fields
            if let Some(a) = parts.answer {
                push(SectionId::Answer, a);
            }
            if let Some(changed) = parts.changed_fields {
                push(SectionId::ChangedFields, changed);
            }
            if let Some(kc) = parts.key_changes {
                push(SectionId::KeyChanges, kc);
            }
            if let Some(cr) = parts.check_results {
                push(SectionId::CheckResults, cr);
            }
            if let Some(ev) = parts.evidence {
                push(SectionId::Evidence, ev);
            }
        }
        HandoffMode::RelayPackage => {
            push(SectionId::Goal, parts.goal);
            push(SectionId::Acceptance, parts.acceptance);
            push(SectionId::Scope, parts.scope);
            if let Some(cp) = parts.confirmed_progress {
                push(SectionId::ConfirmedProgress, cp);
            }
            if let Some(fail) = parts.failure_location {
                push(SectionId::FailureLocation, fail);
            }
            if let Some(kc) = parts.key_changes {
                push(SectionId::KeyChanges, kc);
            }
            if let Some(cr) = parts.check_results {
                push(SectionId::CheckResults, cr);
            }
            if let Some(ev) = parts.evidence {
                push(SectionId::Evidence, ev);
            }
            if let Some(next) = parts.next_step {
                push(SectionId::NextStep, next);
            }
        }
        HandoffMode::Review => {
            push(SectionId::ReviewQuestions, parts.review_questions);
            push(SectionId::Acceptance, parts.acceptance);
            push(SectionId::CandidateIdentity, parts.candidate_identity);
            if let Some(kc) = parts.key_changes {
                push(SectionId::KeyChanges, kc);
            }
            if let Some(cr) = parts.check_results {
                push(SectionId::CheckResults, cr);
            }
            if let Some(u) = parts.unresolved {
                push(SectionId::Unresolved, u);
            }
        }
    }
    let refs = &parts.evidence_refs;

    let footer = result_contract_footer(lang);
    let footer_bytes = footer.as_bytes().len();
    let notice = if lang == "en" {
        TRUNCATED_NOTICE_EN
    } else {
        TRUNCATED_NOTICE_ZH
    };
    let notice_cost = notice.as_bytes().len() + 2; // notice + blank line before the body

    // ---- 2. must-keep floor: full must-keep body + result contract ----
    let must: Vec<(SectionId, String)> = sections
        .iter()
        .filter(|(id, _)| id.tier(mode) == Tier::MustKeep)
        .cloned()
        .collect();
    let must_bytes = render_text(&must).as_bytes().len();
    let floor = must_bytes + footer_bytes;
    if floor > budget {
        return Err(HandoffError::OverBudget { required: floor, limit: budget });
    }

    // ---- 3. one deterministic selection pass over optionals + references ----
    // Sections are kept or omitted whole; references have definite item
    // boundaries and are fitted one by one into a SINGLE refs section that is
    // only attached at the end, so repeated fitting never duplicates titles or
    // references and the space they use is counted exactly once.
    #[derive(Default)]
    struct Selection {
        cur: Vec<(SectionId, String)>,
        included: Vec<SectionId>,
        excluded: Vec<BillSection>,
        omitted_count: usize,
        refs_selected: Vec<String>,
        refs_omitted_count: usize,
        refs_omitted_bytes: usize,
    }
    let fit = |mut remaining: usize| -> Selection {
        let mut sel = Selection { cur: must.clone(), ..Default::default() };
        let optional: Vec<(SectionId, String)> = sections
            .iter()
            .filter(|(id, _)| id.tier(mode) != Tier::MustKeep)
            .cloned()
            .collect();
        for tier in [Tier::First, Tier::Then] {
            for (id, body) in optional.iter() {
                if id.tier(mode) != tier {
                    continue;
                }
                let mut cand = sel.cur.clone();
                cand.push((*id, body.clone()));
                let delta =
                    render_text(&cand).as_bytes().len() - render_text(&sel.cur).as_bytes().len();
                if delta <= remaining {
                    sel.cur = cand;
                    sel.included.push(*id);
                } else {
                    sel.omitted_count += 1;
                    sel.excluded.push(BillSection {
                        title: id.title(),
                        bytes: body.as_bytes().len(),
                        reason: Some("over_budget_omitted".into()),
                        must: false,
                    });
                }
            }
        }
        // references: one growing section, cost measured against the sectionless base
        let base_bytes = render_text(&sel.cur).as_bytes().len();
        let mut refs_cost = 0usize;
        for r in refs {
            let mut lines = sel.refs_selected.clone();
            lines.push(r.clone());
            let body = lines
                .iter()
                .map(|d| format!("- {d}\n"))
                .collect::<String>();
            let mut cand = sel.cur.clone();
            cand.push((SectionId::EvidenceRefs, body));
            let total = render_text(&cand).as_bytes().len() - base_bytes;
            let delta = total - refs_cost;
            if delta <= remaining {
                remaining -= delta;
                refs_cost = total;
                sel.refs_selected.push(r.clone());
            } else {
                sel.omitted_count += 1;
                sel.refs_omitted_count += 1;
                sel.refs_omitted_bytes += r.as_bytes().len();
            }
        }
        if !sel.refs_selected.is_empty() {
            let body = sel
                .refs_selected
                .iter()
                .map(|d| format!("- {d}\n"))
                .collect::<String>();
            sel.cur.push((SectionId::EvidenceRefs, body));
        }
        sel
    };

    // Pass A fits everything into the plain floor headroom. When material had
    // to be omitted, the notice is counted on demand: refit with the notice
    // reserved if it can still fit alongside the must-keep floor; if the
    // notice itself cannot fit, the delivered body ships without it and the
    // bill still discloses every omission.
    let pass_a = fit(budget - floor);
    let (selection, notice_used) = if pass_a.omitted_count == 0 {
        (pass_a, false)
    } else if floor + notice_cost <= budget {
        let pass_b = fit(budget - floor - notice_cost);
        (pass_b, true)
    } else {
        (pass_a, false)
    };
    let Selection {
        cur,
        included: _,
        mut excluded,
        omitted_count,
        refs_selected: delivered_refs,
        refs_omitted_count: refs_omitted,
        refs_omitted_bytes,
    } = selection;
    let has_omissions = omitted_count > 0;
    if refs_omitted > 0 {
        excluded.push(BillSection {
            title: SectionId::EvidenceRefs.title(),
            bytes: refs_omitted_bytes,
            reason: Some("over_budget_omitted".into()),
            must: false,
        });
    }

    // ---- 5. render: must-keep first (display order), then fitted optional ----
    let text = render_text(&cur);
    let truncated = has_omissions;
    let delivered_text = if truncated && notice_used {
        format!("{notice}\n\n{text}{footer}")
    } else {
        format!("{text}{footer}")
    };
    let final_bytes = delivered_text.as_bytes().len();
    if final_bytes > budget {
        // construction guarantees this never triggers; fail closed if it ever does
        return Err(HandoffError::OverBudget { required: final_bytes, limit: budget });
    }

    // ---- 6. U3 交接账单: must-send, omitted-this-time, exact bytes ----
    let mut bill_included: Vec<BillSection> = cur
        .iter()
        .map(|(id, body)| BillSection {
            title: id.title(),
            bytes: body.as_bytes().len(),
            reason: None,
            must: id.tier(mode) == Tier::MustKeep,
        })
        .collect();
    bill_included.sort_by(|a, b| a.title.cmp(&b.title));
    excluded.sort_by(|a, b| a.title.cmp(&b.title));

    let body = HandoffBody {
        sha256: sha256_hex(delivered_text.as_bytes()),
        bytes: final_bytes,
        text: delivered_text,
        evidence_refs: delivered_refs.clone(),
        mode: mode.as_str().to_string(),
        budget_bytes: budget,
    };
    let bill = HandoffBill {
        mode: mode.as_str().into(),
        bytes: body.bytes,
        sha256: body.sha256.clone(),
        budget_bytes: budget,
        truncated,
        included: bill_included,
        excluded,
        footer_bytes,
        comparison_bytes: None,
        required_bytes: floor,
        omitted_count,
        refs_included: delivered_refs.len(),
        refs_omitted,
    };
    Ok(Assembled { body, bill })
}

/// U3 交接账单 for one assembled handoff: the exact scope the confirmation binds.
#[derive(Clone, Debug, serde::Serialize, Default)]
pub struct HandoffBill {
    pub mode: String,
    /// final delivered bytes (UTF-8), including notice and reporting contract
    pub bytes: usize,
    /// digest of the exact body this bill describes (approval binds this)
    pub sha256: String,
    pub budget_bytes: usize,
    /// true when optional material was omitted this time
    pub truncated: bool,
    /// sections actually sent, with their sent byte counts
    pub included: Vec<BillSection>,
    /// sections left out, with the reason (over_budget_omitted)
    pub excluded: Vec<BillSection>,
    /// bytes of the reporting contract footer (always present)
    pub footer_bytes: usize,
    /// bytes of the locally constructed comparison payload (对照), when computed
    pub comparison_bytes: Option<usize>,
    /// floor the must-keep content plus contract (and notice reserve) cannot go under
    pub required_bytes: usize,
    /// optional sections + references omitted this time for the budget
    pub omitted_count: usize,
    pub refs_included: usize,
    pub refs_omitted: usize,
}

#[derive(Clone, Debug, serde::Serialize, Default)]
pub struct BillSection {
    pub title: String,
    pub bytes: usize,
    pub reason: Option<String>,
    /// true when the section belongs to the mode's must-keep set
    pub must: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffMode {
    FirstRun,
    ResumeDelta,
    RelayPackage,
    Review,
}

impl HandoffMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            HandoffMode::FirstRun => "first_run",
            HandoffMode::ResumeDelta => "resume_delta",
            HandoffMode::RelayPackage => "relay_package",
            HandoffMode::Review => "review",
        }
    }
}

/// Buildable parts; empty strings are skipped.
#[derive(Default, Clone, Debug)]
pub struct HandoffParts {
    pub goal: String,
    pub acceptance: String,
    pub scope: String,
    pub baseline: String,
    pub answer: Option<String>,
    pub changed_fields: Option<String>,
    pub confirmed_progress: Option<String>,
    pub failure_location: Option<String>,
    pub key_changes: Option<String>,
    pub evidence: Option<String>,
    pub next_step: Option<String>,
    pub review_questions: String,
    pub candidate_identity: String,
    pub check_results: Option<String>,
    pub unresolved: Option<String>,
    pub evidence_refs: Vec<String>,
}

/// Deterministic digest of a bill's selection result (who got in, who was
/// omitted). Used by tests to pin that fixed input produces a fixed selection.
pub fn selection_digest(bill: &HandoffBill) -> String {
    sha256_hex(
        json!({
            "mode": bill.mode,
            "included": bill.included,
            "excluded": bill.excluded,
            "omitted_count": bill.omitted_count,
            "refs_included": bill.refs_included,
            "refs_omitted": bill.refs_omitted,
        })
        .to_string()
        .as_bytes(),
    )
}
