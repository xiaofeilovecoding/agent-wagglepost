//! H1 · Unified capability table: one registration/probe entry per runtime.
//!
//! States (合同要求至少五档): not_installed → needs_config → probed → verified →
//! unsupported. Probing is strictly read-only: version/help checks only — it never
//! submits a model request, installs anything, or scans personal sessions or
//! credential stores. Verification is bound to an exact executable version; when
//! the on-device version changes, affected role verification degrades back to
//! probed (旧 unknown 维持 unknown — a capability that was never verified is never
//! promoted by a version change).
//!
//! 验证通过 evidence comes from two sources, recorded per record:
//! - `real_loop`: an attempt dispatched through wagglepost completed on this exact
//!   version with confirmed process-tree stop (written by core on attempt success).
//! - `offline_contract`: the version's own help/transport was verified locally
//!   (`wagglepost verify <adapter>`): entry executable, structured-output flags
//!   present, and — only where the vendor exposes one — a native permission
//!   boundary flag. Offline verification NEVER opens a writer role by itself.
//!
//! A runtime whose tools cannot be natively restricted (no deny/read-only
//! mechanism) keeps its roles `experimental`: the UI shows 实验/仅检测 and routing
//! never auto-selects it (auto_route = false).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const STATE_NOT_INSTALLED: &str = "not_installed";
pub const STATE_NEEDS_CONFIG: &str = "needs_config";
pub const STATE_PROBED: &str = "probed";
pub const STATE_VERIFIED: &str = "verified";
pub const STATE_UNSUPPORTED: &str = "unsupported";

/// How the verification evidence was produced (H1: 只在所声明版本和角色有证据时宣称支持).
pub const EVID_OFFLINE_CONTRACT: &str = "offline_contract";
pub const EVID_REAL_LOOP: &str = "real_loop";

/// Probe-time fact: this runtime exposes a native mechanism that can restrict the
/// role (vendor flag/config), recorded as evidence text. A role without a boundary
/// is experimental — never auto-routed.
#[derive(Clone, Debug)]
pub struct RoleProbe {
    pub role: &'static str, // "writer" | "reviewer"
    /// the entry/transport for this role exists on this version
    pub supported: bool,
    /// native permission boundary evidence; None = cannot restrict natively
    pub boundary: Option<&'static str>,
}

impl RoleProbe {
    pub fn experimental(&self) -> bool {
        self.boundary.is_none()
    }
}

/// Display record: probe facts + verification overlay.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProbeRecord {
    pub adapter: String,
    pub executable: Option<String>,
    pub version: Option<String>,
    /// not_installed | needs_config | probed | verified | unsupported
    pub state: String,
    /// how stopping is enforced, e.g. "job_object_tree" (wagglepost-side containment)
    pub stop_method: Option<String>,
    /// "none" | "native_session" (same-session resume supported with an id)
    pub same_session_resume: String,
    /// where usage numbers come from, e.g. "provider_usage_events"; null = unknown
    pub usage_source: Option<String>,
    pub roles: Vec<RoleCap>,
    /// version the current verification evidence is bound to (empty = never)
    pub verified_version: String,
    pub verified_at: String,
    /// offline_contract | real_loop | "" (never verified)
    pub evidence: String,
    pub probe_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RoleCap {
    /// "writer" | "reviewer"
    pub role: String,
    pub supported: bool,
    pub verified: bool,
    /// native permission boundary evidence (None = experimental role)
    pub permission_boundary: Option<String>,
    /// routing may auto-select (verified + boundary + not experimental)
    pub auto_route: bool,
    /// 实验/仅检测: cannot be natively restricted → never auto-routed
    pub experimental: bool,
}

impl ProbeRecord {
    pub fn state_label(&self, lang: &str) -> &'static str {
        match self.state.as_str() {
            STATE_NOT_INSTALLED => if lang == "en" { "Not installed" } else { "未安装" },
            STATE_NEEDS_CONFIG => if lang == "en" { "Needs setup" } else { "需配置" },
            STATE_PROBED => if lang == "en" { "Probed" } else { "已探测" },
            STATE_VERIFIED => if lang == "en" { "Verified" } else { "验证通过" },
            STATE_UNSUPPORTED => if lang == "en" { "Unsupported" } else { "不支持" },
            _ => if lang == "en" { "Unknown" } else { "未知" },
        }
    }

    pub fn role(&self, role: &str) -> Option<&RoleCap> {
        self.roles.iter().find(|r| r.role == role)
    }

    /// Routing gate: this role may be auto-dispatched only with verification,
    /// a proven native boundary and no experimental mark.
    pub fn routable(&self, role: &str) -> bool {
        self.role(role)
            .map(|r| r.verified && r.auto_route && !r.experimental)
            .unwrap_or(false)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "adapter": self.adapter,
            "executable": self.executable,
            "version": self.version,
            "state": self.state,
            "state_label_zh": self.state_label("zh"),
            "state_label_en": self.state_label("en"),
            "stop_method": self.stop_method,
            "same_session_resume": self.same_session_resume,
            "usage_source": self.usage_source,
            "roles": self.roles,
            "verified_version": self.verified_version,
            "verified_at": self.verified_at,
            "evidence": self.evidence,
            "probe_error": self.probe_error,
        })
    }

    /// Build probe facts + verification overlay. Verification bound to a different
    /// version OR a different binary degrades to probed; never-verified stays
    /// unknown (旧 unknown 维持 unknown).
    pub fn compose(probe: ProbeRecord, ver: Option<&VerificationRecord>) -> ProbeRecord {
        let mut rec = probe;
        let Some(ver) = ver else {
            return rec;
        };
        let live = rec.version.clone().unwrap_or_default();
        // an unbound record cannot prove WHICH binary was verified → fail closed
        // against any live path (roles stay closed until a bound record exists)
        // binary identity: path must match AND, when a content hash is bound,
        // the record must carry the same hash (a same-path replacement degrades
        // at the dispatch gate; here path/version binding is checked cheaply)
        let binary_changed = match (&ver.exe_path, &rec.executable) {
            (Some(bound), Some(live_exe)) => bound != live_exe,
            // in-process runtimes have no binary: None == None matches
            (None, None) => false,
            // an unbound record facing a real binary (or vice versa) cannot
            // prove which binary was verified → fail closed
            _ => true,
        };
        if ver.version != live || binary_changed {
            rec.probe_error = Some(if binary_changed {
                format!(
                    "verification was bound to binary {}, now {}; capabilities re-verify on the next real run",
                    ver.exe_path.as_deref().unwrap_or("?"),
                    rec.executable.as_deref().unwrap_or("?")
                )
            } else {
                format!(
                    "verification was bound to version {}, installed is {}; capabilities re-verify on the next real run",
                    ver.version,
                    if live.is_empty() { "?" } else { &live }
                )
            });
            return rec; // 版本/二进制变化 → 受影响能力降级，重验证
        }
        rec.verified_version = ver.version.clone();
        rec.verified_at = ver.verified_at.clone();
        rec.evidence = ver.evidence.clone();
        for rc in rec.roles.iter_mut() {
            if ver.roles.contains(&rc.role) {
                rc.verified = true;
                rc.auto_route = !rc.experimental && rc.permission_boundary.is_some();
            }
        }
        if rec.roles.iter().any(|r| r.verified) {
            rec.state = STATE_VERIFIED.into();
        }
        rec
    }
}

/// Persisted evidence: which roles were verified on exactly this version.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct VerificationRecord {
    pub version: String,
    pub verified_at: String,
    /// offline_contract | real_loop
    pub evidence: String,
    pub roles: Vec<String>,
    /// WP22: the verified capability is bound to THIS binary; a path change
    /// degrades the record (the installed thing is no longer the verified one).
    #[serde(default)]
    pub exe_path: Option<String>,
    /// WP26: content identity (sha256) of the verified binary — a same-path
    /// replacement fails closed at the dispatch gate.
    #[serde(default)]
    pub exe_sha256: Option<String>,
}

/// WP26: revoke all role authorization for an adapter, keeping the record as an
/// audit trail. Called on verification failure and at the dispatch gate when the
/// binary content no longer matches the verified identity.
pub fn revoke(store: &crate::store::Store, adapter: &str, reason: &str) {
    let Some(mut ver) = load_verified(store, adapter) else {
        return;
    };
    if ver.roles.is_empty() {
        return;
    }
    ver.roles.clear();
    save_verified(store, adapter, &ver);
    let _ = reason;
}

/// meta key for the persisted verification record of one adapter
pub fn meta_key(adapter: &str) -> String {
    format!("capability_verified:{adapter}")
}

pub fn load_verified(store: &crate::store::Store, adapter: &str) -> Option<VerificationRecord> {
    let raw = store.meta_get(&meta_key(adapter))?;
    serde_json::from_str(&raw).ok()
}

pub fn save_verified(store: &crate::store::Store, adapter: &str, ver: &VerificationRecord) {
    if let Ok(s) = serde_json::to_string(ver) {
        store.meta_set(&meta_key(adapter), &s);
    }
}

/// Upgrade one role after a real wagglepost attempt completed with confirmed stop
/// on the live version AND the same binary. Existing other-role evidence is
/// preserved; a version or path change starts a fresh binding.
pub fn note_real_loop(
    store: &crate::store::Store,
    adapter: &str,
    version: &str,
    exe_path: Option<&str>,
    exe_sha256: Option<&str>,
    role: &str,
) {
    let matches_binding = |v: &VerificationRecord| {
        v.version == version
            && match (&v.exe_path, exe_path) {
                (Some(a), Some(b)) => a == b,
                (None, None) => true,
                _ => false,
            }
            && match (&v.exe_sha256, exe_sha256) {
                (Some(a), Some(b)) => a == b,
                (None, None) => true,
                _ => false,
            }
    };
    let mut ver = load_verified(store, adapter)
        .filter(|v| matches_binding(v))
        .unwrap_or(VerificationRecord {
            version: version.to_string(),
            verified_at: crate::store::timestamp(),
            evidence: EVID_REAL_LOOP.into(),
            roles: Vec::new(),
            exe_path: exe_path.map(|s| s.to_string()),
            exe_sha256: exe_sha256.map(|s| s.to_string()),
        });
    if !ver.roles.iter().any(|r| r == role) {
        ver.roles.push(role.to_string());
    }
    ver.evidence = EVID_REAL_LOOP.into();
    save_verified(store, adapter, &ver);
}

/// Resolve a PATH hit to a directly executable file. npm-style shims (shell script
/// / .cmd / .ps1) are followed to the real .exe they exec — read-only resolution,
/// no config is modified.
pub fn resolve_entry(p: &Path) -> PathBuf {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("exe") | Some("com") => return p.to_path_buf(),
        _ => {}
    }
    // standard npm shim layout: <dir>/node_modules/<pkg>/bin/<name>.exe
    let dir = p.parent().unwrap_or(p);
    for tail in [
        "node_modules/@anthropic-ai/claude-code/bin/claude.exe",
        "node_modules/@anthropic-ai/claude-code/cli.js",
    ] {
        let cand = dir.join(tail.replace('/', "\\"));
        if tail.ends_with(".exe") && cand.is_file() {
            return cand;
        }
    }
    // last resort: parse the shim text for an .exe path token
    if let Ok(text) = std::fs::read_to_string(p) {
        for token in text.split_whitespace() {
            let t = token.trim_matches('"').trim_matches('\'');
            if t.to_ascii_lowercase().ends_with(".exe") {
                let cand = PathBuf::from(t.replace("%dp0%\\", "").replace("%~dp0", ""));
                let cand = if cand.is_absolute() {
                    cand
                } else {
                    dir.join(&cand)
                };
                if cand.is_file() {
                    return cand;
                }
            }
        }
    }
    p.to_path_buf()
}
