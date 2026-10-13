//! How current a remembered statement is.
//!
//! A conclusion or a verified claim was true when it was checked, on the page it was checked on.
//! Prices, versions and availability change; a published specification changes slowly; a historical
//! fact does not. This module turns the recorded dates into a plain verdict (`Current`, `Stale`,
//! `Unknown`) under a documented policy, so that an old answer is never presented as if it were
//! checked today. It never invents a date: with no recorded date the verdict is `Unknown`.

use serde::{Deserialize, Serialize};

use super::model::{EpistemicStatus, MemoryKind, MemoryRecord};

const DAY_MS: u64 = 86_400_000;

/// How long things stay current, in days. Defaults are conservative and configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshnessPolicy {
    /// Facts about the present that change over time (specifications, versions, announcements).
    pub time_sensitive_days: u32,
    /// Prices and availability.
    pub price_days: u32,
    /// Everything else that was checked against a source.
    pub general_days: u32,
    /// When the question asks for the latest information ("latest", "current", "today").
    pub fresh_question_days: u32,
}

impl Default for FreshnessPolicy {
    fn default() -> Self {
        Self {
            time_sensitive_days: 30,
            price_days: 14,
            general_days: 365,
            fresh_question_days: 7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Freshness {
    /// Within its window.
    Current { age_days: u32 },
    /// Older than its window: do not treat it as current.
    Stale { age_days: u32, window_days: u32 },
    /// No date was recorded.
    Unknown,
    /// Not a statement about the world that ages (a preference, a question).
    NotApplicable,
}

impl Freshness {
    pub fn label(&self) -> String {
        match self {
            Freshness::Current { age_days } => format!("CURRENT ({age_days} d old)"),
            Freshness::Stale {
                age_days,
                window_days,
            } => {
                format!(
                    "STALE ({age_days} d old, was good for {window_days} d): do not treat as current, re-verify"
                )
            }
            Freshness::Unknown => "date unknown: do not assume it is current".to_string(),
            Freshness::NotApplicable => String::new(),
        }
    }

    pub fn needs_refresh(&self) -> bool {
        matches!(self, Freshness::Stale { .. } | Freshness::Unknown)
    }
}

/// Wording that marks a statement about money or availability.
pub fn mentions_price(text: &str) -> bool {
    let lower = text.to_lowercase();
    text.contains(['$', '€', '£'])
        || [
            "price",
            "precio",
            "cost ",
            "costs ",
            "cuesta",
            " mxn",
            " usd",
            " eur",
            "in stock",
            "discount",
            "descuento",
            "msrp",
        ]
        .iter()
        .any(|w| lower.contains(w))
}

/// Whether a claim of this category (or this wording) is about something that changes.
pub fn is_time_sensitive(category: Option<&str>, text: &str) -> bool {
    if mentions_price(text) {
        return true;
    }
    matches!(
        category,
        Some(
            "product_specification"
                | "real_world_performance"
                | "event_or_announcement"
                | "statistic"
        )
    )
}

pub fn assess(
    record: &MemoryRecord,
    now_ms: u64,
    wants_fresh: bool,
    policy: &FreshnessPolicy,
) -> Freshness {
    let ages = record.kind == MemoryKind::Evidence
        || record.time_sensitive
        || (record.kind == MemoryKind::Conclusion && record.time_sensitive);
    if !ages {
        return Freshness::NotApplicable;
    }
    // A verified historical fact does not age.
    if record.status == EpistemicStatus::Historical && !record.time_sensitive {
        return Freshness::NotApplicable;
    }
    let Some(since) = record.retrieved_ms else {
        return Freshness::Unknown;
    };
    let age_days = (now_ms.saturating_sub(since) / DAY_MS) as u32;
    let mut window = record.valid_days.unwrap_or_else(|| {
        if mentions_price(&record.text) {
            policy.price_days
        } else if record.time_sensitive {
            policy.time_sensitive_days
        } else {
            policy.general_days
        }
    });
    if wants_fresh && record.time_sensitive {
        window = window.min(policy.fresh_question_days);
    }
    if age_days <= window {
        Freshness::Current { age_days }
    } else {
        Freshness::Stale {
            age_days,
            window_days: window,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::model::{Authorization, Lifecycle, MemoryScope, Origin};

    fn evidence(
        text: &str,
        retrieved_days_ago: Option<u64>,
        time_sensitive: bool,
        status: EpistemicStatus,
    ) -> MemoryRecord {
        let now = NOW;
        MemoryRecord {
            id: "m".into(),
            kind: MemoryKind::Evidence,
            scope: MemoryScope::Session,
            session_id: None,
            project_id: None,
            origin: Origin::Evidence,
            status,
            lifecycle: Lifecycle::Active,
            text: text.into(),
            short: None,
            topic: None,
            tags: vec![],
            authorization: Authorization::AutomaticSession,
            sensitive: false,
            consultation_id: None,
            content_hash: String::new(),
            created_ms: now,
            updated_ms: now,
            retrieved_ms: retrieved_days_ago.map(|d| now - d * DAY_MS),
            published: None,
            valid_days: None,
            category: None,
            time_sensitive,
            source_url: None,
            expires_ms: None,
        }
    }

    const NOW: u64 = 10_000 * DAY_MS;
    const P: FreshnessPolicy = FreshnessPolicy {
        time_sensitive_days: 30,
        price_days: 14,
        general_days: 365,
        fresh_question_days: 7,
    };

    #[test]
    fn prices_age_faster_than_specifications_and_both_faster_than_general_claims() {
        let price = evidence(
            "The Rigol costs 3,500 MXN",
            Some(20),
            true,
            EpistemicStatus::PreviouslyVerified,
        );
        assert!(matches!(
            assess(&price, NOW, false, &P),
            Freshness::Stale {
                age_days: 20,
                window_days: 14
            }
        ));
        let spec = evidence(
            "It has 50 MHz of bandwidth",
            Some(20),
            true,
            EpistemicStatus::PreviouslyVerified,
        );
        assert!(matches!(
            assess(&spec, NOW, false, &P),
            Freshness::Current { age_days: 20 }
        ));
        let general = evidence(
            "The tool was released with a manual",
            Some(200),
            false,
            EpistemicStatus::PreviouslyVerified,
        );
        assert!(matches!(
            assess(&general, NOW, false, &P),
            Freshness::Current { .. }
        ));
        let old_general = evidence(
            "The tool was released with a manual",
            Some(400),
            false,
            EpistemicStatus::PreviouslyVerified,
        );
        assert!(matches!(
            assess(&old_general, NOW, false, &P),
            Freshness::Stale {
                window_days: 365,
                ..
            }
        ));
    }

    #[test]
    fn a_question_that_wants_the_latest_shortens_the_window_of_time_sensitive_facts_only() {
        let spec = evidence(
            "It has 50 MHz of bandwidth",
            Some(10),
            true,
            EpistemicStatus::PreviouslyVerified,
        );
        assert!(matches!(
            assess(&spec, NOW, false, &P),
            Freshness::Current { .. }
        ));
        assert!(matches!(
            assess(&spec, NOW, true, &P),
            Freshness::Stale { window_days: 7, .. }
        ));
        let general = evidence(
            "Ohm's law relates voltage and current",
            Some(10),
            false,
            EpistemicStatus::PreviouslyVerified,
        );
        assert!(matches!(
            assess(&general, NOW, true, &P),
            Freshness::Current { .. }
        ));
    }

    #[test]
    fn a_missing_date_is_unknown_never_invented_and_historical_facts_do_not_age() {
        let undated = evidence(
            "Some claim",
            None,
            true,
            EpistemicStatus::PreviouslyVerified,
        );
        assert_eq!(assess(&undated, NOW, false, &P), Freshness::Unknown);
        assert!(Freshness::Unknown.needs_refresh());
        let history = evidence(
            "The standard was published in 1987",
            Some(5000),
            false,
            EpistemicStatus::Historical,
        );
        assert_eq!(assess(&history, NOW, false, &P), Freshness::NotApplicable);
        let mut pref = evidence(
            "I prefer two channels",
            Some(1),
            false,
            EpistemicStatus::UserProvided,
        );
        pref.kind = MemoryKind::Preference;
        assert_eq!(assess(&pref, NOW, true, &P), Freshness::NotApplicable);
    }

    #[test]
    fn an_explicit_validity_overrides_the_default_window() {
        let mut r = evidence(
            "Promo ends",
            Some(3),
            true,
            EpistemicStatus::PreviouslyVerified,
        );
        r.valid_days = Some(2);
        assert!(matches!(
            assess(&r, NOW, false, &P),
            Freshness::Stale { window_days: 2, .. }
        ));
    }

    #[test]
    fn the_stale_label_says_not_to_treat_it_as_current() {
        let l = Freshness::Stale {
            age_days: 45,
            window_days: 14,
        }
        .label();
        assert!(l.contains("STALE") && l.contains("do not treat as current"));
        assert!(
            mentions_price("costs $8,000")
                && mentions_price("precio 3 500")
                && !mentions_price("50 MHz")
        );
        assert!(
            is_time_sensitive(Some("statistic"), "x")
                && is_time_sensitive(None, "price: $5")
                && !is_time_sensitive(Some("definition"), "x")
        );
    }
}
