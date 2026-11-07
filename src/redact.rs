//! WP16: bounded secret redaction for agent-emitted diagnostics — wire events,
//! agent results (summary / question / blocked reason), inbox messages and raw
//! stderr logs are scrubbed BEFORE they reach persistence, the API/SSE surface
//! or a relay handoff. The token VALUE is replaced while its TYPE and the
//! surrounding diagnostic shape stay intact (same error class, same location).
//! User code, patches and diffs never pass through here.
//!
//! This is deliberately NOT the environment-inheritance boundary (the launch
//! env allowlist in runner::filter_env) — that keeps secrets OUT of the agent;
//! this keeps anything the agent PRINTS from persisting verbatim.

/// One span of token-charset bytes.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'~' | b'+' | b'=')
}

const PREFIXES: &[&str] = &[
    "github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "sk-ant-", "sk-", "xoxb-", "xoxp-", "xoxa-", "AKIA", "eyJ",
];

/// Redact credential-shaped spans from a free-text diagnostic. Idempotent and
/// bounded: only `Bearer <token>` schemes and known token prefixes are touched;
/// everything else (including all non-ASCII text) passes through byte-exact.
pub fn redact(s: &str) -> String {
    let bytes = s.as_bytes();
    // collect (start, end, replacement) spans; every boundary is ASCII-aligned
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // "bearer <token>" — case-insensitive scheme, ≥8 token chars after separators
        if bytes.len() - i >= 6 && bytes[i..i + 6].eq_ignore_ascii_case(b"bearer") {
            let mut j = i + 6;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b':' | b'_' | b'-' | b'=') {
                j += 1;
            }
            let start = j;
            while j < bytes.len() && is_token_byte(bytes[j]) {
                j += 1;
            }
            if j - start >= 8 {
                // keep a recognizable provider prefix (ghp_, sk-, ...) so the log
                // still says WHICH credential type leaked, never the secret value
                let token = &s[start..j];
                let replacement = match PREFIXES.iter().find(|p| token.starts_with(*p)) {
                    Some(p) => format!("Bearer {p}[REDACTED]"),
                    None => "Bearer [REDACTED]".to_string(),
                };
                spans.push((i, j, replacement));
                i = j;
                continue;
            }
        }
        let mut advanced = false;
        for p in PREFIXES {
            if bytes[i..].starts_with(p.as_bytes()) {
                let mut j = i + p.len();
                while j < bytes.len() && is_token_byte(bytes[j]) {
                    j += 1;
                }
                if j - i >= p.len() + 8 {
                    spans.push((i, j, format!("{p}[REDACTED]")));
                    i = j;
                    advanced = true;
                    break;
                }
            }
        }
        if !advanced {
            i += 1;
        }
    }
    if spans.is_empty() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut last = 0;
    for (a, b, rep) in &spans {
        out.push_str(&s[last..*a]);
        out.push_str(rep);
        last = *b;
    }
    out.push_str(&s[last..]);
    out
}

/// Redact every string leaf of a JSON payload (wire events carry arbitrary
/// structured payloads; only string values are rewritten).
pub fn redact_value(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => serde_json::Value::String(redact(s)),
        serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(redact_value).collect()),
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.iter().map(|(k, val)| (k.clone(), redact_value(val))).collect(),
        ),
        other => other.clone(),
    }
}

/// Scrub every agent-authored free-text field of a structured result BEFORE it is
/// persisted as a decision, checkpoint, review, inbox message or relay body.
/// The status/booleans/option ids are kept so routing and the error class stay
/// diagnosable; only the token VALUE is replaced.
pub fn redact_agent_result(r: &mut crate::model::AgentResult) {
    r.summary = r.summary.as_ref().map(|s| redact(s));
    r.question = r.question.as_ref().map(|s| redact(s));
    r.blocked_reason = r.blocked_reason.as_ref().map(|s| redact(s));
    for o in &mut r.options {
        o.label_zh = redact(&o.label_zh);
        o.label_en = redact(&o.label_en);
        o.effect_zh = redact(&o.effect_zh);
        o.effect_en = redact(&o.effect_en);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_bearer_and_known_prefixes_keeps_type() {
        let x = redact("Authorization: Bearer ghp_AbcdefGhijklMnOpqr1234567890 done");
        assert_eq!(x, "Authorization: Bearer ghp_[REDACTED] done");
        let y = redact("connect failed: Bearer_sk-ant-api03-aaaaaaaaaaaaaaaaaaaa");
        assert!(y.contains("[REDACTED]") && y.contains("connect failed"));
        let z = redact("AKIAABCDEFGHIJKLMNOP at iam");
        assert_eq!(z, "AKIA[REDACTED] at iam");
        // untouched prose and short runs
        assert_eq!(redact("simulated quota exhausted"), "simulated quota exhausted");
        assert_eq!(redact("see sk-12 for details"), "see sk-12 for details");
        // multibyte text passes through byte-exact
        assert_eq!(redact("写入被拒绝（parent traversal）"), "写入被拒绝（parent traversal）");
        // idempotent
        let once = redact("Bearer Abcdef1234567890xyz");
        assert_eq!(redact(&once), once);
    }
}
