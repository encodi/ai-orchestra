# AI Orchestra: reference (CLI, configuration, milestones)

A local-first, multi-agent orchestrator written in Rust. It drives the **official
terminal CLIs** of several AI coding assistants as managed subprocesses:

| Provider id | CLI | Alias |
|---|---|---|
| `claude` | Anthropic Claude Code (`claude`) | |
| `codex` | OpenAI Codex CLI (`codex`) | |
| `google` | Google Antigravity CLI (`agy`) | `agy`, `antigravity` |

**What it is:** a general-purpose multi-AI consultation and decision-support tool. Ask a question
(`ai-orchestra consult "Which oscilloscope is best for learning electronics?"`) and several
independent assistants answer it, challenge each other, and one of them writes a considered answer
that keeps the disagreements and says what is uncertain. Everything is automatic by default and
everything can be overridden.

**Research, not just agreement (Milestone 5).** Several assistants agreeing is not verification. With
research on, AI Orchestra retrieves real pages itself through a hardened fetcher, has a model judge
only passages it cut out of those pages, computes each claim's status (`SUPPORTED`, `CONTRADICTED`,
`MIXED_EVIDENCE`, `INSUFFICIENT_EVIDENCE`, `NOT_CHECKED`) from that, and writes the citations and the
list of sources from its own records, never from a model. See [`docs/research.md`](research.md).

**Smart orchestration (Milestone 6).** More agents is not automatically better. `consult` now decides
how much AI work a question needs: `--strategy auto` (default) picks `fast` (one agent, the evidence
check kept), `balanced` (one agent, with an independent review or a further agent added only when the
question or the evidence calls for it) or `thorough` (independent perspectives, evidence check and up
to two cross-review rounds), and records why. Limits (`--max-agent-calls`, `--max-duration`,
`--max-parallel`) are enforced while it runs, `--dry-run` shows the plan without calling anything, and
what you set yourself (`--mode`, `--coordinator`, `--agents`, `--rounds`, `--depth`) is never
overridden. See [`docs/smart-orchestration.md`](smart-orchestration.md) and the (simulated)
[`docs/performance-benchmarks.md`](performance-benchmarks.md).

**Memory and context (Milestone 7).** A follow-up should not have to resend the whole earlier
conversation. AI Orchestra keeps a local SQLite database (FTS5 full-text search, no embeddings, no cloud)
of what earlier consultations concluded, what the user said they want and what was checked against
sources, and puts a **bounded, labelled block** of the relevant part before the question - the original
question is preserved and the block says it is data, not instructions. Persistent memory is written only
when you ask; consultations write session memory, and `--memory off` / `--no-save` switch that off. See
[`docs/memory-architecture.md`](memory-architecture.md) and
[`docs/context-optimization.md`](context-optimization.md).

**Everyday use and quality evaluation (Milestone 8).** `ai-orchestra` in a terminal opens a conversation
(`ai-orchestra chat` works anywhere): ask, follow up, change the mode, coordinator, agents, rounds, depth, web
use, strategy, memory or output with `/commands` (or leave them AUTO), look at sources, each agent's answer and
the disagreements, resume earlier sessions and search the history. Results are shown in the shape of their mode
(a recommendation first for a decision, findings and sources for research) with the caveats kept. A separate
`eval` command group measures whether several agents answer better than one - with datasets that carry their
own reference evidence, deterministic checks, citation checks that separate *valid* from *supported*, reproducible
blind pairwise comparison, statistics that refuse to compare small samples, and reports that never merge
fixture, mock and live results. It calls no provider unless you pass `--yes-run-live`. **One authorized live run
(six easy cases, Codex only) found no difference in correctness between one agent and the `balanced` and `thorough`
strategies, which cost 1.8x and 6.3x the calls. A second run on six harder cases (59 calls) was again passed 6/6
by every arm, and an AI judge (labelled, not ground truth) split about evenly. One human judge, blind, preferred `thorough` over the single
agent in 6 of 6 comparisons and split evenly on `balanced` - too few and too lopsided in length to say that
`thorough` answers better.**
See [`docs/user-experience.md`](user-experience.md), [`docs/quality-evaluation.md`](quality-evaluation.md)
and [`docs/evaluation-benchmarks.md`](evaluation-benchmarks.md).

**Local web interface (Milestone 9).** `ai-orchestra web` serves a browser interface at
`http://127.0.0.1:8765` (loopback only; nothing leaves your machine and no model API is involved) over the same
engines: a chat with live progress and cancellation, a visual form for every consultation setting (kept apart
as saved defaults, this session, this question and automatic), an **Agent Theater** that shows a consultation
as a conversation between three pixel robots using only the messages and events that were really recorded
(with replay of past consultations), history with search and replay, a research and sources explorer that
separates what AI Orchestra retrieved from what agents merely said, reports, memory management and provider
status. The frontend (React + TypeScript) is compiled and embedded in the binary; Node is only needed to
rebuild it. See [`docs/web-ui-user-guide.md`](web-ui-user-guide.md),
[`docs/web-ui-architecture.md`](web-ui-architecture.md) and [`docs/agent-theater.md`](agent-theater.md).

**v0.9.1 additions.** An animated pixel stage above the chat (the characters walk by stage and speak bubbles that
are real recorded messages), a conversation view in which reviewers address each other by name, a reply-language
rule that stops answers mixing English and Spanish, a "summarise" button (one explicit call to the coordinator),
editable characters (name, personality, look) and tokens per agent in Reports. See
[`docs/web-ui-user-guide.md`](web-ui-user-guide.md).

**Status:** Milestone 9 (**local web interface**) is implemented and tested with mock agents, fake CLIs and a
real browser; no real model was called while building it. Milestone 8 (**chat, settings, presentation, quality evaluation**) is implemented and tested with
scripted agents, fake CLIs and temporary stores; the optional browser UI was assessed and deferred. Milestone 7 (**memory, context assembly and token optimization**: SQLite + FTS5 memory,
sessions, `memory` / `session` commands, `--memory`, `--session`, `--context-budget`, `--explain-context`,
six MCP memory tools) is implemented and tested with scripted agents, fake CLIs and temporary databases;
it was run live against Codex three times on 2026-10-12 (one run each: the recalled context reached the
prompt at roughly a third of the extra input tokens of pasting the previous answer by hand, with no quality
claim; the first research run exposed and fixed a false-positive in the personal-data check; see
[`docs/validation.md`](validation.md)). Milestone 6 (**smart orchestration**: strategies, adaptive deliberation, budgets, provider
history, quality gate, decision traces) is implemented and tested with scripted agents, fake CLIs
and a local web server; it was run against the real CLIs once (a fast / balanced / thorough comparison, see
`docs/performance-benchmarks.md`); its benchmarks are
simulated. Milestone 1 (`doctor`, `ask`), Milestone 2 (multi-agent `debate`, saved history,
`--live`, `--log-file`, per-agent model/effort settings) and Milestone 3 (`ai-orchestra mcp serve`,
a native MCP server) are validated live; see [`docs/validation.md`](validation.md) and
[`docs/mcp.md`](mcp.md). Milestone 5 (**research and fact checking**: `consult` with `--web`, `--depth`, `--url`, `--verify`,
`inspect`, `capabilities`, and the MCP tools `research`, `verify_claim`, `inspect_sources` and
`get_research_report`) is implemented and tested with mock agents, a local web server and real public
pages for the fetcher; **web search through the provider CLIs was run live once** (Claude and Codex expose their search
URLs; the findings, the source-ranking fix made after it and its live re-run are in [`docs/validation.md`](validation.md)). Milestone 4 (**`consult`**: automatic routing, five consultation
modes, independent perspectives with bounded cross-review, an MCP `consult` tool) is implemented and
tested with mocks and fake CLIs and then run live against all three CLIs: automatic routing to
decision, research and review, debate mode, the brief style, two review rounds, a forced timeout, and
the MCP `consult` tool from Claude Code, Codex and Antigravity (see [`docs/validation.md`](validation.md)).

## Authentication and cost model

AI Orchestra never talks to a model API. It spawns the CLI you already installed
and signed in to, so requests count against *your existing subscription*.

- No API keys are required, read, stored or forwarded.
- No OAuth tokens or credential files are read, copied or proxied.
- `API_KEY`-style environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
  `GEMINI_API_KEY`, ...) are **removed from the child environment** by default
  (`general.scrub_api_key_env`), so a stray key in your shell cannot silently
  move a CLI onto usage-based billing. `doctor` warns when such variables exist
  and when a CLI is logged in with an API key.
- Authentication and quota failures are detected and reported as such, with a
  distinct exit code (see below).

## Installation

Requires a recent stable Rust toolchain (developed on 1.99, edition 2024) and
whichever of the three CLIs you want to use, already logged in.

```sh
cargo build --release
# binary: target/release/ai-orchestra
```

## Usage

```sh
ai-orchestra doctor              # installed? version? logged in? (sends no prompts)
ai-orchestra doctor --json

ai-orchestra ask claude "Explain ownership in Rust"
ai-orchestra ask codex  "Review this architecture"
ai-orchestra ask google "Find potential design problems"

echo "long prompt..." | ai-orchestra ask claude -     # prompt from stdin
ai-orchestra ask claude "..." --json                  # normalized response as JSON
ai-orchestra ask codex  "..." --cwd ~/project --timeout 120 --model <model>
ai-orchestra ask claude "Fix the failing test" --allow-write   # explicit opt-in

ai-orchestra consult "Which oscilloscope is best for learning electronics?"   # everything automatic
ai-orchestra consult "Compare Rust and C++" --mode decision
ai-orchestra consult "Explain quantum entanglement" --mode learn
ai-orchestra consult "Research quantum computing" --mode research --rounds 2
ai-orchestra consult "..." --coordinator codex --agents claude,codex,google --rounds 2
ai-orchestra consult "..." --plan          # show the routing and the call budget; calls nothing
ai-orchestra consult "Explain electrical resistance" --strategy fast      # one agent, no review
ai-orchestra consult "Which oscilloscope should I buy?" --strategy balanced   # review only if it helps
ai-orchestra consult "Analyze this scientific claim" --strategy thorough  # full panel, two rounds
ai-orchestra consult "Compare these products" --strategy auto --explain-plan
ai-orchestra consult "..." --max-agent-calls 4 --max-duration 120 --max-parallel 2
ai-orchestra consult "..." --dry-run       # the plan and the reasons; no agent, no fetch
ai-orchestra consult "What changed recently in quantum computing?" --mode research --depth deep
ai-orchestra consult "Compare two oscilloscopes" --mode review --web on     # sources, claims, citations
ai-orchestra consult "Summarize and verify this study" --url https://example.org/study
ai-orchestra consult "The Model X has 100 MHz of bandwidth" --verify --url https://maker.example/spec
ai-orchestra inspect https://example.org/page   # fetch pages like research does; no agent, no quota
ai-orchestra capabilities                       # what each CLI can do for web research
ai-orchestra "Should I learn Rust or Go first?"   # shorthand for consult (quoted, multi-word)

ai-orchestra debate "What is the best architecture for a Rust MCP server?"
ai-orchestra debate "..." --coordinator codex --rounds 2        # claude | codex | google
ai-orchestra debate "..." --peers claude,google                  # choose the reviewers
ai-orchestra debate "..." --output json                          # the typed record on stdout
ai-orchestra debate "..." --transcript     # also print every proposal and review at the end
ai-orchestra debate "..." --live           # full text of each step as it completes (stderr)
ai-orchestra debate "..." --quiet          # no progress lines
ai-orchestra debate "..." --log-file run.jsonl   # follow from another terminal: tail -f run.jsonl
ai-orchestra debate "..." --no-save        # do not write this debate to the history

ai-orchestra history                       # saved debates, newest first (--limit N, --output json)
ai-orchestra show <debate-id>              # decision + timings (--transcript, --output json)

ai-orchestra mcp serve --host claude       # MCP server over stdio (started by the MCP host)
ai-orchestra mcp config claude             # print setup commands for a host (registers nothing)
```

Global options: `--config <file>`, `-v` / `-vv` for logs on stderr (`RUST_LOG`
also works). The answer goes to stdout; the footer line and diagnostics go to
stderr, so the output pipes cleanly.

### Chat and evaluation (Milestone 8)

```sh
ai-orchestra                      # in a terminal: the conversation (also: ai-orchestra chat [--session ID] [--set strategy=balanced])
ai-orchestra history search "oscilloscope"          # find earlier consultations by their words
ai-orchestra eval run starter --offline             # evaluate recorded responses; calls no provider
ai-orchestra eval report <run> | compare <a> <b> | inspect <case> --run <run> | export <run>
ai-orchestra eval blind <run> --pairs single-codex:thorough --seed 7   # blind packet; then: eval judge ...
ai-orchestra eval run comparison --live --arms single:codex,balanced,thorough   # prints the plan and stops
ai-orchestra eval run hard --live --arms single:codex,balanced,thorough         # 12 harder cases (~120 calls); same rules
ai-orchestra eval feedback <consultation-id> --rating inaccurate --note "..."   # --list / --delete <id>
```

In the chat: `/settings`, `/mode decision`, `/strategy balanced`, `/auto all`, `/ask --mode review <question>`,
`/sources`, `/answers`, `/disagreements`, `/why`, `/new`, `/resume <id>`, `/history words`, `/feedback helpful`,
`/exit`. Precedence: safety rules, then this question, then this session, then saved preferences (`/save`), then
AUTO - a value you set is never changed silently. Ctrl-C cancels the running consultation and keeps the chat;
a cancelled or partial run never says "complete". `eval` results are grouped by kind (fixture, mock, recorded
live, live) and a fixture report says first that it describes the fixtures; a live evaluation prints its plan
(calls per case and arm, the ceiling) and does nothing until `--yes-run-live`.

### Memory and context (Milestone 7)

```sh
# The second question reaches the agent with the first one's conclusion, labelled, within a budget.
ai-orchestra consult "Research three portable oscilloscopes for learning electronics and compare them."
ai-orchestra consult "Of the three above, which one is the best value for money?" --explain-context

ai-orchestra consult "..." --memory off          # nothing recalled, nothing remembered (no database is created)
ai-orchestra consult "..." --no-save             # recall, but remember nothing
ai-orchestra consult "..." --context-budget 800  # at most ~800 estimated tokens of recalled context
ai-orchestra consult "..." --session ses-4b1f    # continue a particular AI Orchestra session
ai-orchestra consult "..." --plan --explain-context   # retrieval only: shows what would be sent, calls no agent

ai-orchestra memory add "My oscilloscope budget is 8000 MXN" --kind preference --topic budget   # persistent, on purpose
ai-orchestra memory list | search "budget" | show <id> [--original] | correct <id> "..." | forget <id>
ai-orchestra memory export [--file f.json] | clear --scope session | stats | status | check | prune | import-history
ai-orchestra session list | show | resume <id> | clear
```

| `--memory` | Reads | Remembers after a consultation |
|---|---|---|
| `auto` (default) | session + project | session memory |
| `off` | nothing | nothing |
| `session` / `project` | the session / session + project | session memory |
| `global` | session + project + global | session memory |

What to know: memory is **local, owner-only (`0600`) and not encrypted at rest**; credentials are never
stored; personal data needs `--allow-sensitive` and is never sent to an agent; `memory forget` really
overwrites the content. A newer explicit statement of yours supersedes an older one (the old one is kept,
marked, and not sent); facts that were checked against a source carry their retrieval date and an old price is
sent with a notice to re-check it. AI Orchestra's sessions are its own: **provider-native session resume is
not used** (the CLIs document it; it was not verified), `session resume` says so. Token figures are
**estimates** (bytes / 3), labelled as such; a smaller context is not automatically a better answer.
`[memory] default = "off"` in the config switches it all off for the CLI and the MCP server.

### Smart orchestration (Milestone 6)

| Strategy | What it does | Calls (3 agents, no research) |
|---|---|---|
| `fast` | one agent answers; the evidence check is kept; no review | 1 |
| `balanced` | one agent answers; an independent review only if the question or evidence calls for it; one escalation at most | 1 to 4 |
| `thorough` | all agents answer independently; evidence check; two cross-review rounds | 10 |
| `auto` (default) | chooses one of the above from the question, the agents and the limits, and says why | depends |

`--strategy` only sets defaults: whatever you name (`--mode`, `--coordinator`, `--agents`, `--rounds`,
`--depth`, `--web`, `--url`) stays as you named it, and a combination that cannot work (for example
`--agents a,b,c --rounds 2 --max-agent-calls 5`) is rejected before any agent is called. Limits are
enforced while it runs; when one is reached the work in flight is cancelled, what finished is kept and
the result is `partial`, never presented as complete or verified. A structural **quality gate**
checks that required evidence, valid citations, visible contradictions, the review you asked for and
honest status labels are present. The decision trace (`ai-orchestra show <id>`) records the plan, every
escalation or stop with its reason, the budget used and the time breakdown. `[smart]
default_strategy = "off"` in the config keeps the fixed Milestone 4/5 workflow. Details, the automatic
rules and the failure modes: [`docs/smart-orchestration.md`](smart-orchestration.md).

### Research and fact checking (Milestone 5)

`consult` researches when the question needs outside facts (`--web auto`, the default), always with
`--web on`, never with `--web off`. A basic educational question never browses. What happens:

1. an agent whose CLI has web search finds candidate pages (or you pass `--url`s; no search is
   needed then);
2. **AI Orchestra fetches the pages itself**, safely: HTTPS only, public addresses only (loopback,
   private, link-local, cloud-metadata and internal names are refused for the URL, every redirect and
   every DNS answer), robots.txt honored, bounded time and size, HTML and plain text only, nothing
   executed;
3. the agents answer from that evidence and list the claims they rely on;
4. each claim is checked against passages of the pages (agents that did not make it judge only those
   passages), and its status is **computed**; several pages repeating one origin count as one
   independent source;
5. the final answer cites `[n]` only recorded sources (fabricated citations are removed) and ends with
   a **generated** claim table, a **generated** list of sources (full URL, published / updated /
   retrieved dates, never invented) and the limitations.

`--depth quick|standard|deep` sets sources (3 / 6 / 12), agents, review rounds and verification calls;
`--max-sources`, `--url` (repeatable), `--freshness-days` refine it, and manual settings always win.
Everything, including the capability matrix for Claude Code, Codex and Antigravity and what is not
yet verified, is in [`docs/research.md`](research.md).

### How `consult` works (Milestone 4)

`consult` is the general entry point. **Automatic by default, full manual control when requested:**
the mode, coordinator, participating agents and review rounds are chosen from the question and
from which agents are available, and any of them can be set explicitly. An explicit choice is never
silently changed; when something cannot be honoured (an agent that is not installed, `--rounds`
with a single participant) the routing says so. The chosen routing is printed to stderr before any
agent is called (`--quiet` hides it, `--plan` prints it and stops), and saved with the result.

```
ai-orchestra consult "<question>" [--mode auto|decision|research|review|learn|debate]
                                  [--coordinator auto|claude|codex|google]
                                  [--agents auto|claude,codex,google] [--rounds auto|1|2]
                                  [--length auto|brief|full]
                                  [--timeout SECS] [--output text|json] [--plan]
                                  [--transcript] [--live] [--quiet] [--log-file F] [--no-save]
```

**Modes.** All five use the same engine; they differ in what the agents are asked to produce.

| Mode | For | The final answer has sections for |
|---|---|---|
| `decision` | choosing between alternatives | answer, recommendation, alternatives and criteria, objective evidence vs. personal preferences, trade-offs/costs/risks, how priorities change the recommendation |
| `research` | investigating a topic | answer, central question, established facts, hypotheses, opinions and contested claims, contradictions, source references, evidence limitations |
| `review` | products, technologies, services, articles, claims | answer, recommendation, criteria, strengths and weaknesses, alternatives, specifications vs. subjective assessments, outdated or unverified information |
| `learn` | teaching a subject | one coherent lesson, prerequisites, corrections made after independent review |
| `debate` | explicit deliberation | the Milestone 2 engine unchanged (proposal, reviews, synthesis), so participants challenge a proposal and disagreements are preserved |

Every mode also ends with *areas of agreement*, *significant disagreements*, *assumptions* and
*evidence limitations*, which are read back into the saved record. Decision mode never invents
numeric probabilities or confidence percentages. Without web research, the agents are told never to
invent URLs, citations or studies, and the answer states that nothing was verified externally; with
research (next section) the claims are checked against pages the program retrieved.

**Length.** The default answer is a structured report. When the question asks for a short answer
("in two sentences", "briefly", "one line", "tl;dr") the **brief style** is used automatically: no
long preliminary analysis, reviews of at most five bullets, and a final answer with only *Answer*,
*Significant disagreements*, *Assumptions* and *Evidence limitations*. `--length brief` or
`--length full` overrides the choice. It changes how much is written, not how many agents are asked.

**The workflow** (all modes except `debate`):

1. *Independent perspectives.* Every participant, the coordinator included, answers in parallel
   without seeing the others, so nobody is anchored on the coordinator's opinion. The prompt also
   asks each one for the main question, criteria, assumptions, information gaps and whether outside
   evidence is needed (no extra call).
2. *Cross-review (1 or 2 rounds).* Each participant that answered reads the others' answers and looks
   for mistakes, unsupported claims, contradictions and missing criteria, and says what it would
   revise. Round 2 also shows the round-1 critiques. A participant that failed is not asked again.
3. *Synthesis.* The coordinator writes one answer that answers the question first, keeps the
   disagreements (attributed, and marked unresolved when they are), separates evidence from
   opinion, and says so when the evidence is not enough. Votes are never counted: agreement between
   models is not proof, and they can share the same blind spots.

At most `N + rounds × N + 1` calls for `N` agents (3 agents: 7 calls with one round, 10 with two).

**Automatic routing** is a fixed, deterministic rule set (no model call is ever made to route):

| Decision | Rule |
|---|---|
| Mode | Weighted keyword cues in the question (English plus a few Spanish): an explicit "debate" wins; otherwise "should I / choose / vs / compare / trade-off" → decision; "buy / price / specs / product / review / worth" → review; "research / latest / is it true / claim / evidence" → research; "explain / teach / how does / what is / from scratch" → learn. Ties go decision > review > research > learn; no cue at all → research. |
| Coordinator | `general.coordinator` if it is usable, otherwise the first usable agent. No provider is assumed to be better than another. |
| Agents | Every usable agent (at most 5). Agents that are missing, disabled or hosting the MCP server are listed as *not used* with the reason. |
| Rounds | 1; 2 for contested or high-stakes wording (risk, safety, medical, legal, invest, controversial, debate, "actually true", claims) or a long question. |
| Lightweight | A short (≤ 20 words), uncontested `learn` question with nothing set by hand uses **one agent and no review** (1 call). Setting `--agents`, `--rounds` or a non-learn mode turns this off. |

Manual settings always win: `--mode`, `--coordinator`, `--agents` and `--rounds` are marked
*manual* in the routing and in the saved record. The coordinator must be one of the participating
agents (otherwise it is an error, not a silent change), a manually requested agent that cannot run
is reported and skipped (an error if none can), and a consultation with a single participant runs
without review rounds (a `--rounds` value is reported as ignored).

**Result and status.** The answer goes to stdout; routing, progress, warnings and the saved id go
to stderr. A consultation is `complete`, `partial` (something failed: exit code 5, the output is
labelled incomplete, and when the coordinator itself failed the printed text is the first available
individual answer, clearly marked as *not* a synthesis), `failed` (nobody answered) or `cancelled`
(Ctrl-C). Failed participants are never retried, in particular quota failures. Consultations are
saved with the same store as debates (`history` shows a KIND column when any are present; `show` and
the MCP `get_debate` read both) with the routing decision, manual overrides, every individual
answer and review, the sections read back from the synthesis (recommendation, agreement,
disagreement, assumptions, evidence limitations), per-agent call counts, per-stage and per-call
timings, errors and, only where a CLI reports it, token counts. Calls run concurrently within a
stage, bounded by `general.max_concurrent_agents`, each with the per-call timeout, and the whole
consultation under `debate.total_timeout_secs`.

**Things to keep in mind.** Agreement between assistants is weak evidence: they can be wrong in the
same way. Without web research nothing is checked against external sources, so prices, versions and
recent facts need your own check; with research, claims are checked against retrieved pages, and
that check is still model-assisted (see the research section). Each
participant is a separate subscription request with a large prompt: the default for a contested
question with three agents is 10 requests; use `--plan` first, `--agents` to narrow it, and
`--rounds 1`.

### How `debate` works

**A round is one parallel pass in which every reviewer answers once.** The default is one round;
two is the maximum (`--rounds 1|2`, `debate.rounds`).

| Phase | Who | What |
|---|---|---|
| A. Initial proposal | the coordinator | answers the question |
| B. Round 1: independent review | every other available agent, in parallel | reviews the proposal: (1) mistakes, (2) challenged assumptions, (3) missing requirements, (4) improvements, (5) disagreements explained constructively. Reviewers cannot see each other's work |
| C. Round 2: cross-review (only with `--rounds 2`) | every reviewer that answered in round 1, in parallel | reads **all** round-1 reviews and reconsiders: what changed its mind, what it withdraws, what it still disputes |
| D. Final synthesis | the coordinator | receives every review from every round and writes: Recommended solution, Technical reasoning, Areas of agreement, Significant disagreements (attributed, resolved or unresolved), Unresolved risks, Suggested next steps |

Agent calls: `1 + rounds × reviewers + 1`, so 4 with three agents and one round, 6 with two.
Reviewers are capped at 4 and `debate.total_timeout_secs` stops everything when exceeded.

**Result status.** A partially failed debate is never reported as a success:

| Status | Meaning | stdout | Exit code |
|---|---|---|---|
| `complete` | every planned round ran, every reviewer answered every round, the synthesis succeeded | the synthesis | 0 |
| `partial` | there is a result, but a reviewer or round failed, or the synthesis failed. The warnings and `steps` say what; without a synthesis the output is the **unreviewed initial proposal**, labelled as such | the best result available | **5** |
| `failed` | no result (the initial proposal failed, or the debate was cancelled or timed out) | nothing | the cause: 3 auth, 4 quota, 124 timeout, 130 cancelled, 1 other |

Agents that are *not installed or disabled* are skipped and reported ("not consulted"); that
alone does not make a debate partial, since nothing failed. A debate with no usable reviewer at
all is refused up front.

Guarantees:

- **Read-only.** Debate calls never request write access.
- **No automatic retries.** A reviewer that fails in round 1 is not asked again in round 2, so a
  quota or auth failure is never retried.
- **No loops.** Every call is a one-shot prompt to a leaf CLI and agent output is only ever text
  embedded in other prompts (as delimited data); it is never executed and cannot trigger calls.
  Agents run with `AI_ORCHESTRA_ACTIVE=1`, and `ask` / `debate` refuse to start when it is set,
  so an agent cannot start a nested orchestration. An agent whose own session this runs inside
  (Claude under Claude Code, or Codex when its session markers are present) is skipped as a
  reviewer and refused as coordinator.
- **Disagreement is preserved:** the synthesis prompt forbids smoothing it over, and every raw
  review stays in the saved record (`show --transcript`).
- Prompt sizes are budgeted to fit `agy`'s argument-size limit (asserted in tests for the worst
  case of 4 reviewers over 2 rounds).

#### Example: three agents on a Rust problem

*Illustrative only: written by hand to show the shape of a run, not output from a live run.*

```text
$ ai-orchestra debate "Design a high-performance Rust task scheduler" --coordinator claude --rounds 2
[Debate] 20261011-094600-0a1f3c · Claude leads · reviewers: Codex, Google · 2 round(s)
[Coordinator] Preparing initial proposal...
[Coordinator] Initial proposal ready (6.1s)
[Codex] Reviewing...
[Google] Reviewing...
[Codex] Review ready (9.8s)
[Google] Review ready (13.2s)
[Codex] Reconsidering after cross-review (round 2)...
[Google] Reconsidering after cross-review (round 2)...
[Codex] Cross-review ready (8.7s)
[Google] Cross-review ready (12.9s)
[Coordinator] Producing final synthesis...
[Coordinator] Final synthesis ready (11.4s)
[Debate] COMPLETE · 6 calls · 62.0s
```

- **Claude (coordinator)** proposes a work-stealing design: per-worker Chase-Lev deques, a global
  injector queue, `tokio`-free park/unpark with a `Parker`, and `Arc<Task>` with an atomic state word.
- **Codex** reviews: the deque needs `crossbeam-deque` or careful `unsafe` with loom tests;
  `Arc<Task>` per spawn allocates, so use an intrusive task header and a slab; cancellation and
  panics inside tasks are unspecified.
- **Google** reviews independently: a thread-per-core, shared-nothing design would avoid stealing's
  contention and cache misses entirely, at the price of imbalance; it also asks for a
  requirements section (task latency target, expected task count, blocking vs async tasks).
- **Round 2:** after reading each other's review, Codex withdraws its "use crossbeam" point for
  unsafe-free code but keeps the loom-testing requirement; Google concedes stealing is right for
  uneven workloads but holds that thread-per-core wins for uniform ones.
- **Synthesis** recommends work stealing with `crossbeam-deque`, an intrusive task header,
  structured cancellation and a loom test suite; under *Significant disagreements* it keeps
  Google's thread-per-core alternative as unresolved pending the workload; under *Unresolved
  risks* it lists task panics and priority inversion; *Suggested next steps* are a benchmark
  harness with uniform and skewed workloads, then the loom tests.

### MCP server (Milestone 3)

`ai-orchestra mcp serve` exposes AI Orchestra to an MCP host so it can use the other agents as tools
inside its own conversation: `list_agents`, `ask_agent`, `debate`, `consult`, `research`, `verify_claim`,
`inspect_sources`, `get_research_report`, `get_task_status`, `cancel_task`, `get_debate` and (Milestone 7) the
memory tools `search_memory`, `get_memory`, `get_session_context`, `explain_context`, `save_memory` and
`forget_memory`, and (Milestone 8) `get_eval_report` (read-only; it never starts an evaluation) and
`submit_feedback` (under the same two permissions as memory writes). `consult` takes `question` and optionally `mode`, `coordinator`,
`rounds`, `agents`, `timeout_seconds` and `wait_seconds`, runs the same Rust engine as the CLI, and
is host-aware: the agent hosting the server is never started (automatic choices skip it, an explicit
request for it as coordinator is refused with an explanation, and the answer tells the host to add
its own view). It reuses the same engine, adapters and history as the CLI, speaks MCP over stdio
only (stdout carries nothing but protocol messages), runs long calls as bounded, cancellable tasks,
and protects against recursion: the host agent is never called through it, delegation depth is
limited and propagated to child processes, and children are started in isolated mode where their CLI
allows it. Nothing is registered with any host automatically; `ai-orchestra mcp config <host>` prints
the exact commands. Setup for Claude Code, Codex CLI and Antigravity, the tool schemas, how to verify
discovery, security notes, troubleshooting and a proposed first live test are in
[`docs/mcp.md`](mcp.md).

### Saved debates

Every debate is saved as one JSON file (typed, with Serde) in `debate.history_dir` (default
`~/.local/share/ai-orchestra/debates`, or under `$XDG_DATA_HOME`). Files are written atomically
(temp file, fsync, rename), created owner-only (0600 in a 0700 directory), and credential-shaped
strings are masked first. Failed and cancelled debates are saved too, with what had been gathered.
A record holds: id (`YYYYMMDD-HHMMSS-xxxxxx`), question, coordinator, reviewers, skipped agents,
planned rounds, status, every step (phase, round, agent, start offset, duration, outcome, error
kind such as `timeout` / `quota` / `auth` / `malformed`, and the model and effort used), the
initial proposal, the final decision, warnings and total duration.

```sh
ai-orchestra history                       # newest first
ai-orchestra show 20261011-0946            # a unique prefix is enough
ai-orchestra show 20261011-094600-0a1f3c --transcript
ai-orchestra show 20261011-094600-0a1f3c --output json
```

A failure to save is a warning, never a failure of the debate. `--no-save` or
`debate.save = false` turns persistence off. Saved debates are for inspection: nothing resumes a
provider-native session, and each agent call is a fresh, bounded prompt built by ai-orchestra.

### Models and effort

By default ai-orchestra passes **no model and no effort**: each CLI uses its own configuration
(`~/.claude/settings.json`, `~/.codex/config.toml`, and `agy`'s default). `ai-orchestra doctor`
shows what is set in ai-orchestra's config and says "CLI default" otherwise; it does not read
the CLIs' own settings. Override them per provider, the same way as the coordinator:

```toml
# ai-orchestra.toml
[providers.codex]
model = "gpt-6-luna"
effort = "low"

[providers.claude]
effort = "medium"
```

```sh
ai-orchestra ask codex "..." --model gpt-6-luna --effort low
ai-orchestra debate "..." --model claude=opus --effort codex=low --effort claude=medium
```

Precedence: command line, then the config file, then the CLI's own setting. In `debate`, the
flags take `AGENT=VALUE` (repeatable; the agent can be `claude`, `codex`, `google` or `agy`) and
apply only to that agent. How each is passed:

| | model | effort | valid effort levels |
|---|---|---|---|
| Claude | `--model` | `--effort` | low, medium, high, xhigh, max |
| Codex | `--model` | `-c model_reasoning_effort="…"` | depends on the model; Codex rejects unsupported levels |
| Google (`agy`) | `--model` | `--effort` | low, medium, high, xhigh, max |

Invalid levels, and model names that start with `-` or contain spaces, are rejected at startup
or before the first agent runs. `agy`'s model names also encode an effort
(`gemini-3.8-flash-low`; list them with `agy models`), so combining a model with a different
`--effort` is up to the CLI to resolve. Lower effort generally means fewer reasoning tokens and
faster replies, but this tool cannot see your plan's credit cost, so check your provider's usage
page rather than assuming.

#### Which model and effort actually ran

Every `ask` response (`--json`, and the stderr footer) and every `--live` debate reply says what
was used, e.g. `claude-sonnet-5-5 (requested sonnet) · effort medium`. Two different facts are
kept apart (`model_requested` / `effort_requested` vs `model_reported` in the JSON):

- **Requested** is what ai-orchestra passed (flag or config). When nothing was passed it says
  `CLI-default model` / `CLI-default effort` instead of guessing.
- **Reported** is the model the CLI itself says it used. **Only Claude reports this** (from the
  `modelUsage` field of its JSON result; this field has not been checked against live output
  yet, and when it is absent the label simply falls back to what was requested). Codex's event stream and
  `agy`'s print-mode JSON, as parsed here, carry no model name.

So with nothing configured for `agy` you cannot see its model from ai-orchestra. Its own log
(`~/.gemini/antigravity-cli/log/`) shows the default is `Gemini 3.8 Flash (High)`, and ai-orchestra
deliberately does not scrape another tool's logs. To make it visible and stable, pin one in the
config (`providers.google.model`, names from `agy models`); then the footer shows it as requested.

### Choosing the coordinator

The coordinator is just the agent that writes the proposal and consolidates the critiques;
nothing is hardcoded. Choose it per run or set a default:

```sh
ai-orchestra debate "..." --coordinator codex              # this run only (claude | codex | google | agy)
ai-orchestra debate "..." --coordinator google --peers claude,codex   # also pick the critics
```

```toml
# ai-orchestra.toml: the default when --coordinator is not given
[general]
coordinator = "codex"
```

The coordinator is never also a critic. Without `--peers`, every other available agent
critiques. An agent that cannot run (disabled, not installed, or the one whose own session you
are in) is skipped with a stated reason.

### Watching a debate

`ask` and `debate` are non-streaming: each CLI runs to completion and its whole reply is parsed,
so you see each *message* when it is finished, not token by token.

- **Default:** short progress lines on **stderr**, for example `[Codex] Reviewing...`,
  `[Codex] Review ready (9.8s)`, `[Coordinator] Producing final synthesis...`. `--quiet`
  silences them. stdout carries only the result, so piping keeps working.
- `--live` replaces them with the **full text** of each proposal, review and synthesis the moment it
  completes. Reviews from parallel reviewers appear in the order they actually finish. (The
  decision therefore appears twice on a terminal: once live, once at the end.)
- `--log-file FILE` appends one JSON object per step (`started`, `call_started`,
  `call_finished`, `finished` or `failed`, each with a `ts_ms`; roles are `proposal`, `review`,
  `cross_review`, `synthesis`), flushed per line, so `tail -f FILE` in a second terminal follows
  the run. Created owner-only (0600), credential-shaped strings masked, appended across runs, and
  a bad path fails before any agent is called.
- The flags are independent. None adds requests or changes the workflow; `-v` never logs prompts
  or replies.

```sh
# terminal 1
ai-orchestra debate "Monorepo or polyrepo?" --live --log-file /tmp/debate.jsonl
# terminal 2 (optional): one line per finished step
tail -f /tmp/debate.jsonl | jq -c 'select(.event=="call_finished") | {round, role, provider, ok, duration_ms}'
```

### Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | other failure (spawn error, non-zero exit, malformed output, recursion refused) |
| 2 | bad usage or configuration, unknown provider, prompt too large |
| 3 | authentication problem: sign in with the official CLI |
| 4 | subscription usage / rate limit reached |
| 5 | `debate` or `consult` finished only **partially**: the result is printed, but treat it as incomplete |
| 124 | timed out (agent terminated, or the debate's total time budget exceeded) |
| 130 | cancelled (Ctrl-C; agent terminated) |

## Configuration

See [`ai-orchestra.example.toml`](../ai-orchestra.example.toml). Lookup order:
`--config`, `$AI_ORCHESTRA_CONFIG`, `./ai-orchestra.toml`,
`~/.config/ai-orchestra/config.toml`. Without a file, defaults apply. You can set
per-provider executable paths, default model, extra arguments and timeouts, plus
global limits: timeout, concurrent agents, captured output size and prompt size.
Unknown keys and unknown provider names are rejected.

## Safety defaults

- **Read-only unless you say otherwise.** `--allow-write` is the only way to
  enable edits, per invocation:
  - Claude: `--tools Read,Grep,Glob` (no Bash/Edit tools exist) + `--permission-prompts none`;
    with `--allow-write`: `--permission-mode acceptEdits`.
  - Codex: `--sandbox read-only`; with `--allow-write`: `workspace-write`.
    `danger-full-access` and `--dangerously-bypass-approvals-and-sandbox` are never used.
  - Google: `--mode plan`; with `--allow-write`: `--mode accept-edits`.
    `--dangerously-skip-permissions` is never used.
- **No shell.** Arguments are passed as a vector; prompts go over stdin where the
  CLI supports it (Claude, Codex), so they are not visible in `ps` or limited by
  argv size. Nothing an agent outputs is ever executed.
- **No recursion.** If the process is running inside the target agent (e.g.
  `CLAUDECODE` is set because you are in a Claude Code session), `ask` refuses.
  Override with `allow_nested = true`.
- **Bounded.** A semaphore caps concurrent agent processes; stdout is capped
  (`max_output_bytes`, the rest is drained and dropped), stderr is capped at 64 KiB.
- **Clean termination.** Timeouts and Ctrl-C send SIGTERM to the agent's whole
  process group, then SIGKILL after 2 s, so grandchildren do not linger.
- **Memory is data, local and opt-in for persistence.** Recalled text is neutralised and marked as data
  that cannot change a task, permission or flag; credentials are never stored; sensitive records never
  reach an agent; agents can never write or delete memory through MCP unless the operator allowed it
  (`allow_mcp_writes`) *and* the user asked; there is no telemetry. The file is owner-only but **not
  encrypted**.
- **No secrets retained.** The conversation store redacts token-shaped strings,
  and `doctor` only reports the login *method* (never email or org ids).
  Tracing never logs prompts or command-line arguments.

## Architecture

```
main.rs (clap) ──► Orchestrator ──► ProviderRegistry ──► dyn AgentProvider
                      │                                   ├─ ClaudeAdapter
                      ├─ ConversationStore                ├─ CodexAdapter
                      ├─ DebateStore (JSON history)       ├─ GoogleAdapter
                      └─ InvokeContext ─► ProcessManager  └─ MockProvider (tests)
```

| Module | Role |
|---|---|
| `provider` | `AgentProvider` trait and the shared pipeline: recursion guard → prompt limit → resolve executable → build command → run → classify failure (auth / quota / exit) → parse. |
| `providers/{claude,codex,google}` | Everything CLI-specific: flags, output parsing, auth probing. Adding a provider means adding one file and one registry line. |
| `providers/mock` | Scripted in-process provider; spawns nothing. |
| `process` | `ProcessManager`: semaphore-bounded spawn, stdin feeding, capped capture, timeout, cancellation, process-group cleanup, env scrubbing. |
| `orchestrator` | Wires config, registry, store, and exposes `ask` / `doctor`; `exchange` is the single logged request/response primitive. |
| `smart/*` | Milestone 6: `strategy` and `engine` (the deterministic `SmartStrategyEngine`: strategy, flow, agents, rounds, limits and the explanation), `budget` (`ResourceBudgetManager`: calls, one deadline, parallelism, response size), `adaptive` (`AdaptiveDeliberationController`: continue, escalate or stop from structured results), `quality` (`QualityGate`), `profiles` (bounded local provider statistics), `reuse` (identical calls answered once), `trace`, `metrics` and `run` (the decision trace and measured metrics stored with the record). |
| `ux/*` | Milestone 8, usability: `chat` (the engine behind `ai-orchestra chat`: lines in, consultations and settings changes out, testable with a scripted terminal), `settings` (the layered, validated settings), `present` (how a result is shown per mode, plus the sources / answers / disagreements / why / stats views), `prefs` (saved preferences). `chat_cmd` is the terminal around it. |
| `eval/*` | Milestone 8, quality evaluation: `dataset` (cases with their own reference evidence), `rubric`, `citations` (mechanical validity, kept apart from support), `run` (run records, the offline runner, storage), `blind` (seeded blind pairwise comparison and recorded judgments), `stats` (medians, Wilson and seeded bootstrap intervals, minimum sample sizes), `report`, `live` (the plan and the explicitly authorised live runner), `feedback`. `eval_cmd` is the CLI. |
| `memory/*` | Milestone 7: `model` (scopes, kinds, trust, lifecycle), `db` (SQLite + FTS5 schema, migrations, transactions, recovery), `retrieval` (the `MemoryRetrievalEngine` and its lexical backend), `context` (the `ContextAssemblyEngine`, `SharedContext`, the explain report), `compress` (deterministic digests, dedupe), `freshness`, `sensitive`, `tokens` (labelled estimates), `service` (async facade: prepare, record, add, search, forget, export, import, check), `continuation` (provider-session position), `summarize` (the optional one-call AI summary). `consult/memory_flow` wires it into a consultation; `memory_cmd` is the CLI; `mcp_memory` the MCP views. |
| `research/*` | Milestone 5: `net` (URL and address policy, safe DNS resolver), `fetch` (the source fetcher), `html` (readable text and metadata), `robots`, `source` (numbered source records), `discovery` (pluggable source discovery), `claims`, `verify` (passage retrieval and judgement parsing), `quality` (qualitative source assessment, origin groups, freshness), `citations`, `plan` (web/depth decisions), `engine` (the research workflow), `capabilities` (what each CLI can do). |
| `consult` | Milestone 4: the `ConsultationRouter` (deterministic, pluggable through the `RoutingStrategy` trait), consultation modes and prompts, the independent-perspectives / cross-review / synthesis engine, and the `ConsultationMeta` stored on the record. `debate` mode runs the engine below. |
| `debate` | Milestone 2 engine: reviewer selection, parallel review / cross-review rounds, synthesis, prompt builders and size budgets, the typed `DebateOutcome` record, complete / partial / failed status; emits `DebateEvent`s as it goes. |
| `history` | Debate ids, atomic owner-only JSON persistence, listing and prefix lookup (`history`, `show`). |
| `mcp` | Milestone 3: the MCP server (official `rmcp` SDK): eleven tools, typed validated inputs, bounded structured results, host and depth protection, graceful shutdown. |
| `tasks` | Bounded task manager for the MCP server: ids, six states, concurrency and queue limits, per-task timeouts, cancellation, retention, shutdown. |
| `progress` | Renders `DebateEvent`s as short progress lines or `--live` text, and writes the redacted, owner-only JSONL log for `--log-file`. |
| `store` | `ConversationStore`: bounded in-memory log of prompts, responses, decisions and errors with agent identity and timestamps. |
| `doctor` | Installation / auth report built from non-AI commands. |
| `config`, `error`, `types` | TOML config, typed errors (`thiserror`), request / response types. |

`ProviderId` is a string newtype, not an enum, so new providers do not touch the
orchestration core. The coordinator is a config value, not hardcoded.

## CLI flags used (verified from local `--help`)

- `claude -p --output-format json --no-session-persistence --strict-mcp-config --permission-prompts none --tools … [--permission-mode acceptEdits] [--model]`; `claude auth status`
- `codex exec --json --ephemeral --skip-git-repo-check --color never --sandbox … [--model] -`; `codex login status`
- `agy --output-format json --mode … [--model] -p <prompt>`

## Testing

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

No test contacts a real provider. Unit tests (in-process `MockProvider`s) cover parsers, argument
builders, the process manager (timeout, cancellation, process-group kill, output caps, bounded
concurrency), the store, history persistence (atomic writes, ids, prefix lookup, redaction) and the
debate engine: the initial proposal, one- and two-round discussions, a configurable coordinator,
reviewer timeouts, quota and malformed output, missing providers, coordinator failure, persisted
state, final synthesis, cancellation, the total time budget, the recursion guards and prompt-size
limits, and the consultation router (mode rules for the documented example questions, every
manual override, automatic and manual agents and coordinators, rounds, the lightweight and
single-agent paths), the consultation engine (independent perspectives that provably run in
parallel, one and two cross-review rounds, every mode's prompts and sections, coordinator and
participant failures, timeouts, cancellation, persistence and rendering) and the unchanged shape
of plain debate records. Parallelism is proven with a barrier that only opens when all reviewers are running at
once (not with timing). `tests/mock_cli.rs` runs the real binary against fake `claude` / `codex` /
`agy` shell scripts, covering the same workflow end to end (including a real SIGINT), `history` /
`show`, exit codes, env scrubbing, read-only argument shapes and the Milestone 1 commands, which
are unchanged. Every test gets a private `HOME`, so tests never write to your real history.

Smart orchestration: `tests/smart.rs` (37 tests) runs every strategy, the adaptive escalation and
stopping rules, the budgets (call limit, global deadline, parallel limit, cancellation), conflicts,
the quality gate, the provider history, the evidence interplay and the saved record with scripted
agents and a local web server; `tests/smart_mcp.rs` (8 tests) does the same through the MCP tools;
`tests/benchmark.rs` is the simulated benchmark. Concurrency is proven with barriers and with span
overlap, not with timing thresholds. The Milestone 1-5 suites run unchanged against the fixed
workflow (`[smart] default_strategy = "off"` in their configs).

Chat and evaluation: `tests/ux_chat.rs` (14 tests: the conversation and follow-ups, `/new` and `/resume`, settings and
their precedence, saved preferences, manual settings never overridden, the answer views, cancellation, a partial
result, history search and reopening, feedback, project isolation, sources and citations); `tests/eval.rs` (19: dataset
parsing and validation, offline runs that call nothing, rubric and citation checks, small-sample reporting, the live
runner with mock agents - plan, ceiling, cancellation, failures, provenance, isolation); the Milestone 8 block of
`tests/mock_cli.rs` drives the real binary (`chat` from a pipe, bare `ai-orchestra` without a terminal, `history search`,
`eval ...`, blind judging, live plan without the flag, feedback); `tests/memory_mcp.rs` covers the two new MCP tools.

Memory and context: `tests/memory.rs` (35 tests: the product-comparison flow, memory off / `--no-save`,
scopes and project isolation, prompt-injection text in memory, credentials and sensitive records, the
budget as a ceiling, freshness, a corrupt database, cancellation, concurrent consultations, file
permissions, deletion that really removes bytes, import of earlier history, the one-call AI summary, debate
mode); `tests/memory_mcp.rs` (12) the six memory tools and their permission rules; `tests/memory_eval.rs`
the 15-scenario evaluation with its four conditions; the Milestone 7 block of `tests/mock_cli.rs` drives
the real binary (`memory`, `session`, the consult flags, a damaged database); the `memory::*` unit tests
cover the schema, migrations, concurrency, corruption, retrieval, assembly and compression. The legacy CLI and
MCP e2e suites run with `[memory] default = "off"`.

MCP: `tests/mcp_inprocess.rs` connects a real `rmcp` client to the real server over an in-memory pipe
with mock agents (initialization, discovery and schemas, validation, errors, timeouts, task states,
cancellation, concurrency limits, debate, consult and retrieval, recursion layers); `tests/mcp_e2e.rs` drives
the real binary over real pipes (stdout purity under verbose logging, SIGTERM shutdown that kills a
running agent, depth limits, setup output). Task limits are tested with barriers and task state, not
timings.

## Limitations

- **Verified live:** `ask` against all three CLIs; `debate` (including a complete three-agent
  debate with Codex answering and `--rounds 1`); and the MCP server with Codex, Claude Code and
  Antigravity as hosts; and `consult` (all modes, the default three-agent run, two rounds, the brief
  style, a forced timeout, and the MCP tool from all three hosts). A real quota failure was not
  provoked live; it is covered by recorded real wording and fake-CLI tests.
- **Correlated errors.** The agents are different products but similar technology. They can agree on
  something false, and a cross-review can reinforce a shared mistake. Treat a consultation as
  structured advice, not as verification.
- **Research is model-assisted at its last step.** Pages are retrieved by AI Orchestra and passages are
  cut from them by the program, but whether a passage supports a claim is judged by an AI model, which
  can misread it (the output says "model-dependent"). Passage finding is lexical, so a claim phrased
  very differently from the page may find no passage. Web search through the provider CLIs was run
  live (Claude Code and Codex expose their search results; Antigravity is unsupported); see
  `docs/research.md` for what is and is not known per CLI.
- **Routing is keyword rules.** It is deterministic and explainable but crude: a question phrased
  without the cue words falls back to research, and the English/Spanish cue lists are small. Use
  `--plan` to check and `--mode` to override.
- **Consultations are saved only when they finish, fail or are cancelled by a signal we can catch.**
  A process killed with SIGKILL leaves no record.
- **Agents can read the working directory.** Debate agents are read-only but run in the current
  directory (or `--cwd`) and may read files there, which can bias an answer to a general question.
  Run from a neutral directory or pass `--cwd` for such questions.
- **`--log-file` appends**, so reuse of one file mixes runs (and formats across versions). Use a
  fresh file per run.
- **Not verified live:** that each CLI actually *enforces* its read-only flags (Claude's tool
  restriction, `codex --sandbox read-only`, `agy --mode plan`); error-message wording used to
  classify auth and quota failures; behavior with very large outputs.
- **`agy` can return an empty response.** Seen live once: in plan mode `agy` denied a command the
  model tried to run, and the turn ended with no text. Debate prompts now ask agents to answer in
  plain text even if a tool is denied (not yet verified to prevent it); the error says what to try.
- `agy` reports hidden reasoning in `output_tokens` (1002 for a one-word reply), and is the
  slowest of the three, so it dominates debate latency and likely quota use.
- A debate with three agents and two rounds makes up to 7 subscription requests, each with
  large prompts; expect it to consume noticeably more quota than `ask`.
- `agy` has no auth-status command, so its login state is reported as unknown
  until the first request, and its prompt travels as a command-line argument
  (visible in `ps`; capped at 128 KiB).
- Read-only enforcement for `agy` (`--mode plan`) relies on that mode's
  semantics; it was not verified against the CLI's behavior.
- `reported_cost_usd` is whatever the CLI prints; on subscriptions it is
  informational, not a charge.
- Each agent call is a fresh, stateless session (`--no-session-persistence` /
  `--ephemeral`). `ask` keeps its conversation log only in memory; `consult` can recall earlier
  consultations through the local memory database (Milestone 7).
- Auth/quota detection matches error text, so unfamiliar wording falls back to a
  generic error that still includes the CLI's message.
- Process-group termination is Unix-only; on Windows only the direct child is
  killed. Development and testing are macOS only so far.
- **MCP:** verified live with Codex, Claude Code and Antigravity as hosts (discovery, `ask_agent`,
  host refusal, coordination, `debate` with task ids, Codex's `--ignore-user-config`). Headless
  `agy -p` needs a `permissions.allow` rule to use any MCP tool. `agy` has no way to isolate a child
  from MCP servers, so only the depth limit protects against it loading this one. See
  `docs/mcp.md`.
- **Smart orchestration is not live-validated.** The strategies, the escalation rules and the
  benchmarks are tested with scripted agents; whether `balanced` gives answers as useful as `thorough`
  for real questions is unmeasured, and a reviewer's `AGREE` is one model's judgement. `auto` reads the
  wording of the question, which is blunt. See `docs/smart-orchestration.md` for the full list.
- **Quality of multi-agent answers is unmeasured.** The evaluation system is built and tested; one live run (six easy
  cases, Codex only, one run per arm) found no difference in correctness and a 1.8x / 6.3x cost for the two
  multi-agent strategies, and its blind judgments are still pending. Its offline example uses hand-written fixtures; its deterministic checks are keyword
  based; judgement criteria are left to people; one live run per arm could not separate a real difference from
  variation. Nothing in this repository shows that consulting several agents beats one. The chat has no
  full-screen interface, no streaming and reads one line at a time; the browser UI is not built.
- **Memory and context are only lightly live-validated.** Recall, assembly and compression are tested with
  scripted agents and temporary databases, and ran live against Codex only (one run per arm, 2026-10-12), so
  they show that the right facts reach a prompt, not that a real model answers better, faster or with less
  subscription usage. Retrieval is lexical (FTS5): a follow-up
  that shares no words with what it continues and carries no continuation cue relies on the "latest turn"
  rule. `debate` mode is recorded but does not recall. The database is **not encrypted at rest**; the
  sensitivity classifier is heuristic; token figures are estimates (bytes / 3). Provider-native session
  resume is documented by the CLIs but neither used nor verified. See `docs/memory-architecture.md`.
- Debate rounds (the `debate` command, and `consult` with `--rounds` or `--strategy thorough`) are a fixed sequence with no early exit on consensus. Reviewers are independent in
  round 1 and see each other's round-1 reviews in round 2 (only with `--rounds 2`).
- The Codex recursion markers (`CODEX_THREAD_ID`, `CODEX_SESSION_ID`, `CODEX_SANDBOX`,
  `CODEX_SHELL`) are names found in the Codex binary; they have not been observed set in a live
  Codex session. A false positive can be overridden with `allow_nested = true`.
- History files are plain JSON on disk, owner-only; nothing prunes old debates.
- Not implemented: collaborative coding (deferred; AI Orchestra does not modify code), a web
  dashboard, reading PDFs, JavaScript-rendered pages, paywalled or login-protected pages.

## Live verification

Full record, with measurements and the exact reproduction commands, in
[`docs/validation.md`](validation.md). Summary:

| Provider | Status | Elapsed | Errors |
|---|---|---|---|
| Google (`agy` 1.3.3) | PASS, `ORCHESTRA_GOOGLE_OK` exact | 19.89 s | None |
| Claude (2.1.296) | PASS, `ORCHESTRA_CLAUDE_OK` exact (run in a plain terminal; refused inside a Claude Code session by the recursion guard, as intended) | 4.55 s | None |
| Codex (0.160.1) | PASS, `ORCHESTRA_CODEX_OK` exact | 8.35 s | None |

Run from a **plain terminal** (not inside a Claude Code session); these use your subscriptions.

```sh
cargo build --release
./target/release/ai-orchestra doctor
./target/release/ai-orchestra ask claude "Reply with exactly: ORCHESTRA_CLAUDE_OK" --json
./target/release/ai-orchestra ask codex  "Reply with exactly: ORCHESTRA_CODEX_OK" --json
./target/release/ai-orchestra ask google "Reply with exactly: ORCHESTRA_GOOGLE_OK" --json
```

Debate: the revised v0.2 workflow ran live once (2 rounds, Claude + Google, Codex out of credits:
5 calls in 87.8 s, `partial`, exit code 5; details in `docs/validation.md`). The complete
three-agent debate, from a plain terminal (not inside a Claude Code or Codex session), once Codex
has credits again:

```sh
./target/release/ai-orchestra debate \
  "What is the best architecture for a high-performance Rust MCP server?" \
  --coordinator claude --rounds 2 --live --log-file /tmp/debate-3agents.jsonl
```

That is 6 subscription requests (proposal, 2 reviews, 2 cross-reviews, synthesis). Add
`--rounds 1` for the cheaper 4-request version, or `--peers google` to leave Codex out (for
example while its credits are spent). Afterwards, `ai-orchestra history` and
`ai-orchestra show <id> --transcript` replay it.

Verified live on 2026-10-11 (see `docs/validation.md`): a complete three-agent debate with Codex
answering, `--rounds 1`, a real provider timeout, Claude and Codex auth-failure wording, and write
attempts against each CLI (no file was created). Not yet seen live: real Google auth-failure wording.

If an adapter fails with a malformed-output error, run that CLI directly with the flags listed
under "CLI flags used" and share its raw output; the adapter's parser is the only thing that needs
to change (`-vv` logs do not include raw output, by design).
