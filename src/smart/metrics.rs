//! Run metrics computed from what actually happened: the recorded steps. Nothing is estimated and
//! no token or cost figure is invented: only the durations, sizes and counts that were observed.

use serde::{Deserialize, Serialize};

use crate::debate::Step;

/// Observable facts about one consultation's agent calls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMetrics {
    /// Agent calls made (successful or not).
    pub calls: usize,
    pub failed_calls: usize,
    /// Sum of the durations of all calls: the work the providers did, ignoring overlap.
    pub provider_ms_total: u64,
    /// Time during which at least one call was running. `wall_ms - provider_busy_ms` is spent
    /// outside any agent call: fetching pages, building prompts, waiting, finishing up.
    pub provider_busy_ms: u64,
    /// Most calls that were running at the same moment.
    pub peak_concurrency: usize,
    /// Characters of text the successful calls returned.
    pub response_chars: u64,
}

impl RunMetrics {
    pub fn from_steps(steps: &[Step]) -> Self {
        let mut edges: Vec<(u64, i32)> = Vec::with_capacity(steps.len() * 2);
        for s in steps {
            edges.push((s.offset_ms, 1));
            edges.push((s.offset_ms.saturating_add(s.duration_ms), -1));
        }
        // At the same millisecond a call that ends is processed before one that starts, so calls
        // that merely touch are not counted as overlapping.
        edges.sort_by_key(|&(t, d)| (t, d));
        let (mut running, mut peak) = (0i32, 0i32);
        let (mut busy, mut busy_since) = (0u64, 0u64);
        for (t, delta) in edges {
            if running == 0 && delta > 0 {
                busy_since = t;
            }
            running += delta;
            peak = peak.max(running);
            if running == 0 {
                busy += t.saturating_sub(busy_since);
            }
        }
        Self {
            calls: steps.len(),
            failed_calls: steps.iter().filter(|s| !s.ok).count(),
            provider_ms_total: steps.iter().map(|s| s.duration_ms).sum(),
            provider_busy_ms: busy,
            peak_concurrency: usize::try_from(peak.max(0)).unwrap_or(0),
            response_chars: steps
                .iter()
                .filter_map(|s| s.text.as_ref())
                .map(|t| t.chars().count() as u64)
                .sum(),
        }
    }

    /// Time spent outside agent calls, given the consultation's wall-clock duration.
    pub fn overhead_ms(&self, wall_ms: u64) -> u64 {
        wall_ms.saturating_sub(self.provider_busy_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debate::Role;
    use crate::types::ProviderId;

    fn step(offset: u64, duration: u64, ok: bool) -> Step {
        Step {
            phase: Role::Perspective,
            round: 0,
            agent: ProviderId::new("a"),
            offset_ms: offset,
            duration_ms: duration,
            ok,
            text: ok.then(|| "abc".to_string()),
            error: None,
            error_kind: None,
            settings: None,
            usage: None,
        }
    }

    #[test]
    fn overlapping_calls_are_counted_once_in_busy_time_and_raise_the_peak() {
        let m = RunMetrics::from_steps(&[
            step(0, 100, true),
            step(20, 100, true),
            step(300, 50, false),
        ]);
        assert_eq!(m.calls, 3);
        assert_eq!(m.failed_calls, 1);
        assert_eq!(m.provider_ms_total, 250);
        assert_eq!(m.provider_busy_ms, 170, "0..120 and 300..350");
        assert_eq!(m.peak_concurrency, 2);
        assert_eq!(m.response_chars, 6);
        assert_eq!(m.overhead_ms(400), 230);
    }

    #[test]
    fn calls_that_only_touch_do_not_overlap_and_no_calls_is_all_zero() {
        let m = RunMetrics::from_steps(&[step(0, 100, true), step(100, 100, true)]);
        assert_eq!((m.peak_concurrency, m.provider_busy_ms), (1, 200));
        assert_eq!(RunMetrics::from_steps(&[]), RunMetrics::default());
        assert_eq!(RunMetrics::default().overhead_ms(5), 5);
    }
}
