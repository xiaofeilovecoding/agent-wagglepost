//! HTTP layer: loopback-only server with Host/Origin validation, startup-code →
//! HttpOnly session, CSRF for browser writes, bearer channel for the CLI.
//! All mutating endpoints are idempotent via Idempotency-Key + canonical digest.

use crate::core::{Core, CoreError, SubmitInput};
use crate::ids::{canonical_digest, new_token, sha256_hex};
use crate::model::Role;
use crate::routing;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

pub struct AppState {
    pub core: Arc<Core>,
    pub session_token: String,
    pub csrf_token: String,
    /// set of trusted origins observed at boot (loopback only)
    pub port: u16,
}

type SharedState = Arc<AppState>;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/session", post(create_session))
        .route("/v1/session/csrf", get(get_csrf))
        .route("/v1/adapters", get(adapters))
        .route("/v1/doctor", get(doctor))
        .route("/v1/requests/{id}", get(get_request))
        .route("/v1/tasks", post(create_task).get(list_tasks))
        .route("/v1/tasks/{id}/trust", post(trust_repo))
        .route("/v1/tasks/{id}/plan", post(create_plan_route))
        .route("/v1/tasks/{id}/plan/approve", post(approve_plan))
        .route("/v1/tasks/{id}/route-preview", get(route_preview))
        .route("/v1/tasks/{id}/dispatch", post(dispatch))
        .route("/v1/tasks/{id}", get(get_task))
        .route("/v1/tasks/{id}/reroute", post(reroute))
        .route("/v1/tasks/{id}/accept", post(accept))
        .route("/v1/tasks/{id}/handoff-preview", get(handoff_preview))
        .route("/v1/tasks/{id}/diff", get(task_diff))
        .route("/v1/attempts/{id}/cancel", post(cancel_attempt))
        .route("/v1/attempts/{id}", get(get_attempt))
        .route("/v1/decisions", get(list_decisions))
        .route("/v1/decisions/{id}/answer", post(answer_decision))
        .route("/v1/messages", get(list_messages))
        .route("/v1/events", get(sse_events))
        .route("/v1/resume-options/{id}", get(resume_options))
        .route("/v1/__tick", post(tick))
        .with_state(state)
}

// ---------------- auth ----------------

fn host_ok(host: &str, port: u16) -> bool {
    let _h = host.rsplit(':').next().unwrap_or("");
    let h = if host.starts_with('[') {
        host.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    };
    let _ = h;
    // strict: Host must be 127.0.0.1[:port] or localhost[:port] with our port
    let ok = host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}");
    let _ = h;
    ok
}

async fn guard(
    state: &SharedState,
    method: &Method,
    headers: &HeaderMap,
    cookie_session_ok: bool,
    bearer_ok: bool,
) -> Result<(), Response> {
    // 0. Authentication: every guarded endpoint (GET and SSE included) requires an
    //    exact session cookie or the CLI bearer; static assets and /v1/health stay public.
    if !bearer_ok && !cookie_session_ok {
        return Err(error_response(StatusCode::UNAUTHORIZED, "auth_required", "authenticate via the startup code or the CLI bearer token"));
    }
    // 1. Host check (DNS rebinding)
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !host_ok(host, state.port) {
        return Err(error_response(StatusCode::FORBIDDEN, "bad_host", "Host must be the loopback address of this server"));
    }
    // 2. Origin check for browser + mutating verbs
    if method != Method::GET && method != Method::HEAD {
        let origin = headers.get("origin").and_then(|v| v.to_str().ok());
        match origin {
            Some(o) => {
                let expected = format!("http://127.0.0.1:{}", state.port);
                let expected2 = format!("http://localhost:{}", state.port);
                if o != expected && o != expected2 {
                    return Err(error_response(StatusCode::FORBIDDEN, "bad_origin", "Origin is not the loopback server"));
                }
                // browser-like callers (Origin present) must carry the CSRF token that only
                // an authenticated session could obtain; wrong/absent → reject
                let csrf = headers
                    .get("x-csrf-token")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if csrf != state.csrf_token {
                    return Err(error_response(StatusCode::FORBIDDEN, "csrf", "browser-session mutations need the CSRF token"));
                }
            }
            None => {
                // non-browser callers (CLI) must present the bearer token
                if !bearer_ok {
                    return Err(error_response(StatusCode::FORBIDDEN, "auth_required", "missing origin and bearer token"));
                }
            }
        }
    }
    let _ = (cookie_session_ok, bearer_ok);
    Ok(())
}

/// Combined auth: either a valid bearer token (CLI) or the session cookie (web).
/// Returns (is_bearer,) — CSRF already validated in guard for cookie callers.
fn session_cookie(headers: &HeaderMap) -> Option<String> {
    // exact name/value parse: "xwp_session=..." must NOT authenticate
    let raw = headers.get("cookie").and_then(|v| v.to_str().ok())?;
    for pair in raw.split(';') {
        let pair = pair.trim();
        if let Some((name, value)) = pair.split_once('=') {
            if name.trim() == "wp_session" {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

fn auth_context(state: &SharedState, headers: &HeaderMap) -> (bool, bool) {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == format!("Bearer {}", state.session_token))
        .unwrap_or(false);
    let cookie = session_cookie(headers)
        .map(|v| v == state.session_token)
        .unwrap_or(false);
    (bearer, cookie)
}

pub fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = json!({"error": {"code": code, "message": message}});
    (status, Json(body)).into_response()
}

fn core_error_response(e: &CoreError) -> Response {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST);
    let mut body = json!({"error": {"code": e.code, "message": e.message}});
    if let Some(r) = &e.reason {
        body["error"]["reason"] = json!(r);
    }
    (status, Json(body)).into_response()
}

// ---------------- handlers ----------------

async fn health(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    // public health check: host validation only, no auth (WP01 allowlist)
    let host = headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !host_ok(host, state.port) {
        return error_response(StatusCode::FORBIDDEN, "bad_host", "Host must be the loopback address of this server");
    }
    Json(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")})).into_response()
}

async fn create_session(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // the code exchange runs before any session exists: validate Host strictly and
    // require a same-origin Origin header (browsers always send one on POST)
    let host = headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !host_ok(host, state.port) {
        return error_response(StatusCode::FORBIDDEN, "bad_host", "Host must be the loopback address of this server");
    }
    let origin = headers.get("origin").and_then(|v| v.to_str().ok()).unwrap_or("");
    let expected_origin = format!("http://127.0.0.1:{}", state.port);
    let expected_origin2 = format!("http://localhost:{}", state.port);
    if origin != expected_origin && origin != expected_origin2 {
        return error_response(StatusCode::FORBIDDEN, "bad_origin", "code exchange requires the loopback origin");
    }
    // one-time startup code exchange
    let code = body.get("code").and_then(|c| c.as_str()).unwrap_or("");
        // atomic single-use consumption: concurrent exchanges, exactly one wins
    if code.is_empty() || !state.core.store.consume_startup_code(code) {
        return error_response(StatusCode::FORBIDDEN, "bad_code", "startup code rejected");
    }
    let cookie = format!(
        "wp_session={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400",
        state.session_token
    );
    let mut resp = Json(json!({"session": true, "csrf": state.csrf_token})).into_response();
    resp.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&cookie).unwrap(),
    );
    let _ = headers;
    resp
}

async fn get_csrf(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if !bearer && !cookie {
        return error_response(StatusCode::UNAUTHORIZED, "auth_required", "authenticate first");
    }
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    Json(json!({"csrf": state.csrf_token})).into_response()
}

async fn adapters(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    let doc = state.core.doctor();
    Json(json!({"adapters": doc["adapters"]})).into_response()
}

async fn doctor(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    Json(state.core.doctor()).into_response()
}

async fn list_tasks(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    let tasks = state.core.store.list_tasks();
    let inbox = state.core.inbox();
    Json(json!({"tasks": tasks, "inbox": inbox})).into_response()
}

async fn create_task(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::POST, &headers, cookie, bearer).await {
        return r;
    }
    // idempotency: Idempotency-Key header or request_id in body
    let request_id = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| body.get("request_id").and_then(|r| r.as_str()).map(|s| s.to_string()));
    let digest = canonical_digest(&body);
    let (request_id, rec) = match request_id {
        Some(rid) if !rid.is_empty() => {
            match state.core.begin_request(&rid, "submit", None, digest) {
                Ok(rec) if rec.status == "completed" && !rec.result.is_null() => {
                    return Json(rec.result).into_response();
                }
                Ok(rec) => (rid, Some(rec)),
                Err(e) => return core_error_response(&e),
            }
        }
        _ => (format!("auto-{}", new_token()), None),
    };
    let input = match parse_submit(&body) {
        Ok(i) => i,
        Err(e) => {
            if let Some(rec) = rec {
                state.core.fail_request(rec, json!({"error": "submit validation failed"}));
            }
            return core_error_response(&e);
        }
    };
    match state.core.submit(input) {
        Ok(t) => {
            let receipt = routing::build_receipt(&request_id, &t, None, false);
            state.core.store.meta_set(&format!("last_request:{}", t.task_id), &request_id);
            let result = json!({"task": t, "receipt": receipt});
            // persist the completed receipt so replays return the SAME task (WP05)
            if let Some(mut rec) = rec {
                rec.task_id = Some(t.task_id.clone());
                state.core.finish_request(rec, result.clone(), None);
            }
            Json(result).into_response()
        }
        Err(e) => {
            if let Some(rec) = rec {
                state.core.fail_request(rec, json!({"error": {"code": e.code.clone(), "message": e.message.clone()}}));
            }
            core_error_response(&e)
        }
    }
}

fn parse_submit(body: &Value) -> Result<SubmitInput, CoreError> {
    let goal = body
        .get("goal")
        .and_then(|g| g.as_str())
        .ok_or_else(|| CoreError::bad("goal_required", "goal is required"))?
        .to_string();
    let repo_path = body
        .get("repo_path")
        .and_then(|r| r.as_str())
        .ok_or_else(|| CoreError::bad("repo_path_required", "repo_path is required"))?
        .to_string();
    let recipe = match body.get("recipe").and_then(|r| r.as_str()).unwrap_or("direct") {
        "write_then_review" => crate::model::Recipe::WriteThenReview,
        _ => crate::model::Recipe::Direct,
    };
    let arr = |key: &str| -> Vec<String> {
        body.get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(SubmitInput {
        title: body.get("title").and_then(|t| t.as_str()).unwrap_or("").into(),
        resume_budget: body.get("resume_budget").and_then(|b| b.as_u64()),
        goal,
        acceptance: arr("acceptance"),
        repo_path,
        base_commit: body
            .get("base_commit")
            .and_then(|b| b.as_str())
            .map(|s| s.to_string()),
        allowed_paths: arr("allowed_paths"),
        forbidden_paths: arr("forbidden_paths"),
        recipe,
        allowed_adapters: arr("allowed_adapters"),
        timeout_secs: body.get("timeout_secs").and_then(|t| t.as_u64()),
    })
}

async fn trust_repo(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::POST, &headers, cookie, bearer).await {
        return r;
    }
    let task = match state.core.store.get_task(&id) {
        Some(t) => t,
        None => return error_response(StatusCode::NOT_FOUND, "not_found", "task not found"),
    };
    match state.core.trust_repo(&task.repo_path) {
        Ok(()) => Json(json!({"trusted": true, "repo": task.repo_path})).into_response(),
        Err(e) => core_error_response(&e),
    }
}

async fn create_plan_route(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::POST, &headers, cookie, bearer).await {
        return r;
    }
    let writer = body.get("writer_adapter").and_then(|w| w.as_str()).map(|s| s.to_string());
    match state.core.create_plan(&id, writer) {
        Ok(plan) => {
            let route = state.core.route_preview(&id, Role::Writer).ok();
            Json(json!({"plan": plan, "route": route})).into_response()
        }
        Err(e) => core_error_response(&e),
    }
}

async fn approve_plan(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::POST, &headers, cookie, bearer).await {
        return r;
    }
    let hash = body
        .get("summary_hash")
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .to_string();
    let handoff_digest = body.get("handoff_digest").and_then(|h| h.as_str());
    match state.core.approve_plan(&id, &hash, handoff_digest) {
        Ok(t) => {
            let receipt = routing::build_receipt("approve", &t, None, false);
            Json(json!({"task": t, "receipt": receipt})).into_response()
        }
        Err(e) => core_error_response(&e),
    }
}

#[derive(serde::Deserialize)]
struct RouteQuery {
    role: Option<String>,
}

async fn route_preview(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Query(q): Query<RouteQuery>,
    headers: HeaderMap,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    let role = match q.role.as_deref() {
        Some("reviewer") => Role::Reviewer,
        _ => Role::Writer,
    };
    match state.core.route_preview(&id, role) {
        Ok(ev) => Json(ev).into_response(),
        Err(e) => core_error_response(&e),
    }
}

async fn dispatch(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::POST, &headers, cookie, bearer).await {
        return r;
    }
    let request_id = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| body.get("request_id").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    if request_id.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "request_id_required", "dispatch needs an Idempotency-Key (the client saves it before sending)");
    }
    let digest = canonical_digest(&json!({"op": "dispatch", "task": id, "body": body}));
    let mut rec = match state.core.begin_request(&request_id, "dispatch", Some(&id), digest) {
        Ok(r) => r,
        Err(e) => return core_error_response(&e),
    };
    if rec.status == "completed" {
        if let Some(aid) = &rec.attempt_id {
            if let Some(task) = state.core.store.get_task(&id) {
                let has_open_decision = state
                    .core
                    .store
                    .list_decisions(true)
                    .iter()
                    .any(|d| d.task_id == id);
                let receipt = routing::build_receipt(&request_id, &task, Some(aid), has_open_decision);
                return Json(json!({"receipt": receipt, "replayed": true})).into_response();
            }
        }
    }
    match state.core.dispatch(&id, &mut rec) {
        Ok(t) => {
            state.core.store.meta_set(&format!("last_request:{id}"), &request_id);
            let has_open_decision = state
                .core
                .store
                .list_decisions(true)
                .iter()
                .any(|d| d.task_id == id);
            let receipt = routing::build_receipt(&request_id, &t, rec.attempt_id.as_deref(), has_open_decision);
            let result = json!({"receipt": receipt});
            let aid = rec.attempt_id.clone();
            state.core.finish_request(rec, result.clone(), aid);
            Json(result).into_response()
        }
        Err(e) => {
            state.core.fail_request(rec, json!({"error": {"code": e.code, "message": e.message}}));
            core_error_response(&e)
        }
    }
}

async fn get_task(State(state): State<SharedState>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    match state.core.task_snapshot(&id) {
        Some(snap) => Json(snap).into_response(),
        None => error_response(StatusCode::NOT_FOUND, "not_found", "task not found"),
    }
}

async fn get_attempt(State(state): State<SharedState>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    match state.core.store.get_attempt(&id) {
        Some(a) => Json(json!({"attempt": a})).into_response(),
        None => error_response(StatusCode::NOT_FOUND, "not_found", "attempt not found"),
    }
}

async fn cancel_attempt(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::POST, &headers, cookie, bearer).await {
        return r;
    }
    let request_id = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| body.get("request_id").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| format!("auto-cancel-{}", new_token()));
    let digest = canonical_digest(&json!({"op": "cancel", "attempt": id}));
    let rec = match state.core.begin_request(&request_id, "cancel", None, digest) {
        Ok(r) => r,
        Err(e) => return core_error_response(&e),
    };
    if rec.status == "completed" {
        return Json(rec.result).into_response();
    }
    match state.core.request_cancel(&id) {
        Ok(()) => {
            let result = json!({"cancelling": true, "attempt_id": id, "request_id": request_id});
            state.core.finish_request(rec, result.clone(), Some(id.clone()));
            Json(result).into_response()
        }
        Err(e) => {
            state.core.fail_request(rec, json!({"error": {"code": e.code, "message": e.message}}));
            core_error_response(&e)
        }
    }
}

async fn list_decisions(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    let open_only = q.get("status").map(|s| s == "open").unwrap_or(false);
    Json(json!({"decisions": state.core.store.list_decisions(open_only)})).into_response()
}

async fn answer_decision(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::POST, &headers, cookie, bearer).await {
        return r;
    }
    let option_id = body
        .get("option_id")
        .and_then(|o| o.as_str())
        .unwrap_or("")
        .to_string();
    let revision = body.get("revision").and_then(|r| r.as_u64()).unwrap_or(0);
    let request_id = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| body.get("request_id").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    if request_id.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "request_id_required", "answer needs an Idempotency-Key");
    }
    let digest = canonical_digest(&json!({"op": "answer", "decision": id, "option": option_id, "revision": revision}));
    let mut rec = match state.core.begin_request(&request_id, "answer", None, digest) {
        Ok(r) => r,
        Err(e) => return core_error_response(&e),
    };
    if rec.status == "completed" {
        return Json(rec.result).into_response();
    }
    // WP09: an OPEN card requires the approved body digest; a duplicate (already
    // answered) replays the original receipt without one
    let approved_digest = body
        .get("handoff_digest")
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(d) = state.core.store.get_decision(&id) {
        if d.status == "open" && approved_digest.is_empty() {
            return error_response(
                StatusCode::CONFLICT,
                "handoff_digest_required",
                "the resume body must be previewed and approved: fetch the handoff preview and resend with its digest",
            );
        }
    }
    match state.core.answer_decision(&id, &option_id, revision, &approved_digest, &mut rec) {
        Ok((d, task)) => {
            let receipt = routing::build_receipt(&request_id, &task, Some(&d.attempt_id), true);
            let result = json!({"receipt": receipt, "decision": d, "queued": true});
            state.core.finish_request(rec, result.clone(), Some(d.attempt_id.clone()));
            Json(result).into_response()
        }
        Err(e) => {
            state.core.fail_request(rec, json!({"error": {"code": e.code, "message": e.message}}));
            core_error_response(&e)
        }
    }
}

async fn reroute(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::POST, &headers, cookie, bearer).await {
        return r;
    }
    let target = body
        .get("target_adapter")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let request_id = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| body.get("request_id").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    if request_id.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "request_id_required", "reroute needs an Idempotency-Key");
    }
    let digest = canonical_digest(&json!({"op": "reroute", "task": id, "target": target}));
    let mut rec = match state.core.begin_request(&request_id, "reroute", Some(&id), digest) {
        Ok(r) => r,
        Err(e) => return core_error_response(&e),
    };
    if rec.status == "completed" {
        return Json(rec.result).into_response();
    }
    // WP09 weekend: the relay body must be previewed and approved before queueing
    let approved_digest = body
        .get("handoff_digest")
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .to_string();
    if approved_digest.is_empty() {
        return error_response(
            StatusCode::CONFLICT,
            "handoff_digest_required",
            "the relay body must be previewed and approved: fetch the handoff preview and resend with its digest",
        );
    }
    match state.core.reroute(&id, &target, &approved_digest, &mut rec) {
        Ok(t) => {
            let receipt = routing::build_receipt(&request_id, &t, None, false);
            let result = json!({"receipt": receipt});
            state.core.finish_request(rec, result.clone(), None);
            Json(result).into_response()
        }
        Err(e) => {
            state.core.fail_request(rec, json!({"error": {"code": e.code, "message": e.message}}));
            core_error_response(&e)
        }
    }
}

async fn accept(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::POST, &headers, cookie, bearer).await {
        return r;
    }
    let cand = body
        .get("candidate_hash")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let request_id = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| body.get("request_id").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    if request_id.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "request_id_required", "accept needs an Idempotency-Key");
    }
    let digest = canonical_digest(&json!({"op": "accept", "task": id, "candidate": cand}));
    let mut rec = match state.core.begin_request(&request_id, "accept", Some(&id), digest) {
        Ok(r) => r,
        Err(e) => return core_error_response(&e),
    };
    if rec.status == "completed" && !rec.result.is_null() {
        return Json(rec.result).into_response();
    }
    match state.core.accept(&id, &cand, &mut rec) {
        Ok((t, result)) => {
            let receipt = routing::build_receipt(&request_id, &t, None, false);
            let mut result = result;
            result["receipt"] = receipt;
            state.core.finish_request(rec, result.clone(), None);
            Json(result).into_response()
        }
        Err(e) => {
            state.core.fail_request(rec, json!({"error": {"code": e.code, "message": e.message}}));
            core_error_response(&e)
        }
    }
}

async fn handoff_preview(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    // preview the handoff that the NEXT step would send (writer or reviewer)
    let role = q.get("role").map(|r| match r.as_str() {
        "reviewer" => Role::Reviewer,
        _ => Role::Writer,
    }).unwrap_or(Role::Writer);
    let Some(task) = state.core.store.get_task(&id) else {
        return error_response(StatusCode::NOT_FOUND, "not_found", "task not found");
    };
    let Some(plan) = state.core.store.latest_plan(&id) else {
        return error_response(StatusCode::NOT_FOUND, "not_found", "no plan");
    };
    let Some(step) = plan.steps.iter().find(|s| s.role == role) else {
        return error_response(StatusCode::NOT_FOUND, "not_found", "no such step");
    };
    let generation = task.current_generation + 1;
    let pending = match (q.get("decision"), q.get("option")) {
        (Some(d), Some(o)) => Some((d.as_str(), o.as_str())),
        _ => None,
    };
    let relay_cp = if step.role == crate::model::Role::Writer && generation > 1 {
        state.core.checkpoint_view(&task)
    } else {
        None
    };
    match state.core.assemble_handoff(&task, step, generation, pending, relay_cp) {
        Ok(h) => Json(json!({
            "handoff": {
                "text": h.text, "bytes": h.bytes, "sha256": h.sha256,
                "mode": h.mode, "budget_bytes": h.budget_bytes,
                "evidence_refs": h.evidence_refs,
            },
            "approved_digest": sha256_hex(h.text.as_bytes()),
        }))
        .into_response(),
        Err(e) => core_error_response(&e),
    }
}

async fn task_diff(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    let Some(cand) = state.core.store.current_candidate(&id) else {
        return error_response(StatusCode::NOT_FOUND, "not_found", "no candidate");
    };
    let diff = cand
        .patch_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    Json(json!({
        "candidate_hash": cand.candidate_hash,
        "label": cand.label,
        "changed_files": cand.changed_files,
        "frozen_commit": cand.frozen_commit,
        "diff": diff,
        "generation": cand.generation,
    }))
    .into_response()
}

async fn list_messages(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    Json(json!({"messages": state.core.inbox()})).into_response()
}

async fn resume_options(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    let (same_session, native_id) = state.core.resume_same_session_possible(&id);
    Json(json!({
        "same_session_resume": same_session,
        "native_id": native_id,
        "note": if same_session {
            "resume will send only the delta to the confirmed native session"
        } else {
            "same native session cannot be confirmed; a full compact package requires preview and approval"
        },
    }))
    .into_response()
}

async fn tick(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    state.core.scheduler_tick();
    Json(json!({"ticked": true})).into_response()
}

async fn get_request(State(state): State<SharedState>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    match state.core.store.get_request(&id) {
        Some(rec) => {
            // reconciliation semantics: completed → replay result; processing after
            // restart → unknown state honestly; never suggest auto-resend
            let status = rec.status.clone();
            Json(json!({
                "request": rec,
                "reconciliation": match status.as_str() {
                    "completed" => "completed",
                    "processing" => "unknown",
                    _ => "unknown",
                },
                "guidance": match status.as_str() {
                    "completed" => "replay the stored result with the same request id",
                    _ => "no completion evidence; do not auto-resend — check the task timeline and reconcile manually",
                },
            }))
            .into_response()
        }
        None => error_response(StatusCode::NOT_FOUND, "not_found", "request id not found; absence of a record does not prove the request never started"),
    }
}

async fn sse_events(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Response {
    let (bearer, cookie) = auth_context(&state, &headers);
    if let Err(r) = guard(&state, &Method::GET, &headers, cookie, bearer).await {
        return r;
    }
    let rx = state.core.bus.subscribe();
    let last_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    // replay missed events from the store (window), then live
    let missed = state.core.store.list_events_after(last_id, 500);
    let live = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let data = serde_json::json!({
                        "seq": ev.seq,
                        "task_id": ev.task_id,
                        "attempt_id": ev.attempt_id,
                        "generation": ev.generation,
                        "kind": ev.kind,
                        "payload": ev.payload,
                        "ts": ev.ts,
                    });
                    return Some((
                        Ok::<_, Infallible>(SseEvent::default()
                            .event(ev.kind.clone())
                            .id(ev.seq.to_string())
                            .data(data.to_string())),
                        rx,
                    ));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    });
    let replay = futures_util::stream::iter(missed.into_iter().map(|ev| {
        let data = serde_json::json!({
            "seq": ev.seq,
            "task_id": ev.task_id,
            "attempt_id": ev.attempt_id,
            "generation": ev.generation,
            "kind": ev.kind,
            "payload": ev.payload,
            "ts": ev.ts,
        });
        Ok::<_, Infallible>(SseEvent::default()
            .event(ev.kind.clone())
            .id(ev.seq.to_string())
            .data(data.to_string()))
    }));
    let stream = replay.chain(live);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping")).into_response()
}
