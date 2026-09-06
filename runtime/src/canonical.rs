//! Canonical JSON and digests.
//!
//! Every user-visible commitment in Hwahap is bound to a digest: the plan challenge
//! (`CONFIRM PLAN <challenge>`), the ship challenge (`SHIP <challenge>`), answer bindings, and the
//! journal hash chain. All of them go through this module so that "same content" means
//! "same bytes" exactly once, in one place.
//!
//! Canonical form:
//! - object keys sorted by Unicode scalar value,
//! - no insignificant whitespace,
//! - strings escaped by `serde_json`'s minimal escaping,
//! - numbers restricted to integers representable as `i64`/`u64`.
//!
//! Floats are rejected rather than rounded. A digest that silently depends on float formatting is a
//! digest that silently changes, and a challenge the user already typed would stop matching.

use serde::Serialize;
use serde_json::{Map, Value};
use std::fmt::Write as _;

use crate::error::{Error, Result};

/// Length of the user-facing challenge suffix, in hex characters.
pub const CHALLENGE_LEN: usize = 8;

/// A sha256 digest rendered as `sha256:<64 lowercase hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// Hashes raw bytes.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        use sha2::{Digest as _, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Digest(format!("sha256:{}", hex::encode(hasher.finalize())))
    }

    /// Hashes the canonical encoding of `value`.
    pub fn of_canonical(value: &Value) -> Result<Self> {
        Ok(Self::of_bytes(canonical_json(value)?.as_bytes()))
    }

    /// Hashes the canonical encoding of any serializable value.
    pub fn of<T: Serialize>(value: &T) -> Result<Self> {
        let json = serde_json::to_value(value).map_err(|e| Error::Internal(e.to_string()))?;
        Self::of_canonical(&json)
    }

    /// The all-zero digest, used as the hash chain's genesis link.
    pub fn zero() -> Self {
        Digest(format!("sha256:{}", "0".repeat(64)))
    }

    /// The full `sha256:<hex>` form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The uppercase hex prefix the user types back, e.g. `7F3A91C2`.
    ///
    /// Uppercase because the user retypes it; mixed case invites transcription errors that would
    /// read as a mismatched plan.
    pub fn challenge(&self) -> String {
        self.0
            .trim_start_matches("sha256:")
            .chars()
            .take(CHALLENGE_LEN)
            .flat_map(char::to_uppercase)
            .collect()
    }

    /// Parses a `sha256:<64 hex>` string.
    pub fn parse(text: &str) -> Result<Self> {
        let hex = text.strip_prefix("sha256:").ok_or_else(|| {
            Error::Corrupt(format!("digest is missing the sha256: prefix: {text}"))
        })?;
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(Error::Corrupt(format!(
                "digest must be 64 lowercase hex characters: {text}"
            )));
        }
        Ok(Digest(text.to_string()))
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Serializes `value` into canonical JSON.
pub fn canonical_json(value: &Value) -> Result<String> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let _ = write!(out, "{i}");
            } else if let Some(u) = n.as_u64() {
                let _ = write!(out, "{u}");
            } else {
                return Err(Error::Corrupt(format!(
                    "canonical JSON rejects the non-integer number {n}"
                )));
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, key) in sorted_keys(map).into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(&map[key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn sorted_keys(map: &Map<String, Value>) -> Vec<&String> {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_unstable();
    keys
}

fn write_string(s: &str, out: &mut String) {
    // `serde_json` already emits the shortest legal escaping for a string, and it never fails for a
    // `&str`, so reuse it rather than maintaining a second escaper that could drift from it.
    let encoded = serde_json::to_string(s).expect("serializing a &str cannot fail");
    out.push_str(&encoded);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_keys_are_sorted_by_unicode_scalar_value() {
        let value = json!({"b": 1, "a": 2, "A": 3, "\u{00e9}": 4, "0": 5});
        assert_eq!(
            canonical_json(&value).unwrap(),
            r#"{"0":5,"A":3,"a":2,"b":1,"é":4}"#
        );
    }

    #[test]
    fn key_order_does_not_change_the_digest() {
        let a: Value = serde_json::from_str(r#"{"x":1,"y":{"p":true,"q":[1,2]}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"y":{"q":[1,2],"p":true},"x":1}"#).unwrap();
        assert_eq!(
            Digest::of_canonical(&a).unwrap(),
            Digest::of_canonical(&b).unwrap()
        );
    }

    #[test]
    fn array_order_does_change_the_digest() {
        let a = json!([1, 2]);
        let b = json!([2, 1]);
        assert_ne!(
            Digest::of_canonical(&a).unwrap(),
            Digest::of_canonical(&b).unwrap()
        );
    }

    #[test]
    fn whitespace_is_not_significant() {
        let a: Value = serde_json::from_str("{\n  \"x\" : 1\n}").unwrap();
        let b: Value = serde_json::from_str(r#"{"x":1}"#).unwrap();
        assert_eq!(canonical_json(&a).unwrap(), canonical_json(&b).unwrap());
    }

    #[test]
    fn floats_are_rejected_rather_than_rounded() {
        let err = canonical_json(&json!({"x": 1.5})).unwrap_err();
        assert!(
            matches!(err, Error::Corrupt(_)),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn integers_at_the_i64_and_u64_boundaries_survive() {
        let value = json!({"min": i64::MIN, "max": u64::MAX});
        assert_eq!(
            canonical_json(&value).unwrap(),
            format!(r#"{{"max":{},"min":{}}}"#, u64::MAX, i64::MIN)
        );
    }

    #[test]
    fn strings_are_escaped_minimally_and_reversibly() {
        let value = json!({"s": "a\"b\\c\nd\te\u{0007}f\u{00e9}"});
        let encoded = canonical_json(&value).unwrap();
        // Control characters take the \u form; printable non-ASCII stays literal.
        assert_eq!(
            encoded,
            concat!(r#"{"s":"a\"b\\c\nd\te"#, r#"\u0007"#, "f\u{00e9}", r#""}"#)
        );
        let round_tripped: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(round_tripped, value);
    }

    #[test]
    fn nested_empty_containers_encode_without_separators() {
        assert_eq!(
            canonical_json(&json!({"a": [], "b": {}})).unwrap(),
            r#"{"a":[],"b":{}}"#
        );
    }

    #[test]
    fn challenge_is_eight_uppercase_hex_characters_of_the_digest() {
        let digest = Digest::of_bytes(b"hwahap");
        let challenge = digest.challenge();
        assert_eq!(challenge.len(), CHALLENGE_LEN);
        assert!(challenge
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase()));
        assert!(digest.as_str().starts_with("sha256:"));
        assert_eq!(
            challenge,
            digest.as_str()["sha256:".len()..][..CHALLENGE_LEN].to_uppercase()
        );
    }

    #[test]
    fn different_content_yields_a_different_challenge() {
        let a = Digest::of_canonical(&json!({"plan": 1})).unwrap();
        let b = Digest::of_canonical(&json!({"plan": 2})).unwrap();
        assert_ne!(a.challenge(), b.challenge());
    }

    #[test]
    fn zero_digest_is_the_genesis_link() {
        assert_eq!(
            Digest::zero().as_str(),
            "sha256:".to_string() + &"0".repeat(64)
        );
        assert_eq!(
            Digest::parse(Digest::zero().as_str()).unwrap(),
            Digest::zero()
        );
    }

    #[test]
    fn parse_rejects_malformed_digests() {
        for bad in [
            "",
            "deadbeef",
            "sha256:",
            "sha256:zz",
            &format!("sha256:{}", "0".repeat(63)),
            &format!("sha256:{}", "0".repeat(65)),
            &format!("sha256:{}", "A".repeat(64)),
        ] {
            assert!(Digest::parse(bad).is_err(), "should have rejected {bad:?}");
        }
    }

    #[test]
    fn parse_accepts_a_digest_this_module_produced() {
        let digest = Digest::of_bytes(b"round trip");
        assert_eq!(Digest::parse(digest.as_str()).unwrap(), digest);
    }
}
