use crate::model::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Store {
    pub conn: Mutex<Connection>,
    pub db_path: PathBuf,
}

fn now_iso() -> String {
    // local-independent UTC ISO timestamp with ms
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    // civil from days algorithm (Howard Hinnant)
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, mth, d, h, m, s, ms
    )
}

pub fn timestamp() -> String {
    now_iso()
}

type RowMapper<T> = fn(&Row) -> rusqlite::Result<T>;

const TASK_COLS: &str = "task_id,revision,title,goal,acceptance,repo_path,repo_id,base_commit,allowed_paths,forbidden_paths,recipe,status,planned_steps,resume_budget,resume_used,launches_used,allowed_adapters,timeout_secs,current_generation,accepted_candidate_hash,created_at,updated_at";

fn row_task(r: &Row) -> rusqlite::Result<Task> {
    Ok(Task {
        task_id: r.get(0)?,
        revision: r.get(1)?,
        title: r.get(2)?,
        goal: r.get(3)?,
        acceptance: json_from(&r.get::<_, String>(4)?),
        repo_path: r.get(5)?,
        repo_id: r.get(6)?,
        base_commit: r.get(7)?,
        allowed_paths: json_from(&r.get::<_, String>(8)?),
        forbidden_paths: json_from(&r.get::<_, String>(9)?),
        recipe: serde_json::from_str(&format!("\"{}\"", r.get::<_, String>(10)?))
            .unwrap_or(Recipe::Direct),
        status: serde_json::from_str(&format!("\"{}\"", r.get::<_, String>(11)?))
            .unwrap_or(TaskStatus::Queued),
        planned_steps: r.get(12)?,
        resume_budget: r.get(13)?,
        resume_used: r.get(14)?,
        launches_used: r.get(15)?,
        allowed_adapters: json_from(&r.get::<_, String>(16)?),
        timeout_secs: r.get(17)?,
        current_generation: r.get(18)?,
        accepted_candidate_hash: r.get(19)?,
        created_at: r.get(20)?,
        updated_at: r.get(21)?,
    })
}

fn json_from<T: serde::de::DeserializeOwned>(s: &str) -> T {
    serde_json::from_str(s).unwrap_or_else(|_| {
        serde_json::from_str("[]").expect("empty json array")
    })
}

fn json_to<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

const ATTEMPT_COLS: &str = "attempt_id,task_id,task_revision,step_id,adapter,adapter_version,role,worktree_id,generation,native_id,request_id,status,launch_no,exit_code,started_at,ended_at,simulated,handoff_digest,pid";

fn row_attempt(r: &Row) -> rusqlite::Result<Attempt> {
    Ok(Attempt {
        attempt_id: r.get(0)?,
        task_id: r.get(1)?,
        task_revision: r.get(2)?,
        step_id: r.get(3)?,
        adapter: r.get(4)?,
        adapter_version: r.get(5)?,
        role: serde_json::from_str(&format!("\"{}\"", r.get::<_, String>(6)?)).unwrap_or(Role::Writer),
        worktree_id: r.get(7)?,
        generation: r.get(8)?,
        native_id: r.get(9)?,
        request_id: r.get(10)?,
        status: serde_json::from_str(&format!("\"{}\"", r.get::<_, String>(11)?))
            .unwrap_or(AttemptStatus::Pending),
        launch_no: r.get(12)?,
        exit_code: r.get(13)?,
        started_at: r.get(14)?,
        ended_at: r.get(15)?,
        simulated: r.get::<_, i64>(16)? != 0,
        handoff_digest: r.get(17)?,
        pid: r.get(18)?,
    })
}

const EVENT_COLS: &str = "seq,task_id,attempt_id,generation,kind,payload,ts";

fn row_event(r: &Row) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        seq: r.get(0)?,
        task_id: r.get(1)?,
        attempt_id: r.get(2)?,
        generation: r.get(3)?,
        kind: r.get(4)?,
        payload: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
        ts: r.get(6)?,
    })
}

const CHECKPOINT_COLS: &str = "checkpoint_id,task_id,revision,attempt_id,base_commit,candidate_hash,changed_files,untracked_files,patch_path,failure_summary,source,created_at";

fn row_checkpoint(r: &Row) -> rusqlite::Result<Checkpoint> {
    Ok(Checkpoint {
        checkpoint_id: r.get(0)?,
        task_id: r.get(1)?,
        revision: r.get(2)?,
        attempt_id: r.get(3)?,
        base_commit: r.get(4)?,
        candidate_hash: r.get(5)?,
        changed_files: json_from(&r.get::<_, String>(6)?),
        untracked_files: json_from(&r.get::<_, String>(7)?),
        patch_path: r.get(8)?,
        failure_summary: r.get(9)?,
        source: serde_json::from_str(&format!("\"{}\"", r.get::<_, String>(10)?))
            .unwrap_or(CheckpointSource::LocalObserved),
        created_at: r.get(11)?,
    })
}

const DECISION_COLS: &str = "decision_id,task_id,revision,attempt_id,checkpoint_id,question,options,status,dedupe_key,merge_count,resolution,resume_request_id,created_at,answered_at";

fn row_decision(r: &Row) -> rusqlite::Result<Decision> {
    Ok(Decision {
        decision_id: r.get(0)?,
        task_id: r.get(1)?,
        revision: r.get(2)?,
        attempt_id: r.get(3)?,
        checkpoint_id: r.get(4)?,
        question: r.get(5)?,
        options: json_from(&r.get::<_, String>(6)?),
        status: r.get(7)?,
        dedupe_key: r.get(8)?,
        merge_count: r.get(9)?,
        resolution: r.get(10)?,
        resume_request_id: r.get(11)?,
        created_at: r.get(12)?,
        answered_at: r.get(13)?,
    })
}

const REQUEST_COLS: &str = "request_id,kind,digest,task_id,attempt_id,status,replayed,result,created_at,updated_at";

fn row_request(r: &Row) -> rusqlite::Result<RequestRecord> {
    Ok(RequestRecord {
        request_id: r.get(0)?,
        kind: r.get(1)?,
        digest: r.get(2)?,
        task_id: r.get(3)?,
        attempt_id: r.get(4)?,
        status: r.get(5)?,
        replayed: r.get::<_, i64>(6)? != 0,
        result: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or(Value::Null),
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

const MESSAGE_COLS: &str = "message_id,task_id,kind,payload,dedupe_key,merge_count,resolved,created_at,resolved_at";

fn row_message(r: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        message_id: r.get(0)?,
        task_id: r.get(1)?,
        kind: r.get(2)?,
        payload: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
        dedupe_key: r.get(4)?,
        merge_count: r.get(5)?,
        resolved: r.get::<_, i64>(6)? != 0,
        created_at: r.get(7)?,
        resolved_at: r.get(8)?,
    })
}

const WORKTREE_COLS: &str = "worktree_id,task_id,generation,repo_path,path,base_commit,created_at,removed_at,keep_reason";

fn row_worktree(r: &Row) -> rusqlite::Result<WorktreeRecord> {
    Ok(WorktreeRecord {
        worktree_id: r.get(0)?,
        task_id: r.get(1)?,
        generation: r.get(2)?,
        repo_path: r.get(3)?,
        path: r.get(4)?,
        base_commit: r.get(5)?,
        created_at: r.get(6)?,
        removed_at: r.get(7)?,
        keep_reason: r.get(8)?,
    })
}

const REVIEW_COLS: &str = "review_id,task_id,revision,attempt_id,candidate_hash,reviewer_adapter,verdict,summary,issues,historical,created_at";

fn row_review(r: &Row) -> rusqlite::Result<ReviewRecord> {
    Ok(ReviewRecord {
        review_id: r.get(0)?,
        task_id: r.get(1)?,
        revision: r.get(2)?,
        attempt_id: r.get(3)?,
        candidate_hash: r.get(4)?,
        reviewer_adapter: r.get(5)?,
        verdict: r.get(6)?,
        summary: r.get(7)?,
        issues: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or(Value::Null),
        historical: r.get::<_, i64>(9)? != 0,
        created_at: r.get(10)?,
    })
}

const CANDIDATE_COLS: &str = "candidate_hash,task_id,generation,attempt_id,base_commit,frozen_commit,changed_files,untracked_files,patch_path,label,current,created_at";

fn row_candidate(r: &Row) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        candidate_hash: r.get(0)?,
        task_id: r.get(1)?,
        generation: r.get(2)?,
        attempt_id: r.get(3)?,
        base_commit: r.get(4)?,
        frozen_commit: r.get(5)?,
        changed_files: json_from(&r.get::<_, String>(6)?),
        untracked_files: json_from(&r.get::<_, String>(7)?),
        patch_path: r.get(8)?,
        label: r.get(9)?,
        current: r.get::<_, i64>(10)? != 0,
        created_at: r.get(11)?,
    })
}

const PLAN_COLS: &str = "plan_id,task_id,task_revision,recipe,steps,summary_hash,created_at,approved_at";

fn row_plan(r: &Row) -> rusqlite::Result<Plan> {
    Ok(Plan {
        plan_id: r.get(0)?,
        task_id: r.get(1)?,
        task_revision: r.get(2)?,
        recipe: serde_json::from_str(&format!("\"{}\"", r.get::<_, String>(3)?)).unwrap_or(Recipe::Direct),
        steps: json_from(&r.get::<_, String>(4)?),
        summary_hash: r.get(5)?,
        created_at: r.get(6)?,
        approved_at: r.get(7)?,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(Store {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
        })
    }

    pub fn open_in_memory() -> Result<Self, String> {
        Self::open(Path::new(":memory:"))
    }


    /// Generic list query: prepare + query_map + collect, empty on error.
    fn query_list<T, P>(&self, sql: &str, p: P, mapper: RowMapper<T>) -> Vec<T>
    where
        P: rusqlite::Params,
    {
        let c = self.conn.lock().unwrap();
        let Ok(mut st) = c.prepare(sql) else {
            return Vec::new();
        };
        st.query_map(p, mapper)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    // ---------- meta kv ----------
    pub fn meta_get(&self, key: &str) -> Option<String> {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT v FROM meta WHERE k=?1", params![key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }
    pub fn meta_set(&self, key: &str, value: &str) {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=?2",
            params![key, value],
        )
        .ok();
    }

    // ---------- tasks ----------
    pub fn insert_task(&self, t: &Task) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            &format!("INSERT INTO tasks({TASK_COLS}) VALUES({})", placeholders(22)),
            params![
                t.task_id, t.revision, t.title, t.goal, json_to(&t.acceptance),
                t.repo_path, t.repo_id, t.base_commit, json_to(&t.allowed_paths),
                json_to(&t.forbidden_paths), t.recipe.as_str(), t.status.as_str(),
                t.planned_steps, t.resume_budget, t.resume_used, t.launches_used,
                json_to(&t.allowed_adapters), t.timeout_secs, t.current_generation,
                t.accepted_candidate_hash, t.created_at, t.updated_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_task(&self, t: &Task) -> Result<(), String> {
        self.insert_or_update_task(t, false)
    }

    fn insert_or_update_task(&self, t: &Task, insert_only: bool) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        if insert_only {
            c.execute(
                &format!("INSERT INTO tasks({TASK_COLS}) VALUES({})", placeholders(22)),
                params![
                    t.task_id, t.revision, t.title, t.goal, json_to(&t.acceptance),
                    t.repo_path, t.repo_id, t.base_commit, json_to(&t.allowed_paths),
                    json_to(&t.forbidden_paths), t.recipe.as_str(), t.status.as_str(),
                    t.planned_steps, t.resume_budget, t.resume_used, t.launches_used,
                    json_to(&t.allowed_adapters), t.timeout_secs, t.current_generation,
                    t.accepted_candidate_hash, t.created_at, t.updated_at
                ],
            )
            .map_err(|e| e.to_string())?;
        } else {
            c.execute(
                "UPDATE tasks SET revision=?2,title=?3,goal=?4,acceptance=?5,repo_path=?6,repo_id=?7,base_commit=?8,allowed_paths=?9,forbidden_paths=?10,recipe=?11,status=?12,planned_steps=?13,resume_budget=?14,resume_used=?15,launches_used=?16,allowed_adapters=?17,timeout_secs=?18,current_generation=?19,accepted_candidate_hash=?20,updated_at=?21 WHERE task_id=?1",
                params![
                    t.task_id, t.revision, t.title, t.goal, json_to(&t.acceptance),
                    t.repo_path, t.repo_id, t.base_commit, json_to(&t.allowed_paths),
                    json_to(&t.forbidden_paths), t.recipe.as_str(), t.status.as_str(),
                    t.planned_steps, t.resume_budget, t.resume_used, t.launches_used,
                    json_to(&t.allowed_adapters), t.timeout_secs, t.current_generation,
                    t.accepted_candidate_hash, t.updated_at
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn get_task(&self, task_id: &str) -> Option<Task> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!("SELECT {TASK_COLS} FROM tasks WHERE task_id=?1"),
            params![task_id],
            row_task as RowMapper<Task>,
        )
        .ok()
    }

    pub fn list_tasks(&self) -> Vec<Task> {
        self.query_list(
            &format!("SELECT {TASK_COLS} FROM tasks ORDER BY created_at DESC"),
            [],
            row_task,
        )
    }

    // ---------- plans ----------
    pub fn insert_plan(&self, p: &Plan) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            &format!("INSERT INTO plans({PLAN_COLS}) VALUES({})", placeholders(8)),
            params![
                p.plan_id, p.task_id, p.task_revision, p.recipe.as_str(),
                json_to(&p.steps), p.summary_hash, p.created_at, p.approved_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn latest_plan(&self, task_id: &str) -> Option<Plan> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!(
                "SELECT {PLAN_COLS} FROM plans WHERE task_id=?1 ORDER BY created_at DESC LIMIT 1"
            ),
            params![task_id],
            row_plan as RowMapper<Plan>,
        )
        .ok()
    }

    pub fn approve_plan(&self, plan_id: &str, approved_at: &str) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE plans SET approved_at=?2 WHERE plan_id=?1",
            params![plan_id, approved_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ---------- attempts ----------
    pub fn insert_attempt(&self, a: &Attempt) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            &format!(
                "INSERT INTO attempts({ATTEMPT_COLS}) VALUES({})",
                placeholders(19)
            ),
            params![
                a.attempt_id, a.task_id, a.task_revision, a.step_id, a.adapter,
                a.adapter_version, a.role.as_str(), a.worktree_id, a.generation,
                a.native_id, a.request_id, a.status.as_str(), a.launch_no,
                a.exit_code, a.started_at, a.ended_at, a.simulated as i64,
                a.handoff_digest, a.pid
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_attempt(&self, a: &Attempt) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE attempts SET task_revision=?3,step_id=?4,adapter=?5,adapter_version=?6,role=?7,worktree_id=?8,generation=?9,native_id=?10,request_id=?11,status=?12,launch_no=?13,exit_code=?14,started_at=?15,ended_at=?16,simulated=?17,handoff_digest=?18,pid=?19 WHERE attempt_id=?1",
            params![
                a.attempt_id, a.task_id, a.task_revision, a.step_id, a.adapter,
                a.adapter_version, a.role.as_str(), a.worktree_id, a.generation,
                a.native_id, a.request_id, a.status.as_str(), a.launch_no,
                a.exit_code, a.started_at, a.ended_at, a.simulated as i64,
                a.handoff_digest, a.pid
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_attempt(&self, attempt_id: &str) -> Option<Attempt> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!("SELECT {ATTEMPT_COLS} FROM attempts WHERE attempt_id=?1"),
            params![attempt_id],
            row_attempt as RowMapper<Attempt>,
        )
        .ok()
    }

    pub fn list_attempts(&self, task_id: &str) -> Vec<Attempt> {
        self.query_list(
            &format!("SELECT {ATTEMPT_COLS} FROM attempts WHERE task_id=?1 ORDER BY launch_no ASC"),
            params![task_id],
            row_attempt,
        )
    }

    pub fn list_live_attempts(&self) -> Vec<Attempt> {
        self.query_list(
            &format!("SELECT {ATTEMPT_COLS} FROM attempts WHERE status IN ('pending','starting','running','waiting_input','stopping')"),
            [],
            row_attempt,
        )
    }

    pub fn next_launch_no(&self, task_id: &str) -> u64 {
        let c = self.conn.lock().unwrap();
        c.query_row(
            "SELECT COALESCE(MAX(launch_no),0)+1 FROM attempts WHERE task_id=?1",
            params![task_id],
            |r| r.get(0),
        )
        .unwrap_or(1)
    }

    // ---------- events ----------
    pub fn append_event(&self, e: &EventRow) -> Result<u64, String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            &format!("INSERT INTO events({EVENT_COLS}) VALUES({})", placeholders(7)),
            params![
                e.seq, e.task_id, e.attempt_id, e.generation, e.kind,
                serde_json::to_string(&e.payload).unwrap_or_default(), e.ts
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(e.seq)
    }

    pub fn next_seq(&self) -> u64 {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM events", [], |r| r.get(0))
            .unwrap_or(1)
    }

    pub fn list_events_after(&self, seq: u64, limit: u64) -> Vec<EventRow> {
        self.query_list(
            &format!("SELECT {EVENT_COLS} FROM events WHERE seq>?1 ORDER BY seq ASC LIMIT ?2"),
            params![seq, limit],
            row_event,
        )
    }

    pub fn list_task_events(&self, task_id: &str, limit: u64) -> Vec<EventRow> {
        self.query_list(
            &format!("SELECT {EVENT_COLS} FROM events WHERE task_id=?1 ORDER BY seq ASC LIMIT ?2"),
            params![task_id, limit],
            row_event,
        )
    }

    pub fn max_seq(&self) -> u64 {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))
            .unwrap_or(0)
    }

    // ---------- checkpoints ----------
    pub fn insert_checkpoint(&self, cp: &Checkpoint) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            &format!(
                "INSERT INTO checkpoints({CHECKPOINT_COLS}) VALUES({})",
                placeholders(12)
            ),
            params![
                cp.checkpoint_id, cp.task_id, cp.revision, cp.attempt_id,
                cp.base_commit, cp.candidate_hash, json_to(&cp.changed_files),
                json_to(&cp.untracked_files), cp.patch_path, cp.failure_summary,
                cp.source_str(), cp.created_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn latest_checkpoint(&self, task_id: &str) -> Option<Checkpoint> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!(
                "SELECT {CHECKPOINT_COLS} FROM checkpoints WHERE task_id=?1 ORDER BY created_at DESC LIMIT 1"
            ),
            params![task_id],
            row_checkpoint as RowMapper<Checkpoint>,
        )
        .ok()
    }

    pub fn get_checkpoint(&self, checkpoint_id: &str) -> Option<Checkpoint> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!("SELECT {CHECKPOINT_COLS} FROM checkpoints WHERE checkpoint_id=?1"),
            params![checkpoint_id],
            row_checkpoint as RowMapper<Checkpoint>,
        )
        .ok()
    }

    // ---------- decisions ----------
    pub fn insert_decision(&self, d: &Decision) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            &format!(
                "INSERT INTO decisions({DECISION_COLS}) VALUES({})",
                placeholders(14)
            ),
            params![
                d.decision_id, d.task_id, d.revision, d.attempt_id, d.checkpoint_id,
                d.question, json_to(&d.options), d.status, d.dedupe_key,
                d.merge_count, d.resolution, d.resume_request_id, d.created_at, d.answered_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_decision(&self, d: &Decision) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE decisions SET status=?8,merge_count=?9,resolution=?10,resume_request_id=?11,answered_at=?14,question=?5,checkpoint_id=?6,options=?7 WHERE decision_id=?1",
            params![
                d.decision_id, d.task_id, d.revision, d.attempt_id, d.question,
                d.checkpoint_id, json_to(&d.options), d.status, d.merge_count,
                d.resolution, d.resume_request_id, d.created_at, d.answered_at, d.answered_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_decision(&self, decision_id: &str) -> Option<Decision> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!("SELECT {DECISION_COLS} FROM decisions WHERE decision_id=?1"),
            params![decision_id],
            row_decision as RowMapper<Decision>,
        )
        .ok()
    }

    pub fn list_decisions(&self, open_only: bool) -> Vec<Decision> {
        if open_only {
            self.query_list(
                &format!("SELECT {DECISION_COLS} FROM decisions WHERE status='open' ORDER BY created_at DESC"),
                [],
                row_decision,
            )
        } else {
            self.query_list(
                &format!("SELECT {DECISION_COLS} FROM decisions ORDER BY created_at DESC"),
                [],
                row_decision,
            )
        }
    }

    pub fn find_open_decision_by_dedupe(&self, dedupe_key: &str) -> Option<Decision> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!(
                "SELECT {DECISION_COLS} FROM decisions WHERE dedupe_key=?1 AND status='open' LIMIT 1"
            ),
            params![dedupe_key],
            row_decision as RowMapper<Decision>,
        )
        .ok()
    }

    // ---------- requests ----------
    pub fn insert_request(&self, r: &RequestRecord) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            &format!(
                "INSERT INTO requests({REQUEST_COLS}) VALUES({})",
                placeholders(10)
            ),
            params![
                r.request_id, r.kind, r.digest, r.task_id, r.attempt_id, r.status,
                r.replayed as i64, serde_json::to_string(&r.result).unwrap_or_default(),
                r.created_at, r.updated_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_request(&self, r: &RequestRecord) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE requests SET status=?5,attempt_id=?4,result=?8,replayed=?6,updated_at=?10 WHERE request_id=?1",
            params![
                r.request_id, r.kind, r.digest, r.task_id, r.attempt_id, r.status,
                r.replayed as i64, serde_json::to_string(&r.result).unwrap_or_default(),
                r.created_at, r.updated_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_request(&self, request_id: &str) -> Option<RequestRecord> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!("SELECT {REQUEST_COLS} FROM requests WHERE request_id=?1"),
            params![request_id],
            row_request as RowMapper<RequestRecord>,
        )
        .ok()
    }

    pub fn find_request_by_digest(&self, digest: &str) -> Option<RequestRecord> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!("SELECT {REQUEST_COLS} FROM requests WHERE digest=?1 ORDER BY created_at DESC LIMIT 1"),
            params![digest],
            row_request as RowMapper<RequestRecord>,
        )
        .ok()
    }

    // ---------- messages ----------
    pub fn upsert_message(&self, m: &Message) -> Result<String, String> {
        let c = self.conn.lock().unwrap();
        let existing: Option<String> = c
            .query_row(
                "SELECT message_id FROM messages WHERE dedupe_key=?1 AND resolved=0 LIMIT 1",
                params![m.dedupe_key],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(mid) = existing {
            c.execute(
                "UPDATE messages SET merge_count=merge_count+1, payload=?3 WHERE message_id=?2",
                params![m.dedupe_key, mid, serde_json::to_string(&m.payload).unwrap_or_default()],
            )
            .map_err(|e| e.to_string())?;
            return Ok(mid);
        }
        c.execute(
            &format!(
                "INSERT INTO messages({MESSAGE_COLS}) VALUES({})",
                placeholders(9)
            ),
            params![
                m.message_id, m.task_id, m.kind,
                serde_json::to_string(&m.payload).unwrap_or_default(),
                m.dedupe_key, m.merge_count, m.resolved as i64, m.created_at, m.resolved_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(m.message_id.clone())
    }

    pub fn resolve_message(&self, dedupe_key: &str) {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE messages SET resolved=1, resolved_at=?2 WHERE dedupe_key=?1 AND resolved=0",
            params![dedupe_key, timestamp()],
        )
        .ok();
    }

    pub fn resolve_message_by_task(&self, task_id: &str, kind: &str) {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE messages SET resolved=1, resolved_at=?3 WHERE task_id=?1 AND kind=?2 AND resolved=0",
            params![task_id, kind, timestamp()],
        )
        .ok();
    }

    pub fn list_messages(&self, open_only: bool) -> Vec<Message> {
        if open_only {
            self.query_list(
                &format!("SELECT {MESSAGE_COLS} FROM messages WHERE resolved=0 ORDER BY created_at DESC"),
                [],
                row_message,
            )
        } else {
            self.query_list(
                &format!("SELECT {MESSAGE_COLS} FROM messages ORDER BY created_at DESC LIMIT 200"),
                [],
                row_message,
            )
        }
    }

    // ---------- worktrees ----------
    pub fn insert_worktree(&self, w: &WorktreeRecord) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            &format!(
                "INSERT INTO worktrees({WORKTREE_COLS}) VALUES({})",
                placeholders(9)
            ),
            params![
                w.worktree_id, w.task_id, w.generation, w.repo_path, w.path,
                w.base_commit, w.created_at, w.removed_at, w.keep_reason
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_worktree(&self, w: &WorktreeRecord) -> Result<(), String> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE worktrees SET removed_at=?7,keep_reason=?8 WHERE worktree_id=?1",
            params![w.worktree_id, w.task_id, w.generation, w.repo_path, w.path, w.base_commit, w.removed_at, w.keep_reason],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_worktree(&self, worktree_id: &str) -> Option<WorktreeRecord> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            &format!("SELECT {WORKTREE_COLS} FROM worktrees WHERE worktree_id=?1"),
            params![worktree_id],
            row_worktree as RowMapper<WorktreeRecord>,
        )
        .ok()
    }

    pub fn list_task_worktrees(&self, task_id: &str) -> Vec<WorktreeRecord> {
        self.query_list(
            &format!("SELECT {WORKTREE_COLS} FROM worktrees WHERE task_id=?1 ORDER BY created_at ASC"),
            params![task_id],
            row_worktree,
        )
    }

    // ---------- candidates ----------
    pub fn insert_candidate(&self, c: &Candidate) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            &format!("INSERT INTO candidates({CANDIDATE_COLS}) VALUES({})", placeholders(12)),
            params![
                c.candidate_hash, c.task_id, c.generation, c.attempt_id, c.base_commit,
                c.frozen_commit, json_to(&c.changed_files), json_to(&c.untracked_files),
                c.patch_path, c.label, c.current as i64, c.created_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn mark_candidates_stale(&self, task_id: &str) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE candidates SET current=0 WHERE task_id=?1",
            params![task_id],
        )
        .ok();
    }

    pub fn get_candidate(&self, task_id: &str, hash: &str) -> Option<Candidate> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {CANDIDATE_COLS} FROM candidates WHERE task_id=?1 AND candidate_hash=?2"),
            params![task_id, hash],
            row_candidate as RowMapper<Candidate>,
        )
        .ok()
    }

    pub fn current_candidate(&self, task_id: &str) -> Option<Candidate> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {CANDIDATE_COLS} FROM candidates WHERE task_id=?1 AND current=1 ORDER BY created_at DESC LIMIT 1"),
            params![task_id],
            row_candidate as RowMapper<Candidate>,
        )
        .ok()
    }

    pub fn list_candidates(&self, task_id: &str) -> Vec<Candidate> {
        self.query_list(
            &format!("SELECT {CANDIDATE_COLS} FROM candidates WHERE task_id=?1 ORDER BY created_at ASC"),
            params![task_id],
            row_candidate,
        )
    }

    // ---------- reviews ----------
    pub fn insert_review(&self, rv: &ReviewRecord) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            &format!("INSERT INTO reviews({REVIEW_COLS}) VALUES({})", placeholders(11)),
            params![
                rv.review_id, rv.task_id, rv.revision, rv.attempt_id, rv.candidate_hash,
                rv.reviewer_adapter, rv.verdict, rv.summary,
                serde_json::to_string(&rv.issues).unwrap_or_default(),
                rv.historical as i64, rv.created_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn mark_reviews_historical(&self, task_id: &str) {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE reviews SET historical=1 WHERE task_id=?1", params![task_id]).ok();
    }

    pub fn list_reviews(&self, task_id: &str) -> Vec<ReviewRecord> {
        self.query_list(
            &format!("SELECT {REVIEW_COLS} FROM reviews WHERE task_id=?1 ORDER BY created_at ASC"),
            params![task_id],
            row_review,
        )
    }

    pub fn latest_review_for(&self, task_id: &str, candidate_hash: &str) -> Option<ReviewRecord> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!(
                "SELECT {REVIEW_COLS} FROM reviews WHERE task_id=?1 AND candidate_hash=?2 AND historical=0 ORDER BY created_at DESC LIMIT 1"
            ),
            params![task_id, candidate_hash],
            row_review as RowMapper<ReviewRecord>,
        )
        .ok()
    }
}

impl Checkpoint {
    fn source_str(&self) -> &'static str {
        match self.source {
            CheckpointSource::AgentReported => "agent_reported",
            CheckpointSource::LocalObserved => "local_observed",
            CheckpointSource::Independent => "independent",
        }
    }
}

fn placeholders(n: usize) -> String {
    (0..n)
        .map(|i| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",")
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tasks(
  task_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, title TEXT NOT NULL, goal TEXT NOT NULL,
  acceptance TEXT NOT NULL, repo_path TEXT NOT NULL, repo_id TEXT NOT NULL, base_commit TEXT NOT NULL,
  allowed_paths TEXT NOT NULL, forbidden_paths TEXT NOT NULL, recipe TEXT NOT NULL, status TEXT NOT NULL,
  planned_steps INTEGER NOT NULL, resume_budget INTEGER NOT NULL, resume_used INTEGER NOT NULL,
  launches_used INTEGER NOT NULL, allowed_adapters TEXT NOT NULL, timeout_secs INTEGER NOT NULL,
  current_generation INTEGER NOT NULL, accepted_candidate_hash TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS plans(
  plan_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, task_revision INTEGER NOT NULL,
  recipe TEXT NOT NULL, steps TEXT NOT NULL, summary_hash TEXT NOT NULL,
  created_at TEXT NOT NULL, approved_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_plans_task ON plans(task_id, created_at);
CREATE TABLE IF NOT EXISTS attempts(
  attempt_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, task_revision INTEGER NOT NULL,
  step_id TEXT NOT NULL, adapter TEXT NOT NULL, adapter_version TEXT NOT NULL, role TEXT NOT NULL,
  worktree_id TEXT, generation INTEGER NOT NULL, native_id TEXT, request_id TEXT, status TEXT NOT NULL,
  launch_no INTEGER NOT NULL, exit_code INTEGER, started_at TEXT, ended_at TEXT,
  simulated INTEGER NOT NULL DEFAULT 0, handoff_digest TEXT, pid INTEGER
);
CREATE INDEX IF NOT EXISTS idx_attempts_task ON attempts(task_id, launch_no);
CREATE INDEX IF NOT EXISTS idx_attempts_status ON attempts(status);
CREATE TABLE IF NOT EXISTS events(
  seq INTEGER PRIMARY KEY, task_id TEXT, attempt_id TEXT, generation INTEGER NOT NULL DEFAULT 0,
  kind TEXT NOT NULL, payload TEXT NOT NULL, ts TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_task ON events(task_id, seq);
CREATE TABLE IF NOT EXISTS checkpoints(
  checkpoint_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, revision INTEGER NOT NULL,
  attempt_id TEXT NOT NULL, base_commit TEXT NOT NULL, candidate_hash TEXT NOT NULL,
  changed_files TEXT NOT NULL, untracked_files TEXT NOT NULL, patch_path TEXT,
  failure_summary TEXT, source TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_checkpoints_task ON checkpoints(task_id, created_at);
CREATE TABLE IF NOT EXISTS decisions(
  decision_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, revision INTEGER NOT NULL,
  attempt_id TEXT NOT NULL, checkpoint_id TEXT, question TEXT NOT NULL, options TEXT NOT NULL,
  status TEXT NOT NULL, dedupe_key TEXT NOT NULL, merge_count INTEGER NOT NULL DEFAULT 1,
  resolution TEXT, resume_request_id TEXT, created_at TEXT NOT NULL, answered_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_decisions_task ON decisions(task_id, status);
CREATE TABLE IF NOT EXISTS requests(
  request_id TEXT PRIMARY KEY, kind TEXT NOT NULL, digest TEXT NOT NULL,
  task_id TEXT, attempt_id TEXT, status TEXT NOT NULL, replayed INTEGER NOT NULL DEFAULT 0,
  result TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_requests_digest ON requests(digest);
CREATE TABLE IF NOT EXISTS messages(
  message_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL,
  dedupe_key TEXT NOT NULL UNIQUE, merge_count INTEGER NOT NULL DEFAULT 1, resolved INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL, resolved_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_messages_task ON messages(task_id, resolved);
CREATE TABLE IF NOT EXISTS worktrees(
  worktree_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, generation INTEGER NOT NULL,
  repo_path TEXT NOT NULL, path TEXT NOT NULL, base_commit TEXT NOT NULL,
  created_at TEXT NOT NULL, removed_at TEXT, keep_reason TEXT
);
CREATE TABLE IF NOT EXISTS reviews(
  review_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, revision INTEGER NOT NULL,
  attempt_id TEXT NOT NULL, candidate_hash TEXT NOT NULL, reviewer_adapter TEXT NOT NULL,
  verdict TEXT, summary TEXT, issues TEXT NOT NULL, historical INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_reviews_task ON reviews(task_id, candidate_hash);
CREATE TABLE IF NOT EXISTS candidates(
  candidate_hash TEXT NOT NULL, task_id TEXT NOT NULL, generation INTEGER NOT NULL,
  attempt_id TEXT NOT NULL, base_commit TEXT NOT NULL, frozen_commit TEXT,
  changed_files TEXT NOT NULL, untracked_files TEXT NOT NULL, patch_path TEXT,
  label TEXT NOT NULL, current INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL,
  PRIMARY KEY(task_id, candidate_hash)
);
"#;
