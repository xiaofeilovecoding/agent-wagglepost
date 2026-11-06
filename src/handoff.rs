use crate::ids::sha256_hex;
use crate::model::*;

pub const DEFAULT_BODY_BUDGET: usize = 8 * 1024;

/// Truncated notice appended when content must be reduced to fit budget.
pub const TRUNCATED_NOTICE_ZH: &str =
    "【蜂递】正文超出默认预算，以下材料经过裁剪；完整变更清单保存在受管工作区。";
pub const TRUNCATED_NOTICE_EN: &str =
    "[wagglepost] Body exceeded default budget; material below was trimmed. The full change list stays in the managed workspace.";

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

/// Assemble a handoff body for a role. Deterministic local assembly: zero model calls.
#[allow(clippy::too_many_arguments)]
pub fn assemble(
    mode: HandoffMode,
    _task: &Task,
    parts: HandoffParts,
    lang: &'static str,
    budget: usize,
) -> Result<HandoffBody, String> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut evidence_refs: Vec<String> = Vec::new();

    match mode {
        HandoffMode::FirstRun => {
            sections.push((zh_en(lang, "目标 / Goal", "Goal"), parts.goal));
            sections.push((
                zh_en(lang, "验收条件 / Acceptance", "Acceptance criteria"),
                parts.acceptance,
            ));
            sections.push((
                zh_en(lang, "范围与约束 / Scope", "Scope & constraints"),
                parts.scope,
            ));
            sections.push((
                zh_en(lang, "基线 / Baseline", "Baseline"),
                parts.baseline,
            ));
            if let Some(next) = parts.next_step {
                sections.push((zh_en(lang, "下一步 / Next step", "Next step"), next));
            }
        }
        HandoffMode::ResumeDelta => {
            // same native session confirmed: send only this round's answer + changed fields
            sections.push((
                zh_en(lang, "本次回答 / Your answer", "Answer to your question"),
                parts.answer.unwrap_or_default(),
            ));
            if let Some(changed) = parts.changed_fields {
                sections.push((zh_en(lang, "变化字段 / Changed fields", "Changed fields"), changed));
            }
            if let Some(ev) = parts.evidence {
                sections.push((zh_en(lang, "证据引用 / Evidence", "Evidence references"), ev));
            }
        }
        HandoffMode::RelayPackage => {
            sections.push((zh_en(lang, "目标 / Goal", "Goal"), parts.goal));
            sections.push((
                zh_en(lang, "验收条件 / Acceptance", "Acceptance criteria"),
                parts.acceptance,
            ));
            sections.push((
                zh_en(lang, "范围与约束 / Scope", "Scope & constraints"),
                parts.scope,
            ));
            sections.push((
                zh_en(lang, "已确认进度 / Confirmed progress", "Confirmed progress"),
                parts.confirmed_progress.unwrap_or_default(),
            ));
            if let Some(fail) = parts.failure_location {
                sections.push((
                    zh_en(lang, "失败位置 / Failure location", "Failure location"),
                    fail,
                ));
            }
            if let Some(kc) = parts.key_changes {
                sections.push((
                    zh_en(lang, "关键变更路径 / Key change paths", "Key change paths"),
                    kc,
                ));
            }
            if let Some(ev) = parts.evidence {
                sections.push((zh_en(lang, "证据索引 / Evidence index", "Evidence index"), ev));
            }
            if let Some(next) = parts.next_step {
                sections.push((zh_en(lang, "下一步 / Next step", "Next step"), next));
            }
        }
        HandoffMode::Review => {
            sections.push((
                zh_en(lang, "复核问题 / Review questions", "Review questions"),
                parts.review_questions,
            ));
            sections.push((
                zh_en(lang, "验收条件 / Acceptance", "Acceptance criteria"),
                parts.acceptance,
            ));
            sections.push((
                zh_en(lang, "候选身份 / Candidate identity", "Candidate identity"),
                parts.candidate_identity,
            ));
            sections.push((
                zh_en(lang, "关键变更 / Key changes", "Key changes"),
                parts.key_changes.unwrap_or_default(),
            ));
            if let Some(cr) = parts.check_results {
                sections.push((
                    zh_en(lang, "检查结果 / Check results", "Check results"),
                    cr,
                ));
            }
            if let Some(u) = parts.unresolved {
                sections.push((zh_en(lang, "未解决项 / Unresolved items", "Unresolved items"), u));
            }
        }
    }

    for r in &parts.evidence_refs {
        evidence_refs.push(r.clone());
    }

    let footer = result_contract_footer(lang);
    let mut text = String::new();
    for (i, (title, body)) in sections.iter().enumerate() {
        if !body.trim().is_empty() {
            if i > 0 || !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!("## {title}\n{body}\n"));
        }
    }
    text.push('\n');
    text.push_str(&footer);

    let bytes = text.as_bytes().len();
    if bytes <= budget {
        return Ok(finish(text, evidence_refs, mode, budget, bytes, None));
    }
    // over budget: trim the largest section bodies (keep required fields per contract),
    // append truncation notice, never silently drop the result contract.
    let mut trimmed: Vec<(String, String)> = sections
        .into_iter()
        .map(|(t, b)| {
            let cap = budget / sections_len_hint(mode);
            (t, trim_body(&b, cap))
        })
        .collect();
    let rebuild = |secs: &[(String, String)]| -> String {
        let mut t = String::new();
        for (i, (title, body)) in secs.iter().enumerate() {
            if !body.trim().is_empty() {
                if i > 0 {
                    t.push('\n');
                }
                t.push_str(&format!("## {title}\n{body}\n"));
            }
        }
        t.push('\n');
        t
    };
    let mut candidate_text = rebuild(&trimmed);
    candidate_text.push_str(&footer);
    // try removing optional sections entirely (evidence > key_changes > unresolved order kept last)
    let optional_order = [zh_en(lang, "证据索引 / Evidence index", "Evidence index"),
        zh_en(lang, "关键变更路径 / Key change paths", "Key change paths"),
        zh_en(lang, "未解决项 / Unresolved items", "Unresolved items")];
    for opt in optional_order.iter() {
        if candidate_text.as_bytes().len() <= budget {
            break;
        }
        let before = candidate_text.as_bytes().len();
        trimmed.retain(|(t, _)| t != opt);
        candidate_text = rebuild(&trimmed);
        candidate_text.push_str(&footer);
        if candidate_text.as_bytes().len() >= before {
            break;
        }
    }
    let bytes2 = candidate_text.as_bytes().len();
    if bytes2 > budget {
        return Err(format!(
            "handoff body {} bytes exceeds budget {} even after trimming; reduce material or raise this dispatch limit",
            bytes2, budget
        ));
    }
    let _ = trimmed.len();
    Ok(finish(
        candidate_text,
        evidence_refs,
        mode,
        budget,
        bytes2,
        Some(TRUNCATED_NOTICE_ZH.to_string()),
    ))
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

fn sections_len_hint(mode: HandoffMode) -> usize {
    match mode {
        HandoffMode::FirstRun => 5,
        HandoffMode::ResumeDelta => 3,
        HandoffMode::RelayPackage => 8,
        HandoffMode::Review => 6,
    }
}

fn trim_body(body: &str, cap: usize) -> String {
    if body.as_bytes().len() <= cap {
        return body.to_string();
    }
    // cut at line boundary under cap bytes
    let mut end = 0usize;
    for (i, line) in body.lines().enumerate() {
        let next = end + line.as_bytes().len() + 1;
        if next > cap {
            if i == 0 {
                end = cap.saturating_sub(3);
            }
            break;
        }
        end = next;
    }
    let mut cut = &body.as_bytes()[..end.min(body.len())];
    while !cut.is_empty() && std::str::from_utf8(cut).is_err() {
        cut = &cut[..cut.len() - 1];
    }
    let s = String::from_utf8_lossy(cut).to_string();
    format!("{}\n…", s.trim_end())
}

fn finish(
    mut text: String,
    evidence_refs: Vec<String>,
    mode: HandoffMode,
    budget: usize,
    bytes: usize,
    notice: Option<String>,
) -> HandoffBody {
    if let Some(n) = notice {
        text = format!("{n}

{text}");
    }
    HandoffBody {
        sha256: sha256_hex(text.as_bytes()),
        bytes: text.as_bytes().len().max(bytes),
        text,
        evidence_refs,
        mode: mode.as_str().to_string(),
        budget_bytes: budget,
    }
}

fn zh_en(_lang: &'static str, zh: &str, _en: &str) -> String {
    // section headers are bilingual in the source strings; keep parity across languages
    zh.to_string()
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
