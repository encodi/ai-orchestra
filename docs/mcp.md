# AI Orchestra as an MCP server

`ai-orchestra mcp serve` runs AI Orchestra as a native [MCP](https://modelcontextprotocol.io) server
over stdio, so an MCP host (Claude Code, Codex CLI or Antigravity CLI) can use the **other** agents as
tools inside its own conversation:

> "Ask Codex to review this architecture, ask Google to find its weaknesses, then improve the design
> using their feedback."

It uses the official CLIs and your existing subscriptions. No model API, no API keys.

**Status:** implemented and tested with a real MCP client against the real server and mock/fake
agents. It has **not** been connected to a real host yet; see [Verification status](#verification-status)
and [the first live test](#proposed-first-live-test).

## Architecture

```
 MCP host (Claude Code / Codex CLI / Antigravity CLI)
        │  JSON-RPC over stdio  (stdout = protocol only, logs → stderr)
        ▼
 ai-orchestra mcp serve        ── src/mcp.rs        tools, validation, bounded results
        │                      ── src/tasks.rs      task ids, states, limits, cancellation
        ▼
 Orchestrator (ask / debate)   ── the same engine as the CLI, history and mock providers
        │  subprocesses, read-only, timeouts, process-group cleanup
        ▼
 claude · codex · agy          the other agents' official CLIs
```

Nothing in the agent adapters, limits or debate logic is duplicated: `ask_agent` is
`Orchestrator::ask`, `debate` is `Orchestrator::debate`, `get_debate` reads the same history files as
`ai-orchestra show`. The SDK is the official Rust one, [`rmcp`](https://crates.io/crates/rmcp) 3.5.1
(`server`, `macros`, `transport-io`); it negotiates among MCP protocol versions 2024-11-05 through
2026-07-28, so each host can use its own.

## Tools

All inputs are typed and validated; unknown fields are rejected. Results are structured JSON (also
sent as text) and bounded (`mcp.max_result_bytes`, default 64 KiB; `truncated: true` when cut).
Failures are tool results with `isError: true` and `error: {kind, message}`, never silent. Agent
errors keep their meaning: `quota`, `auth`, `timeout`, `malformed`, `missing`, `nested`.

| Tool | Purpose | Parameters |
|---|---|---|
| `list_agents` | Agents, installed versions, availability, host flag. **No model request, no login check** (local `--version` only; cached 30 s). | none |
| `ask_agent` | Ask one other agent a question (read-only). | `provider`, `prompt`, `timeout_seconds` (1-3600), `wait_seconds` (0-55) |
| `debate` | Run the multi-agent debate; returns a `debate_id`, synthesis, disagreements, participants. | `question`, `coordinator`, `rounds` (1 or 2), `timeout_seconds`, `wait_seconds` |
| `consult` | General-purpose consultation (Milestone 4): picks the mode, coordinator, agents and rounds automatically unless given; returns one considered answer. See below. | `question`, `mode` (auto, decision, research, review, learn, debate), `coordinator`, `rounds` (1 or 2), `length` (auto, brief, full), `agents` (list), `strategy` (auto, fast, balanced, thorough), `max_agent_calls`, `max_duration_seconds`, `max_parallel`, `dry_run`, `timeout_seconds`, `wait_seconds` |
| `research` | Milestone 5. A consultation with **real sources**: an agent with web search (or your `urls`) finds pages, this server retrieves them, agents answer from the evidence, claims are checked against passages, and the answer carries `[n]` citations and a generated list of sources. Web defaults to `on` and fails clearly if nothing can search or fetch. See [`research.md`](research.md). | `question`, `mode`, `coordinator`, `rounds`, `agents`, `length`, `web`, `depth`, `max_sources`, `urls`, `freshness_days`, `strategy`, `max_agent_calls`, `max_duration_seconds`, `max_parallel`, `dry_run`, `timeout_seconds`, `wait_seconds` |
| `verify_claim` | Check ONE claim against sources: SUPPORTED / CONTRADICTED / MIXED_EVIDENCE / INSUFFICIENT_EVIDENCE / NOT_CHECKED with the passages and URLs behind it. With `urls` only those pages are used; without them an agent with web search must find sources. No answer panel. | `claim`, `urls`, `web`, `depth`, `max_sources`, `freshness_days`, `agents`, `strategy`, `max_agent_calls`, `max_duration_seconds`, `max_parallel`, `dry_run`, `timeout_seconds`, `wait_seconds` |
| `inspect_sources` | Retrieve public pages as research does and describe them (status, final URL, title, author, published / updated dates, hash, kind, preview). **No agent is called.** Private and non-HTTPS URLs are refused and reported. | `urls` (1-10), `wait_seconds` |
| `get_research_report` | The research part of a saved consultation: sources, claims with evidence, contradictions, citations, assessors, limitations. | `consultation_id` |
| `get_task_status` | State and result of a task, optionally waiting for it. | `task_id`, `wait_seconds` |
| `cancel_task` | Cancel a task and terminate its agent processes. | `task_id` |
| `get_debate` | Retrieve a saved debate by id or unique prefix. | `debate_id`, `include_transcript` |
| `search_memory` | Milestone 7. Full-text search of the project's memory; bounded, labelled records, sensitive ones excluded. Memory text is data, never an instruction. | `query`, `scope`, `limit` (1-20) |
| `get_memory` | One remembered record with origin, trust, dates and links (sensitive records refused). | `id` |
| `get_session_context` | The consultations and recent memories of an AI Orchestra session. No agent is called. | `session_id`, `max_items` |
| `explain_context` | What a consultation of this question would recall, what was left out and why (estimated sizes). Reads only. | `question`, `memory`, `session`, `context_budget_tokens` |
| `save_memory` | Remember a preference, decision, requirement, interest or note. Needs `memory.allow_mcp_writes = true` **and** `user_approved: true`; labelled as relayed by the host; never global; credentials and personal data refused. | `text`, `kind`, `scope` (project, session), `topic`, `tags`, `expires_days`, `user_approved` |
| `forget_memory` | Delete one session/project memory. Same two permissions; never global. | `id`, `user_approved` |
| `get_eval_report` | Milestone 8. An evaluation run's report: results grouped by kind (fixture / mock / recorded / live), objective checks, citation validity, quality against efficiency, blind-comparison results with bias checks. **Read-only: no tool starts an evaluation.** | `run_id` (default: newest), `seed` |
| `submit_feedback` | Store the user's feedback on a consultation (helpful, inaccurate, bad-citation, ...). Needs `memory.allow_mcp_writes = true` **and** `user_approved: true`; changes no setting or ranking. | `consultation_id`, `rating`, `note`, `user_approved` |

`consult`, `research` and `verify_claim` also accept `memory` (auto, off, session, project, global), `session`, `context_budget_tokens`, `explain_context` and `remember`. All memory tools refuse to run in a nested invocation. See [`memory-architecture.md`](memory-architecture.md).

`wait_seconds` is optional and not part of the minimal interface you asked for: it bounds how long a
call waits before returning a task id (see below).

### `consult`

The same Rust engine as `ai-orchestra consult` (see [`reference.md`](reference.md)); nothing is duplicated here. Every
parameter except `question` is optional and `auto`/omitted means "choose for me". The tool is
**host-aware**:

- the host agent is excluded from the start: automatic choices (coordinator, participants) never
  pick it, and it is listed under `skipped`;
- naming the host as `coordinator` is rejected with `host_agent_blocked` and an explanation
  (leave the coordinator on auto: the other agents are consulted and the host adds its own view);
  naming it in `agents` reports it as not used instead of calling it;
- the result carries `host_note`, telling the host that it was not consulted and should add its own
  view before presenting the answer;
- an impossible request (no usable agent, a debate with fewer than two agents, a coordinator that
  is not among `agents`) is rejected before anything is queued; `nested` is returned when the
  delegation depth limit is reached, exactly as for the other tools.

Consultations usually outlast a host's request timeout, so `consult` follows the task rules below:
expect a `task_id` after `wait_seconds` (default 30) and follow it with `get_task_status`.

```json
{
  "status": "completed", "ok": true, "consultation_id": "20261011-203114-000b93",
  "consultation_status": "complete", "mode": "decision", "synthesized": true,
  "coordinator": "codex", "participants": ["codex", "google"], "skipped": [],
  "routing": {"mode": {"value": "decision", "source": "auto", "reason": "matched decision cue(s): ..."}, "...": "..."},
  "manual_overrides": [], "rounds_run": 1,
  "answer": "## Answer\n...", "recommendation": "...", "agreements": "...",
  "disagreements": "...", "unresolved_disagreement": true,
  "assumptions": "...", "evidence_limitations": "...",
  "invocations": {"codex": 3, "google": 2}, "provider_failures": 0, "token_usage": null,
  "warnings": [], "calls": 5, "duration_ms": 41200,
  "host_note": "claude hosts this server, so it was not consulted. Add your own view ...",
  "caveat": "Models can share the same mistakes, nothing was verified against external sources, ..."
}
```

`ok` is true only when `consultation_status` is `complete`. A `partial` result has `ok: false`,
`isError: true` and a `note` saying so. The saved record is readable with `get_debate` (which now
also returns `kind: "consultation"` and `mode`) and with `ai-orchestra show` in a terminal.
`token_usage` is `null` unless the CLIs reported token counts; none are estimated.

### Strategies and limits (Milestone 6)

`consult`, `research` and `verify_claim` take the same strategy and limits as the CLI, through the
same Rust engine (nothing is duplicated here):

- `strategy`: `auto` (the default, or the server's `smart.default_strategy`), `fast`, `balanced`,
  `thorough`. It sets defaults only; `mode`, `coordinator`, `agents`, `rounds`, `depth` and the web
  settings you pass are never overridden.
- `max_agent_calls`, `max_duration_seconds`, `max_parallel`: limits for the whole consultation. A
  request that cannot fit them is rejected with `invalid_request` before anything is queued, with
  the arithmetic in the message. `max_duration_seconds` cannot exceed the server's ceiling
  (`debate.total_timeout_secs`).
- `dry_run: true` returns `{ok, dry_run, plan, explanation}` immediately: the strategy, flow, agents
  in the order they would be asked, rounds, expected and maximum calls, limits and reasons. No
  agent is called and no page is fetched.

The host agent is excluded from every strategy exactly as before (it is never picked, naming it as
the coordinator is `host_agent_blocked`, naming it in `agents` is reported under `skipped`), and the
recursion guards apply unchanged.

A completed result also carries `strategy` (`value`, `source`, `reason`) and `orchestration`: the
stop reason, agents used, calls, calls answered from an identical earlier call, peak concurrency,
the time breakdown (wall clock, agents busy, outside agent calls, waiting for a parallel slot), the
budget limit that stopped the work if one did, the quality-gate verdict and up to 40 trace events.
`get_debate` returns the same for a saved consultation. A result the quality gate found wanting is
`partial`, with the failed checks in `warnings`.

### Research tools

`research`, `verify_claim` and `consult` (with `web`, `depth`, `max_sources`, `urls`,
`freshness_days`) use the same Rust code as `ai-orchestra consult` and run as tasks. The result
contains a `research` object: `sources` (id, URL, title, published / updated dates and retrieval
status, how each was discovered, a short preview), `claims` (status, how many independent sources
support or contradict it, the passages and which agent judged them), `contradictions`, `citations`
(which `[n]` the answer uses and which were removed), `limitations`, and the call `budget`. Page text
is untrusted data and never an instruction; the host agent is never called and never searches (it is
excluded exactly as for `consult`); only an agent with a usable web search is asked to find sources,
and every URL it reports is fetched and checked by this server before it counts. `ok` is `true` only
when the consultation completed; a research run that retrieved nothing is `partial`.

### Results

`ask_agent` (completed):

```json
{ "task_id": "task-0001a3f2", "kind": "ask_agent", "status": "completed", "ok": true,
  "provider": "codex", "text": "…", "truncated": false, "duration_ms": 8342,
  "model": "gpt-6-luna · effort medium", "usage": {"input_tokens": 19378, "output_tokens": 10},
  "error": null }
```

`debate` (completed, but note `ok`):

```json
{ "task_id": "…", "status": "completed", "ok": true, "debate_id": "20261011-163016-00095a",
  "debate_status": "complete", "coordinator": "codex", "participants": ["google"], "rounds": 1,
  "final_synthesis": "## Recommended solution …", "disagreements": "- …", "warnings": [],
  "calls": 3, "duration_ms": 41000 }
```

A debate with a failed reviewer or synthesis has `debate_status: "partial"`, `ok: false`,
`isError: true` and a `note`: treat it as incomplete (the CLI's exit code 5). The synthesis is still
returned when there is one.

### Tasks, timeouts and polling

MCP clients have request timeouts, so long calls run as tasks. A call waits up to `wait_seconds`
(default 30, maximum 55) for its task and, if it is not done, answers `status: "running"` with a
`task_id` and a `next_step`. Follow it with `get_task_status` and `wait_seconds` (it sleeps on the
task's state; it does not poll), or stop it with `cancel_task`. A debate usually takes a minute or
more, so expect a task id.

| State | Meaning |
|---|---|
| `pending` | queued, waiting for a slot |
| `running` | an agent process is working |
| `completed` | finished; check `ok` (a partial debate is `completed` with `ok: false`) |
| `failed` | the agent or the engine failed (`error.kind` says how) |
| `cancelled` | cancelled by `cancel_task`, by the client cancelling the request, or at shutdown |
| `timed_out` | exceeded the agent timeout or the task limit; its processes were terminated |

Limits (config `[mcp]`): `max_concurrent_tasks` 4 running at once, `max_queued_tasks` 16 waiting
(more are rejected as `busy`), `max_task_secs` 1800 hard cap, `task_retention` 100 finished tasks
remembered. Agent processes are additionally bounded by `general.max_concurrent_agents`. On
shutdown (client disconnect, SIGINT or SIGTERM) every task is cancelled and its agent processes are
terminated before the server exits.

## Setup

AI Orchestra **never registers itself** with any host. `ai-orchestra mcp config <host>` prints the
exact commands for you to run; nothing is written or changed. Syntax below was read from each CLI's
own `mcp add --help`.

Build and note the absolute path of the binary:

```sh
cd ~/Dev/ai-orchestra && cargo build --release
ls -l $PWD/target/release/ai-orchestra
```

Use `--host` to say which agent launches the server: that agent is **never called through it**.
`--config <file>` is optional; without it the usual lookup applies (`./ai-orchestra.toml`, ...), and
a host usually starts servers from your project directory, so passing an absolute `--config` is
more reliable.

### Claude Code

```sh
claude mcp add --scope user ai-orchestra -- /path/to/ai-orchestra/target/release/ai-orchestra mcp serve --host claude --config /path/to/ai-orchestra/ai-orchestra.toml
```

`--scope` is `local` (this project, just you; the default), `user` (all your projects) or `project`
(a shared `.mcp.json`). Remove with `claude mcp remove ai-orchestra`.

### Codex CLI

```sh
codex mcp add ai-orchestra -- /path/to/ai-orchestra/target/release/ai-orchestra mcp serve --host codex --config /path/to/ai-orchestra/ai-orchestra.toml
```

Remove with `codex mcp remove ai-orchestra`. (Codex stores servers in `~/.codex/config.toml`.)

Details verified locally for Codex (config keys and values read from `codex mcp get` with a
temporary `CODEX_HOME`, no model call):

- Per server, Codex supports `startup_timeout_sec`, `tool_timeout_sec` and
  `default_tools_approval_mode` (`auto`, `prompt`, `writes` or `approve`). Without a setting, an
  interactive Codex asks before using a tool and a non-interactive `codex exec` cannot approve it.
  For an unattended run, pass it **per run** and only for this server:
  `codex exec -c 'mcp_servers.ai-orchestra.default_tools_approval_mode="approve"' ...`
  (Codex honors that override without changing the file).
- A host may start servers with a minimal environment. Register with `--env HOME=... --env PATH=...`
  (`codex mcp add --env KEY=VALUE`), and give each agent an absolute `executable` in the config you
  pass with `--config` (the scheduled test does both). `mcp serve` itself keeps working without a
  home directory; debates are then just not saved and `get_debate` says so.
- A debate or long `ask_agent` returns a task id after `mcp.inline_wait_secs` (30 s). Codex's own
  per-call limit is `tool_timeout_sec`; keep `inline_wait_secs` below it.
- With Codex as the host it is the coordinator: it calls `ask_agent` for Claude and Google and
  synthesizes itself. `debate` through the server uses Claude as coordinator and Google as reviewer,
  never Codex.

### Google Antigravity CLI

```sh
agy mcp add ai-orchestra -- /path/to/ai-orchestra/target/release/ai-orchestra mcp serve --host google --config /path/to/ai-orchestra/ai-orchestra.toml
```

`agy` requires flags before the name and `--` before a command whose arguments start with `-`.
Remove with `agy mcp remove ai-orchestra`; `agy mcp disable ai-orchestra` turns it off without removing.

In headless mode (`agy -p`) agy cannot prompt, so it **auto-denies MCP tool calls** unless
`~/.gemini/antigravity-cli/settings.json` allows them (verified live). The rule format is
`mcp(<server>/<tool>)`, for example:

```json
{ "permissions": { "allow": ["mcp(ai-orchestra/list_agents)", "mcp(ai-orchestra/ask_agent)"] } }
```

In interactive sessions agy asks instead. Use specific tools, not `mcp(*)`. Only `list_agents` has
been exercised through agy so far.

## Verify that a host discovers the tools

1. **The server itself** (no host involved; sends no model request). This pipes a handshake and a
   `tools/list` request and prints what the server answers:

   ```sh
   printf '%s\n' \
     '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"check","version":"0"}}}' \
     '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
     '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
   | ./target/release/ai-orchestra mcp serve --host claude \
   | python3 -c 'import sys,json
   for l in sys.stdin:
       m=json.loads(l)
       if m.get("id")==2: print(", ".join(sorted(t["name"] for t in m["result"]["tools"])))'
   # ask_agent, cancel_task, debate, get_debate, get_task_status, list_agents
   ```

   Every line the server writes to stdout must be one JSON object; if your own pipeline sees anything
   else, that is a bug.

2. **The host's view:** `claude mcp list` (then `/mcp` inside a session), `codex mcp list` and
   `codex mcp get ai-orchestra`, `agy mcp list`. `claude mcp list` may start the server to check
   its health.

3. **From inside the host:** ask it to call `list_agents`. The reply shows the host, each agent's
   installed version and whether it is callable.

## Using another agent

Examples to say to the host:

- *"Use the ai-orchestra `list_agents` tool and tell me which agents are available."*
- *"Use `ask_agent` to ask codex to review this design for concurrency problems, then summarize."*
- *"Run an ai-orchestra `debate` on this architecture decision and show me the disagreements."*
  (If the debate returns a task id, the host follows it with `get_task_status`.)

When the host coordinates (Claude Code asking Codex and Google), it uses `ask_agent` for each peer
and does its own synthesis. A `debate` with the host as coordinator is refused with that
explanation; a `debate` without a coordinator picks another available agent and never uses the host
as a reviewer.

## Preventing recursive execution

Connecting AI Orchestra to Claude Code and then calling Claude again would start a second copy of
the session you are in. Four independent layers prevent loops:

1. **The host is never called.** `--host` (or `mcp.host`, or detection from the agent's session
   markers) names it. `ask_agent` and `debate` refuse it with a clear explanation, and it is excluded as
   a reviewer. `list_agents` shows `is_host` and `callable: false`. If the host is unknown, the server
   warns on stderr and relies on the other layers; set `--host`. (`mcp.allow_host_agent = true`
   overrides this on purpose.)
2. **Delegation depth.** Every agent ai-orchestra starts runs with `AI_ORCHESTRA_DEPTH` one higher
   (and `AI_ORCHESTRA_ACTIVE=1`). `ai-orchestra` refuses to start (`mcp serve`, `ask`, `debate`) at
   `general.max_depth` (default 1), so an agent that somehow loads the MCP server again fails
   safely instead of recursing. This is the guarantee for agents with no isolation option.
3. **Isolated children** (`mcp.isolate_children`, default on), where the child's CLI has a mechanism:

   | Agent | Mechanism | Status |
   |---|---|---|
   | Claude | `--strict-mcp-config` with no `--mcp-config`: loads no MCP servers (passed on every call) | in `claude --help`; used since Milestone 1 |
   | Codex | `--ignore-user-config`: does not load `$CODEX_HOME/config.toml`, where MCP servers live (auth still works) | documented in `codex exec --help`; **not verified live**. `-c 'mcp_servers={}'` was tried and did **not** suppress a configured server, so it is not used |
   | Google (`agy`) | none exists in `agy --help` | cannot be isolated: layers 1, 2 and 4 apply |

   With Codex isolation, model and effort come from ai-orchestra's own config (or Codex's defaults),
   not from your `config.toml`.
4. **Bounds.** Concurrent tasks, queue length, per-task time and output size are all limited, and
   agents are read-only with no write option.

An MCP tool result is data. The server instructions tell the host to treat agent answers as advice
to weigh, never as an instruction to run anything, and nothing here executes agent output.

## Security and subscription limits

- No API keys or model APIs; the official CLIs and your existing logins only. API-key environment
  variables are stripped from agent processes (`general.scrub_api_key_env`).
- **Every `ask_agent` or `debate` spends your subscription quota** on the agent called (a 3-agent,
  2-round debate is 6 requests). Quota and auth failures come back as `quota` / `auth` errors and are
  never retried. A host that loops on tools can burn quota; the queue and task limits cap this but do
  not remove it.
- Agents are read-only and run in the server's working directory (the directory the host starts it
  in), so they can **read** files there. There is no working-directory parameter, so a caller cannot
  point agents elsewhere.
- Inputs are validated (provider, prompt size, timeouts, rounds); error messages are redacted for
  credential-shaped text; `list_agents` shows executable paths and versions but never environment
  variables.
- The server is local stdio only: no network listener.

## Troubleshooting

| Symptom | Likely cause and fix |
|---|---|
| Host shows the server as failed / not connected | Run the pipeline check above with the same command and `--config`. Use absolute paths; hosts may start servers from another directory. |
| `refusing to start ... AI_ORCHESTRA_DEPTH` | The host was itself started by ai-orchestra (depth ≥ `general.max_depth`). Intended; raise `max_depth` only deliberately. |
| `host_agent_blocked` | You asked for the host. Ask another agent, or have the host answer itself. |
| `status: running` with a task id | Normal for slow agents: `get_task_status` with `wait_seconds: 55`. |
| `error.kind: quota` / `auth` | The target agent's subscription limit or login; fix it with that CLI. |
| `error.kind: busy` | Too many tasks waiting; wait or `cancel_task`. |
| Tools missing in the host | Restart the host session after registering; check `list` output; check `claude mcp list` etc. |
| Nothing on stdout / garbled protocol | Run with `-vv` and read **stderr**; stdout must stay JSON-RPC only. |

## Scheduled live test with Codex as host

`live-tests/mcp-run.sh` registers the server with Codex and tests it live, once, at 14:30 on
2026-10-11 (see the note in [`validation.md`](validation.md)). It is the first real-host test; results are written to
`live-tests/results/mcp-<time>/results.md`.

## Verification status

| What | How | Result |
|---|---|---|
| Initialization, capabilities, discovery, schemas, annotations | real `rmcp` client ↔ real server (in memory) | pass |
| All seven tools, validation, bounded output, errors, timeouts, task states, cancellation, concurrency, debate, retrieval, recursion layers | same, mock agents; barriers and task state, no sleeps | pass |
| The real binary over real pipes: real client, stdout purity under `-vv`, SIGTERM shutdown that kills a running agent, depth limits, setup output | `tests/mcp_e2e.rs`, fake agent CLIs | pass |
| Raw handshake against the release binary | the pipeline above | pass |
| Codex as host: discovery, `ask_agent`, host refusal, coordination, `debate`, and task ids | `live-tests/mcp-run.sh`, run 2026-10-11 14:30 | **verified live** (all passed; the task-id path was verified separately with Claude Code and Codex) |
| Codex `--ignore-user-config` really stopping MCP loading | same job, `mcp_isolation` | **verified live** (registered server not started with the flag) |
| Claude Code as host (discovery, `ask_agent`, host refusal, coordination, `debate` with a task id) | `live-tests/mcp-hosts-run.sh`, 2026-10-11 16:02 | **verified live** |
| Codex following a debate task id with `get_task_status` | same job | **verified live** |
| `consult` through MCP: automatic and manual routing, host exclusion and refusal, partial results, tasks, cancellation, clipping, depth limit; and through the real binary with isolated, read-only, depth-marked agents | `tests/mcp_inprocess.rs`, `tests/mcp_e2e.rs` (mock and fake agents) | pass; **not run against real hosts yet** |
| Antigravity as host (`list_agents`) | `live-tests/mcp-agy-run.sh`, 2026-10-11 16:14 | **verified live** in `accept-edits` mode with a temporary `permissions.allow` rule `mcp(ai-orchestra/list_agents)`; headless agy auto-denies MCP tools without such a rule; `plan` mode was unreliable |

## Proposed first live test

Minimal, reversible, one subscription request, scoped to a throwaway directory. Run it yourself
from a plain terminal (not inside another agent session):

```sh
mkdir -p ~/mcp-trial && cd ~/mcp-trial
# 1. Register for this directory only (--scope local), using the real paths:
claude mcp add --scope local ai-orchestra -- /path/to/ai-orchestra/target/release/ai-orchestra mcp serve --host claude --config /path/to/ai-orchestra/ai-orchestra.toml
claude mcp list            # expect: ai-orchestra ... connected
# 2. Start Claude Code here and say:
#      "Use the ai-orchestra list_agents tool and show me the result."          (0 requests)
#      "Use ask_agent to ask google to reply with exactly: ORCHESTRA_MCP_OK"    (1 request)
# 3. Clean up:
claude mcp remove ai-orchestra
```

Expected: Claude Code asks permission to use the tool, `list_agents` shows `claude` as the host and
not callable, and the answer contains `ORCHESTRA_MCP_OK` with no second Claude session started. Then
try Codex as the host the same way (`codex mcp add ...`, `codex mcp list`).
