use rand::Rng;
use sha2::{Digest, Sha256};

pub fn new_id(prefix: &str) -> String {
    let mut b = [0u8; 8];
    rand::thread_rng().fill(&mut b);
    format!("{}-{}", prefix, hex::encode(b))
}

pub fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill(&mut b);
    hex::encode(b)
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

pub fn short_hash(data: &[u8]) -> String {
    sha256_hex(data)[..8].to_string()
}

/// Canonical JSON digest for idempotency keys: serialize with sorted maps.
pub fn canonical_digest(value: &serde_json::Value) -> String {
    fn write(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push(':');
                    write(&m[*k], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(a) => {
                out.push('[');
                for (i, item) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&serde_json::to_string(other).unwrap_or_else(|_| "null".into())),
        }
    }
    let mut s = String::new();
    write(value, &mut s);
    sha256_hex(s.as_bytes())
}
