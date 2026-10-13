mod chat_cmd;
mod eval_cmd;
mod memory_cmd;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use ai_orchestra::consult::{
    ConsultOptions, parse_agents_arg, parse_coordinator_arg, parse_length_arg, parse_mode_arg,
    parse_rounds_arg,
};
use ai_orchestra::debate::{DebateEvent, DebateOptions, DebateStatus, ModelOverride};
use ai_orchestra::history::{DebateStore, render_history};
use ai_orchestra::memory::service::{MemoryRequest, MemoryService};
use ai_orchestra::progress::{EventLog, render_live, render_progress};
use ai_orchestra::research::plan::{Depth, ResearchRequest, WebMode};
use ai_orchestra::smart::profiles::ProfileStore;
use ai_orchestra::smart::strategy::parse_strategy_arg;
use ai_orchestra::{AccessMode, AgentRequest, Config, OrchestraError, Orchestrator};
use clap::{Parser, Subcommand, ValueEnum};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

/// Exit code of `debate` when it produced a result but something failed
/// (a reviewer, a round, or the synthesis). The result is still printed.
const EXIT_PARTIAL: u8 = 5;

#[derive(Parser)]
#[command(
    name = "ai-orchestra",
    version,
    about = "Orchestrate the official Claude Code, Codex and Antigravity CLIs",
    long_about = "Runs the AI coding CLIs you have already installed and signed in to. \
                  No API keys are used and no credentials are read."
)]
struct Cli {
    /// Path to a config file (default: $AI_ORCHESTRA_CONFIG, ./ai-orchestra.toml,
    /// then ~/.config/ai-orchestra/config.toml)
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Increase log verbosity (-v info, -vv debug). RUST_LOG overrides.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// The project that memory belongs to: a directory or a name (default: the working directory)
    #[arg(long, global = true, value_name = "PATH|NAME")]
    project: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

// The command line is parsed once per process, so the size of the largest variant (`consult` has
// many flags) does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Check which agent CLIs are installed and signed in (sends no prompts)
    Doctor {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Send one prompt to one agent
    Ask {
        /// Provider: claude, codex, google (alias: agy)
        provider: String,
        /// The prompt, or `-` to read it from stdin
        prompt: String,
        /// Let the agent modify files in the working directory (default: read-only)
        #[arg(long)]
        allow_write: bool,
        /// Timeout in seconds (default: from config)
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Model passed to the CLI (default: providers.<name>.model, else the CLI's own)
        #[arg(long)]
        model: Option<String>,
        /// Reasoning effort passed to the CLI, e.g. low, medium, high
        /// (default: providers.<name>.effort, else the CLI's own)
        #[arg(long)]
        effort: Option<String>,
        /// Working directory for the agent (default: current directory)
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
        /// Print the full normalized response as JSON
        #[arg(long)]
        json: bool,
    },
    /// Have the agents debate a question: the coordinator proposes, the others
    /// review it in parallel, optionally reconsider after reading each other's
    /// reviews, and the coordinator writes the final synthesis
    ///
    /// A round is one parallel pass in which every reviewer answers once.
    /// Round 1: independent reviews. Round 2 (optional): each reviewer reads all
    /// round-1 reviews and reconsiders. Exit code 5 means a partial result.
    Debate {
        /// The question to discuss, or `-` to read it from stdin
        question: String,
        /// Agent that leads: claude, codex or google (default: general.coordinator)
        #[arg(long)]
        coordinator: Option<String>,
        /// Comma-separated reviewers (default: every other available agent)
        #[arg(long, value_delimiter = ',', value_name = "A,B")]
        peers: Option<Vec<String>>,
        /// Review rounds: 1 or 2 (default: debate.rounds from the config, 1)
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=2))]
        rounds: Option<u8>,
        /// Per-call timeout in seconds (default: from config)
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Working directory for the agents (default: current directory)
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
        /// Model for one agent, repeatable: --model claude=opus --model codex=gpt-6-luna
        #[arg(long = "model", value_name = "AGENT=MODEL")]
        models: Vec<String>,
        /// Reasoning effort for one agent, repeatable: --effort codex=low --effort claude=medium
        #[arg(long = "effort", value_name = "AGENT=LEVEL")]
        efforts: Vec<String>,
        /// Output format of the result on stdout
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
        /// Same as --output json
        #[arg(long, hide = true)]
        json: bool,
        /// Also print every proposal and review after the final result
        #[arg(long)]
        transcript: bool,
        /// Show the full text of each step on stderr as it completes
        /// (instead of the short progress lines)
        #[arg(long)]
        live: bool,
        /// Silence the progress lines on stderr
        #[arg(short, long)]
        quiet: bool,
        /// Append every step as JSON Lines to FILE (created owner-only), so you
        /// can follow along from another terminal with `tail -f FILE`
        #[arg(long, value_name = "FILE")]
        log_file: Option<PathBuf>,
        /// Do not save this debate to the history
        #[arg(long)]
        no_save: bool,
    },
    /// Ask a question and let the agents work it out: decisions, research, reviews,
    /// lessons or debates, with the mode, coordinator, agents and rounds chosen
    /// automatically unless you set them
    ///
    /// Several agents answer independently, then review each other's answers (1 or 2
    /// rounds), and the coordinator writes one answer that keeps their disagreements.
    /// Agreement between models is not proof of truth, and agents are read-only and
    /// do not browse or verify sources. Exit code 5 means a partial result.
    /// `ai-orchestra "your question"` is a shorthand for `ai-orchestra consult "..."`.
    Consult {
        /// The question, or `-` to read it from stdin
        question: String,
        /// auto, decision, research, review, learn or debate
        #[arg(long, default_value = "auto", value_name = "MODE")]
        mode: String,
        /// auto, claude, codex or google (must be one of --agents)
        #[arg(long, default_value = "auto", value_name = "AGENT")]
        coordinator: String,
        /// auto, 1 or 2 cross-review rounds
        #[arg(long, default_value = "auto", value_name = "N")]
        rounds: String,
        /// Web research: auto (when the question needs outside facts), on (fail if impossible)
        /// or off (no network use at all)
        #[arg(long, default_value = "auto", value_name = "MODE")]
        web: String,
        /// Research depth: auto, quick, standard or deep
        #[arg(long, default_value = "auto", value_name = "LEVEL")]
        depth: String,
        /// Most sources to retrieve (1-20; default depends on the depth)
        #[arg(long, value_name = "N")]
        max_sources: Option<usize>,
        /// An HTTPS page to use as a source, repeatable. It is fetched by AI Orchestra itself.
        #[arg(long = "url", value_name = "HTTPS-URL")]
        urls: Vec<String>,
        /// Flag sources whose newest stated date is older than this many days
        #[arg(long, value_name = "DAYS")]
        freshness_days: Option<u32>,
        /// Treat the text as ONE claim to verify against sources instead of a question to answer
        /// (needs --url or an agent that can search the web)
        #[arg(long)]
        verify: bool,
        /// auto, brief or full. auto = brief when the question asks for a short answer
        /// ("in two sentences", "briefly"), otherwise the full report
        #[arg(long, default_value = "auto", value_name = "STYLE")]
        length: String,
        /// auto, or a comma-separated list such as claude,codex,google
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "auto",
            value_name = "A,B"
        )]
        agents: Vec<String>,
        /// Per-call timeout in seconds (default: from config)
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Working directory for the agents (default: current directory)
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
        /// Model for one agent, repeatable: --model claude=opus
        #[arg(long = "model", value_name = "AGENT=MODEL")]
        models: Vec<String>,
        /// Reasoning effort for one agent, repeatable: --effort codex=low
        #[arg(long = "effort", value_name = "AGENT=LEVEL")]
        efforts: Vec<String>,
        /// Output format of the result on stdout
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
        /// Also print every answer and review after the final result
        #[arg(long)]
        transcript: bool,
        /// Show the full text of each step on stderr as it completes
        #[arg(long)]
        live: bool,
        /// Silence the routing and progress lines on stderr
        #[arg(short, long)]
        quiet: bool,
        /// Append every step as JSON Lines to FILE (created owner-only)
        #[arg(long, value_name = "FILE")]
        log_file: Option<PathBuf>,
        /// Do not save this consultation to the history, the provider history or memory
        #[arg(long)]
        no_save: bool,
        /// What may be remembered and recalled: auto (this project's session and project memory;
        /// the default), off, session, project or global (adds your global memory)
        #[arg(long, value_name = "MODE")]
        memory: Option<String>,
        /// Continue this AI Orchestra session (see `session list`); it must belong to this project
        #[arg(long, value_name = "ID")]
        session: Option<String>,
        /// Most (estimated) tokens of recalled context to send with the question
        #[arg(long, value_name = "TOKENS")]
        context_budget: Option<usize>,
        /// Print what was recalled and why, what was left out, and the estimated sizes
        #[arg(long)]
        explain_context: bool,
        /// Show how the question would be routed (mode, coordinator, agents, rounds,
        /// strategy, number of calls and limits) and exit without calling any agent or
        /// fetching any page. `--dry-run` is the same.
        #[arg(long, alias = "dry-run")]
        plan: bool,
        /// How much AI work to use: auto (default; chosen from the question), fast (one agent,
        /// the evidence check kept), balanced (one agent, a review only if the question or the
        /// evidence calls for it) or thorough (independent perspectives, evidence check and up to
        /// two cross-review rounds). Your --mode, --coordinator, --agents, --rounds and --depth
        /// always win over what a strategy would choose.
        #[arg(long, value_name = "STRATEGY")]
        strategy: Option<String>,
        /// Start at most this many agent calls (research planning and verification included)
        #[arg(long, value_name = "N")]
        max_agent_calls: Option<usize>,
        /// Stop starting new work after this many seconds; finished results are kept and the
        /// result is marked partial
        #[arg(long, value_name = "SECS")]
        max_duration: Option<u64>,
        /// Run at most this many of this consultation's agent calls at the same time
        #[arg(long, value_name = "N")]
        max_parallel: Option<usize>,
        /// Print the orchestration plan (strategy, flow, calls, limits and the reasons) before
        /// running, even with --quiet
        #[arg(long)]
        explain_plan: bool,
    },
    /// See, search, add, correct, export and delete what AI Orchestra remembers (calls no agent)
    Memory {
        #[command(subcommand)]
        command: memory_cmd::MemoryCommand,
    },
    /// List, inspect, continue and clear AI Orchestra sessions (not provider CLI sessions)
    Session {
        #[command(subcommand)]
        command: memory_cmd::SessionCommand,
    },
    /// Show what each agent CLI can do for web research and how well that is known (sends nothing)
    Capabilities {
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
    /// Fetch public pages the way research does and show what was retrieved: status, final URL,
    /// title, dates, hash and a text preview. No agent is called and no quota is used.
    Inspect {
        /// HTTPS URLs to retrieve
        #[arg(required = true)]
        urls: Vec<String>,
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
    /// Run AI Orchestra as an MCP server so Claude Code, Codex CLI or Antigravity
    /// can use the other agents as tools
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Talk to AI Orchestra: ask, follow up, change settings, look at sources and disagreements
    /// (also what plain `ai-orchestra` does in a terminal)
    Chat {
        /// Continue this AI Orchestra session instead of starting a new one
        #[arg(long, value_name = "ID")]
        session: Option<String>,
        /// Start with a setting, repeatable: --set mode=decision --set strategy=balanced
        #[arg(long = "set", value_name = "SETTING=VALUE")]
        settings: Vec<String>,
    },
    /// Start the local web interface (a browser UI on this machine; the terminal keeps working)
    Web {
        /// Port to listen on (default: [web] port, 8765)
        #[arg(long)]
        port: Option<u16>,
        /// Address to listen on. Only loopback is accepted unless --allow-non-local is given
        #[arg(long, default_value = "127.0.0.1")]
        host: std::net::IpAddr,
        /// Allow a non-loopback address. The interface has no user accounts: only on a network you trust
        #[arg(long)]
        allow_non_local: bool,
        /// Open the page in your default browser
        #[arg(long)]
        open: bool,
    },
    /// Measure whether a multi-agent answer is better than a single agent's: datasets, offline
    /// evaluation, blind comparison, reports, feedback (live runs need --yes-run-live)
    Eval {
        #[command(subcommand)]
        command: eval_cmd::EvalCommand,
    },
    /// List saved debates and consultations, newest first (or `history search "words"`)
    History {
        /// Maximum number of debates to list
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
        #[command(subcommand)]
        command: Option<HistoryCommand>,
    },
    /// Show one saved debate or consultation (full id or a unique prefix of it)
    Show {
        id: String,
        /// Also print every proposal and review
        #[arg(long)]
        transcript: bool,
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
    /// `ai-orchestra "a question"` is a shorthand for `consult` (only a quoted,
    /// multi-word question is accepted, so a mistyped command never spends quota)
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Subcommand)]
enum HistoryCommand {
    /// Find saved consultations whose question or answer contains every word (no model is called)
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve MCP over stdio (stdout carries only protocol messages; logs go to
    /// stderr). Normally started by the MCP host, not by hand.
    Serve {
        /// The agent that launches this server: claude, codex or google. It is
        /// never called through the server (default: mcp.host from the config,
        /// else detected from the environment)
        #[arg(long)]
        host: Option<String>,
    },
    /// Print copy-paste setup commands for a host. Registers nothing.
    Config {
        /// Host to print instructions for: claude, codex or google
        host: String,
    },
}

fn init_logging(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

/// Parses repeated `AGENT=VALUE` flags into per-agent overrides.
fn parse_overrides(
    models: &[String],
    efforts: &[String],
) -> Result<BTreeMap<String, ModelOverride>, OrchestraError> {
    let mut map: BTreeMap<String, ModelOverride> = BTreeMap::new();
    for (flag, items) in [("--model", models), ("--effort", efforts)] {
        for item in items {
            let (agent, value) = item
                .split_once('=')
                .filter(|(a, v)| !a.is_empty() && !v.is_empty())
                .ok_or_else(|| {
                    OrchestraError::Config(format!(
                        "{flag} for a debate must look like AGENT=VALUE, got `{item}`"
                    ))
                })?;
            let slot = map.entry(agent.to_lowercase()).or_default();
            if flag == "--model" {
                slot.model = Some(value.to_string());
            } else {
                slot.effort = Some(value.to_string());
            }
        }
    }
    Ok(map)
}

fn read_prompt(arg: &str, max: usize) -> Result<String, OrchestraError> {
    if arg != "-" {
        return Ok(arg.to_string());
    }
    let mut buf = Vec::new();
    // Read one byte past the limit so oversized input is detected, not truncated.
    std::io::stdin()
        .lock()
        .take(max as u64 + 1)
        .read_to_end(&mut buf)?;
    if buf.len() > max {
        return Err(OrchestraError::PromptTooLarge {
            len: buf.len(),
            max,
            target: "stdin".to_string(),
        });
    }
    String::from_utf8(buf).map_err(|_| OrchestraError::Config("stdin is not valid UTF-8".into()))
}

/// A token that is cancelled on Ctrl-C, which makes the running agents be
/// terminated cleanly instead of being orphaned.
fn cancel_on_ctrl_c() -> CancellationToken {
    let cancel = CancellationToken::new();
    let on_ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_ctrl_c.cancel();
        }
    });
    cancel
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, OrchestraError> {
    serde_json::to_string_pretty(value).map_err(|e| OrchestraError::Config(e.to_string()))
}

/// Starts the task that prints progress (and writes the JSON Lines log). Opens the log first so
/// a bad path fails before any agent is called.
#[allow(clippy::type_complexity)]
fn start_progress(
    live: bool,
    quiet: bool,
    log_file: Option<&std::path::Path>,
) -> Result<
    (
        Option<tokio::sync::mpsc::UnboundedSender<DebateEvent>>,
        Option<tokio::task::JoinHandle<()>>,
    ),
    OrchestraError,
> {
    let mut log = match log_file {
        Some(path) => Some(EventLog::create(path).map_err(|e| {
            OrchestraError::Config(format!("cannot open log file {}: {e}", path.display()))
        })?),
        None => None,
    };
    let show_progress = live || !quiet;
    if !show_progress && log.is_none() {
        return Ok((None, None));
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        let mut log_warned = false;
        while let Some(event) = rx.recv().await {
            if live {
                eprintln!("{}", render_live(&event));
            } else if show_progress && let Some(line) = render_progress(&event) {
                eprintln!("{line}");
            }
            if let Some(log) = log.as_mut()
                && let Err(e) = log.write(&event)
                && !log_warned
            {
                log_warned = true;
                eprintln!("warning: cannot write to the log file: {e}");
            }
        }
    });
    Ok((Some(tx), Some(printer)))
}

/// Attaches the local provider performance history, unless it is switched off in the config.
fn with_profile_store(orchestrator: Orchestrator, config: &Config) -> Orchestrator {
    match config.resolved_profiles_path() {
        Some(path) => {
            orchestrator.with_profiles(ProfileStore::new(path, config.smart.profile_settings()))
        }
        None => orchestrator,
    }
}

struct MemoryFlags {
    memory: Option<String>,
    session: Option<String>,
    context_budget: Option<usize>,
    explain_context: bool,
    no_save: bool,
}

/// Attaches the memory service and turns the memory flags into a request. A memory that cannot be
/// opened is a warning, not a failure: the consultation then runs without it. Explicit flags that
/// cannot be honoured are errors, so nothing is silently ignored.
fn build_memory_request(
    config: &Config,
    flags: MemoryFlags,
    project: Option<&str>,
) -> Result<(Option<Arc<MemoryService>>, Option<MemoryRequest>), OrchestraError> {
    use ai_orchestra::memory::model::{MemoryMode, parse_memory_arg};
    if flags.context_budget == Some(0) {
        return Err(OrchestraError::Config(
            "--context-budget must be greater than 0".into(),
        ));
    }
    let mode = match parse_memory_arg(flags.memory.as_deref())? {
        Some(m) => m,
        None => config.memory.mode()?,
    };
    let wants_memory = mode != MemoryMode::Off;
    if !wants_memory && (flags.session.is_some() || flags.context_budget.is_some()) {
        return Err(OrchestraError::Config(
            "--session and --context-budget need memory; they cannot be combined with --memory off"
                .into(),
        ));
    }
    if !wants_memory && !flags.explain_context {
        return Ok((None, None));
    }
    let svc = match memory_cmd::service_for(config) {
        Ok(s) => s,
        Err(e) if flags.memory.is_none() && flags.session.is_none() => {
            eprintln!("warning: memory is unavailable ({e}); continuing without it");
            return Ok((None, None));
        }
        Err(e) => return Err(e),
    };
    let mut req = MemoryRequest::new(mode);
    req.session = flags.session;
    req.context_budget_tokens = flags.context_budget;
    req.explain = flags.explain_context;
    req.write = !flags.no_save;
    req.project = project.map(memory_cmd::project_ref);
    Ok((Some(svc), Some(req)))
}

fn history_store(config: &Config) -> Result<DebateStore, OrchestraError> {
    config
        .debate
        .resolved_history_dir()
        .map(DebateStore::new)
        .ok_or_else(|| {
            OrchestraError::Config(
                "cannot determine where debates are saved (no home directory); \
                 set debate.history_dir in the config"
                    .into(),
            )
        })
}

async fn run(cli: Cli) -> Result<u8, OrchestraError> {
    let (config, config_path) = Config::load(cli.config.as_deref())?;
    if let Some(path) = &config_path {
        tracing::info!(path = %path.display(), "loaded config");
    }

    // History commands only read saved files; they never touch an agent.
    match &cli.command {
        Command::History {
            command:
                Some(HistoryCommand::Search {
                    query,
                    limit,
                    output,
                }),
            ..
        } => {
            let hits = history_store(&config)?.search(query, *limit);
            match output {
                OutputFormat::Json => println!("{}", to_json(&hits)?),
                OutputFormat::Text if hits.is_empty() => println!("Nothing matches."),
                OutputFormat::Text => {
                    for h in &hits {
                        println!(
                            "{}  {}  [{}]  {}\n    ...{}...",
                            h.summary.id,
                            ai_orchestra::history::format_utc(h.summary.created_at_ms),
                            h.summary.status,
                            h.summary.question.chars().take(80).collect::<String>(),
                            h.snippet
                        );
                    }
                }
            }
            return Ok(0);
        }
        Command::History { limit, output, .. } => {
            let listing = history_store(&config)?.list();
            match output {
                OutputFormat::Text => print!("{}", render_history(&listing, *limit)),
                OutputFormat::Json => println!(
                    "{}",
                    to_json(&listing.debates.iter().take(*limit).collect::<Vec<_>>())?
                ),
            }
            return Ok(0);
        }
        Command::Show {
            id,
            transcript,
            output,
        } => {
            let debate = history_store(&config)?.load(id)?;
            match output {
                OutputFormat::Text => {
                    print!("{}", debate.render_summary());
                    if *transcript {
                        println!("\n---\n\n{}", debate.render_transcript());
                    }
                }
                OutputFormat::Json => println!("{}", to_json(&debate)?),
            }
            return Ok(0);
        }
        Command::Mcp {
            command: McpCommand::Config { host },
        } => {
            let binary = std::env::current_exe()?;
            let explicit = cli
                .config
                .as_deref()
                .map(std::fs::canonicalize)
                .transpose()?;
            print!(
                "{}",
                ai_orchestra::mcp::setup_instructions(host, &binary, explicit.as_deref())?
            );
            return Ok(0);
        }
        _ => {}
    }

    let mut orchestrator = Orchestrator::new(config.clone())?;
    let project_arg = cli.project.clone();

    match cli.command {
        Command::Chat { session, settings } => {
            if config.debate.save {
                match history_store(&config) {
                    Ok(store) => orchestrator = orchestrator.with_history(store),
                    Err(e) => eprintln!("warning: consultations will not be saved: {e}"),
                }
            }
            orchestrator = with_profile_store(orchestrator, &config);
            if config.memory.mode()? != ai_orchestra::memory::model::MemoryMode::Off {
                match memory_cmd::service_for(&config) {
                    Ok(svc) => orchestrator = orchestrator.with_memory(svc),
                    Err(e) => eprintln!(
                        "warning: memory is unavailable ({e}); follow-ups will not recall earlier answers"
                    ),
                }
            }
            let mut presets = Vec::new();
            for item in &settings {
                let (k, v) = item.split_once('=').ok_or_else(|| {
                    OrchestraError::Config(format!("--set needs SETTING=VALUE, got `{item}`"))
                })?;
                let key = ai_orchestra::ux::settings::Key::parse(k)
                    .ok_or_else(|| OrchestraError::Config(format!("unknown setting `{k}`")))?;
                presets.push((key, v.to_string()));
            }
            chat_cmd::run(orchestrator, config.clone(), session, presets).await
        }
        Command::Eval { command } => eval_cmd::run(command, &config, Some(orchestrator)).await,
        Command::Web {
            port,
            host,
            allow_non_local,
            open,
        } => {
            if config.debate.save {
                match history_store(&config) {
                    Ok(store) => orchestrator = orchestrator.with_history(store),
                    Err(e) => eprintln!("warning: consultations will not be saved: {e}"),
                }
            }
            orchestrator = with_profile_store(orchestrator, &config);
            if config.memory.mode()? != ai_orchestra::memory::model::MemoryMode::Off {
                match memory_cmd::service_for(&config) {
                    Ok(svc) => orchestrator = orchestrator.with_memory(svc),
                    Err(e) => eprintln!(
                        "warning: memory is unavailable ({e}); the Memory page will say so"
                    ),
                }
            }
            let port = port.unwrap_or(config.web.port);
            let listener = ai_orchestra::web::bind(&ai_orchestra::web::ServeOptions {
                host,
                port,
                allow_non_local,
            })
            .await?;
            let bound = listener.local_addr()?;
            let state = ai_orchestra::web::AppState::new(
                Arc::new(orchestrator),
                config.clone(),
                bound.port(),
            );
            let url = format!(
                "http://{}:{}/",
                if bound.ip().is_loopback() && bound.ip().is_ipv4() {
                    "127.0.0.1".to_string()
                } else {
                    bound.ip().to_string()
                },
                bound.port()
            );
            println!("AI Orchestra web interface: {url}");
            println!(
                "Local only. Nothing is sent to an agent until you ask a question there. Press Ctrl-C to stop."
            );
            if !ai_orchestra::web::assets::has_frontend() {
                eprintln!(
                    "note: this build has no web interface bundled; build it with `cd web && npm install && npm run build`, then `cargo build --release`."
                );
            }
            if open {
                let opener = if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                };
                let _ = std::process::Command::new(opener).arg(&url).spawn();
            }
            let shutdown = CancellationToken::new();
            let on_ctrl_c = shutdown.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                on_ctrl_c.cancel();
            });
            ai_orchestra::web::serve(listener, state, shutdown).await?;
            Ok(0)
        }
        Command::Memory { command } => memory_cmd::run_memory(command, project_arg, &config).await,
        Command::Session { command } => {
            memory_cmd::run_session(command, project_arg, &config).await
        }
        Command::Doctor { json } => {
            let report = orchestrator.doctor().await;
            if json {
                println!("{}", to_json(&report)?);
            } else {
                print!("{}", report.render());
            }
            if !report.any_usable() {
                return Err(OrchestraError::Config(
                    "no usable agent CLI found (see the report above)".into(),
                ));
            }
            Ok(0)
        }
        Command::Ask {
            provider,
            prompt,
            allow_write,
            timeout,
            model,
            effort,
            cwd,
            json,
        } => {
            let prompt = read_prompt(&prompt, orchestrator.config().general.max_prompt_bytes)?;
            let mut request = AgentRequest::new(prompt).with_access(if allow_write {
                AccessMode::AllowWrite
            } else {
                AccessMode::ReadOnly
            });
            if allow_write {
                eprintln!(
                    "note: write access enabled; the agent may modify files in the working directory"
                );
            }
            if let Some(secs) = timeout {
                if secs == 0 {
                    return Err(OrchestraError::Config(
                        "--timeout must be greater than 0".into(),
                    ));
                }
                request = request.with_timeout(Duration::from_secs(secs));
            }
            if let Some(model) = model {
                request = request.with_model(model);
            }
            if let Some(effort) = effort {
                request = request.with_effort(effort);
            }
            if let Some(cwd) = cwd {
                request = request.with_cwd(cwd);
            }

            let cancel = cancel_on_ctrl_c();

            let (_, response) = orchestrator.ask(&provider, request, &cancel).await?;
            if json {
                println!("{}", to_json(&response)?);
            } else {
                println!("{}", response.text);
                eprintln!(
                    "\n[{} · {} · {:.1}s{}{}]",
                    response.provider,
                    response.settings_label(),
                    response.duration_ms as f64 / 1000.0,
                    response
                        .usage
                        .as_ref()
                        .map(|u| format!(
                            " · tokens in/out {}/{}",
                            u.input_tokens.map_or("?".into(), |n| n.to_string()),
                            u.output_tokens.map_or("?".into(), |n| n.to_string())
                        ))
                        .unwrap_or_default(),
                    if response.truncated {
                        " · OUTPUT TRUNCATED"
                    } else {
                        ""
                    }
                );
            }
            Ok(0)
        }
        Command::Debate {
            question,
            coordinator,
            peers,
            rounds,
            timeout,
            cwd,
            models,
            efforts,
            output,
            json,
            transcript,
            live,
            quiet,
            log_file,
            no_save,
        } => {
            let question = read_prompt(&question, ai_orchestra::debate::QUESTION_MAX_BYTES)?;
            if timeout == Some(0) {
                return Err(OrchestraError::Config(
                    "--timeout must be greater than 0".into(),
                ));
            }
            let json = json || output == OutputFormat::Json;
            if config.debate.save && !no_save {
                orchestrator = orchestrator.with_history(history_store(&config)?);
            }
            let mut options = DebateOptions {
                question,
                coordinator,
                peers,
                rounds,
                cwd,
                timeout: timeout.map(Duration::from_secs),
                overrides: parse_overrides(&models, &efforts)?,
                exclude: Vec::new(),
                isolate: false,
                events: None,
                consultation: None,
                general_purpose: false,
                smart: None,
                style: None,
            };
            let (events, printer) = start_progress(live, quiet, log_file.as_deref())?;
            options.events = events;

            let cancel = cancel_on_ctrl_c();
            let result = orchestrator.debate(options, &cancel).await;
            // The sender is gone once the debate returns; let the printer drain.
            if let Some(printer) = printer {
                let _ = printer.await;
            }
            let outcome = result?;

            if json {
                println!("{}", to_json(&outcome)?);
            } else {
                println!("{}", outcome.final_answer);
                if transcript {
                    println!("\n---\n\n{}", outcome.render_transcript());
                }
            }
            for w in &outcome.warnings {
                eprintln!("warning: {w}");
            }
            if let Some(path) = &outcome.saved_to {
                eprintln!(
                    "\n[saved as {} · `ai-orchestra show {}` · {}]",
                    outcome.id,
                    outcome.id,
                    path.display()
                );
            }
            if outcome.status != DebateStatus::Complete {
                eprintln!(
                    "PARTIAL RESULT: the debate did not complete cleanly (see the warnings above); \
                     treat the output as incomplete. Exit code {EXIT_PARTIAL}."
                );
                return Ok(EXIT_PARTIAL);
            }
            Ok(0)
        }
        Command::Consult {
            question,
            mode,
            coordinator,
            rounds,
            web,
            depth,
            max_sources,
            urls,
            freshness_days,
            verify,
            length,
            agents,
            timeout,
            cwd,
            models,
            efforts,
            output,
            transcript,
            live,
            quiet,
            log_file,
            no_save,
            memory,
            session,
            context_budget,
            explain_context,
            plan,
            strategy,
            max_agent_calls,
            max_duration,
            max_parallel,
            explain_plan,
        } => {
            let question = read_prompt(&question, ai_orchestra::debate::QUESTION_MAX_BYTES)?;
            if timeout == Some(0) {
                return Err(OrchestraError::Config(
                    "--timeout must be greater than 0".into(),
                ));
            }
            let json = output == OutputFormat::Json;
            if config.debate.save && !no_save {
                orchestrator = orchestrator.with_history(history_store(&config)?);
            }
            if !no_save {
                orchestrator = with_profile_store(orchestrator, &config);
            }
            let (memory_service, memory_request) = build_memory_request(
                &config,
                MemoryFlags {
                    memory,
                    session,
                    context_budget,
                    explain_context,
                    no_save,
                },
                project_arg.as_deref(),
            )?;
            if let Some(svc) = memory_service {
                orchestrator = orchestrator.with_memory(svc);
            }
            for (name, value) in [
                ("--max-agent-calls", max_agent_calls.map(|n| n as u64)),
                ("--max-duration", max_duration),
                ("--max-parallel", max_parallel.map(|n| n as u64)),
            ] {
                if value == Some(0) {
                    return Err(OrchestraError::Config(format!(
                        "{name} must be greater than 0"
                    )));
                }
            }
            // --strategy beats the configured default; `off` in the configuration keeps the fixed
            // Milestone 4/5 orchestration for requests that name no strategy.
            let strategy = match parse_strategy_arg(strategy.as_deref())? {
                Some(s) => Some(s),
                None => config.smart.strategy()?,
            };
            let mut options = ConsultOptions {
                strategy,
                max_agent_calls,
                max_duration: max_duration.map(Duration::from_secs),
                max_parallel,
                question,
                mode: parse_mode_arg(Some(&mode))?,
                coordinator: parse_coordinator_arg(Some(&coordinator)),
                agents: parse_agents_arg(&agents)?,
                rounds: parse_rounds_arg(Some(&rounds))?,
                length: parse_length_arg(Some(&length))?,
                research: ResearchRequest {
                    web: WebMode::parse(Some(&web))?,
                    depth: Depth::parse(Some(&depth))?,
                    max_sources,
                    urls,
                    freshness_days,
                },
                claim_check: verify,
                fetcher: None,
                cwd,
                timeout: timeout.map(Duration::from_secs),
                overrides: parse_overrides(&models, &efforts)?,
                exclude: Vec::new(),
                isolate: false,
                events: None,
                memory: memory_request,
                style: None,
            };
            // Routing is deterministic and free: show it before any agent is called.
            let decision = orchestrator.plan_consultation(&options)?;
            if explain_plan && !plan {
                for line in decision.render().lines() {
                    eprintln!("[plan] {line}");
                }
            }
            if plan {
                // Retrieval only: reads memory, calls no agent, writes nothing.
                let prepared = if explain_context {
                    orchestrator.preview_context(&options).await?
                } else {
                    None
                };
                if json {
                    println!("{}", to_json(&decision)?);
                } else {
                    print!("{}", decision.render());
                }
                if explain_context {
                    match prepared {
                        Some(p) => print!("\n{}", p.report.render_text()),
                        None => println!(
                            "\nContext: memory is off for this request, so nothing would be recalled."
                        ),
                    }
                }
                return Ok(0);
            }
            if !quiet && !explain_plan {
                for line in decision.render().lines() {
                    eprintln!("[routing] {line}");
                }
            }
            let (events, printer) = start_progress(live, quiet, log_file.as_deref())?;
            options.events = events;

            let cancel = cancel_on_ctrl_c();
            let result = orchestrator.consult(options, &cancel).await;
            if let Some(printer) = printer {
                let _ = printer.await;
            }
            let outcome = result?;

            if json {
                println!("{}", to_json(&outcome)?);
            } else {
                println!("{}", outcome.final_answer);
                if transcript {
                    println!("\n---\n\n{}", outcome.render_transcript());
                }
            }
            for w in &outcome.warnings {
                eprintln!("warning: {w}");
            }
            if explain_context {
                match outcome
                    .consultation
                    .as_ref()
                    .and_then(|m| m.context.as_ref())
                {
                    Some(ctx) => eprintln!("\n{}", ctx.render_text()),
                    None => eprintln!("\nContext: memory was not used for this consultation."),
                }
            }
            if let Some(path) = &outcome.saved_to {
                eprintln!(
                    "\n[saved as {} · `ai-orchestra show {}` · {}]",
                    outcome.id,
                    outcome.id,
                    path.display()
                );
            }
            if outcome.status != DebateStatus::Complete {
                eprintln!(
                    "PARTIAL RESULT: the consultation did not complete cleanly (see the warnings \
                     above); treat the output as incomplete. Exit code {EXIT_PARTIAL}."
                );
                return Ok(EXIT_PARTIAL);
            }
            Ok(0)
        }
        Command::Capabilities { output } => {
            let rows: Vec<(
                String,
                ai_orchestra::research::capabilities::ResearchCapabilities,
            )> = orchestrator
                .registry()
                .all()
                .iter()
                .map(|p| (p.id().to_string(), p.research_capabilities()))
                .collect();
            match output {
                OutputFormat::Text => print!(
                    "{}",
                    ai_orchestra::research::capabilities::render_matrix(&rows)
                ),
                OutputFormat::Json => println!("{}", to_json(&rows)?),
            }
            Ok(0)
        }
        Command::Inspect { urls, output } => {
            use ai_orchestra::research::source::{Discovery, DiscoveryMethod, SourceSet};
            let fetcher = std::sync::Arc::new(
                ai_orchestra::research::fetch::SourceFetcher::new(config.research.fetch_policy())
                    .map_err(|e| OrchestraError::Config(e.to_string()))?,
            );
            let cancel = cancel_on_ctrl_c();
            let mut set = SourceSet::new();
            let mut ids = Vec::new();
            for u in &urls {
                match set.add_candidate(
                    u,
                    Discovery {
                        method: DiscoveryMethod::UserProvided,
                        provider: None,
                    },
                ) {
                    Some(id) => ids.push((id, u.clone())),
                    None => return Err(OrchestraError::Config(format!("`{u}` is not a URL"))),
                }
            }
            let targets: Vec<String> = ids
                .iter()
                .filter_map(|(id, _)| set.get(*id).map(|r| r.original_url.clone()))
                .collect();
            let results = fetcher.fetch_all(&targets, &cancel).await;
            for ((id, _), (_, result)) in ids.iter().zip(results) {
                set.apply_fetch(*id, result);
            }
            set.mark_duplicates();
            match output {
                OutputFormat::Json => println!("{}", to_json(&set.records())?),
                OutputFormat::Text => {
                    for r in set.records() {
                        println!("[{}] {}", r.id, r.original_url);
                        println!(
                            "    status:     {:?}{}",
                            r.status,
                            r.error
                                .as_deref()
                                .map(|e| format!(" ({e})"))
                                .unwrap_or_default()
                        );
                        if let Some(u) = &r.final_url {
                            println!("    final URL:  {u}");
                        }
                        println!(
                            "    title:      {}",
                            r.title.as_deref().unwrap_or("(none found)")
                        );
                        println!(
                            "    published:  {}   updated: {}",
                            r.published.as_deref().unwrap_or("not stated"),
                            r.updated.as_deref().unwrap_or("not stated")
                        );
                        if let Some(h) = &r.content_hash {
                            println!(
                                "    sha256:     {h}   ({} characters of text)",
                                r.text_chars
                            );
                        }
                        println!("    kind:       {} — {}", r.kind.label(), r.kind_note);
                        if !r.text_preview.is_empty() {
                            println!(
                                "    preview:    {}",
                                r.text_preview.chars().take(300).collect::<String>()
                            );
                        }
                        for l in &r.limitations {
                            println!("    limitation: {l}");
                        }
                    }
                }
            }
            if set.records().iter().all(|r| !r.is_retrieved()) {
                return Ok(1);
            }
            Ok(0)
        }
        Command::Mcp {
            command: McpCommand::Serve { host },
        } => {
            // stdout belongs to the protocol: nothing below may print to it.
            // A host may start us with a minimal environment (no HOME), so a missing
            // history directory must not stop the server: get_debate then reports
            // that debates are not being saved.
            if config.debate.save {
                match history_store(&config) {
                    Ok(store) => orchestrator = orchestrator.with_history(store),
                    Err(e) => tracing::warn!("debates will not be saved: {e}"),
                }
            }
            orchestrator = with_profile_store(orchestrator, &config);
            if config.memory.mode()? != ai_orchestra::memory::model::MemoryMode::Off {
                match memory_cmd::service_for(&config) {
                    Ok(svc) => orchestrator = orchestrator.with_memory(svc),
                    Err(e) => tracing::warn!("memory will not be available: {e}"),
                }
            }
            let settings = ai_orchestra::mcp::McpSettings::from_config(
                &config,
                &orchestrator,
                host.as_deref(),
            )?;
            ai_orchestra::mcp::serve_stdio(std::sync::Arc::new(orchestrator), settings).await?;
            Ok(0)
        }
        // Handled before the orchestrator was built.
        Command::History { .. } | Command::Show { .. } | Command::Mcp { .. } => Ok(0),
        Command::External(_) => Err(OrchestraError::Config(
            "internal error: the question shorthand was not expanded".into(),
        )),
    }
}

/// `ai-orchestra "a question" [options]` means `ai-orchestra consult "a question" [options]`.
/// Only a multi-word question is accepted so that a mistyped subcommand is an error, not a
/// request that spends subscription quota.
fn expand_shorthand(cli: Cli) -> Cli {
    let Command::External(args) = &cli.command else {
        return cli;
    };
    let first = args.first().map(String::as_str).unwrap_or("");
    if !first.contains(char::is_whitespace) {
        clap::Error::raw(
            clap::error::ErrorKind::InvalidSubcommand,
            format!(
                "unrecognized subcommand '{first}'\n\nTo ask a question, quote it: \
                 ai-orchestra \"your question here\"\n"
            ),
        )
        .exit();
    }
    let mut argv = vec!["ai-orchestra".to_string(), "consult".to_string()];
    argv.extend(args.iter().cloned());
    match Cli::try_parse_from(argv) {
        Ok(mut parsed) => {
            parsed.config = cli.config.or(parsed.config);
            parsed.verbose = parsed.verbose.max(cli.verbose);
            parsed
        }
        Err(e) => e.exit(),
    }
}

/// Parses the command line. With no subcommand, a terminal gets the interactive chat; anything
/// else (a script, a pipe) gets the usual usage error, exactly as before.
fn parse_cli() -> Cli {
    use std::io::IsTerminal;
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e)
            if e.kind() == clap::error::ErrorKind::MissingSubcommand
                && std::io::stdin().is_terminal()
                && std::io::stdout().is_terminal() =>
        {
            let mut args: Vec<String> = std::env::args().collect();
            args.push("chat".to_string());
            Cli::try_parse_from(args).unwrap_or_else(|e| e.exit())
        }
        Err(e) => e.exit(),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = expand_shorthand(parse_cli());
    init_logging(cli.verbose);
    match run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}
