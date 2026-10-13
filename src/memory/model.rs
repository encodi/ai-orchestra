//! The vocabulary of memory: scopes, kinds, who said it, how much it can be trusted, and the
//! records stored in the database.
//!
//! Two ideas run through everything here. First, *remembering is not knowing*: a record keeps its
//! origin (the user, an agent, a retrieved source) and an epistemic status, and a conclusion that
//! appeared in a consultation is never promoted to a verified fact by being stored. Second, *nothing
//! is rewritten silently*: a correction is a new record that supersedes the old one, and the old one
//! stays readable.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{OrchestraError, Result};

/// Where a stored record lives, and therefore who may see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// One AI Orchestra session of one project. Written automatically from consultations.
    Session,
    /// One project. Written only on the user's explicit authorization.
    Project,
    /// Every project. Written only on the user's explicit authorization.
    Global,
}

impl MemoryScope {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryScope::Session => "session",
            MemoryScope::Project => "project",
            MemoryScope::Global => "global",
        }
    }
}

impl std::fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MemoryScope {
    type Err = OrchestraError;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "session" => Ok(Self::Session),
            "project" => Ok(Self::Project),
            "global" => Ok(Self::Global),
            other => Err(OrchestraError::Config(format!(
                "unknown memory scope `{other}` (use session, project or global)"
            ))),
        }
    }
}

/// What a consultation may read from memory (`--memory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMode {
    /// The default: the project's current session and its approved project memories.
    Auto,
    /// No memory is read and none is written.
    Off,
    /// Only the current session.
    Session,
    /// The current session and approved memories of this project.
    Project,
    /// Also approved memories that apply to every project.
    Global,
}

impl MemoryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryMode::Auto => "auto",
            MemoryMode::Off => "off",
            MemoryMode::Session => "session",
            MemoryMode::Project => "project",
            MemoryMode::Global => "global",
        }
    }

    /// Whether records of `scope` may be read in this mode. `global_in_auto` is the configuration
    /// switch that lets `auto` include global memories (off by default).
    pub fn reads(self, scope: MemoryScope, global_in_auto: bool) -> bool {
        match (self, scope) {
            (MemoryMode::Off, _) => false,
            (_, MemoryScope::Session) => true,
            (MemoryMode::Session, _) => false,
            (MemoryMode::Auto | MemoryMode::Project, MemoryScope::Project) => true,
            (MemoryMode::Auto, MemoryScope::Global) => global_in_auto,
            (MemoryMode::Project, MemoryScope::Global) => false,
            (MemoryMode::Global, _) => true,
        }
    }
}

impl std::fmt::Display for MemoryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MemoryMode {
    type Err = OrchestraError;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "off" => Ok(Self::Off),
            "session" => Ok(Self::Session),
            "project" => Ok(Self::Project),
            "global" => Ok(Self::Global),
            other => Err(OrchestraError::Config(format!(
                "unknown memory mode `{other}` (use auto, off, session, project or global)"
            ))),
        }
    }
}

/// `--memory`: nothing means "the configured default".
pub fn parse_memory_arg(value: Option<&str>) -> Result<Option<MemoryMode>> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) => v.parse().map(Some),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// A question the user asked.
    Question,
    /// The condensed result of a consultation (what the panel concluded).
    Conclusion,
    /// One agent's own conclusion. Stored for the audit trail, never sent to another agent.
    Finding,
    /// A disagreement or limitation a consultation left open.
    Unresolved,
    /// A claim that was checked against sources, with its status and where it came from.
    Evidence,
    Preference,
    Decision,
    Requirement,
    Interest,
    Note,
}

impl MemoryKind {
    pub const ALL: [MemoryKind; 10] = [
        MemoryKind::Question,
        MemoryKind::Conclusion,
        MemoryKind::Finding,
        MemoryKind::Unresolved,
        MemoryKind::Evidence,
        MemoryKind::Preference,
        MemoryKind::Decision,
        MemoryKind::Requirement,
        MemoryKind::Interest,
        MemoryKind::Note,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MemoryKind::Question => "question",
            MemoryKind::Conclusion => "conclusion",
            MemoryKind::Finding => "finding",
            MemoryKind::Unresolved => "unresolved",
            MemoryKind::Evidence => "evidence",
            MemoryKind::Preference => "preference",
            MemoryKind::Decision => "decision",
            MemoryKind::Requirement => "requirement",
            MemoryKind::Interest => "interest",
            MemoryKind::Note => "note",
        }
    }

    /// Kinds a user can add by hand.
    pub fn user_addable(self) -> bool {
        matches!(
            self,
            MemoryKind::Preference
                | MemoryKind::Decision
                | MemoryKind::Requirement
                | MemoryKind::Interest
                | MemoryKind::Note
        )
    }

    /// Kinds that state what the user wants, so a newer one replaces an older one on the same topic.
    pub fn is_user_intent(self) -> bool {
        matches!(
            self,
            MemoryKind::Preference | MemoryKind::Requirement | MemoryKind::Decision
        )
    }
}

impl std::fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MemoryKind {
    type Err = OrchestraError;

    fn from_str(s: &str) -> Result<Self> {
        let wanted = s.trim().to_ascii_lowercase();
        MemoryKind::ALL
            .into_iter()
            .find(|k| k.as_str() == wanted)
            .ok_or_else(|| {
                OrchestraError::Config(format!(
                    "unknown memory kind `{s}` (use {})",
                    MemoryKind::ALL.map(|k| k.as_str()).join(", ")
                ))
            })
    }
}

/// Who a record came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    User,
    /// The final answer of a consultation: what the panel concluded together.
    ConsultationFinal,
    /// One agent's own words.
    Agent(String),
    /// A claim checked against retrieved sources.
    Evidence,
    Imported,
    System,
}

impl Origin {
    pub fn as_string(&self) -> String {
        match self {
            Origin::User => "user".to_string(),
            Origin::ConsultationFinal => "consultation_final".to_string(),
            Origin::Agent(a) => format!("agent:{a}"),
            Origin::Evidence => "evidence".to_string(),
            Origin::Imported => "imported".to_string(),
            Origin::System => "system".to_string(),
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "user" => Origin::User,
            "consultation_final" => Origin::ConsultationFinal,
            "evidence" => Origin::Evidence,
            "imported" => Origin::Imported,
            "system" => Origin::System,
            other => match other.strip_prefix("agent:") {
                Some(a) => Origin::Agent(a.to_string()),
                None => Origin::System,
            },
        }
    }

    pub fn is_agent(&self) -> bool {
        matches!(self, Origin::Agent(_))
    }
}

impl Serialize for Origin {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_string())
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Origin::parse(&s))
    }
}

/// How far a remembered statement can be trusted. Remembering never raises it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicStatus {
    /// Something the user said about themselves or their wishes.
    UserProvided,
    /// An agent's interpretation or recommendation: advice, not a fact.
    AgentInterpretation,
    /// A claim that was checked against retrieved sources and supported when it was checked.
    PreviouslyVerified,
    /// A claim nobody checked.
    Unverified,
    /// A claim the sources contradicted when it was checked.
    Contradicted,
    /// A verified fact about the past that does not expire.
    Historical,
}

impl EpistemicStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EpistemicStatus::UserProvided => "user_provided",
            EpistemicStatus::AgentInterpretation => "agent_interpretation",
            EpistemicStatus::PreviouslyVerified => "previously_verified",
            EpistemicStatus::Unverified => "unverified",
            EpistemicStatus::Contradicted => "contradicted",
            EpistemicStatus::Historical => "historical",
        }
    }

    /// The label shown to the agents next to the statement.
    pub fn label(self) -> &'static str {
        match self {
            EpistemicStatus::UserProvided => "stated by the user",
            EpistemicStatus::AgentInterpretation => "AI-generated, not verified",
            EpistemicStatus::PreviouslyVerified => "was verified when checked; not re-checked now",
            EpistemicStatus::Unverified => "unverified",
            EpistemicStatus::Contradicted => "CONTRADICTED by sources when checked",
            EpistemicStatus::Historical => "historical fact, verified when checked",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "user_provided" => Self::UserProvided,
            "previously_verified" => Self::PreviouslyVerified,
            "unverified" => Self::Unverified,
            "contradicted" => Self::Contradicted,
            "historical" => Self::Historical,
            _ => Self::AgentInterpretation,
        }
    }

    /// Used to break ties and to decide which of two duplicates to keep.
    pub fn rank(self) -> u8 {
        match self {
            EpistemicStatus::UserProvided => 5,
            EpistemicStatus::PreviouslyVerified | EpistemicStatus::Historical => 4,
            EpistemicStatus::Contradicted => 3,
            EpistemicStatus::Unverified => 2,
            EpistemicStatus::AgentInterpretation => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Active,
    /// Replaced by a newer record. Kept, readable, and left out of retrieval.
    Superseded,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Active => "active",
            Lifecycle::Superseded => "superseded",
        }
    }
}

/// How a record came to be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorization {
    /// The user asked for it (`memory add`, a corrected record, an MCP save the user approved).
    ExplicitUser,
    /// Written automatically as session memory of a consultation. Never persistent.
    AutomaticSession,
    /// Brought in from earlier saved consultations by an explicit import.
    Imported,
}

impl Authorization {
    pub fn as_str(self) -> &'static str {
        match self {
            Authorization::ExplicitUser => "explicit_user",
            Authorization::AutomaticSession => "automatic_session",
            Authorization::Imported => "imported",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "explicit_user" => Self::ExplicitUser,
            "imported" => Self::Imported,
            _ => Self::AutomaticSession,
        }
    }
}

/// How a link between two records reads: `from` <kind> `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// `from` replaces `to`.
    Supersedes,
    /// `from` and `to` disagree and neither replaced the other.
    Contradicts,
    /// `from` was condensed from `to`.
    DerivedFrom,
    /// `from` is evidence for `to`.
    EvidenceFor,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkKind::Supersedes => "supersedes",
            LinkKind::Contradicts => "contradicts",
            LinkKind::DerivedFrom => "derived_from",
            LinkKind::EvidenceFor => "evidence_for",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "supersedes" => Some(Self::Supersedes),
            "contradicts" => Some(Self::Contradicts),
            "derived_from" => Some(Self::DerivedFrom),
            "evidence_for" => Some(Self::EvidenceFor),
            _ => None,
        }
    }
}

/// A record as it is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub kind: MemoryKind,
    pub scope: MemoryScope,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
    pub origin: Origin,
    pub status: EpistemicStatus,
    pub lifecycle: Lifecycle,
    pub text: String,
    /// A one-line form that keeps the label and the qualification, for tight budgets.
    pub short: Option<String>,
    pub topic: Option<String>,
    pub tags: Vec<String>,
    pub authorization: Authorization,
    /// The text looked personal; it is never sent to an agent as context.
    pub sensitive: bool,
    /// The saved consultation this came from (its id in the debate history).
    pub consultation_id: Option<String>,
    pub content_hash: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// When the evidence was retrieved. Never invented: absent means unknown.
    pub retrieved_ms: Option<u64>,
    pub published: Option<String>,
    /// How many days a time-sensitive statement stays current, if known.
    pub valid_days: Option<u32>,
    pub category: Option<String>,
    pub time_sensitive: bool,
    pub source_url: Option<String>,
    pub expires_ms: Option<u64>,
}

/// What to store. Ids, hashes and timestamps are assigned by the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMemory {
    pub kind: MemoryKind,
    pub scope: MemoryScope,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
    pub origin: Origin,
    pub status: EpistemicStatus,
    pub text: String,
    pub short: Option<String>,
    pub topic: Option<String>,
    pub tags: Vec<String>,
    pub authorization: Authorization,
    pub sensitive: bool,
    pub consultation_id: Option<String>,
    pub retrieved_ms: Option<u64>,
    pub published: Option<String>,
    pub valid_days: Option<u32>,
    pub category: Option<String>,
    pub time_sensitive: bool,
    pub source_url: Option<String>,
    pub expires_ms: Option<u64>,
}

impl NewMemory {
    pub fn new(kind: MemoryKind, scope: MemoryScope, text: impl Into<String>) -> Self {
        Self {
            kind,
            scope,
            session_id: None,
            project_id: None,
            origin: Origin::User,
            status: EpistemicStatus::UserProvided,
            text: text.into(),
            short: None,
            topic: None,
            tags: Vec::new(),
            authorization: Authorization::ExplicitUser,
            sensitive: false,
            consultation_id: None,
            retrieved_ms: None,
            published: None,
            valid_days: None,
            category: None,
            time_sensitive: false,
            source_url: None,
            expires_ms: None,
        }
    }
}

/// Normalised text used for duplicate detection: case, spacing and punctuation do not matter.
pub fn normalize_for_hash(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = true;
    for c in text.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim_end().to_string()
}

/// The identity of a record's content within one place: the same words, kind and place.
pub fn content_hash(
    kind: MemoryKind,
    scope: MemoryScope,
    project: Option<&str>,
    session: Option<&str>,
    text: &str,
) -> String {
    let mut h = Sha256::new();
    for part in [
        kind.as_str(),
        scope.as_str(),
        project.unwrap_or(""),
        session.unwrap_or(""),
        &normalize_for_hash(text),
    ] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A session of AI Orchestra: a series of related consultations in one project that share context.
/// It is not a session of any provider's CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub project_id: String,
    pub title: Option<String>,
    pub created_ms: u64,
    pub last_used_ms: u64,
    pub turns: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_decide_which_scopes_can_be_read() {
        use MemoryMode as M;
        use MemoryScope as S;
        let reads = |m: MemoryMode, s: MemoryScope, g: bool| m.reads(s, g);
        assert!(!reads(M::Off, S::Session, true), "off reads nothing");
        assert!(reads(M::Auto, S::Session, false) && reads(M::Auto, S::Project, false));
        assert!(
            !reads(M::Auto, S::Global, false),
            "auto leaves global out unless configured"
        );
        assert!(reads(M::Auto, S::Global, true));
        assert!(
            reads(M::Session, S::Session, false)
                && !reads(M::Session, S::Project, false)
                && !reads(M::Session, S::Global, false)
        );
        assert!(reads(M::Project, S::Project, false) && !reads(M::Project, S::Global, false));
        assert!(reads(M::Global, S::Global, false) && reads(M::Global, S::Project, false));
    }

    #[test]
    fn names_round_trip_and_unknown_values_are_rejected() {
        for s in ["auto", "off", "session", "project", "global"] {
            assert_eq!(s.parse::<MemoryMode>().unwrap().as_str(), s);
        }
        assert!("everything".parse::<MemoryMode>().is_err());
        assert!("galaxy".parse::<MemoryScope>().is_err());
        for k in MemoryKind::ALL {
            assert_eq!(k.as_str().parse::<MemoryKind>().unwrap(), k);
        }
        assert!("secret".parse::<MemoryKind>().is_err());
        assert_eq!(parse_memory_arg(None).unwrap(), None);
        assert_eq!(
            parse_memory_arg(Some(" Project ")).unwrap(),
            Some(MemoryMode::Project)
        );
    }

    #[test]
    fn origins_round_trip_through_their_text_form() {
        for o in [
            Origin::User,
            Origin::ConsultationFinal,
            Origin::Agent("claude".into()),
            Origin::Evidence,
            Origin::Imported,
            Origin::System,
        ] {
            assert_eq!(Origin::parse(&o.as_string()), o);
        }
        let json = serde_json::to_string(&Origin::Agent("codex".into())).unwrap();
        assert_eq!(json, "\"agent:codex\"");
        assert_eq!(
            serde_json::from_str::<Origin>(&json).unwrap(),
            Origin::Agent("codex".into())
        );
    }

    #[test]
    fn the_content_hash_ignores_case_and_spacing_but_not_the_place() {
        let h =
            |text: &str, scope, project| content_hash(MemoryKind::Note, scope, project, None, text);
        assert_eq!(
            h("Budget: 5,000 MXN", MemoryScope::Project, Some("p")),
            h("  budget 5 000   mxn ", MemoryScope::Project, Some("p"))
        );
        assert_ne!(
            h("budget", MemoryScope::Project, Some("p")),
            h("budget", MemoryScope::Project, Some("q"))
        );
        assert_ne!(
            h("budget", MemoryScope::Project, Some("p")),
            h("budget", MemoryScope::Global, None)
        );
    }

    #[test]
    fn only_remembering_never_raises_trust() {
        assert!(EpistemicStatus::UserProvided.rank() > EpistemicStatus::AgentInterpretation.rank());
        assert!(EpistemicStatus::PreviouslyVerified.rank() > EpistemicStatus::Unverified.rank());
        assert!(
            EpistemicStatus::AgentInterpretation
                .label()
                .contains("not verified")
        );
        assert!(
            EpistemicStatus::PreviouslyVerified
                .label()
                .contains("not re-checked now")
        );
    }
}
