//! Descriptive statistics and seeded uncertainty estimates.
//!
//! Small and dependency-free. Everything is deterministic: the same data and seed give the same
//! interval. Intervals are reported only from [`MIN_FOR_INTERVAL`] observations, because below that a
//! bootstrap interval looks like precision the data cannot give; under it the counts are shown as
//! counts and the report says the sample is too small to compare. Nothing here says anything about
//! causality, and a preference rate is a preference rate, not an accuracy.

use serde::{Deserialize, Serialize};

/// Fewest observations for which an interval is reported at all.
pub const MIN_FOR_INTERVAL: usize = 10;
pub const BOOTSTRAP_RESAMPLES: usize = 2000;

/// A small, fixed PRNG (SplitMix64). Same seed, same sequence, on every platform.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`), without modulo bias that matters at these sizes.
    pub fn below(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        ((u128::from(self.next_u64()) * n as u128) >> 64) as usize
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

/// A stable 64-bit hash of text, for deriving per-item seeds.
pub fn hash64(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    let idx = (p * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Percentile-bootstrap interval of the mean of `values` (paired observations already reduced to one
/// number each). `None` below [`MIN_FOR_INTERVAL`].
pub fn bootstrap_mean_ci(values: &[f64], seed: u64, level: f64) -> Option<(f64, f64)> {
    if values.len() < MIN_FOR_INTERVAL {
        return None;
    }
    let mut rng = SplitMix64::new(seed);
    let n = values.len();
    let mut means: Vec<f64> = (0..BOOTSTRAP_RESAMPLES)
        .map(|_| (0..n).map(|_| values[rng.below(n)]).sum::<f64>() / n as f64)
        .collect();
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let tail = (1.0 - level) / 2.0;
    Some((
        percentile_sorted(&means, tail),
        percentile_sorted(&means, 1.0 - tail),
    ))
}

/// A proportion with its Wilson 95% interval (reported only from [`MIN_FOR_INTERVAL`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    pub k: usize,
    pub n: usize,
    pub rate: Option<f64>,
    pub interval: Option<(f64, f64)>,
}

impl Rate {
    pub fn new(k: usize, n: usize) -> Rate {
        let rate = (n > 0).then(|| k as f64 / n as f64);
        let interval = (n >= MIN_FOR_INTERVAL).then(|| {
            let (z, nf, p) = (1.96_f64, n as f64, k as f64 / n as f64);
            let denom = 1.0 + z * z / nf;
            let centre = (p + z * z / (2.0 * nf)) / denom;
            let half = z * ((p * (1.0 - p) / nf + z * z / (4.0 * nf * nf)).sqrt()) / denom;
            ((centre - half).max(0.0), (centre + half).min(1.0))
        });
        Rate {
            k,
            n,
            rate,
            interval,
        }
    }

    pub fn render(&self) -> String {
        match (self.rate, self.interval) {
            (None, _) => "n/a (no observations)".to_string(),
            (Some(r), Some((lo, hi))) => format!(
                "{}/{} = {:.0}% (95% interval {:.0}%-{:.0}%)",
                self.k,
                self.n,
                r * 100.0,
                lo * 100.0,
                hi * 100.0
            ),
            (Some(r), None) => format!(
                "{}/{} = {:.0}% (too few to give an interval)",
                self.k,
                self.n,
                r * 100.0
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preference {
    A,
    B,
    Tie,
    Insufficient,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairwiseSummary {
    pub judged: usize,
    pub wins_a: usize,
    pub wins_b: usize,
    pub ties: usize,
    pub insufficient: usize,
    /// (wins of A + half the ties) / (items with a decision); `None` with no decided item.
    pub preference_a: Option<f64>,
    pub interval: Option<(f64, f64)>,
    pub note: String,
}

/// Summarises paired judgments of A against B. Paired: one observation per item; ties count half;
/// "insufficient information" is not a vote and is reported separately.
pub fn pairwise_summary(outcomes: &[Preference], seed: u64) -> PairwiseSummary {
    let count = |p: Preference| outcomes.iter().filter(|o| **o == p).count();
    let (wins_a, wins_b, ties, insufficient) = (
        count(Preference::A),
        count(Preference::B),
        count(Preference::Tie),
        count(Preference::Insufficient),
    );
    let decided = wins_a + wins_b + ties;
    let values: Vec<f64> = outcomes
        .iter()
        .filter_map(|o| match o {
            Preference::A => Some(1.0),
            Preference::B => Some(0.0),
            Preference::Tie => Some(0.5),
            Preference::Insufficient => None,
        })
        .collect();
    let preference_a = mean(&values);
    let interval = bootstrap_mean_ci(&values, seed, 0.95);
    let note = if decided == 0 {
        "no decided comparison yet".to_string()
    } else if decided < MIN_FOR_INTERVAL {
        format!(
            "only {decided} decided comparison(s): too few to report an interval or to call a difference; read the counts, not the rate"
        )
    } else if interval.is_some_and(|(lo, hi)| lo <= 0.5 && hi >= 0.5) {
        "the interval includes 50%: this sample does not separate the two".to_string()
    } else {
        "the interval excludes 50% for this sample (a preference, not a measure of accuracy, and not causal)".to_string()
    };
    PairwiseSummary {
        judged: outcomes.len(),
        wins_a,
        wins_b,
        ties,
        insufficient,
        preference_a,
        interval,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_is_reproducible_and_shuffles_every_element() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        assert_eq!(
            (0..5).map(|_| a.next_u64()).collect::<Vec<_>>(),
            (0..5).map(|_| b.next_u64()).collect::<Vec<_>>()
        );
        assert_ne!(SplitMix64::new(1).next_u64(), SplitMix64::new(2).next_u64());
        let mut v: Vec<u32> = (0..20).collect();
        SplitMix64::new(7).shuffle(&mut v);
        let mut sorted = v.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>());
        assert_ne!(v, (0..20).collect::<Vec<_>>());
        let mut r = SplitMix64::new(3);
        assert!((0..1000).all(|_| r.below(7) < 7));
        assert_eq!(hash64("abc"), hash64("abc"));
        assert_ne!(hash64("abc"), hash64("abd"));
    }

    #[test]
    fn median_and_mean_handle_even_odd_and_empty() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&[]), None);
        assert_eq!(mean(&[1.0, 2.0, 6.0]), Some(3.0));
        assert_eq!(mean(&[]), None);
    }

    #[test]
    fn the_bootstrap_interval_is_reproducible_bounded_and_withheld_for_small_samples() {
        let v: Vec<f64> = (0..20).map(|i| if i < 14 { 1.0 } else { 0.0 }).collect();
        let a = bootstrap_mean_ci(&v, 99, 0.95).unwrap();
        assert_eq!(
            a,
            bootstrap_mean_ci(&v, 99, 0.95).unwrap(),
            "same seed, same interval"
        );
        assert!(
            a.0 <= 0.7 && 0.7 <= a.1 && a.0 >= 0.0 && a.1 <= 1.0,
            "{a:?}"
        );
        assert!(a.0 > 0.4 && a.1 < 0.95, "{a:?}");
        assert_eq!(bootstrap_mean_ci(&v[..9], 99, 0.95), None);
        // A different seed may move the ends a little, never the sample mean.
        let b = bootstrap_mean_ci(&v, 100, 0.95).unwrap();
        assert!((a.0 - b.0).abs() < 0.1 && (a.1 - b.1).abs() < 0.1);
    }

    #[test]
    fn rates_carry_a_wilson_interval_only_from_ten_observations() {
        assert_eq!(Rate::new(0, 0).rate, None);
        let small = Rate::new(3, 4);
        assert_eq!(small.rate, Some(0.75));
        assert!(small.interval.is_none() && small.render().contains("too few"));
        let big = Rate::new(18, 20);
        let (lo, hi) = big.interval.unwrap();
        assert!(lo > 0.65 && lo < 0.9 && hi > 0.9 && hi <= 1.0, "{lo} {hi}");
        let all = Rate::new(10, 10).interval.unwrap();
        assert_eq!(all.1, 1.0);
    }

    #[test]
    fn pairwise_ties_count_half_insufficient_is_not_a_vote_and_small_samples_say_so() {
        use Preference::*;
        let s = pairwise_summary(&[A, A, B, Tie, Insufficient], 1);
        assert_eq!(
            (s.wins_a, s.wins_b, s.ties, s.insufficient, s.judged),
            (2, 1, 1, 1, 5)
        );
        assert_eq!(s.preference_a, Some((1.0 + 1.0 + 0.0 + 0.5) / 4.0));
        assert!(s.interval.is_none() && s.note.contains("too few"));
        let none = pairwise_summary(&[Insufficient], 1);
        assert_eq!(none.preference_a, None);
        assert!(none.note.contains("no decided"));
        let many: Vec<Preference> = (0..24).map(|i| if i % 2 == 0 { A } else { B }).collect();
        let s = pairwise_summary(&many, 5);
        assert!(
            s.interval.is_some() && s.note.contains("includes 50%"),
            "{}",
            s.note
        );
        let lopsided: Vec<Preference> = (0..24).map(|i| if i < 22 { A } else { B }).collect();
        assert!(pairwise_summary(&lopsided, 5).note.contains("excludes 50%"));
    }
}
