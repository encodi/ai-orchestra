# AI Orchestra

[![CI](https://github.com/encodi/ai-orchestra/actions/workflows/ci.yml/badge.svg)](https://github.com/encodi/ai-orchestra/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**Ask several AI coding assistants the same question, let them review each other, and get one considered
answer that keeps the disagreements.** AI Orchestra is a local-first, multi-agent orchestrator written in
Rust. It does not talk to any model API: it drives the **official terminal CLIs** you already have installed
and signed in to, as managed subprocesses.

| Agent | CLI it drives | Provider id |
|---|---|---|
| Anthropic Claude Code | `claude` | `claude` |
| OpenAI Codex CLI | `codex` | `codex` |
| Google Antigravity | `agy` | `google` |

*Español: [README.es.md](README.es.md)*

![The Agent Theater replaying a recorded consultation](docs/screenshots/desktop-dark-theater-record.png)

> The screenshots in this README were taken with the project's **simulated agents** (the fake CLIs used by the
> browser tests), so the text in them is placeholder text, not real model output. The interface is available in
> Spanish and English (toggle in the sidebar); the images show the Spanish one.

## Why

- **One question, several independent answers.** Each agent answers on its own first, then reviews the others,
  and a coordinator writes the final answer, listing what is uncertain and where the agents disagreed.
- **Your subscriptions, no API keys.** Requests go through the CLIs you already pay for. No keys, tokens or
  credential files are read, stored or forwarded, and `*_API_KEY` variables are scrubbed from the child
  environment so a stray key cannot move a CLI onto usage-based billing.
- **Read-only by default.** Agents cannot write files unless you pass an explicit flag.
- **Agreement is not verification.** In research mode AI Orchestra fetches the pages itself through a hardened
  fetcher, has a model judge only passages it cut out of those pages, and writes the citations from its own
  records, never from a model.
- **Honest numbers.** Token figures are the CLIs' own; anything else is labelled as an estimate. Failed or
  partial runs are never reported as complete.
- **Local only.** The web interface listens on loopback, validates `Host`/`Origin`, and nothing leaves your
  machine except what the CLIs themselves send.

## What you get

| | |
|---|---|
| `ai-orchestra consult` | Smart consultation: picks the strategy (`fast`, `balanced`, `thorough`), the agents and the rounds for you, within call/time budgets, and says why. Everything can be overridden. |
| `ai-orchestra debate` | A bounded debate: the coordinator proposes, the others review, optionally a second round, then a synthesis. |
| Research and fact checking | `--web`, `--depth`, `--url`, `--verify`: sources and claim statuses (`SUPPORTED`, `CONTRADICTED`, …) computed from retrieved evidence. |
| Chat | `ai-orchestra` in a terminal: ask, follow up, change settings with `/commands`, resume sessions. |
| Memory | Local SQLite + FTS5 memory that adds a small, labelled block of relevant context to follow-ups. Written only when you ask. |
| MCP server | `ai-orchestra mcp serve` lets Claude Code, Codex or Antigravity use the *other* agents as tools. |
| Web interface | `ai-orchestra web`: chat, **Agent Theater**, history, research explorer, reports, memory and settings. |
| Evaluation | `ai-orchestra eval`: measures whether several agents beat one, with blind comparison and statistics that refuse to compare small samples. |

## Install

You need a recent stable [Rust](https://rustup.rs) toolchain (edition 2024; developed on 1.99) and at least
one of the agent CLIs above, already logged in. Node.js is **not** needed: the compiled web interface is
committed (`web/dist`) and embedded in the binary.

```sh
git clone https://github.com/encodi/ai-orchestra.git
cd ai-orchestra
cargo build --release
# the binary: target/release/ai-orchestra   (put it on your PATH if you like)
```

Check what is available (this sends no prompt and uses no quota):

```sh
./target/release/ai-orchestra doctor
```

### Optional configuration

Everything works with defaults. To pin models or the coordinator, copy the example and edit it:

```sh
cp ai-orchestra.example.toml ai-orchestra.toml
```

Lookup order: `--config <file>`, then `$AI_ORCHESTRA_CONFIG`, then `./ai-orchestra.toml`, then
`~/.config/ai-orchestra/config.toml`. Mind that **`$AI_ORCHESTRA_CONFIG` wins over `./ai-orchestra.toml`**: if a
leftover variable points at an old file, your project settings are silently ignored.

## Use it

### From the terminal

```sh
# One question, everything automatic
ai-orchestra consult "Which oscilloscope is best for learning electronics?"

# Show the plan and the call budget without calling any agent
ai-orchestra consult "Compare Rust and Go for a CLI tool" --dry-run

# Choose things yourself
ai-orchestra consult "Compare Rust and C++" --mode decision --strategy thorough
ai-orchestra consult "..." --coordinator codex --agents claude,codex,google --rounds 2
ai-orchestra consult "..." --max-agent-calls 4 --max-duration 120

# Research with sources and claim checking
ai-orchestra consult "What changed recently in quantum computing?" --mode research --depth deep
ai-orchestra consult "The Model X has 100 MHz of bandwidth" --verify --url https://maker.example/spec

# A bounded debate, following it live
ai-orchestra debate "What is the best architecture for a Rust MCP server?" --live

# Conversation mode, history and one agent only
ai-orchestra
ai-orchestra history search "oscilloscope"
ai-orchestra ask claude "Explain ownership in Rust"
```

The answer goes to stdout and diagnostics to stderr, so the output pipes cleanly. Exit codes distinguish
success, a partial result (something failed), authentication and quota problems, and timeouts. The complete
command reference is in [docs/reference.md](docs/reference.md).

### From the browser

```sh
ai-orchestra web --open     # http://127.0.0.1:8765
```

![Chat with live progress and the pixel stage](docs/screenshots/desktop-dark-chat-progress.png)

- **Chat** with live progress, cancellation and a form for every consultation setting (saved defaults, this
  session, this question, or automatic).
- **Agent Theater**: the consultation as a conversation between pixel characters on a stage. Speech bubbles
  are the real recorded messages, never invented dialogue; past consultations can be replayed.
- **Summarise**: one explicit call to the coordinator, labelled as written by that agent; it never replaces
  the report.

![A summary written by the coordinator](docs/screenshots/desktop-dark-summary.png)

- **Characters**: give each agent a name, a short personality and a look. It shapes tone only, never what an
  agent is allowed to do.

![Editing the characters](docs/screenshots/desktop-dark-characters.png)

- **Research & sources**: what AI Orchestra retrieved, separated from what agents merely said.

![Research and sources](docs/screenshots/desktop-dark-research.png)

- **Reports**: measured data from your own consultations, including the tokens each CLI reported per agent.
  Remaining quota is *not* shown because no CLI exposes it without a model call.

![Reports with usage per agent](docs/screenshots/desktop-dark-reports-usage.png)

More in the [web interface guide](docs/web-ui-user-guide.md).

### As an MCP server

```sh
ai-orchestra mcp config claude    # prints setup instructions for a host; registers nothing
```

Details and the safety layers (the host agent is never callable through its own server) are in
[docs/mcp.md](docs/mcp.md).

## Safety model in short

AI Orchestra never uses a shell (it builds argument vectors), never executes anything an agent produced,
keeps agents read-only unless you opt in, redacts secrets before saving anything, treats web pages and
recalled memory as **data, never instructions**, and refuses recursion (an agent cannot start an
orchestration from inside an orchestration). The full list of rules is in [AGENTS.md](AGENTS.md).

## Project status

This is a young project. It is tested with mock agents, fake CLIs, temporary databases and a real browser, and
parts of it were run live against the real CLIs by the maintainer; the record of what was and was not verified
live is in [docs/validation.md](docs/validation.md).

- Developed and tested on **macOS**. Linux is exercised by the CI; Windows is untested.
- The CLIs change often. Adapters follow each CLI's documented flags, and a CLI update can break one of them:
  please open an issue with the output of `ai-orchestra doctor`.
- In the maintainer's own small blind evaluations, several agents were **not** shown to give more correct
  answers than one (see [docs/evaluation-benchmarks.md](docs/evaluation-benchmarks.md)). The tool is built to
  surface disagreement and uncertainty, and to measure instead of assume.

## Documentation

| | |
|---|---|
| [docs/reference.md](docs/reference.md) | CLI reference, configuration, architecture, limitations |
| [docs/smart-orchestration.md](docs/smart-orchestration.md) | Strategies, budgets, decision traces |
| [docs/research.md](docs/research.md) | Research, the fetcher and claim checking |
| [docs/memory-architecture.md](docs/memory-architecture.md) | Memory and context assembly |
| [docs/user-experience.md](docs/user-experience.md) | The terminal chat |
| [docs/quality-evaluation.md](docs/quality-evaluation.md) | The evaluation system |
| [docs/mcp.md](docs/mcp.md) | The MCP server |
| [docs/web-ui-user-guide.md](docs/web-ui-user-guide.md), [architecture](docs/web-ui-architecture.md), [Agent Theater](docs/agent-theater.md) | The web interface |

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first: it explains the branch workflow,
the checks every change must pass, and the hard rules (no model APIs, no live AI calls in tests, read-only by
default). Please follow the [Code of Conduct](CODE_OF_CONDUCT.md). Security issues: see
[SECURITY.md](SECURITY.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in this project, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.

Copyright © 2026 Enrique C.

*Claude Code, Codex and Antigravity are products of their respective owners. This project is independent and
is not affiliated with or endorsed by Anthropic, OpenAI or Google.*
