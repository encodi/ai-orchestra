//! Lightweight local performance profiles: what was *observed* about each provider's calls.
//!
//! Only facts that the program measured itself are kept: how many calls, how many succeeded or
//! failed and how (timeout, usage limit, authentication, other), how long successful calls took (an
//! exponentially weighted moving average per kind of call, so a verification and a long synthesis
//! are not mixed), how much text came back, and the token counts that a CLI itself reported. Nothing
//! is estimated: no token counts or costs are invented, startup overhead is not separable from the
//! rest of a call and is therefore not recorded, and latency is never read as a measure of how good
//! an answer is.
//!
//! The history is bounded (a handful of numbers per provider and kind of call) and perishable:
//! a profile is only used while it is recent and based on enough calls. Without such a profile
//! the engine falls back to the registry order, as if there were no history at all.
//!
//! Persistence is one small JSON file written atomically with owner-only permissions. It holds
//! no prompts, answers, argument lists, environment values or credentials.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::debate::{Role, Step};

const SCHEMA: u32 = 1;
/// Providers kept in the file; a registry never has more than a few.
const MAX_PROVIDERS: usize = 16;

/// Tunable, with documented defaults (see `[smart]` in the configuration).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileSettings {
    /// Weight of the newest sample in the moving average (0 to 1].
    pub ewma_alpha: f64,
    /// A profile older than this many days is not used.
    pub max_age_days: u32,
    /// Successful calls needed before the latency is used.
    pub min_samples: u32,
    /// Consecutive failures after which a provider counts as degraded...
    pub degraded_after_failures: u32,
    /// ...for this many hours after the last failure.
    pub degraded_window_hours: u32,
}

impl Default for ProfileSettings {
    fn default() -> Self {
        Self {
            ewma_alpha: 0.3,
            max_age_days: 14,
            min_samples: 3,
            degraded_after_failures: 3,
            degraded_window_hours: 24,
        }
    }
}

/// An exponentially weighted moving average.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ewma {
    pub mean: f64,
    pub samples: u32,
}

impl Ewma {
    pub fn update(&mut self, x: f64, alpha: f64) {
        self.mean = if self.samples == 0 {
            x
        } else {
            alpha * x + (1.0 - alpha) * self.mean
        };
        self.samples = self.samples.saturating_add(1);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub invocations: u64,
    pub successes: u64,
    pub failures: u64,
    pub timeouts: u64,
    pub quota_errors: u64,
    pub auth_errors: u64,
    /// Latency of successful calls in milliseconds, per kind of call (`perspective`, `review`...).
    #[serde(default)]
    pub latency_ms: BTreeMap<String, Ewma>,
    /// Characters per successful reply.
    #[serde(default)]
    pub response_chars: Ewma,
    /// Token counts, only from calls whose CLI reported them.
    pub calls_reporting_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub last_updated_ms: u64,
    pub last_success_ms: Option<u64>,
    pub last_failure_ms: Option<u64>,
    pub last_failure_kind: Option<String>,
    /// Failures in a row (a success resets it).
    pub failure_streak: u32,
    /// Whether the provider was usable the last time the program looked, and its web search
    /// support label from the capability matrix.
    pub last_available: Option<bool>,
    pub web_search: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileFile {
    pub schema: u32,
    pub providers: BTreeMap<String, ProviderProfile>,
}

impl Default for ProfileFile {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            providers: BTreeMap::new(),
        }
    }
}

/// One finished call, reduced to what is worth remembering.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub provider: String,
    pub role: Role,
    pub ok: bool,
    pub error_kind: Option<String>,
    pub duration_ms: u64,
    pub response_chars: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl Observation {
    pub fn from_step(s: &Step) -> Self {
        Self {
            provider: s.agent.to_string(),
            role: s.phase,
            ok: s.ok,
            error_kind: s.error_kind.clone(),
            duration_ms: s.duration_ms,
            response_chars: s.text.as_ref().map_or(0, |t| t.chars().count() as u64),
            input_tokens: s.usage.as_ref().and_then(|u| u.input_tokens),
            output_tokens: s.usage.as_ref().and_then(|u| u.output_tokens),
        }
    }
}

/// The kind of call a latency belongs to.
pub fn role_key(role: Role) -> &'static str {
    match role {
        Role::Planning => "planning",
        Role::Verification => "verification",
        Role::Perspective | Role::Proposal => "answer",
        Role::Review | Role::CrossReview => "review",
        Role::Synthesis => "synthesis",
    }
}

/// Calls that say nothing about the provider: the user cancelled, we declined to start, the CLI
/// is not installed (availability is looked at live), or the orchestration was nested.
fn counts_for_profile(kind: Option<&str>) -> bool {
    !matches!(kind, Some("cancelled" | "budget" | "missing" | "nested"))
}

impl ProviderProfile {
    pub(crate) fn observe(&mut self, o: &Observation, now_ms: u64, alpha: f64) {
        if !o.ok && !counts_for_profile(o.error_kind.as_deref()) {
            return;
        }
        self.invocations += 1;
        self.last_updated_ms = now_ms;
        if o.ok {
            self.successes += 1;
            self.failure_streak = 0;
            self.last_success_ms = Some(now_ms);
            self.latency_ms
                .entry(role_key(o.role).to_string())
                .or_default()
                .update(o.duration_ms as f64, alpha);
            self.response_chars.update(o.response_chars as f64, alpha);
            if o.input_tokens.is_some() || o.output_tokens.is_some() {
                self.calls_reporting_tokens += 1;
                self.input_tokens = self
                    .input_tokens
                    .saturating_add(o.input_tokens.unwrap_or(0));
                self.output_tokens = self
                    .output_tokens
                    .saturating_add(o.output_tokens.unwrap_or(0));
            }
        } else {
            self.failures += 1;
            self.failure_streak = self.failure_streak.saturating_add(1);
            self.last_failure_ms = Some(now_ms);
            let kind = o.error_kind.clone().unwrap_or_else(|| "error".to_string());
            match kind.as_str() {
                "timeout" => self.timeouts += 1,
                "quota" => self.quota_errors += 1,
                "auth" => self.auth_errors += 1,
                _ => {}
            }
            self.last_failure_kind = Some(kind);
        }
    }
}

/// How a provider looks to the engine right now.
#[derive(Debug, Clone, PartialEq)]
pub enum Health {
    /// No usable history (none, too old, or too few calls).
    Unknown,
    Good,
    /// Several failures in a row, recently. Deprioritised, never excluded.
    Degraded(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderView {
    pub health: Health,
    /// Typical latency of its answers, only when the history is recent and large enough.
    pub answer_latency_ms: Option<u64>,
    pub note: String,
}

/// The history as of one moment, cheap to query and independent of the file.
#[derive(Debug, Clone, Default)]
pub struct ProfileSnapshot {
    file: ProfileFile,
    settings: ProfileSettings,
    now_ms: u64,
}

impl ProfileSnapshot {
    pub fn new(file: ProfileFile, settings: ProfileSettings, now_ms: u64) -> Self {
        Self {
            file,
            settings,
            now_ms,
        }
    }

    pub fn empty(settings: ProfileSettings, now_ms: u64) -> Self {
        Self::new(ProfileFile::default(), settings, now_ms)
    }

    pub fn is_empty(&self) -> bool {
        self.file.providers.is_empty()
    }

    pub fn profile(&self, id: &str) -> Option<&ProviderProfile> {
        self.file.providers.get(id)
    }

    pub fn view(&self, id: &str) -> ProviderView {
        let unknown = |why: &str| ProviderView {
            health: Health::Unknown,
            answer_latency_ms: None,
            note: format!("{id}: {why}"),
        };
        let Some(p) = self.file.providers.get(id) else {
            return unknown("no history yet, registry order used");
        };
        let age_ms = self.now_ms.saturating_sub(p.last_updated_ms);
        let day_ms = 24 * 3600 * 1000u64;
        if age_ms > u64::from(self.settings.max_age_days) * day_ms {
            return unknown(&format!(
                "history is older than {} days and is ignored",
                self.settings.max_age_days
            ));
        }
        let degraded = p.failure_streak >= self.settings.degraded_after_failures
            && p.last_failure_ms.is_some_and(|t| {
                self.now_ms.saturating_sub(t)
                    <= u64::from(self.settings.degraded_window_hours) * 3600 * 1000
            });
        if degraded {
            return ProviderView {
                health: Health::Degraded(format!(
                    "{} failures in a row, the last one `{}`",
                    p.failure_streak,
                    p.last_failure_kind.as_deref().unwrap_or("error")
                )),
                answer_latency_ms: None,
                note: format!(
                    "{id}: {} failures in a row recently, so it is tried last",
                    p.failure_streak
                ),
            };
        }
        // Answers: independent perspectives and proposals; fall back to any kind of call.
        let answers = p.latency_ms.get("answer");
        let pooled = answers.filter(|e| e.samples >= self.settings.min_samples);
        match pooled {
            Some(e) => ProviderView {
                health: Health::Good,
                answer_latency_ms: Some(e.mean.round().max(0.0) as u64),
                note: format!(
                    "{id}: answers took about {} ms over {} recent call(s)",
                    e.mean.round().max(0.0) as u64,
                    e.samples
                ),
            },
            None => ProviderView {
                health: Health::Good,
                answer_latency_ms: None,
                note: format!(
                    "{id}: fewer than {} recorded answers, latency not used",
                    self.settings.min_samples
                ),
            },
        }
    }
}

/// The profile file on disk. All access is best-effort: a damaged or unwritable file is reported
/// to the caller and never stops a consultation.
#[derive(Debug)]
pub struct ProfileStore {
    path: PathBuf,
    settings: ProfileSettings,
    lock: Mutex<()>,
}

impl ProfileStore {
    pub fn new(path: PathBuf, settings: ProfileSettings) -> Self {
        Self {
            path,
            settings,
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn settings(&self) -> ProfileSettings {
        self.settings
    }

    fn read(&self) -> Result<ProfileFile, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                let file: ProfileFile = serde_json::from_str(&text).map_err(|e| {
                    format!("the profile file is not valid JSON ({e}); ignoring it")
                })?;
                if file.schema != SCHEMA {
                    return Err(format!(
                        "the profile file has schema {}, expected {SCHEMA}; ignoring it",
                        file.schema
                    ));
                }
                Ok(file)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProfileFile::default()),
            Err(e) => Err(format!("cannot read the profile file: {e}")),
        }
    }

    /// The history as of `now_ms`. An unreadable file reads as no history.
    pub fn snapshot(&self, now_ms: u64) -> (ProfileSnapshot, Option<String>) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        match self.read() {
            Ok(file) => (ProfileSnapshot::new(file, self.settings, now_ms), None),
            Err(why) => (ProfileSnapshot::empty(self.settings, now_ms), Some(why)),
        }
    }

    /// Adds the observations of one consultation. `availability` is `(provider, usable, web
    /// search label)` as seen when it ran.
    pub fn record(
        &self,
        observations: &[Observation],
        availability: &[(String, bool, String)],
        now_ms: u64,
    ) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // A damaged file is replaced rather than left to block the history for good.
        let mut file = self.read().unwrap_or_default();
        for o in observations {
            if file.providers.len() >= MAX_PROVIDERS && !file.providers.contains_key(&o.provider) {
                continue;
            }
            file.providers
                .entry(o.provider.clone())
                .or_default()
                .observe(o, now_ms, self.settings.ewma_alpha);
        }
        for (id, usable, web) in availability {
            if file.providers.len() >= MAX_PROVIDERS && !file.providers.contains_key(id) {
                continue;
            }
            let p = file.providers.entry(id.clone()).or_default();
            p.last_available = Some(*usable);
            p.web_search = Some(web.clone());
        }
        self.write(&file)
    }

    fn write(&self, file: &ProfileFile) -> Result<(), String> {
        use std::io::Write as _;
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts
                .open(&tmp)
                .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
            f.write_all(text.as_bytes())
                .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| format!("cannot replace {}: {e}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 3600 * 1000;

    fn ok(provider: &str, ms: u64) -> Observation {
        Observation {
            provider: provider.into(),
            role: Role::Perspective,
            ok: true,
            error_kind: None,
            duration_ms: ms,
            response_chars: 100,
            input_tokens: None,
            output_tokens: None,
        }
    }

    fn fail(provider: &str, kind: &str) -> Observation {
        Observation {
            provider: provider.into(),
            role: Role::Perspective,
            ok: false,
            error_kind: Some(kind.into()),
            duration_ms: 5,
            response_chars: 0,
            input_tokens: None,
            output_tokens: None,
        }
    }

    fn store() -> (tempfile::TempDir, ProfileStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = ProfileStore::new(
            dir.path().join("p").join("profiles.json"),
            ProfileSettings::default(),
        );
        (dir, s)
    }

    #[test]
    fn the_moving_average_follows_recent_calls_without_forgetting_the_past_at_once() {
        let mut e = Ewma::default();
        e.update(100.0, 0.3);
        assert_eq!(e.mean, 100.0);
        e.update(200.0, 0.3);
        assert!((e.mean - 130.0).abs() < 1e-9);
        assert_eq!(e.samples, 2);
    }

    #[test]
    fn calls_are_counted_by_outcome_and_only_successes_feed_the_latency() {
        let mut p = ProviderProfile::default();
        for o in [
            ok("a", 100),
            fail("a", "timeout"),
            fail("a", "quota"),
            fail("a", "auth"),
            fail("a", "error"),
            ok("a", 300),
        ] {
            p.observe(&o, 1_000, 0.5);
        }
        assert_eq!((p.invocations, p.successes, p.failures), (6, 2, 4));
        assert_eq!((p.timeouts, p.quota_errors, p.auth_errors), (1, 1, 1));
        assert_eq!(
            p.latency_ms["answer"].samples, 2,
            "failures never feed the latency"
        );
        assert!((p.latency_ms["answer"].mean - 200.0).abs() < 1e-9);
        assert_eq!(p.failure_streak, 0, "a success resets the streak");
        assert_eq!(p.last_failure_kind.as_deref(), Some("error"));
    }

    #[test]
    fn cancellations_and_our_own_budget_stops_say_nothing_about_the_provider() {
        let mut p = ProviderProfile::default();
        for kind in ["cancelled", "budget", "missing", "nested"] {
            p.observe(&fail("a", kind), 1, 0.3);
        }
        assert_eq!(p, ProviderProfile::default());
    }

    #[test]
    fn token_counts_are_summed_only_from_calls_that_reported_them() {
        let mut p = ProviderProfile::default();
        let mut with = ok("a", 10);
        with.input_tokens = Some(7);
        with.output_tokens = Some(3);
        p.observe(&with, 1, 0.3);
        p.observe(&ok("a", 10), 1, 0.3);
        assert_eq!(
            (p.calls_reporting_tokens, p.input_tokens, p.output_tokens),
            (1, 7, 3)
        );
        assert_eq!(p.invocations, 2);
    }

    #[test]
    fn a_provider_without_enough_recent_history_has_unknown_health_and_no_latency() {
        let mut file = ProfileFile::default();
        let mut p = ProviderProfile::default();
        p.observe(&ok("a", 100), 10 * DAY, 0.3);
        file.providers.insert("a".into(), p);
        let snap = ProfileSnapshot::new(file.clone(), ProfileSettings::default(), 10 * DAY);
        let v = snap.view("a");
        assert_eq!(v.health, Health::Good);
        assert_eq!(v.answer_latency_ms, None, "one sample is not enough");
        assert_eq!(snap.view("zzz").health, Health::Unknown);
        // Old history is ignored altogether.
        let old = ProfileSnapshot::new(file, ProfileSettings::default(), 40 * DAY);
        assert_eq!(old.view("a").health, Health::Unknown);
        assert!(old.view("a").note.contains("older than 14 days"));
    }

    #[test]
    fn enough_recent_calls_give_a_latency_and_a_failure_streak_degrades_for_a_day() {
        let mut p = ProviderProfile::default();
        for ms in [100, 120, 140] {
            p.observe(&ok("a", ms), 5 * DAY, 0.5);
        }
        let mut file = ProfileFile::default();
        file.providers.insert("a".into(), p.clone());
        let snap = ProfileSnapshot::new(file, ProfileSettings::default(), 5 * DAY);
        // 100, then 0.5 * 120 + 0.5 * 100 = 110, then 0.5 * 140 + 0.5 * 110 = 125
        assert_eq!(snap.view("a").answer_latency_ms, Some(125));
        for _ in 0..3 {
            p.observe(&fail("a", "quota"), 5 * DAY + 1000, 0.5);
        }
        let mut file = ProfileFile::default();
        file.providers.insert("a".into(), p);
        let snap = ProfileSnapshot::new(file.clone(), ProfileSettings::default(), 5 * DAY + 2000);
        assert!(matches!(snap.view("a").health, Health::Degraded(_)));
        // Two days later the streak no longer counts against it.
        let later = ProfileSnapshot::new(file, ProfileSettings::default(), 7 * DAY);
        assert!(!matches!(later.view("a").health, Health::Degraded(_)));
    }

    #[test]
    fn the_store_round_trips_a_bounded_file_and_survives_damage() {
        let (_dir, s) = store();
        let (snap, warning) = s.snapshot(1);
        assert!(
            snap.is_empty() && warning.is_none(),
            "a missing file is just no history"
        );
        s.record(
            &[ok("a", 50), fail("b", "timeout")],
            &[("a".into(), true, "documented".into())],
            5,
        )
        .unwrap();
        let (snap, _) = s.snapshot(6);
        assert_eq!(snap.profile("a").unwrap().successes, 1);
        assert_eq!(
            snap.profile("a").unwrap().web_search.as_deref(),
            Some("documented")
        );
        assert_eq!(snap.profile("b").unwrap().timeouts, 1);
        // Damage: ignored on read (with a warning), replaced on the next write.
        std::fs::write(s.path(), "{ not json").unwrap();
        let (snap, warning) = s.snapshot(7);
        assert!(snap.is_empty());
        assert!(warning.unwrap().contains("not valid JSON"));
        s.record(&[ok("a", 60)], &[], 8).unwrap();
        assert_eq!(s.snapshot(9).0.profile("a").unwrap().successes, 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(s.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "owner-only");
        }
        let text = std::fs::read_to_string(s.path()).unwrap();
        assert!(!text.contains("prompt"), "no prompt text is ever stored");
    }

    #[test]
    fn the_number_of_stored_providers_is_bounded() {
        let (_dir, s) = store();
        let many: Vec<Observation> = (0..40).map(|i| ok(&format!("p{i}"), 1)).collect();
        s.record(&many, &[], 1).unwrap();
        assert_eq!(s.snapshot(2).0.file.providers.len(), MAX_PROVIDERS);
    }
}
