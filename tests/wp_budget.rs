//! W1/W3: byte-budget selection gates — must-keep floors, whole-section
//! selection, omission counting, reference constraint, determinism, UTF-8
//! byte discipline, digest invalidation. Zero model calls, all synthetic.

mod common;

use serde_json::json;
use wagglepost::handoff::{self, HandoffError, HandoffMode, HandoffParts};
use wagglepost::model::Task;

fn task() -> Task {
    serde_json::from_value(json!({
        "task_id": "t_budget",
        "revision": 1,
        "title": "budget probe",
        "goal": "g",
        "acceptance": [],
        "repo_path": "D:/synthetic/repo",
        "repo_id": "synthetic",
        "base_commit": "0000000000000000000000000000000000000000",
        "allowed_paths": [],
        "forbidden_paths": [],
        "recipe": "direct",
        "status": "awaiting_approval",
        "planned_steps": 1,
        "resume_budget": 1,
        "resume_used": 0,
        "launches_used": 0,
        "allowed_adapters": ["fake"],
        "timeout_secs": 60,
        "current_generation": 0,
        "accepted_candidate_hash": null,
        "created_at": "2026-11-11T00:00:00Z",
        "updated_at": "2026-11-11T00:00:00Z"
    }))
    .unwrap()
}

fn first_run_parts() -> HandoffParts {
    HandoffParts {
        goal: "实现字节预算组包 / implement byte-budget assembly".into(),
        acceptance: "- 字节一致 / bytes identical".into(),
        scope: "允许路径 / allowed paths:\n  - src/handoff.rs".into(),
        baseline: "base_commit: 0000000".into(),
        next_step: Some("按验收条件实现 / implement".into()),
        ..Default::default()
    }
}

#[test]
fn budget_fixed_input_lang_budget_is_byte_identical() {
    let t = task();
    let a1 = handoff::assemble(HandoffMode::FirstRun, &t, first_run_parts(), "zh", 8 * 1024).unwrap();
    let a2 = handoff::assemble(HandoffMode::FirstRun, &t, first_run_parts(), "zh", 8 * 1024).unwrap();
    assert_eq!(a1.body.sha256, a2.body.sha256, "same input → same digest");
    assert_eq!(a1.body.text, a2.body.text);
    assert_eq!(a1.body.bytes, a2.body.bytes);
    assert_eq!(
        handoff::selection_digest(&a1.bill),
        handoff::selection_digest(&a2.bill),
        "same input → same selection"
    );
    assert_eq!(a1.body.bytes, a1.body.text.as_bytes().len(), "bytes are UTF-8 bytes");
}

#[test]
fn budget_must_keep_floor_blocks_with_required_and_limit() {
    let t = task();
    let mut parts = first_run_parts();
    parts.goal = "必".repeat(9 * 1024); // 27 KiB of CJK must-keep content
    let err = handoff::assemble(HandoffMode::FirstRun, &t, parts, "zh", 8 * 1024).unwrap_err();
    match err {
        HandoffError::OverBudget { required, limit } => {
            assert_eq!(limit, 8 * 1024);
            // required covers the FULL goal (3 bytes per char), titles, separators
            // and the reporting contract — not a trimmed fragment
            assert!(required >= 9 * 1024 * 3, "required {required} covers full content");
            assert!(required > limit);
        }
    }
}

#[test]
fn budget_must_keep_never_trimmed_optional_omitted_whole() {
    let t = task();
    let mut parts = first_run_parts();
    parts.key_changes = Some("k".repeat(8 * 1024)); // 8 KiB optional (tier 1)
    let a = handoff::assemble(HandoffMode::FirstRun, &t, parts, "zh", 8 * 1024).unwrap();
    // must-keep goal delivered in FULL (no partial trim of must-keep)
    assert!(a.body.text.contains("实现字节预算组包 / implement byte-budget assembly"));
    // optional oversized section omitted whole, never partially cut
    assert!(!a.body.text.contains("kkkkk"), "oversized optional omitted whole");
    assert_eq!(a.bill.omitted_count, 1);
    assert!(a.bill.truncated);
    assert!(a.body.text.contains("【蜂递】正文超出预算"), "notice present, never silent");
    assert!(a.body.bytes <= 8 * 1024, "delivered bytes respect the budget");
    assert!(a.bill.excluded.iter().any(|s| s.reason.as_deref() == Some("over_budget_omitted")));
}

#[test]
fn budget_skip_non_fitting_item_then_fit_shorter_ones() {
    let t = task();
    let parts = HandoffParts {
        goal: "g".into(),
        acceptance: "a".into(),
        scope: "s".into(),
        baseline: "b".into(),
        confirmed_progress: Some("cp".into()),
        failure_location: Some("fl".into()),
        next_step: Some("ns".into()),
        // tier 1: oversized change paths first, then small check results
        key_changes: Some("c".repeat(6 * 1024)),
        check_results: Some("checks ok ✅".into()),
        // tier 2: small evidence description
        evidence: Some("evidence note ✓".into()),
        ..Default::default()
    };
    let roomy = handoff::assemble(HandoffMode::RelayPackage, &t, parts.clone(), "zh", 8 * 1024).unwrap();
    assert_eq!(roomy.bill.omitted_count, 0, "everything fits under 8 KiB");
    // tight budget: must-keep floor + enough headroom for the notice reserve
    // and the two short sections — the big section is skipped and the shorter
    // ones still ride along
    let tight_budget = roomy.bill.required_bytes + 400;
    let tight = handoff::assemble(HandoffMode::RelayPackage, &t, parts, "zh", tight_budget).unwrap();
    assert!(tight.body.text.contains("checks ok ✅"), "shorter item after a skip still fits");
    assert!(tight.body.text.contains("evidence note ✓"), "later tier item still fits");
    assert!(!tight.body.text.contains("cccccc"), "oversized item omitted");
    assert_eq!(tight.bill.omitted_count, 1);
    assert!(tight.bill.truncated);
    assert!(tight.bill.bytes <= tight_budget, "bytes within budget");
    assert!(tight.bill.bytes >= tight.bill.required_bytes, "must-keep floor respected");
}

#[test]
fn budget_must_keep_fields_complete_per_mode() {
    let t = task();
    // FirstRun: goal / acceptance / scope / baseline / next step are must-keep
    let a = handoff::assemble(HandoffMode::FirstRun, &t, first_run_parts(), "zh", 8 * 1024).unwrap();
    for marker in [
        "目标 / Goal",
        "验收条件 / Acceptance",
        "范围与约束 / Scope",
        "基线 / Baseline",
        "下一步 / Next step",
    ] {
        assert!(a.body.text.contains(&format!("## {marker}\n")), "must-keep {marker}");
        assert!(
            a.bill.included.iter().any(|s| s.title == marker && s.must),
            "bill marks {marker} as must-send"
        );
    }
    // RelayPackage: confirmed progress + failure location are must-keep
    let relay = HandoffParts {
        goal: "g".into(),
        acceptance: "a".into(),
        scope: "s".into(),
        confirmed_progress: Some("上一写者已保存的改动文件：p.txt".into()),
        failure_location: Some("失败位置见检查点 / see checkpoint".into()),
        next_step: Some("ns".into()),
        ..Default::default()
    };
    let r = handoff::assemble(HandoffMode::RelayPackage, &t, relay, "zh", 8 * 1024).unwrap();
    assert!(r.body.text.contains("上一写者已保存的改动文件：p.txt"));
    assert!(r.body.text.contains("失败位置见检查点"));
    // Review: questions / acceptance / candidate identity / unresolved are must-keep
    let review = HandoffParts {
        acceptance: "a".into(),
        review_questions: "1. 是否实现目标？".into(),
        candidate_identity: "候选短 ID / candidate short id: deadbeef".into(),
        unresolved: Some("未解决：性能回归待复核".into()),
        ..Default::default()
    };
    let v = handoff::assemble(HandoffMode::Review, &t, review, "zh", 8 * 1024).unwrap();
    for marker in ["复核问题 / Review questions", "候选身份 / Candidate identity", "未解决项 / Unresolved items"] {
        assert!(v.body.text.contains(&format!("## {marker}\n")), "review must-keep {marker}");
    }
}

#[test]
fn budget_hidden_references_cannot_smuggle_omitted_content() {
    let t = task();
    let parts = HandoffParts {
        acceptance: "a".into(),
        review_questions: "q".into(),
        candidate_identity: "cid".into(),
        unresolved: Some("u".into()),
        key_changes: Some("k".repeat(6 * 1024)), // tier 1, will be omitted
        evidence_refs: vec!["evidence://task/t_budget/candidate/abcdef12".into()],
        ..Default::default()
    };
    // leave less room than the references block needs
    let roomy = handoff::assemble(HandoffMode::Review, &t, parts.clone(), "zh", 8 * 1024).unwrap();
    assert_eq!(roomy.bill.refs_included, 1, "roomy budget delivers the reference");
    assert!(roomy.body.evidence_refs.contains(&"evidence://task/t_budget/candidate/abcdef12".into()));
    assert!(roomy.body.text.contains("evidence://task/t_budget/candidate/abcdef12"));

    // leave less room than the references block needs (floor + a few bytes)
    let tight_budget = roomy.bill.required_bytes + 16;
    let tight = handoff::assemble(HandoffMode::Review, &t, parts, "zh", tight_budget).unwrap();
    assert_eq!(tight.bill.refs_omitted, 1, "reference omitted with the budget");
    assert!(tight.body.evidence_refs.is_empty(), "adapter receives no hidden reference");
    assert!(
        !tight.body.text.contains("evidence://"),
        "omitted evidence cannot ride back through hidden references"
    );
    assert!(tight.bill.omitted_count >= 1);
}

#[test]
fn budget_exact_fit_and_byte_exact_accounting() {
    let t = task();
    // no optional material at all: required == must-keep + contract, and the
    // exact-fit assembly lands on the budget byte for byte (CJK counts 3, emoji 4)
    let parts = HandoffParts {
        goal: "目标🦀中文".into(),
        acceptance: "验收".into(),
        scope: "范围".into(),
        baseline: "base_commit: 0000000".into(),
        next_step: Some("下一步".into()),
        ..Default::default()
    };
    let roomy = handoff::assemble(HandoffMode::FirstRun, &t, parts.clone(), "zh", 8 * 1024).unwrap();
    assert_eq!(roomy.bill.omitted_count, 0);
    assert_eq!(roomy.bill.required_bytes, roomy.bill.bytes);
    let exact = handoff::assemble(HandoffMode::FirstRun, &t, parts, "zh", roomy.bill.required_bytes).unwrap();
    assert_eq!(exact.body.bytes, roomy.bill.required_bytes, "exact fit lands on the limit");
    assert!(!exact.bill.truncated);
    assert!(!exact.body.text.contains("【蜂递】"), "no notice when nothing is omitted");
    assert_eq!(exact.body.sha256, roomy.body.sha256, "identical body at exact budget");
}

#[test]
fn budget_digest_invalidated_by_budget_lang_and_content() {
    let t = task();
    let base = handoff::assemble(HandoffMode::FirstRun, &t, first_run_parts(), "zh", 8 * 1024).unwrap();
    let same = handoff::assemble(HandoffMode::FirstRun, &t, first_run_parts(), "zh", 8 * 1024).unwrap();
    assert_eq!(base.body.sha256, same.body.sha256);
    // language change → different contract footer → different digest
    let en = handoff::assemble(HandoffMode::FirstRun, &t, first_run_parts(), "en", 8 * 1024).unwrap();
    assert_ne!(base.body.sha256, en.body.sha256, "language changes the body");
    // content change → different digest
    let mut changed = first_run_parts();
    changed.goal = "不同的目标 / a different goal".into();
    let c = handoff::assemble(HandoffMode::FirstRun, &t, changed, "zh", 8 * 1024).unwrap();
    assert_ne!(base.body.sha256, c.body.sha256, "content change invalidates the old digest");
    // budget change that forces omission → different digest and selection
    let mut big = first_run_parts();
    big.key_changes = Some("k".repeat(5 * 1024));
    let roomy = handoff::assemble(HandoffMode::FirstRun, &t, big.clone(), "zh", 8 * 1024).unwrap();
    let tight_budget = roomy.bill.required_bytes + 64;
    let tight = handoff::assemble(HandoffMode::FirstRun, &t, big, "zh", tight_budget).unwrap();
    assert_ne!(roomy.body.sha256, tight.body.sha256, "budget change invalidates the old digest");
    assert!(tight.bill.omitted_count >= 1);
}

#[test]
fn budget_utf8_content_never_split_and_counted_exactly() {
    let t = task();
    let parts = HandoffParts {
        goal: "🦀🐝🈶".into(),
        acceptance: "中文验收条件 ✓".into(),
        scope: "范围：含 emoji 😀 与中文".into(),
        baseline: "base_commit: 0000000".into(),
        next_step: Some("下一步 🚀".into()),
        ..Default::default()
    };
    let a = handoff::assemble(HandoffMode::FirstRun, &t, parts, "zh", 8 * 1024).unwrap();
    assert_eq!(a.body.bytes, a.body.text.as_bytes().len());
    // re-render from the delivered text: valid UTF-8 round-trip, no split chars
    let delivered = &a.body.text;
    assert!(std::str::from_utf8(delivered.as_bytes()).is_ok());
    assert!(delivered.contains("🦀🐝🈶"), "emoji delivered intact");
    assert!(delivered.contains("范围：含 emoji 😀 与中文"));
    // byte-exact floor: raising the budget by one byte over required still fits;
    // lowering below required blocks rather than splitting content
    let floor = a.bill.required_bytes;
    let one_under = handoff::assemble(
        HandoffMode::FirstRun,
        &t,
        HandoffParts {
            goal: "🦀🐝🈶".into(),
            acceptance: "中文验收条件 ✓".into(),
            scope: "范围：含 emoji 😀 与中文".into(),
            baseline: "base_commit: 0000000".into(),
            next_step: Some(format!("下一步 {}", "🚀")),
            ..Default::default()
        },
        "zh",
        floor.saturating_sub(1),
    );
    assert!(matches!(one_under, Err(HandoffError::OverBudget { .. })), "no partial must-keep trim");
}

#[test]
fn budget_relay_digest_changes_when_material_changes() {
    // W3: 变动后的旧摘要失效 — HTTP level. A stale digest must be refused
    // before anything is queued or launched.
    let env = common::TestEnv::new();
    let (task_id, _) = env.ready_task_unapproved("direct", Some("fake"), None);
    let real = env.relay_digest(&task_id, None, None);
    let tampered = common::TestEnv::tamper_digest(&real);
    let err = env
        .req_with_if_match(
            "POST",
            &format!("/v1/tasks/{task_id}/plan/approve"),
            Some(&json!({"summary_hash": env.plan_hash(&task_id), "handoff_digest": tampered})),
            Some("budget-stale-1"),
            env.revision_of(&task_id),
        )
        .unwrap_err();
    assert_eq!(err.0, 409, "stale digest refused at approval: {}", err.1);
    assert_eq!(err.1["error"]["code"], json!("handoff_digest_mismatch"));
    let snap = env.req("GET", &format!("/v1/tasks/{task_id}"), None, None).unwrap();
    assert_eq!(snap["task"]["launches_used"], json!(0), "nothing launched");
}

#[test]
fn budget_multiple_short_references_one_section_no_duplicates() {
    let t = task();
    // three short references that all fit: ONE section, every reference once
    let parts = HandoffParts {
        acceptance: "a".into(),
        review_questions: "q".into(),
        candidate_identity: "cid".into(),
        unresolved: Some("u".into()),
        evidence_refs: vec![
            "evidence://task/t_budget/candidate/aaa11111".into(),
            "evidence://task/t_budget/candidate/bbb22222".into(),
            "evidence://task/t_budget/candidate/ccc33333".into(),
        ],
        ..Default::default()
    };
    let a = handoff::assemble(HandoffMode::Review, &t, parts.clone(), "zh", 8 * 1024).unwrap();
    assert_eq!(a.bill.refs_included, 3);
    assert_eq!(a.bill.refs_omitted, 0);
    let occurrences = a.body.text.matches("证据索引 / Evidence index").count();
    assert_eq!(occurrences, 1, "exactly one references section");
    for r in ["aaa11111", "bbb22222", "ccc33333"] {
        assert_eq!(a.body.text.matches(r).count(), 1, "reference {r} delivered once");
    }
    assert_eq!(a.body.evidence_refs.len(), 3, "adapter receives all three");
    assert_eq!(a.bill.omitted_count, 0);
    // determinism: the same selection renders byte-identically
    let b = handoff::assemble(HandoffMode::Review, &t, parts, "zh", 8 * 1024).unwrap();
    assert_eq!(a.body.sha256, b.body.sha256);
    assert_eq!(a.body.bytes, b.body.bytes);
}

#[test]
fn budget_long_reference_omitted_then_shorter_ones_ride() {
    let t = task();
    let long_ref = format!("evidence://task/t_budget/candidate/{}", "d".repeat(400));
    let parts = HandoffParts {
        acceptance: "a".into(),
        review_questions: "q".into(),
        candidate_identity: "cid".into(),
        unresolved: Some("u".into()),
        evidence_refs: vec![
            long_ref,
            "evidence://task/t_budget/candidate/short1".into(),
            "evidence://task/t_budget/candidate/short2".into(),
        ],
        ..Default::default()
    };
    let roomy = handoff::assemble(HandoffMode::Review, &t, parts.clone(), "zh", 8 * 1024).unwrap();
    assert_eq!(roomy.bill.refs_included, 3);
    // tight budget: floor + room for the notice reserve and the two short
    // references — the long reference is skipped first, the short ones ride
    let tight_budget = roomy.bill.required_bytes + 340;
    let tight = handoff::assemble(HandoffMode::Review, &t, parts, "zh", tight_budget).unwrap();
    assert_eq!(tight.bill.refs_included, 2, "shorter references ride after the skip");
    assert_eq!(tight.bill.refs_omitted, 1);
    assert!(tight.body.text.contains("short1"));
    assert!(tight.body.text.contains("short2"));
    assert!(!tight.body.text.contains(&"d".repeat(400)));
    // omitted bytes on the bill match the skipped reference exactly
    let omitted_entry = tight
        .bill
        .excluded
        .iter()
        .find(|s| s.title == "证据索引 / Evidence index")
        .expect("omitted references listed on the bill");
    assert_eq!(omitted_entry.bytes, 400 + "evidence://task/t_budget/candidate/".len());
    assert!(tight.bill.bytes <= tight_budget);
}

#[test]
fn budget_chinese_reference_exact_boundary() {
    let t = task();
    // CJK reference: 3 bytes per char; the section must land byte-exactly on
    // the remaining budget when it fits, and one byte under must refuse it
    // (never split a character)
    let zh_ref = "证据://任务/t_budget/候选/哈希值".to_string();
    let parts = HandoffParts {
        acceptance: "a".into(),
        review_questions: "q".into(),
        candidate_identity: "cid".into(),
        unresolved: Some("u".into()),
        evidence_refs: vec![zh_ref.clone()],
        ..Default::default()
    };
    let roomy = handoff::assemble(HandoffMode::Review, &t, parts.clone(), "zh", 8 * 1024).unwrap();
    assert_eq!(roomy.bill.refs_included, 1);
    assert!(roomy.body.text.contains(&zh_ref), "CJK reference delivered intact");
    // the delivered body accounts the CJK bytes exactly
    let exact_budget = roomy.bill.bytes;
    let exact = handoff::assemble(HandoffMode::Review, &t, parts.clone(), "zh", exact_budget).unwrap();
    assert_eq!(exact.body.bytes, exact_budget, "CJK boundary lands byte-exactly");
    assert_eq!(exact.bill.refs_included, 1);
    // one byte under: the reference cannot fit and the omission notice cannot
    // fit either — the body ships whole (must-keep + contract only) without a
    // notice, while the bill discloses the omitted reference
    let one_under = handoff::assemble(HandoffMode::Review, &t, parts, "zh", exact_budget - 1).unwrap();
    assert_eq!(one_under.bill.refs_included, 0);
    assert_eq!(one_under.bill.refs_omitted, 1);
    assert!(one_under.bill.truncated);
    assert!(!one_under.body.text.contains(&zh_ref));
    assert!(!one_under.body.text.contains("【蜂递】"), "notice only ships when it fits");
}

#[test]
fn budget_required_floor_is_must_keep_body_plus_contract() {
    let t = task();
    // no optional material: the floor is the must-keep body + the contract,
    // and the exact-fit assembly lands on it
    let parts = first_run_parts();
    let a = handoff::assemble(HandoffMode::FirstRun, &t, parts.clone(), "zh", 8 * 1024).unwrap();
    assert_eq!(a.bill.required_bytes, a.bill.bytes);
    // with optional material the floor stays the must-keep body + contract:
    // the omission notice is counted on demand, inside the budget
    let mut with_optional = parts;
    with_optional.key_changes = Some("k".repeat(7 * 1024));
    let roomy = handoff::assemble(HandoffMode::FirstRun, &t, with_optional, "zh", 8 * 1024).unwrap();
    let floor_only = handoff::assemble(
        HandoffMode::FirstRun,
        &t,
        first_run_parts(),
        "zh",
        roomy.bill.required_bytes,
    )
    .expect("floor budget fits the must-keep body + contract alone");
    assert_eq!(floor_only.bill.omitted_count, 0);
    assert_eq!(floor_only.bill.bytes, roomy.bill.required_bytes);
}
