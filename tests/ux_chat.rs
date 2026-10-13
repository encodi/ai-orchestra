//! The interactive chat (Milestone 8), driven with scripted lines, mock agents and temporary stores.
//! No terminal, no real model and no real home directory.

#![cfg(unix)]

#[path = "../src/research/testserver.rs"]
mod testserver;

#[path = "common/scripted.rs"]
mod scripted;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use ai_orchestra::Orchestrator;
use ai_orchestra::config::Config;
use ai_orchestra::consult::{ConsultMode, ConsultOptions};
use ai_orchestra::eval::feedback::{FeedbackStore, Rating};
use ai_orchestra::history::DebateStore;
use ai_orchestra::memory::service::{MemoryService, MemorySettings};
use ai_orchestra::provider::{AgentProvider, ProviderRegistry};
use ai_orchestra::providers::mock::{MockProvider, MockReply};
use ai_orchestra::research::fetch::SourceFetcher;
use ai_orchestra::research::net::FetchPolicy;
use ai_orchestra::research::plan::{ResearchRequest, WebMode};
use ai_orchestra::smart::strategy::Strategy;
use ai_orchestra::ux::chat::{ChatEngine, ChatIo, Flow};
use ai_orchestra::ux::present;
use ai_orchestra::ux::settings::Key;
use scripted::Behavior;
use testserver::{Route, TestServer};
use tokio_util::sync::CancellationToken;

const MAKER: &str = include_str!("fixtures/research/maker_spec.html");

const LIST_A: &str = "## Answer\n\nThe three oscilloscopes are the Rigol DHO804, the Siglent SDS1104X-E and the Hantek DSO2D15.\n\n## Recommendation\n\nThe Hantek DSO2D15 is the best value.\n\n## Significant disagreements\n\nThe agents disagree on whether 2 channels are enough.\n\n## Evidence limitations\n\nNothing was verified externally.\n";
const VALUE_A: &str = "## Answer\n\nThe Hantek DSO2D15 has the best value for money.\n";

fn shop(prompt: &str) -> String {
    let q = prompt.rsplit("</memory_context>").next().unwrap_or(prompt);
    if q.contains("Of the three above") {
        VALUE_A.into()
    } else {
        LIST_A.into()
    }
}

/// A scripted terminal: it records what was printed and can interrupt on a marker.
struct Io {
    out: String,
    status: String,
    cancel: CancellationToken,
    /// Cancel the consultation when a progress line contains this.
    interrupt_on: Option<&'static str>,
}

impl Io {
    fn new() -> Self {
        Self {
            out: String::new(),
            status: String::new(),
            cancel: CancellationToken::new(),
            interrupt_on: None,
        }
    }

    fn take_out(&mut self) -> String {
        std::mem::take(&mut self.out)
    }
}

impl ChatIo for Io {
    fn out(&mut self, text: &str) {
        self.out.push_str(text);
    }

    fn status(&mut self, text: &str) {
        self.status.push_str(text);
        self.status.push('\n');
        if self.interrupt_on.is_some_and(|m| text.contains(m)) {
            self.cancel.cancel();
        }
    }

    fn begin_consultation(&mut self) -> CancellationToken {
        self.cancel = CancellationToken::new();
        self.cancel.clone()
    }
}

struct Rig {
    orch: Arc<Orchestrator>,
    config: Config,
    agents: Vec<Arc<MockProvider>>,
    dir: tempfile::TempDir,
}

fn shop_agents() -> Vec<Arc<MockProvider>> {
    ["lead", "beta", "gamma"]
        .iter()
        .map(|n| {
            Arc::new(
                MockProvider::new(n)
                    .with_reply(MockReply::Responder(Arc::new(|req| shop(&req.prompt)))),
            )
        })
        .collect()
}

fn rig_in(dir: tempfile::TempDir, project: Option<&str>, agents: Vec<Arc<MockProvider>>) -> Rig {
    let mut config = Config::default();
    config.general.coordinator = "lead".into();
    config.memory.path = Some(dir.path().join("memory.db").display().to_string());
    config.memory.project = project.map(str::to_string);
    let providers: Vec<Arc<dyn AgentProvider>> = agents
        .iter()
        .map(|a| a.clone() as Arc<dyn AgentProvider>)
        .collect();
    let svc = MemoryService::new(
        dir.path().join("memory.db"),
        MemorySettings::from_config(&config),
    );
    let orch = Orchestrator::with_registry(config.clone(), ProviderRegistry::new(providers))
        .unwrap()
        .with_history(DebateStore::new(dir.path().join("debates")))
        .with_memory(svc);
    Rig {
        orch: Arc::new(orch),
        config,
        agents,
        dir,
    }
}

fn rig() -> Rig {
    rig_in(
        tempfile::tempdir().unwrap(),
        Some("chat-tests"),
        shop_agents(),
    )
}

impl Rig {
    fn engine(&self, resume: Option<String>) -> ChatEngine {
        ChatEngine::new(
            self.orch.clone(),
            self.config.clone(),
            Some(self.dir.path().join("preferences.json")),
            Some(FeedbackStore::new(self.dir.path().join("feedback.json"))),
            resume,
        )
        .unwrap()
    }

    fn prompts(&self) -> Vec<String> {
        self.agents
            .iter()
            .flat_map(|a| a.requests().into_iter().map(|r| r.prompt))
            .collect()
    }

    fn calls(&self) -> usize {
        self.agents.iter().map(|a| a.requests().len()).sum()
    }
}

async fn say(e: &mut ChatEngine, io: &mut Io, line: &str) -> String {
    e.handle(line, io).await;
    io.take_out()
}

// ------------------------------------------------------------------------------------ the flow

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conversation_starts_answers_and_continues_the_same_session() {
    let r = rig();
    let mut e = r.engine(None);
    let mut io = Io::new();
    assert!(e.banner().contains("/help"), "{}", e.banner());
    assert_eq!(e.handle("/help", &mut io).await, Flow::Continue);
    assert!(io.take_out().contains("/settings"));

    say(&mut e, &mut io, "/strategy fast").await;
    let first = say(
        &mut e,
        &mut io,
        "Research three portable oscilloscopes for learning electronics",
    )
    .await;
    assert!(
        first.contains("Hantek DSO2D15") && first.contains("[complete"),
        "{first}"
    );
    assert!(
        io.status.contains("Mode:") && io.status.contains("Coordinator:"),
        "routing is shown before the work: {}",
        io.status
    );
    let session = e
        .session()
        .expect("a session after the first answer")
        .to_string();

    let second = say(
        &mut e,
        &mut io,
        "Of the three above, which is the best value?",
    )
    .await;
    assert!(second.contains("best value"), "{second}");
    assert_eq!(
        e.session(),
        Some(session.as_str()),
        "the follow-up continues the session"
    );
    let prompt = r.prompts().pop().unwrap();
    assert!(
        prompt.contains("<memory_context") && prompt.contains("Hantek DSO2D15"),
        "the follow-up carries the earlier result"
    );
    assert!(prompt.contains("Of the three above, which is the best value?"));
    assert_eq!(e.handle("/exit", &mut io).await, Flow::Exit);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_starts_a_fresh_session_and_resume_returns_to_an_old_one() {
    let r = rig();
    let mut e = r.engine(None);
    let mut io = Io::new();
    say(&mut e, &mut io, "/strategy fast").await;
    say(&mut e, &mut io, "Research three portable oscilloscopes").await;
    let old = e.session().unwrap().to_string();

    let msg = say(&mut e, &mut io, "/new").await;
    assert!(
        msg.contains("new session") && msg.contains("settings stay"),
        "{msg}"
    );
    say(
        &mut e,
        &mut io,
        "Of the three above, which is the best value?",
    )
    .await;
    let new = e.session().unwrap().to_string();
    assert_ne!(old, new);
    assert!(
        !r.prompts().pop().unwrap().contains("Hantek DSO2D15"),
        "a new session recalls nothing of the old one"
    );

    let sessions = say(&mut e, &mut io, "/sessions").await;
    assert!(
        sessions.contains(&old)
            && sessions.contains(&new)
            && sessions.contains(&format!("* {new}")),
        "{sessions}"
    );
    let resumed = say(&mut e, &mut io, &format!("/resume {old}")).await;
    assert!(
        resumed.contains("no provider session is resumed") && resumed.contains("not re-checked"),
        "{resumed}"
    );
    assert_eq!(e.session(), Some(old.as_str()));
    say(
        &mut e,
        &mut io,
        "Of the three above, which is the best value?",
    )
    .await;
    assert!(
        r.prompts().pop().unwrap().contains("Hantek DSO2D15"),
        "the old session's results are recalled again"
    );
    assert!(
        say(&mut e, &mut io, "/resume ses-nope")
            .await
            .contains("no session")
    );

    // A chat started with a session id continues it.
    let mut e2 = r.engine(Some(old.clone()));
    assert!(e2.banner().contains(&format!("Continuing session {old}")));
    say(&mut e2, &mut io, "/strategy fast").await;
    say(&mut e2, &mut io, "Which of the three above is cheapest?").await;
    assert_eq!(e2.session(), Some(old.as_str()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_are_validated_shown_with_their_source_and_returned_to_auto() {
    let r = rig();
    let mut e = r.engine(None);
    let mut io = Io::new();
    assert!(say(&mut e, &mut io, "/mode").await.contains("mode = AUTO"));
    assert!(
        say(&mut e, &mut io, "/mode decision")
            .await
            .contains("set to decision for this session")
    );
    assert!(
        say(&mut e, &mut io, "/rounds 3")
            .await
            .contains("accepts: auto, 1, 2")
    );
    assert!(
        say(&mut e, &mut io, "/mode")
            .await
            .contains("mode = decision"),
        "a refused value changes nothing"
    );
    assert!(
        say(&mut e, &mut io, "/agents lead,nobody")
            .await
            .contains("nobody"),
        "an agent that does not exist is refused"
    );
    assert!(
        say(&mut e, &mut io, "/agents")
            .await
            .contains("agents = AUTO"),
        "and nothing changed"
    );
    assert!(
        say(&mut e, &mut io, "/agents lead,beta")
            .await
            .contains("agents set to lead,beta")
    );
    let settings = say(&mut e, &mut io, "/settings").await;
    assert!(
        settings.contains("decision") && settings.contains("(this session)"),
        "{settings}"
    );
    assert!(
        settings.contains("AUTO")
            && settings.contains("Precedence")
            && settings.contains("read-only"),
        "{settings}"
    );
    assert!(
        say(&mut e, &mut io, "/bogus")
            .await
            .contains("Unknown command")
    );
    assert!(
        say(&mut e, &mut io, "/auto mode")
            .await
            .contains("back to AUTO")
    );
    assert!(say(&mut e, &mut io, "/mode").await.contains("AUTO"));
    assert!(
        say(&mut e, &mut io, "/auto all")
            .await
            .contains("Every session setting")
    );
    assert!(
        !say(&mut e, &mut io, "/settings")
            .await
            .contains("(this session)")
    );
    assert!(say(&mut e, &mut io, "/mode auto").await.contains("AUTO"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_settings_are_never_overridden_and_a_request_overrides_only_its_own_question() {
    let r = rig();
    let mut e = r.engine(None);
    let mut io = Io::new();
    say(&mut e, &mut io, "/agents lead").await;
    say(&mut e, &mut io, "/coordinator lead").await;
    say(&mut e, &mut io, "/mode decision").await;
    say(&mut e, &mut io, "Which oscilloscope should a beginner buy?").await;
    let used: Vec<usize> = r.agents.iter().map(|a| a.requests().len()).collect();
    assert!(
        used[0] >= 1 && used[1] == 0 && used[2] == 0,
        "only the named agent was called: {used:?}"
    );
    assert!(
        io.status.contains("Mode: decision") && io.status.contains("Agents: lead"),
        "{}",
        io.status
    );

    // One question with its own mode: the session setting is untouched afterwards.
    io.status.clear();
    say(
        &mut e,
        &mut io,
        "/ask --mode learn Explain how an oscilloscope probe works",
    )
    .await;
    assert!(io.status.contains("Mode: learn"), "{}", io.status);
    assert!(
        say(&mut e, &mut io, "/mode")
            .await
            .contains("mode = decision")
    );
    assert!(
        say(&mut e, &mut io, "/ask --mood learn x")
            .await
            .contains("unknown setting")
    );
    assert!(
        say(&mut e, &mut io, "/ask --mode")
            .await
            .contains("needs a value")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_preferences_start_a_new_chat_and_a_session_setting_still_wins() {
    let r = rig();
    let mut e = r.engine(None);
    let mut io = Io::new();
    say(&mut e, &mut io, "/strategy balanced").await;
    say(&mut e, &mut io, "/web off").await;
    let saved = say(&mut e, &mut io, "/save").await;
    assert!(
        saved.contains("Saved as your starting preferences"),
        "{saved}"
    );
    let mode = std::fs::metadata(r.dir.path().join("preferences.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "preferences are owner-only");

    let mut fresh = r.engine(None);
    let settings = say(&mut fresh, &mut io, "/settings").await;
    assert!(
        settings.contains("balanced") && settings.contains("(saved preference)"),
        "{settings}"
    );
    assert!(fresh.banner().contains("strategy=balanced"));
    say(&mut fresh, &mut io, "/strategy fast").await;
    assert!(
        say(&mut fresh, &mut io, "/settings").await.contains("fast"),
        "the session beats the preference"
    );
    assert!(
        say(&mut fresh, &mut io, "/prefs")
            .await
            .contains("strategy = balanced")
    );
    assert!(
        say(&mut fresh, &mut io, "/auto strategy")
            .await
            .contains("AUTO")
    );
    assert!(
        say(&mut fresh, &mut io, "/settings")
            .await
            .contains("balanced"),
        "back to the saved preference"
    );
    assert!(
        say(&mut fresh, &mut io, "/prefs clear")
            .await
            .contains("removed")
    );
    assert!(!r.dir.path().join("preferences.json").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_answer_views_show_agents_disagreements_the_plan_and_the_numbers() {
    let r = rig();
    let mut e = r.engine(None);
    let mut io = Io::new();
    assert!(
        say(&mut e, &mut io, "/sources")
            .await
            .contains("Nothing to show yet")
    );
    say(&mut e, &mut io, "/strategy thorough").await;
    say(&mut e, &mut io, "/mode decision").await;
    let answer = say(&mut e, &mut io, "Which oscilloscope should a beginner buy?").await;
    assert!(
        answer.contains("Recommendation") && answer.contains("Disagreements"),
        "{answer}"
    );

    let answers = say(&mut e, &mut io, "/answers").await;
    assert!(
        answers.contains("independent answer") && answers.contains("final synthesis"),
        "{answers}"
    );
    assert!(
        answers.contains("lead") && answers.contains("beta"),
        "{answers}"
    );
    assert!(say(&mut e, &mut io, "/answers full").await.len() >= answers.len());
    let disagreements = say(&mut e, &mut io, "/disagreements").await;
    assert!(
        disagreements.contains("2 channels")
            || disagreements.contains("No significant disagreement"),
        "{disagreements}"
    );
    let why = say(&mut e, &mut io, "/why").await;
    assert!(why.contains("mode:") && why.contains("strategy"), "{why}");
    let stats = say(&mut e, &mut io, "/stats").await;
    assert!(
        stats.contains("agent calls") && stats.contains("not reported by the CLIs"),
        "{stats}"
    );
    assert!(
        say(&mut e, &mut io, "/sources")
            .await
            .contains("No web research")
    );
    assert!(
        say(&mut e, &mut io, "/last")
            .await
            .contains("Recommendation")
    );
}

// --------------------------------------------------------------------- cancellation and failure

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_during_a_consultation_cancels_it_and_it_is_never_called_complete() {
    // The agents wait on a gate nobody opens: the consultation is in flight until it is cancelled.
    let gate = Arc::new(tokio::sync::Barrier::new(2));
    let agents: Vec<Arc<MockProvider>> = ["lead", "beta", "gamma"]
        .iter()
        .map(|n| {
            Arc::new(
                MockProvider::new(n)
                    .with_gate(gate.clone())
                    .with_reply(MockReply::Text(LIST_A.into())),
            )
        })
        .collect();
    let r = rig_in(tempfile::tempdir().unwrap(), Some("cancel"), agents);
    let mut e = r.engine(None);
    let mut io = Io::new();
    say(&mut e, &mut io, "/strategy fast").await;
    io.interrupt_on = Some("Producing final synthesis");
    let out = say(&mut e, &mut io, "Research three portable oscilloscopes").await;
    assert!(out.contains("CANCELLED"), "{out}");
    assert!(!out.contains("[complete"), "{out}");
    // The chat is still usable and the cancelled run did not become a remembered conclusion.
    assert_eq!(e.handle("/settings", &mut io).await, Flow::Continue);
    let svc = r.orch.memory().unwrap();
    let project = svc.default_project(None);
    assert!(
        svc.list(
            &project,
            None,
            Some(ai_orchestra::memory::model::MemoryKind::Conclusion),
            true,
            5
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_reviewer_gives_a_partial_result_that_says_so() {
    let mut failing = MockProvider::new("beta");
    for _ in 0..40 {
        failing = failing.with_reply(MockReply::Failure("scripted failure".into()));
    }
    let agents = vec![
        Arc::new(
            MockProvider::new("lead")
                .with_reply(MockReply::Responder(Arc::new(|req| shop(&req.prompt)))),
        ),
        Arc::new(failing),
        Arc::new(
            MockProvider::new("gamma")
                .with_reply(MockReply::Responder(Arc::new(|req| shop(&req.prompt)))),
        ),
    ];
    let r = rig_in(tempfile::tempdir().unwrap(), Some("partial"), agents);
    let mut e = r.engine(None);
    let mut io = Io::new();
    say(&mut e, &mut io, "/strategy thorough").await;
    say(&mut e, &mut io, "/mode decision").await;
    let out = say(&mut e, &mut io, "Which oscilloscope should a beginner buy?").await;
    assert!(out.contains("PARTIAL RESULT"), "{out}");
    assert!(!out.contains("[complete"), "{out}");
    assert!(say(&mut e, &mut io, "/answers").await.contains("FAILED"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_request_never_reaches_an_agent_and_the_chat_goes_on() {
    let r = rig();
    let mut e = r.engine(None);
    let mut io = Io::new();
    let out = say(&mut e, &mut io, "/ask --strategy turbo a question").await;
    assert!(out.contains("turbo"), "{out}");
    assert!(
        say(&mut e, &mut io, "/ask --mode review")
            .await
            .contains("Type the question")
    );
    assert!(say(&mut e, &mut io, "   ").await.is_empty());
    assert_eq!(r.calls(), 0);
}

// ------------------------------------------------------------------------- history and feedback

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_lists_searches_and_reopens_with_a_note_that_nothing_was_rechecked() {
    let r = rig();
    let mut e = r.engine(None);
    let mut io = Io::new();
    say(&mut e, &mut io, "/strategy fast").await;
    say(
        &mut e,
        &mut io,
        "Research three portable oscilloscopes for learning electronics",
    )
    .await;
    let id = e.last().unwrap().id.clone();
    assert!(say(&mut e, &mut io, "/history").await.contains(&id));
    let found = say(&mut e, &mut io, "/history hantek").await;
    assert!(found.contains(&id) && found.contains("/open"), "{found}");
    assert!(
        say(&mut e, &mut io, "/history zebra")
            .await
            .contains("Nothing matches")
    );
    let calls = r.calls();
    let reopened = say(&mut e, &mut io, &format!("/open {id}")).await;
    assert!(reopened.contains("Hantek DSO2D15"), "{reopened}");
    assert_eq!(r.calls(), calls, "reopening calls no agent");
    assert!(
        say(&mut e, &mut io, "/open nope")
            .await
            .contains("no saved debate")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_is_stored_locally_privately_and_changes_nothing() {
    let r = rig();
    let mut e = r.engine(None);
    let mut io = Io::new();
    assert!(
        say(&mut e, &mut io, "/feedback helpful")
            .await
            .contains("Nothing to give feedback on")
    );
    say(&mut e, &mut io, "/strategy fast").await;
    say(&mut e, &mut io, "Research three portable oscilloscopes").await;
    let id = e.last().unwrap().id.clone();
    assert!(
        say(&mut e, &mut io, "/feedback great")
            .await
            .contains("Usage")
    );
    let msg = say(
        &mut e,
        &mut io,
        "/feedback bad-citation the [1] link does not say that",
    )
    .await;
    assert!(msg.contains("changes nothing by itself"), "{msg}");
    let store = FeedbackStore::new(r.dir.path().join("feedback.json"));
    let all = store.for_consultation(&id).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].rating, Rating::BadCitation);
    assert!(all[0].note.contains("does not say that"));
    assert_eq!(
        std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    // Feedback did not become a preference or a setting.
    assert!(
        say(&mut e, &mut io, "/prefs")
            .await
            .contains("No saved preferences")
    );
    let settings = say(&mut e, &mut io, "/settings").await;
    assert!(
        !settings.contains("feedback") && !settings.contains("bad-citation"),
        "{settings}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_of_another_project_cannot_be_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let a = rig_in(
        tempfile::tempdir().unwrap(),
        Some("project-a"),
        shop_agents(),
    );
    let mut e = a.engine(None);
    let mut io = Io::new();
    say(&mut e, &mut io, "/strategy fast").await;
    say(&mut e, &mut io, "Research three portable oscilloscopes").await;
    let session = e.session().unwrap().to_string();
    // The same database, another project.
    let mut config = a.config.clone();
    config.memory.project = Some("project-b".into());
    let svc = MemoryService::new(
        a.dir.path().join("memory.db"),
        MemorySettings::from_config(&config),
    );
    let providers: Vec<Arc<dyn AgentProvider>> = a
        .agents
        .iter()
        .map(|x| x.clone() as Arc<dyn AgentProvider>)
        .collect();
    let orch = Orchestrator::with_registry(config.clone(), ProviderRegistry::new(providers))
        .unwrap()
        .with_memory(svc);
    let mut b = ChatEngine::new(Arc::new(orch), config, None, None, None).unwrap();
    let out = say(&mut b, &mut io, &format!("/resume {session}")).await;
    assert!(out.contains("another project"), "{out}");
    assert!(
        say(&mut b, &mut io, "/sessions")
            .await
            .contains("No sessions")
    );
    drop(dir);
}

// ------------------------------------------------------------------------------ sources display

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sources_citations_and_verification_status_are_shown_for_a_researched_answer() {
    let pages = TestServer::start(vec![("/maker", Route::Html(MAKER.into()))]).await;
    let behavior = Behavior {
        claims: true,
        delay_ms: 1,
        ..Behavior::normal("budget")
    };
    let answering = {
        let behavior = behavior.clone();
        MockReply::Responder(Arc::new(move |req| {
            scripted::answer_for(&behavior, &req.prompt)
        }))
    };
    let lead = Arc::new(
        MockProvider::new("lead")
            .with_web_search()
            .with_reply(MockReply::WebText {
                text: "{}".into(),
                urls: vec![pages.url("/maker")],
            })
            .with_reply(answering),
    );
    let others = [
        scripted::scripted("beta", &behavior, false),
        scripted::scripted("gamma", &behavior, false),
    ];
    let r = rig_in(
        tempfile::tempdir().unwrap(),
        Some("sources"),
        vec![lead, others[0].clone(), others[1].clone()],
    );
    let fetcher = Arc::new(
        SourceFetcher::new(FetchPolicy {
            respect_robots: false,
            ..FetchPolicy::for_local_tests()
        })
        .unwrap(),
    );
    let outcome = r
        .orch
        .consult(
            ConsultOptions {
                question: "Does the Model X oscilloscope really have 100 MHz of analog bandwidth?"
                    .into(),
                mode: Some(ConsultMode::Research),
                strategy: Some(Strategy::Fast),
                research: ResearchRequest {
                    web: WebMode::On,
                    ..Default::default()
                },
                fetcher: Some(fetcher),
                ..Default::default()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let sources = present::render_sources(&outcome);
    assert!(
        sources.contains("[1]") && sources.contains("retrieved") && sources.contains("published"),
        "{sources}"
    );
    assert!(
        sources.contains("model-dependent") && sources.contains("SUPPORTED"),
        "{sources}"
    );
    let answer = present::render_answer(&outcome);
    assert!(
        answer.contains("Sources:") && answer.contains("/sources"),
        "{answer}"
    );
    // Reopening later says the sources were not re-checked.
    let note = ai_orchestra::ux::chat::staleness_note(
        &outcome,
        ai_orchestra::history::now_ms() + 90 * 86_400_000,
        30,
    )
    .unwrap();
    assert!(
        note.contains("does not re-check") && note.contains("may have changed"),
        "{note}"
    );
    assert!(
        ai_orchestra::ux::chat::staleness_note(&outcome, ai_orchestra::history::now_ms(), 30)
            .unwrap()
            .contains("0 day")
    );
}

#[test]
fn every_setting_key_round_trips_through_its_name() {
    for key in Key::ALL {
        assert_eq!(Key::parse(key.name()), Some(key));
    }
    let _ = PathBuf::new();
}
