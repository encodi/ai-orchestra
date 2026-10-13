//! Reproducible evaluation of the context layer (Milestone 7): fifteen scenarios, four conditions.
//!
//! For each scenario a deterministic history is stored, a follow-up question is asked, and the
//! context an agent would receive is built four ways:
//!
//! * `memory off`: nothing (what Milestones 1-6 sent);
//! * `full history`: every earlier question and answer of the session, concatenated (the naive way
//!   to give a model its history; a reference, never sent by AI Orchestra);
//! * `retrieved`: the relevant records with a very large budget (nothing compressed away);
//! * `compressed`: the same retrieval under the normal context budget.
//!
//! The checks are deterministic: do the essential facts, source URLs, caveats and unresolved
//! disagreements survive, does anything that must not be there (a superseded preference, another
//! project's data, sensitive text, an agent's private opinion) leak. They say nothing about how
//! well a real model would answer; sizes are character counts and *estimated* tokens
//! (bytes / 3, a labelled estimate, not a provider count). Run with `--nocapture` to see the table;
//! it is also written to `$CARGO_TARGET_TMPDIR/memory-eval.md`.

use std::fmt::Write as _;
use std::sync::Arc;

use ai_orchestra::Config;
use ai_orchestra::debate::DebateStatus;
use ai_orchestra::history::now_ms;
use ai_orchestra::memory::continuation;
use ai_orchestra::memory::model::{
    EpistemicStatus, MemoryKind, MemoryMode, MemoryScope, NewMemory, Origin,
};
use ai_orchestra::memory::service::{
    AddInput, MemoryRequest, MemoryService, MemorySettings, ProjectRef, RecordInput,
};
use ai_orchestra::memory::tokens::estimate_tokens;

const DAY: u64 = 86_400_000;
const NORMAL_BUDGET: usize = 2000;
const HUGE_BUDGET: usize = 100_000;

struct Eval {
    svc: Arc<MemoryService>,
    project: ProjectRef,
    _dir: tempfile::TempDir,
    turns: usize,
}

impl Eval {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let svc = MemoryService::new(
            dir.path().join("memory.db"),
            MemorySettings::from_config(&Config::default()),
        );
        Self {
            svc,
            project: ProjectRef::named("eval"),
            _dir: dir,
            turns: 0,
        }
    }

    fn request(&self, mode: MemoryMode, budget: usize) -> MemoryRequest {
        let mut r = MemoryRequest::new(mode);
        r.project = Some(self.project.clone());
        r.context_budget_tokens = Some(budget);
        r
    }

    /// Stores one finished consultation the way the orchestrator does.
    async fn turn(&mut self, question: &str, answer: &str) {
        let prepared = self
            .svc
            .prepare(
                &self.request(MemoryMode::Auto, NORMAL_BUDGET),
                question,
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        self.turns += 1;
        let id = format!("hist-{}", self.turns);
        self.svc
            .record(RecordInput {
                prepared: &prepared,
                history_id: &id,
                question,
                mode: "decision",
                strategy: None,
                status: DebateStatus::Complete,
                calls: 1,
                final_answer: answer,
                steps: &[],
                research: None,
                extra_ai_calls: 0,
            })
            .await
            .unwrap();
    }

    async fn preference(&self, text: &str, topic: &str) {
        self.svc
            .add(AddInput {
                project: self.project.clone(),
                scope: MemoryScope::Project,
                kind: MemoryKind::Preference,
                text: text.into(),
                topic: Some(topic.into()),
                tags: vec![],
                session: None,
                allow_sensitive: false,
                expires_days: None,
            })
            .await
            .unwrap();
    }

    /// Stores a record directly (evidence, agent opinions, other projects): what a research run or
    /// another tool would have written.
    async fn insert(
        &self,
        project: &ProjectRef,
        build: impl FnOnce(&mut NewMemory) + Send + 'static,
    ) -> String {
        let pid = project.id.clone();
        let name = project.name.clone();
        self.svc
            .run(move |db| {
                let mut m = NewMemory::new(MemoryKind::Note, MemoryScope::Project, "");
                m.project_id = Some(pid.clone());
                build(&mut m);
                let now = now_ms();
                db.upsert_project(&pid, None, name.as_deref(), now)?;
                Ok(db.insert_memory(&m, now)?.id)
            })
            .await
            .unwrap()
    }

    /// What each condition would send, for `question`.
    async fn conditions(&self, question: &str, mode: MemoryMode, budget: usize) -> Conditions {
        let history = match self.svc.session_detail(&self.project, None).await {
            Ok(d) => d
                .consultations
                .iter()
                .map(|c| {
                    format!(
                        "Q: {}\nA: {}\n",
                        c.question,
                        c.final_answer.clone().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Err(_) => String::new(),
        };
        let retrieved = self
            .svc
            .prepare(
                &self.request(mode, HUGE_BUDGET),
                question,
                usize::MAX / 2,
                false,
            )
            .await
            .unwrap();
        let compressed = self
            .svc
            .prepare(&self.request(mode, budget), question, 48 * 1024, false)
            .await
            .unwrap();
        Conditions {
            off: String::new(),
            full_history: history,
            retrieved: retrieved.text,
            compressed: compressed.text,
            retrieval_us: compressed.report.retrieval_us,
            assembly_us: compressed.report.assembly_us,
        }
    }
}

struct Conditions {
    off: String,
    full_history: String,
    retrieved: String,
    compressed: String,
    retrieval_us: u64,
    assembly_us: u64,
}

impl Conditions {
    fn all(&self) -> [(&'static str, &str); 4] {
        [
            ("memory off", &self.off),
            ("full history", &self.full_history),
            ("retrieved", &self.retrieved),
            ("compressed", &self.compressed),
        ]
    }
}

fn has(text: &str, needle: &str) -> bool {
    text.to_lowercase().contains(&needle.to_lowercase())
}

fn kept(text: &str, facts: &[&str]) -> usize {
    facts.iter().filter(|f| has(text, f)).count()
}

fn leaks(text: &str, forbidden: &[&str]) -> usize {
    forbidden.iter().filter(|f| has(text, f)).count()
}

struct Row {
    id: usize,
    name: &'static str,
    facts: usize,
    forbidden: usize,
    sizes: Vec<(&'static str, usize, usize, usize, usize)>,
    retrieval_us: u64,
    assembly_us: u64,
}

/// One scenario's expectations.
struct Expect<'a> {
    id: usize,
    name: &'static str,
    question: &'a str,
    /// Essential facts, caveats, URLs and unresolved disagreements that must reach the agent.
    must_have: Vec<&'a str>,
    /// Text that must never reach the agent.
    must_not: Vec<&'a str>,
    mode: MemoryMode,
    budget: usize,
}

fn expect<'a>(
    id: usize,
    name: &'static str,
    question: &'a str,
    must_have: Vec<&'a str>,
    must_not: Vec<&'a str>,
) -> Expect<'a> {
    Expect {
        id,
        name,
        question,
        must_have,
        must_not,
        mode: MemoryMode::Auto,
        budget: NORMAL_BUDGET,
    }
}

async fn run(e: &Eval, x: &Expect<'_>) -> (Row, Conditions) {
    let c = e.conditions(x.question, x.mode, x.budget).await;
    let sizes = c
        .all()
        .iter()
        .map(|(n, t)| {
            (
                *n,
                t.chars().count(),
                estimate_tokens(t),
                kept(t, &x.must_have),
                leaks(t, &x.must_not),
            )
        })
        .collect();
    let row = Row {
        id: x.id,
        name: x.name,
        facts: x.must_have.len(),
        forbidden: x.must_not.len(),
        sizes,
        retrieval_us: c.retrieval_us,
        assembly_us: c.assembly_us,
    };
    (row, c)
}

const LIST_A: &str = "## Answer\n\nThe three oscilloscopes are the Rigol DHO804 (70 MHz, 4 channels), the Siglent SDS1104X-E (100 MHz, 4 channels) and the Hantek DSO2D15 (150 MHz, 2 channels).\n\n## Recommendation\n\nThe Hantek DSO2D15 is the best value, the Siglent SDS1104X-E the best all-rounder.\n\n## Assumptions\n\nPrices are list prices.\n\n## Evidence limitations\n\nNothing was verified externally.\n\n## Significant disagreements\n\nThe agents disagree on whether 2 channels are enough for a beginner.\n";

fn filler(i: usize) -> (String, String) {
    (
        format!("Explain topic number {i} about unrelated gardening and cooking, part {i}"),
        format!(
            "## Answer\n\nTopic {i}: {}\n",
            "a long, unrelated explanation about soil, seeds, sauces and seasonal produce. "
                .repeat(40)
        ),
    )
}

// ---------------------------------------------------------------------------------- the scenarios

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_evaluation_suite() {
    let mut rows: Vec<Row> = Vec::new();
    let storage;

    // 1. Follow-up to a previous purchasing decision.
    {
        let mut e = Eval::new();
        e.preference("My oscilloscope budget is 5000 MXN", "budget")
            .await;
        e.turn(
            "Research three portable oscilloscopes for learning electronics and compare them.",
            LIST_A,
        )
        .await;
        let x = expect(
            1,
            "follow-up to a purchase decision",
            "Of the three above, which one is the best value for money?",
            vec![
                "Rigol DHO804",
                "Siglent SDS1104X-E",
                "Hantek DSO2D15",
                "5000 MXN",
                "2 channels are enough",
            ],
            vec![],
        );
        let (row, c) = run(&e, &x).await;
        assert_eq!(
            row.sizes[2].3,
            x.must_have.len(),
            "retrieved keeps every essential fact: {}",
            c.retrieved
        );
        assert_eq!(
            row.sizes[3].3,
            x.must_have.len(),
            "compressed keeps every essential fact"
        );
        assert_eq!(
            row.sizes[0].3, 0,
            "without memory the follow-up has none of it"
        );
        assert_eq!(
            c.compressed
                .matches("agents disagree on whether 2 channels")
                .count(),
            1,
            "a disagreement stated in the conclusion is not repeated: {}",
            c.compressed
        );
        assert!(has(&c.compressed, "Question:") || has(&c.compressed, "Q:"));
        rows.push(row);
    }

    // 2. Revisiting an older scientific investigation, buried under later, unrelated work.
    {
        let mut e = Eval::new();
        e.turn(
            "Does intermittent fasting extend human longevity? Review the evidence.",
            "## Answer\n\nEvidence in humans for longevity benefits of intermittent fasting is limited; the strongest signals come from mouse studies.\n\n## Evidence limitations\n\nNo long randomized human trial exists.\n\n## Significant disagreements\n\nThe agents disagree on how far mouse results transfer to humans.\n",
        )
        .await;
        for i in 0..8 {
            let (q, a) = filler(i);
            e.turn(&q, &a).await;
        }
        let x = expect(
            2,
            "revisiting an older investigation",
            "Back to the intermittent fasting longevity question: what was left unresolved?",
            vec!["limited", "mouse studies", "how far mouse results transfer"],
            vec!["soil, seeds, sauces"],
        );
        let (row, c) = run(&e, &x).await;
        assert_eq!(
            row.sizes[3].3,
            x.must_have.len(),
            "the old investigation is found by relevance, not recency: {}",
            c.compressed
        );
        assert_eq!(
            row.sizes[3].4, 0,
            "unrelated later work is not dragged in: {}",
            c.compressed
        );
        assert!(
            c.compressed.chars().count() * 4 < c.full_history.chars().count(),
            "far smaller than the whole history"
        );
        rows.push(row);
    }

    // 3. A new preference supersedes an old one.
    {
        let mut e = Eval::new();
        e.preference("My oscilloscope budget is 5000 MXN", "budget")
            .await;
        e.turn(
            "Research three portable oscilloscopes for learning electronics and compare them.",
            LIST_A,
        )
        .await;
        e.preference("My oscilloscope budget is 8000 MXN", "budget")
            .await;
        let x = expect(
            3,
            "new preference supersedes the old",
            "Does my oscilloscope budget change which of the three you recommend?",
            vec!["8000 MXN"],
            vec!["5000 MXN"],
        );
        let (row, _) = run(&e, &x).await;
        assert_eq!(row.sizes[2].3, 1);
        assert_eq!(
            row.sizes[2].4, 0,
            "retrieved: the superseded preference is not sent"
        );
        assert_eq!(
            row.sizes[3].4, 0,
            "compressed: the superseded preference is not sent"
        );
        rows.push(row);
    }

    // 4. Reusing earlier verified evidence, with its source and date.
    {
        let e = Eval::new();
        let p = e.project.clone();
        e.insert(&p, |m| {
            m.kind = MemoryKind::Evidence;
            m.origin = Origin::Evidence;
            m.status = EpistemicStatus::PreviouslyVerified;
            m.text = "SUPPORTED: The Rigol DHO804 has 70 MHz of analog bandwidth (checked against the manufacturer datasheet)".into();
            m.topic = Some("rigol dho804 bandwidth".into());
            m.retrieved_ms = Some(now_ms() - 2 * DAY);
            m.source_url = Some("https://example.com/rigol-dho804-datasheet".into());
            m.category = Some("product_specification".into());
        })
        .await;
        let id = e
            .svc
            .list(&p, None, Some(MemoryKind::Evidence), false, 5)
            .await
            .unwrap()[0]
            .id
            .clone();
        let rid = id.clone();
        e.svc
            .run(move |db| {
                db.add_evidence_refs(
                    &rid,
                    &[ai_orchestra::memory::db::EvidenceRef {
                        url: Some("https://example.com/rigol-dho804-datasheet".into()),
                        title: Some("Rigol DHO804 datasheet".into()),
                        published: Some("2026-09-01".into()),
                        updated: None,
                        retrieved_ms: Some(now_ms() - 2 * DAY),
                        content_hash: Some("0123456789abcdef".into()),
                        relation: Some("supports".into()),
                        quote: Some("70 MHz analog bandwidth".into()),
                    }],
                )
            })
            .await
            .unwrap();
        let x = expect(
            4,
            "reusing verified evidence",
            "What bandwidth does the Rigol DHO804 have, according to what we already checked?",
            vec!["70 MHz", "example.com/rigol-dho804-datasheet", "retrieved"],
            vec![],
        );
        let (row, _) = run(&e, &x).await;
        assert_eq!(
            row.sizes[3].3, 3,
            "the claim, its source URL and its retrieval date survive compression"
        );
        rows.push(row);
    }

    // 5. Outdated pricing information.
    {
        let e = Eval::new();
        let p = e.project.clone();
        e.insert(&p, |m| {
            m.kind = MemoryKind::Evidence;
            m.origin = Origin::Evidence;
            m.status = EpistemicStatus::PreviouslyVerified;
            m.text = "SUPPORTED: The Siglent SDS1104X-E costs 380 USD at the shop page".into();
            m.retrieved_ms = Some(now_ms() - 400 * DAY);
            m.time_sensitive = true;
            m.category = Some("price".into());
            m.source_url = Some("https://example.com/shop/siglent".into());
        })
        .await;
        let x = expect(
            5,
            "outdated pricing",
            "What is the current price of the Siglent SDS1104X-E?",
            vec!["380 USD", "may be out of date"],
            vec![],
        );
        let (row, c) = run(&e, &x).await;
        assert_eq!(
            row.sizes[3].3, 2,
            "the old price is sent only together with the warning that it needs re-checking: {}",
            c.compressed
        );
        rows.push(row);
    }

    // 6. Contradictory memories are exposed, not silently merged.
    {
        let e = Eval::new();
        let p = e.project.clone();
        e.insert(&p, |m| {
            m.kind = MemoryKind::Evidence;
            m.origin = Origin::Evidence;
            m.status = EpistemicStatus::Contradicted;
            m.text = "CONTRADICTED: The Model X has 100 MHz of analog bandwidth; the datasheet says 70 MHz".into();
            m.topic = Some("model x bandwidth".into());
            m.retrieved_ms = Some(now_ms() - DAY);
            m.source_url = Some("https://example.com/modelx".into());
        })
        .await;
        let x = expect(
            6,
            "contradictory memories",
            "Does the Model X really have 100 MHz of analog bandwidth?",
            vec!["contradicted", "70 MHz"],
            vec![],
        );
        let (row, c) = run(&e, &x).await;
        assert_eq!(row.sizes[3].3, 2, "{}", c.compressed);
        rows.push(row);
    }

    // 7. Long-session compression: the fact needed is in turn 3 of 30.
    {
        let mut e = Eval::new();
        for i in 0..2 {
            let (q, a) = filler(i);
            e.turn(&q, &a).await;
        }
        e.turn(
            "Which probe attenuation should I use for the Rigol DHO804?",
            "## Answer\n\nUse the 10x probe setting; the 1x setting loads the circuit and limits bandwidth to 6 MHz.\n\n## Evidence limitations\n\nNot verified against the probe datasheet.\n",
        )
        .await;
        for i in 3..30 {
            let (q, a) = filler(i);
            e.turn(&q, &a).await;
        }
        let x = expect(
            7,
            "long-session compression",
            "Remind me which probe attenuation we settled on for the Rigol DHO804.",
            vec!["10x probe", "6 MHz"],
            vec![],
        );
        let (row, c) = run(&e, &x).await;
        assert_eq!(row.sizes[3].3, 2, "{}", c.compressed);
        let stats = e.svc.stats().await.unwrap();
        storage = format!(
            "storage after the 30-turn session of scenario 7: {} bytes on disk for {} records and {} consultations (SQLite pages plus the full-text index); the saved answers alone are {} bytes",
            stats.file_bytes,
            stats.memories,
            stats.consultations,
            c.full_history.len()
        );
        let reduction = 100.0
            - 100.0 * c.compressed.chars().count() as f64 / c.full_history.chars().count() as f64;
        assert!(
            reduction > 90.0,
            "a 30-turn history shrinks to a small recalled context ({reduction:.1}%)"
        );
        rows.push(row);
    }

    // 8. Independent multi-agent reviews: an agent's private opinion is never sent on.
    {
        let e = Eval::new();
        let p = e.project.clone();
        e.insert(&p, |m| {
            m.kind = MemoryKind::Finding;
            m.origin = Origin::Agent("beta".into());
            m.status = EpistemicStatus::AgentInterpretation;
            m.text =
                "AGENT-ONLY-OPINION: beta privately thinks the Hantek DSO2D15 is a bad buy".into();
        })
        .await;
        e.insert(&p, |m| {
            m.kind = MemoryKind::Conclusion;
            m.origin = Origin::ConsultationFinal;
            m.status = EpistemicStatus::AgentInterpretation;
            m.text = "Question: which oscilloscope? Conclusion (AI agents, not verified): the Hantek DSO2D15 is the value pick".into();
        })
        .await;
        let x = expect(
            8,
            "independent multi-agent reviews",
            "Which oscilloscope is the value pick, the Hantek DSO2D15?",
            vec!["value pick"],
            vec!["AGENT-ONLY-OPINION"],
        );
        let (row, _) = run(&e, &x).await;
        assert_eq!(
            row.sizes[2].4 + row.sizes[3].4,
            0,
            "an individual opinion never reaches another agent"
        );
        rows.push(row);
    }

    // 9. A consultation with memory disabled.
    {
        let mut e = Eval::new();
        e.preference("My oscilloscope budget is 5000 MXN", "budget")
            .await;
        e.turn(
            "Research three portable oscilloscopes for learning electronics and compare them.",
            LIST_A,
        )
        .await;
        let mut x = expect(
            9,
            "memory disabled",
            "Of the three above, which one is the best value for money?",
            vec!["Hantek DSO2D15", "5000 MXN"],
            vec![],
        );
        x.mode = MemoryMode::Off;
        let (row, c) = run(&e, &x).await;
        assert!(
            c.retrieved.is_empty() && c.compressed.is_empty(),
            "off means nothing is recalled"
        );
        assert_eq!(row.sizes[3].3, 0);
        rows.push(row);
    }

    // 10. Irrelevant historical information is not retrieved.
    {
        let mut e = Eval::new();
        let (q, a) = filler(1);
        e.turn(&q, &a).await;
        e.preference("I like growing tomatoes on the balcony", "hobby")
            .await;
        let x = expect(
            10,
            "irrelevant history",
            "What is the difference between AC and DC coupling on an oscilloscope?",
            vec![],
            vec!["tomatoes"],
        );
        let (row, c) = run(&e, &x).await;
        assert_eq!(row.sizes[3].4, 0, "{}", c.compressed);
        assert_eq!(row.sizes[2].4, 0);
        // The very latest turn may come along, as one short line, in case the question is a
        // follow-up phrased without a pointer; nothing more of the unrelated history does.
        assert!(
            !c.compressed.contains("Full answer of that consultation"),
            "{}",
            c.compressed
        );
        assert!(
            c.compressed.chars().count() < 1500,
            "{}",
            c.compressed.chars().count()
        );
        rows.push(row);
    }

    // 11. Accidental cross-project access.
    {
        let mut e = Eval::new();
        let other = ProjectRef::named("someone-elses-project");
        e.insert(&other, |m| {
            m.kind = MemoryKind::Preference;
            m.text = "CONFIDENTIAL-OTHER-PROJECT: the client wants the Keysight and has a 90000 MXN budget for oscilloscopes".into();
        })
        .await;
        e.turn(
            "Research three portable oscilloscopes for learning electronics and compare them.",
            LIST_A,
        )
        .await;
        let x = expect(
            11,
            "cross-project access",
            "What oscilloscope budget does the client have?",
            vec![],
            vec!["CONFIDENTIAL-OTHER-PROJECT", "90000 MXN"],
        );
        let (row, _) = run(&e, &x).await;
        assert_eq!(row.sizes[2].4 + row.sizes[3].4, 0);
        rows.push(row);
    }

    // 12. A tight budget must not silently drop what it leaves out.
    {
        let mut e = Eval::new();
        e.turn(
            "Can the Hantek DSO2D15 measure mains voltage?",
            "## Answer\n\nNot directly: the Hantek DSO2D15 must never be connected to mains without an isolated differential probe.\n\n## Evidence limitations\n\nThe 30 V input limit comes from an unverified forum post.\n\n## Significant disagreements\n\nThe agents disagree on whether an isolation transformer is enough.\n",
        )
        .await;
        for i in 0..6 {
            e.preference(&format!("Oscilloscope requirement number {i}: must support USB export and at least {i} channels of recording"), &format!("req {i}")).await;
        }
        let mut x = expect(
            12,
            "truncated context with critical caveats",
            "Can I use the Hantek DSO2D15 to measure mains voltage? Which oscilloscope requirement matters?",
            vec![
                "isolated differential probe",
                "30 V",
                "isolation transformer",
            ],
            vec![],
        );
        // Roomy: everything survives. Tight: either the caveat survives or the agent is told something was left out.
        let (row_roomy, c_roomy) = run(&e, &x).await;
        assert_eq!(
            row_roomy.sizes[3].3, 3,
            "with room, every caveat is kept: {}",
            c_roomy.compressed
        );
        x.budget = 330;
        let (row_tight, c_tight) = run(&e, &x).await;
        let all_kept = row_tight.sizes[3].3 == x.must_have.len();
        let told = has(&c_tight.compressed, "left out to fit the context budget")
            || c_tight.compressed.is_empty();
        assert!(
            all_kept || told,
            "a caveat is either kept or its absence is announced: {}",
            c_tight.compressed
        );
        rows.push(row_roomy);
    }

    // 13. Sensitive information is excluded.
    {
        let mut e = Eval::new();
        e.svc
            .add(AddInput {
                project: e.project.clone(),
                scope: MemoryScope::Project,
                kind: MemoryKind::Note,
                text:
                    "Oscilloscope seller contact: maria.lopez@example.com, card 4111 1111 1111 1111"
                        .into(),
                topic: None,
                tags: vec![],
                session: None,
                allow_sensitive: true,
                expires_days: None,
            })
            .await
            .unwrap();
        e.turn(
            "Research three portable oscilloscopes for learning electronics and compare them.",
            LIST_A,
        )
        .await;
        let x = expect(
            13,
            "sensitive information",
            "Who is the oscilloscope seller contact?",
            vec![],
            vec!["maria.lopez", "4111"],
        );
        let (row, _) = run(&e, &x).await;
        assert_eq!(row.sizes[2].4 + row.sizes[3].4, 0);
        // A credential cannot be stored at all.
        assert!(
            e.svc
                .add(AddInput {
                    project: e.project.clone(),
                    scope: MemoryScope::Project,
                    kind: MemoryKind::Note,
                    text: "key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789".into(),
                    topic: None,
                    tags: vec![],
                    session: None,
                    allow_sensitive: true,
                    expires_days: None,
                })
                .await
                .is_err()
        );
        rows.push(row);
    }

    // 14. User-requested deletion.
    {
        let e = Eval::new();
        e.preference(
            "Remember that my oscilloscope supplier is Zeta Instruments",
            "supplier",
        )
        .await;
        let id = e.svc.list(&e.project, None, None, false, 5).await.unwrap()[0]
            .id
            .clone();
        let x = expect(
            14,
            "user-requested deletion",
            "Which oscilloscope supplier should I use?",
            vec![],
            vec!["Zeta Instruments"],
        );
        let before = e.conditions(x.question, x.mode, x.budget).await;
        assert!(
            has(&before.compressed, "Zeta Instruments"),
            "it was recalled before the deletion"
        );
        e.svc.forget(&e.project, &id).await.unwrap();
        let (row, _) = run(&e, &x).await;
        assert_eq!(
            row.sizes[2].4 + row.sizes[3].4,
            0,
            "after deletion nothing of it remains in any context"
        );
        rows.push(row);
    }

    // 15. Provider-native session continuation is unavailable: the local context carries the facts.
    {
        let mut e = Eval::new();
        e.turn(
            "Research three portable oscilloscopes for learning electronics and compare them.",
            LIST_A,
        )
        .await;
        for m in continuation::matrix() {
            assert!(
                !m.used && !m.verified,
                "{} continuation must stay unused",
                m.provider
            );
        }
        let x = expect(
            15,
            "provider session unavailable",
            "Of the three above, which one is the best value for money?",
            vec!["Hantek DSO2D15", "Siglent SDS1104X-E"],
            vec![],
        );
        let (row, _) = run(&e, &x).await;
        assert_eq!(
            row.sizes[3].3, 2,
            "the facts travel in the prompt, not in a provider session"
        );
        rows.push(row);
    }

    // The report.
    let mut table = String::new();
    let _ = writeln!(
        table,
        "| # | scenario | condition | chars | est. tokens | essential kept | forbidden leaked |"
    );
    let _ = writeln!(
        table,
        "|---|----------|-----------|------:|------------:|:--------------:|:----------------:|"
    );
    for r in &rows {
        for (name, chars, tokens, kept, leaked) in &r.sizes {
            let _ = writeln!(
                table,
                "| {} | {} | {} | {} | {} | {}/{} | {}/{} |",
                r.id, r.name, name, chars, tokens, kept, r.facts, leaked, r.forbidden
            );
        }
    }
    let _ = writeln!(
        table,
        "\nretrieval + assembly time of the compressed condition (µs; informational, never asserted):"
    );
    for r in &rows {
        let _ = writeln!(
            table,
            "- scenario {}: retrieval {} µs, assembly {} µs",
            r.id, r.retrieval_us, r.assembly_us
        );
    }
    let _ = writeln!(table, "\n{storage}");
    let _ = writeln!(
        table,
        "\nSizes are characters and estimated tokens (bytes / 3). They are not provider token counts."
    );
    println!("{table}");
    if let Some(dir) = option_env!("CARGO_TARGET_TMPDIR") {
        let _ = std::fs::write(std::path::Path::new(dir).join("memory-eval.md"), &table);
    }
    assert_eq!(rows.len(), 15);

    // Across the suite: compressed and retrieved contexts never leak, and keep what they were given.
    for r in &rows {
        assert_eq!(r.sizes[2].4, 0, "scenario {} (retrieved) leaked", r.id);
        assert_eq!(r.sizes[3].4, 0, "scenario {} (compressed) leaked", r.id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_inputs_give_the_same_context() {
    let mut a = Eval::new();
    let mut b = Eval::new();
    for e in [&mut a, &mut b] {
        e.preference("My oscilloscope budget is 5000 MXN", "budget")
            .await;
        e.turn(
            "Research three portable oscilloscopes for learning electronics and compare them.",
            LIST_A,
        )
        .await;
    }
    let q = "Of the three above, which one is the best value for money?";
    let strip = |t: &str| {
        t.split_whitespace()
            .filter(|w| !w.starts_with("mem-"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (ca, cb) = (
        a.conditions(q, MemoryMode::Auto, NORMAL_BUDGET).await,
        b.conditions(q, MemoryMode::Auto, NORMAL_BUDGET).await,
    );
    // Record ids and dates differ between runs; the content and its order do not.
    let norm = |t: &str| {
        strip(t)
            .split_whitespace()
            .filter(|w| !w.contains('[') && !w.starts_with("20") && !w.starts_with("19"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(norm(&ca.compressed), norm(&cb.compressed));
    // And a second ask on the same database is identical apart from the clock.
    let again = a.conditions(q, MemoryMode::Auto, NORMAL_BUDGET).await;
    assert_eq!(norm(&ca.compressed), norm(&again.compressed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_budget_is_a_ceiling_for_recalled_conclusions_at_every_size() {
    let mut e = Eval::new();
    for i in 0..12 {
        e.turn(
            &format!("Oscilloscope question {i}: explain bandwidth, sample rate and memory depth"),
            &format!("## Answer\n\nOscilloscope answer {i}: {}\n\n## Evidence limitations\n\nNot verified.\n", "bandwidth, sample rate and memory depth interact in detail. ".repeat(30)),
        )
        .await;
    }
    let q = "Summarize what we learned about oscilloscope bandwidth, sample rate and memory depth";
    for budget in [350, 500, 800, 1500, 3000] {
        let c = e.conditions(q, MemoryMode::Auto, budget).await;
        assert!(
            estimate_tokens(&c.compressed) <= budget,
            "budget {budget}: {} estimated tokens",
            estimate_tokens(&c.compressed)
        );
    }
    // More room never means less: the larger budget keeps at least as much.
    let small = e
        .conditions(q, MemoryMode::Auto, 500)
        .await
        .compressed
        .chars()
        .count();
    let large = e
        .conditions(q, MemoryMode::Auto, 3000)
        .await
        .compressed
        .chars()
        .count();
    assert!(large >= small);
}
