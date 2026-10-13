//! `ai-orchestra eval ...`: datasets, offline and live evaluation runs, blind comparison, reports
//! and feedback. Offline commands never call a provider. `eval run --live` prints its plan first and
//! needs `--yes-run-live` to go on.

use std::path::{Path, PathBuf};

use ai_orchestra::error::OrchestraError;
use ai_orchestra::eval::blind;
use ai_orchestra::eval::dataset::{self, Arm, Dataset};
use ai_orchestra::eval::feedback::{Feedback, FeedbackStore, Rating};
use ai_orchestra::eval::live::{self, LiveAuthorization, LiveLimits};
use ai_orchestra::eval::report;
use ai_orchestra::eval::run::{Choice, EvalStore, Evaluator, EvaluatorKind, run_offline};
use ai_orchestra::history::format_utc;
use ai_orchestra::{Config, Orchestrator};
use clap::Subcommand;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Fmt {
    Text,
    Json,
}

#[derive(Subcommand)]
pub enum EvalCommand {
    /// List the built-in datasets and the saved evaluation runs
    List {
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Check a dataset (a built-in name or a JSON file) and print every problem found
    Validate { dataset: String },
    /// Evaluate a dataset. `--offline` uses the responses recorded in it and calls no provider;
    /// `--live` calls the providers (prints its plan, then needs --yes-run-live)
    Run {
        /// A built-in dataset name (`starter`) or the path of a dataset file
        dataset: String,
        #[arg(long, conflicts_with = "live")]
        offline: bool,
        #[arg(long)]
        live: bool,
        /// Comma-separated arms: single:claude, single:codex, single:google, fast, balanced, thorough, auto
        /// (offline: default = every recorded arm; live: required)
        #[arg(long, value_delimiter = ',', value_name = "ARM,ARM")]
        arms: Vec<String>,
        /// Live only: comma-separated case ids (default: every case)
        #[arg(long, value_delimiter = ',', value_name = "ID,ID")]
        cases: Vec<String>,
        /// Live only: most agent calls one consultation may start
        #[arg(long, default_value_t = 10)]
        max_calls: usize,
        /// Live only: refuse a run whose worst case exceeds this many agent calls
        #[arg(long, default_value_t = 150)]
        max_total_calls: usize,
        /// Live only: per-agent-call timeout in seconds
        #[arg(long)]
        timeout: Option<u64>,
        /// Live only: confirms that you want the plan above to run and spend subscription quota
        #[arg(long)]
        yes_run_live: bool,
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Show the report of a run (results grouped by kind, rubric, citations, quality vs efficiency, blind comparison)
    Report {
        run: String,
        /// Seed of the bootstrap intervals (default 1)
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Put two runs side by side (warns about different datasets or kinds of results)
    Compare { run_a: String, run_b: String },
    /// Apply a dataset's checks again to the answers a run already holds (no provider is called);
    /// for when the checks were corrected after the run. Saves a new run and leaves the old one
    Rescore {
        run: String,
        /// The dataset to score against (default: the run's own, as it is built in now)
        #[arg(long)]
        dataset: Option<String>,
    },
    /// Show a case of a dataset, and its results in a run
    Inspect {
        case_id: String,
        #[arg(long, default_value = "starter")]
        dataset: String,
        #[arg(long)]
        run: Option<String>,
    },
    /// Write a run as JSON
    Export {
        run: String,
        #[arg(long, default_value = "json")]
        format: String,
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
    },
    /// Prepare a blind pairwise comparison of two arms of a run and print what a judge sees
    Blind {
        run: String,
        /// Pairs to compare, e.g. single-codex:thorough,single-codex:balanced (each `A:B`; write a single-agent arm as `single-codex`, e.g. `single-codex:thorough`)
        #[arg(long, value_delimiter = ',', value_name = "A:B")]
        pairs: Vec<String>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Record one judgment of a blind item (the identity of the answers stays hidden until `report`)
    Judge {
        run: String,
        item: String,
        /// first, second, tie or insufficient
        #[arg(long)]
        choice: String,
        /// Who is judging
        #[arg(long)]
        evaluator: String,
        /// human (default) or ai. An AI judgment is labelled, kept apart and never ground truth;
        /// a model that produced one of the answers cannot judge it
        #[arg(long, default_value = "human")]
        kind: String,
        #[arg(long, default_value = "")]
        why: String,
    },
    /// Give feedback on a consultation, or list / delete what you gave (stored locally; changes nothing by itself)
    Feedback {
        consultation_id: Option<String>,
        /// helpful, not-helpful, inaccurate, incomplete, bad-citation, slow, verbose or unsatisfactory
        #[arg(long)]
        rating: Option<String>,
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long)]
        list: bool,
        #[arg(long, value_name = "FEEDBACK-ID")]
        delete: Option<String>,
        #[arg(long)]
        clear: bool,
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
}

fn json<T: serde::Serialize>(v: &T) -> Result<String, OrchestraError> {
    serde_json::to_string_pretty(v).map_err(|e| OrchestraError::Config(e.to_string()))
}

fn store(config: &Config) -> Result<EvalStore, OrchestraError> {
    ai_orchestra::eval::runs_dir(config)
        .map(EvalStore::new)
        .ok_or_else(|| OrchestraError::Config("cannot determine where evaluations are kept (no home directory); set memory.path in the config".into()))
}

fn feedback_store(config: &Config) -> Result<FeedbackStore, OrchestraError> {
    ai_orchestra::eval::feedback_path(config)
        .map(FeedbackStore::new)
        .ok_or_else(|| {
            OrchestraError::Config(
                "cannot determine where feedback is kept (no home directory)".into(),
            )
        })
}

fn load_dataset(arg: &str) -> Result<Dataset, OrchestraError> {
    if let Some(text) = dataset::builtin(arg) {
        return Dataset::parse(text);
    }
    let path = Path::new(arg);
    let text = std::fs::read_to_string(path).map_err(|e| {
        OrchestraError::Config(format!(
            "cannot read dataset `{arg}`: {e} (built-in: {})",
            dataset::builtin_names().join(", ")
        ))
    })?;
    Dataset::parse(&text)
}

fn parse_arms(list: &[String]) -> Result<Vec<Arm>, OrchestraError> {
    list.iter().map(|a| Arm::parse(a)).collect()
}

/// `a:b` pairs; an arm like `single:codex` is written `single-codex` here.
fn parse_pairs(list: &[String]) -> Result<Vec<(Arm, Arm)>, OrchestraError> {
    list.iter()
        .map(|p| {
            let (a, b) = p.split_once(':').ok_or_else(|| {
                OrchestraError::Config(format!("a pair looks like A:B, got `{p}`"))
            })?;
            let fix = |s: &str| s.replacen("single-", "single:", 1);
            Ok((Arm::parse(&fix(a))?, Arm::parse(&fix(b))?))
        })
        .collect()
}

pub async fn run(
    cmd: EvalCommand,
    config: &Config,
    orchestrator: Option<Orchestrator>,
) -> Result<u8, OrchestraError> {
    match cmd {
        EvalCommand::List { output } => {
            let runs = store(config).map(|s| s.list()).unwrap_or_default();
            match output {
                Fmt::Json => println!(
                    "{}",
                    json(&serde_json::json!({
                        "datasets": dataset::builtin_names(),
                        "runs": runs.iter().map(|r| serde_json::json!({
                            "id": r.id, "created_ms": r.created_ms, "kind": r.kind, "dataset": r.dataset_id,
                            "dataset_version": r.dataset_version, "arms": r.arms, "cases": r.cases.len(),
                            "provenance": r.provenance_classes(), "synthetic": r.synthetic,
                        })).collect::<Vec<_>>(),
                    }))?
                ),
                Fmt::Text => {
                    println!(
                        "Built-in datasets: {}  (a dataset file path also works)",
                        dataset::builtin_names().join(", ")
                    );
                    if runs.is_empty() {
                        println!(
                            "No evaluation runs yet. `ai-orchestra eval run starter --offline` makes one without calling any provider."
                        );
                    }
                    for r in runs {
                        println!(
                            "{}  {}  {:?}  {} v{}  {} case(s)  arms: {}  results: {}",
                            r.id,
                            format_utc(r.created_ms),
                            r.kind,
                            r.dataset_id,
                            r.dataset_version,
                            r.cases.len(),
                            r.arms
                                .iter()
                                .map(|a| a.0.clone())
                                .collect::<Vec<_>>()
                                .join(","),
                            r.provenance_classes()
                                .iter()
                                .map(|p| p.as_str())
                                .collect::<Vec<_>>()
                                .join("+")
                        );
                    }
                }
            }
        }
        EvalCommand::Validate { dataset } => {
            let text = match dataset::builtin(&dataset) {
                Some(t) => t.to_string(),
                None => std::fs::read_to_string(&dataset)?,
            };
            let parsed: Dataset = serde_json::from_str(&text)
                .map_err(|e| OrchestraError::Config(format!("not a dataset: {e}")))?;
            let problems = parsed.problems();
            if problems.is_empty() {
                println!(
                    "{} v{}: valid ({} case(s), {} categories, hash {}).",
                    parsed.id,
                    parsed.dataset_version,
                    parsed.cases.len(),
                    parsed.categories().len(),
                    parsed.content_hash()
                );
            } else {
                for p in &problems {
                    println!("problem: {p}");
                }
                return Ok(2);
            }
        }
        EvalCommand::Run {
            dataset,
            offline,
            live: live_run,
            arms,
            cases,
            max_calls,
            max_total_calls,
            timeout,
            yes_run_live,
            output,
        } => {
            let ds = load_dataset(&dataset)?;
            if !offline && !live_run {
                return Err(OrchestraError::Config("choose --offline (recorded responses, no provider is called) or --live (calls the providers)".into()));
            }
            let arms = parse_arms(&arms)?;
            let run = if offline {
                run_offline(&ds, (!arms.is_empty()).then_some(arms.as_slice()))?
            } else {
                if arms.is_empty() {
                    return Err(OrchestraError::Config(
                        "--live needs --arms (for example --arms single:codex,balanced,thorough)"
                            .into(),
                    ));
                }
                let orchestrator = orchestrator
                    .ok_or_else(|| OrchestraError::Config("internal: no orchestrator".into()))?;
                let limits = LiveLimits {
                    max_calls_per_consultation: max_calls,
                    max_total_calls,
                    timeout_secs: timeout,
                };
                let selection = (!cases.is_empty()).then_some(cases.as_slice());
                let plan = live::plan(&orchestrator, &ds, selection, &arms, &limits)?;
                print!("{}", plan.render());
                if !yes_run_live {
                    println!(
                        "\nNothing was run. To run exactly this plan, repeat the command with --yes-run-live."
                    );
                    return Ok(0);
                }
                if plan.max_total > max_total_calls {
                    return Err(OrchestraError::Config(format!(
                        "worst case {} calls is above --max-total-calls {max_total_calls}",
                        plan.max_total
                    )));
                }
                let cancel = CancellationToken::new();
                let on_ctrl_c = cancel.clone();
                tokio::spawn(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        on_ctrl_c.cancel();
                    }
                });
                live::run(
                    &orchestrator,
                    &ds,
                    selection,
                    &arms,
                    &limits,
                    &LiveAuthorization::explicitly_granted_by_the_user(),
                    &cancel,
                )
                .await?
            };
            let path = store(config)?.save(&run)?;
            match output {
                Fmt::Json => println!("{}", json(&run)?),
                Fmt::Text => {
                    print!("{}", report::render(&run, 1));
                    eprintln!("\n[saved as {} · {}]", run.id, path.display());
                }
            }
        }
        EvalCommand::Report { run, seed, output } => {
            let r = store(config)?.load(&run)?;
            match output {
                Fmt::Json => println!(
                    "{}",
                    json(
                        &serde_json::json!({"run": r.id, "summaries": report::summarize(&r), "tradeoffs": report::tradeoffs(&r), "pairs": blind::results(&r, seed)})
                    )?
                ),
                Fmt::Text => print!("{}", report::render(&r, seed)),
            }
        }
        EvalCommand::Rescore { run, dataset } => {
            let s = store(config)?;
            let old = s.load(&run)?;
            let ds = load_dataset(dataset.as_deref().unwrap_or(&old.dataset_id))?;
            let new = ai_orchestra::eval::run::rescore(&old, &ds)?;
            let path = s.save(&new)?;
            print!("{}", report::render(&new, 1));
            eprintln!(
                "\n[re-scored from {} · saved as {} · {}]",
                old.id,
                new.id,
                path.display()
            );
        }
        EvalCommand::Compare { run_a, run_b } => {
            let s = store(config)?;
            print!("{}", report::compare(&s.load(&run_a)?, &s.load(&run_b)?));
        }
        EvalCommand::Inspect {
            case_id,
            dataset,
            run,
        } => {
            let ds = load_dataset(&dataset)?;
            let case = ds.case(&case_id).ok_or_else(|| {
                OrchestraError::Config(format!("no case `{case_id}` in {}", ds.id))
            })?;
            println!(
                "{} [{}]{}",
                case.id,
                case.category.as_str(),
                if case.objective {
                    " objective"
                } else {
                    " subjective"
                }
            );
            println!("question: {}", case.question);
            println!("instructions: {}", case.instructions);
            if !case.reference_basis.is_empty() {
                println!("reference basis: {}", case.reference_basis);
            }
            for e in &case.evidence {
                println!("evidence [{}] {} - {}", e.id, e.title, e.url);
            }
            if let Some(run) = run {
                let r = store(config)?.load(&run)?;
                if let Some(c) = r.cases.iter().find(|c| c.case_id == case_id) {
                    for a in &c.arms {
                        println!(
                            "\n--- {} ({}) {} ---",
                            a.arm,
                            a.provenance.as_str(),
                            a.status
                        );
                        println!("{}", a.answer);
                        for ck in &a.evaluation.checks {
                            println!(
                                "  check {:?} {}: {}",
                                ck.kind,
                                ck.id,
                                if ck.ok { "ok" } else { "FAILED" }
                            );
                        }
                        for cr in &a.evaluation.criteria {
                            println!("  {:<32} {:?} - {}", cr.id, cr.outcome, cr.detail);
                        }
                        if let Some(c) = &a.evaluation.citation {
                            println!(
                                "  citations: {} (structurally valid {}), lexically unsupported {}, links outside the source list {}. {}",
                                c.checks.len(),
                                c.structurally_valid,
                                c.unsupported,
                                c.unlisted_urls.len(),
                                c.method
                            );
                        }
                    }
                } else {
                    println!("\n(this run has no result for that case)");
                }
            }
        }
        EvalCommand::Export { run, format, file } => {
            if format != "json" {
                return Err(OrchestraError::Config(
                    "only --format json is supported".into(),
                ));
            }
            let text = json(&store(config)?.load(&run)?)?;
            match file {
                Some(p) => {
                    use std::io::Write as _;
                    let mut opts = std::fs::OpenOptions::new();
                    opts.write(true).create(true).truncate(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        opts.mode(0o600);
                    }
                    opts.open(&p)?.write_all(text.as_bytes())?;
                    println!("Wrote {} (owner-only).", p.display());
                }
                None => println!("{text}"),
            }
        }
        EvalCommand::Blind {
            run,
            pairs,
            seed,
            output,
        } => {
            let s = store(config)?;
            let mut r = s.load(&run)?;
            let pairs = if pairs.is_empty() {
                return Err(OrchestraError::Config(
                    "name the pairs to compare, for example --pairs single-codex:thorough".into(),
                ));
            } else {
                parse_pairs(&pairs)?
            };
            if r.blind.is_none() {
                let n = blind::prepare(&mut r, &pairs, seed)?;
                s.save(&r)?;
                eprintln!("Prepared {n} blind item(s) with seed {seed}.");
            }
            let packet = blind::packet(&r)?;
            match output {
                Fmt::Json => println!("{}", json(&packet)?),
                Fmt::Text => {
                    for p in &packet {
                        println!(
                            "=== {} ===\nQuestion: {}\nWhat to look at: {}\n\n--- Answer 1 ({} words)\n{}\n\n--- Answer 2 ({} words)\n{}\n",
                            p.item_id,
                            p.question,
                            p.instructions,
                            p.first.words,
                            p.first.text.trim(),
                            p.second.words,
                            p.second.text.trim()
                        );
                    }
                    println!(
                        "Judge with: ai-orchestra eval judge {} <item> --choice first|second|tie|insufficient --evaluator <you> --why \"...\"\nA longer or better-formatted answer tends to win whatever its quality: judge against the instructions.",
                        r.id
                    );
                }
            }
        }
        EvalCommand::Judge {
            run,
            item,
            choice,
            evaluator,
            kind,
            why,
        } => {
            let s = store(config)?;
            let mut r = s.load(&run)?;
            let choice = Choice::parse(&choice).ok_or_else(|| {
                OrchestraError::Config("choice must be first, second, tie or insufficient".into())
            })?;
            let kind = match kind.as_str() {
                "human" => EvaluatorKind::Human,
                "ai" => EvaluatorKind::Ai,
                other => {
                    return Err(OrchestraError::Config(format!(
                        "kind must be human or ai, got `{other}`"
                    )));
                }
            };
            blind::record(
                &mut r,
                &item,
                choice,
                Evaluator {
                    kind,
                    name: evaluator,
                },
                &why,
            )?;
            s.save(&r)?;
            println!(
                "Recorded. The identity of the answers stays hidden until `eval report {}`.",
                r.id
            );
        }
        EvalCommand::Feedback {
            consultation_id,
            rating,
            note,
            list,
            delete,
            clear,
            output,
        } => {
            let fs = feedback_store(config)?;
            if clear {
                println!("Deleted {} feedback entr(ies).", fs.clear()?);
            } else if let Some(id) = delete {
                println!(
                    "{}",
                    if fs.delete(&id)? {
                        "Deleted."
                    } else {
                        "No such feedback."
                    }
                );
            } else if list || consultation_id.is_none() {
                let all = match &consultation_id {
                    Some(c) => fs.for_consultation(c)?,
                    None => fs.list()?,
                };
                match output {
                    Fmt::Json => println!("{}", json(&all)?),
                    Fmt::Text if all.is_empty() => println!("No feedback stored."),
                    Fmt::Text => {
                        for f in all {
                            println!(
                                "{}  {}  {}  {}{}",
                                f.id,
                                format_utc(f.created_ms),
                                f.consultation_id,
                                f.rating.name(),
                                if f.note.is_empty() {
                                    String::new()
                                } else {
                                    format!("  \"{}\"", f.note)
                                }
                            );
                        }
                    }
                }
            } else if let Some(c) = consultation_id {
                let rating = rating.as_deref().and_then(Rating::parse).ok_or_else(|| {
                    OrchestraError::Config(format!(
                        "--rating must be one of: {}",
                        Rating::NAMES.join(", ")
                    ))
                })?;
                let id = fs.add(Feedback::new(c.clone(), rating, &note))?;
                println!(
                    "Stored as {id} for consultation {c}. It changes nothing by itself; --list shows it and --delete {id} removes it."
                );
            }
        }
    }
    Ok(0)
}
