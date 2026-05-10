use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{SecondsFormat, Utc};
use rand::RngCore;
use regex::Regex;
use std::sync::LazyLock;

pub(crate) static NAME_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*Name:\*\* (.+)").expect("valid regex"));

/// Regex for valid entity IDs: alphanumeric, hyphens, underscores, dots, max 128 chars.
static ENTITY_ID_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]{0,127}$").expect("valid regex"));

pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn today_date() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

pub(crate) fn gen_id(prefix: &str) -> String {
    let mut bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Validate an entity ID against allowed format.
pub(crate) fn validate_entity_id(id: &str) -> anyhow::Result<()> {
    if !ENTITY_ID_REGEX.is_match(id) {
        anyhow::bail!(
            "Invalid entity ID '{}'. Must be 1-128 chars, alphanumeric/hyphens/underscores/dots, starting with alphanumeric.",
            id
        );
    }
    Ok(())
}

/// Sanitize user input for FTS5 MATCH queries.
/// Wraps each word in double quotes to prevent FTS5 syntax injection.
/// Uses implicit AND semantics (space-separated terms in FTS5 = AND).
pub(crate) fn sanitize_fts_query(input: &str) -> String {
    input
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|word| {
            let escaped = word.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(" ")
}
