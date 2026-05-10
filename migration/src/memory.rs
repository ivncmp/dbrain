use crate::config::Tiers;

#[allow(dead_code)] // Kept for future background tier recomputation task
pub(crate) fn compute_tier(last_accessed: &str, access_count: i64, tiers: &Tiers) -> &'static str {
    let parsed = chrono::DateTime::parse_from_rfc3339(last_accessed).ok();
    let days_since = parsed
        .map(|dt| {
            let now = chrono::Utc::now().date_naive();
            let then = dt.date_naive();
            (now - then).num_days()
        })
        .unwrap_or(i64::MAX);

    if days_since <= tiers.hot_days || access_count >= tiers.hot_min_access {
        return "hot";
    }
    if days_since <= tiers.warm_days {
        return "warm";
    }
    "cold"
}
