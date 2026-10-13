# Live validation record

What has been verified against the real CLIs, what has not, and how to reproduce it.
Everything here uses the existing logged-in subscriptions only: no API keys, no login
flows, no changes to authentication settings.

> **About the `live-tests/` paths in this record.** Live runs were made with a maintainer-local harness
> (`live-tests/`: one-shot macOS LaunchAgents, scripts, logs and results). It holds machine-specific paths and
> copies of CLI settings, so it is **not part of this repository**; the results it produced are summarised in
> this document and in the other docs. Every live run needs the maintainer's explicit authorization and the
> logged-in CLIs; nothing in the test suite calls a real model.

Environment: macOS (Darwin 21.6), Rust 1.99, `ai-orchestra` release build.
Binaries: `claude` 2.1.296 · `codex` 0.160.1 · `agy` 1.3.3 (all in `~/.local/bin`).

## Summary

| Provider | Request | Status | Elapsed (wall clock) | Errors |
|---|---|---|---|---|
| Google (`agy`) | `Reply with exactly: ORCHESTRA_GOOGLE_OK` | **PASS** (exact match) | 19.89 s | none; exit 0, stderr empty |
| Google (`agy`) | `Reply with exactly: pong` | PASS (run by the user) | 12.97 s (reported) | none |
| Claude | `Reply with exactly: ORCHESTRA_CLAUDE_OK` | **PASS** (exact match; run by the user in a plain terminal) | 4.55 s (`/usr/bin/time -p` real; 4541 ms reported) | none |
| Claude | same request, inside a Claude Code session | refused by the recursion guard, as intended | n/a | `refusing to start ... running inside claude (CLAUDECODE is set)`, exit 1, nothing spawned |
| Claude | `Reply with exactly: pong` | PASS (run by the user, plain terminal) | 4.49 s (reported) | none |
| Codex | `Reply with exactly: ORCHESTRA_CODEX_OK` | **PASS** (exact match) | 8.35 s | none; exit 0, stderr empty |
| Codex | `Reply with exactly: pong` | PASS (run by the user) | 6.42 s (reported) | none |

"Reported" means the `duration_ms` printed by `ai-orchestra --json`, taken from output the
user pasted; it was not re-measured in this session. The Google `ORCHESTRA_GOOGLE_OK` row was
measured end to end around the whole `ai-orchestra` process; it agrees with the CLI's own
`duration_ms` (19876 ms).

### Why Claude was run outside the validation session

The validation session ran inside Claude Code (`CLAUDECODE` is set), and AI Orchestra refuses
to start a CLI from inside that same CLI's session. That is the intended behavior and the
project's rule against recursive invocation of the running agent; it was not bypassed
(`allow_nested` stays `false`, the variable was not unset). The exact-string run was therefore
done by the user in a plain terminal, and the output below is theirs, pasted verbatim.

## Detail: Claude, `ORCHESTRA_CLAUDE_OK`

Command: `/usr/bin/time -p ai-orchestra ask claude "Reply with exactly: ORCHESTRA_CLAUDE_OK" --json`

| Check | Result |
|---|---|
| Executable / version | `claude` 2.1.296 at `~/.local/bin/claude` |
| Invocation (from the adapter) | `claude -p --output-format json --no-session-persistence --strict-mcp-config --permission-prompts none --tools Read,Grep,Glob`, prompt on stdin |
| Output | one JSON document, parsed into the normalized response; `text == "ORCHESTRA_CLAUDE_OK"` exactly |
| Normalized fields | `provider=claude`, `session_id` present, usage `input_tokens=2`, `output_tokens=20`, `cached_input_tokens=4719`, `reported_cost_usd=0.0090759` (informational: subscription login, nothing billed per request), `truncated=false` |
| Elapsed | 4541 ms per the response; `real 4.55 s` (`user 1.55`, `sys 0.84`) around the whole process |
| Exit code / stderr | not echoed in the pasted output; the JSON was printed and no error text appeared. Exit 0 is implied by the parse and exact match |

Caveat: the paste does not show the exit status or stderr separately, so those are inferred,
not measured. The Google run, which was captured in this session, did record both (exit 0,
stderr empty).

## Detail: Codex, `ORCHESTRA_CODEX_OK`

Command: `ai-orchestra ask codex "Reply with exactly: ORCHESTRA_CODEX_OK" --json`

| Check | Result |
|---|---|
| Executable / version | `codex` 0.160.1 at `~/.local/bin/codex`; `codex login status` reports a ChatGPT (subscription) login |
| Invocation (from the adapter) | `codex exec --json --ephemeral --skip-git-repo-check --color never --sandbox read-only -`, prompt on stdin |
| Exit code | 0 |
| stdout | JSONL events parsed into the normalized response; `text == "ORCHESTRA_CODEX_OK"` exactly |
| stderr | empty |
| Normalized fields | `provider=codex`, `session_id` present, usage `input_tokens=19378`, `output_tokens=10`, `cached_input_tokens=12288`, `truncated=false` |
| Elapsed | 8.35 s end to end; 8342 ms per the response |

Observation: Codex is billed in much larger prompts than the others (about 19k input tokens for a
one-line request, most of it Codex's own system context, 12k of it cached), but the latency
(6.4 s and 8.4 s in two runs) sits between Claude and Google.

## Detail: Google, `ORCHESTRA_GOOGLE_OK`

Command: `ai-orchestra ask google "Reply with exactly: ORCHESTRA_GOOGLE_OK" --json`

| Check | Result |
|---|---|
| Executable detection | `agy` found at `~/.local/bin/agy`, version `1.3.3`. `antigravity` is not on `PATH`. A legacy `gemini` binary is also installed; it is a different product and is deliberately not used. |
| Interface | `agy --help` still lists `-p/--print`, `--output-format`, `--mode`, `--model`, `--print-timeout`. |
| Invocation (from the adapter) | `agy --output-format json --mode plan -p "<prompt>"` |
| Exit code | 0 |
| stdout | one JSON document, parsed successfully into the normalized response; `text == "ORCHESTRA_GOOGLE_OK"` exactly |
| stderr | empty |
| Normalized fields | `provider=google`, `session_id` present, usage `input_tokens=13558`, `output_tokens=693`, `truncated=false` |
| Elapsed | 19.89 s end to end; 19876 ms per the response |

Observations:

- Latency varied between runs of the same trivial prompt (12.97 s, then 19.89 s). Treat `agy`
  as the slowest provider and budget timeouts accordingly (the 300 s default is ample).
- `output_tokens` is large for a one-token answer (1002 and 693 in two runs), so `agy` counts
  hidden reasoning. Quota use per request is higher than the answer length suggests.
- `agy` has no auth-status command, so `doctor` reports its login state as unknown; a
  successful request is the only proof of login.

## Debate, live runs of the *previous* workflow

> **Superseded workflow.** Runs A, B and C below used the first Milestone 2 workflow: the
> coordinator *revised* its proposal between rounds, round 2 reviewers saw only their own earlier
> review, and the final answer had four sections. The v0.2 revision changed that (reviewers read
> each other's reviews in round 2, the coordinator synthesizes once at the end with six sections,
> default one round, maximum two, partial results exit 5). These runs remain valid evidence for
> the adapters, the live view, the log file, model/effort reporting, quota wording and the `agy`
> empty-response finding. They are **not** evidence for the revised workflow, which has not been
> run live. Step names below (`critique`, `revision`, `final`) are the old ones.

Run by the user in a plain terminal; output pasted and the log file `/tmp/debate.jsonl` inspected
afterwards. Codex was out of subscription credits for both runs.

### Run A: `debate "In two sentences: tabs or spaces?" --peers google --rounds 1 --live --log-file /tmp/debate.jsonl`

| Step | Agent | Settings shown | Elapsed |
|---|---|---|---|
| initial proposal | claude | `claude-sonnet-5-5 (requested sonnet) · effort medium` | 4.5 s |
| critique (round 1) | google | `gemini-3.8-flash-medium · CLI-default effort` | 14.6 s |
| final answer (round 1) | claude | `claude-sonnet-5-5 (requested sonnet) · effort medium` | 10.4 s |
| total | | 3 calls (= 1 + 1 x (1 + 1)), complete | 29.6 s |

Result: PASS. The critique was substantive (it caught that the proposal was three sentences, not
two), the coordinator adopted or rejected each point with attribution, kept two points as
"Unresolved" under Disagreements, and the final answer was two sentences.

### Run C: two rounds, `debate "In three sentences: monorepo or polyrepo for a team of five?" --peers google --rounds 2 --live --log-file /tmp/debate2.jsonl`

| Step | Agent | Elapsed |
|---|---|---|
| initial proposal (round 0) | claude (`claude-sonnet-5-5`, effort medium) | 5.6 s |
| critique, round 1 | google (`gemini-3.8-flash-medium`) | 9.6 s |
| revision, round 1 | claude | 8.3 s |
| critique, round 2 | google | 9.9 s |
| final answer, round 2 | claude | 11.0 s |
| total | 5 calls (= 1 + 2 x (1 + 1)), complete | 44.4 s |

Result: PASS. The log (12 lines, 5 `call_finished`) shows the expected sequence
proposal, critique, revision, critique, final, and the call durations sum to the total.

- The round-2 critic referred back to its own round-1 critique ("Assessment of Previous
  Critiques"), so the previous-critique prompt path works live.
- The round-1 coordinator reply was an adopted / partially adopted / rejected list followed by
  a full revised proposal; the round-2 reply used the final Answer / Reasoning / Disagreements /
  Open risks format.
- No empty `agy` response in either critique round (two successes, not proof of a fix).
- No early exit on consensus: round 2 ran although the critic's round-1 verdict was already
  "AGREE WITH CHANGES" and the round-2 verdict was "AGREE". Rounds are fixed by design.

Defect found: in Runs A and C the final answer claimed other panel members "did not respond",
although `--peers google` meant nobody else was asked. The coordinator prompt said to mention
any agent that did not respond without saying who was on the panel. Fixed: the prompt now states
exactly which critics were on the panel and not to mention others (unit-tested; not yet verified
live).

### Run B: earlier run, same question, no `--peers` (critics codex and google)

Both critics failed, which exercised the failure paths live:

| Agent | Outcome | Elapsed |
|---|---|---|
| codex | `usage limit or rate limit reached: You've hit your usage limit ... try again at 1:27 PM`, classified as a **quota** error with the reset time preserved | 4.3 s |
| google | `the agent returned an empty response` | 38.5 s |

Result: the debate ended `complete: false` after 3 calls and stopped with the coordinator's
proposal instead of crashing, as designed (no critique from any peer: no coordinator call, no
second round).

### What this verified

- The full workflow against real CLIs, with parallel critics, within the stated call bound.
- `--live` rendering and `--log-file`: 16 JSON lines across the two runs (appended, not
  truncated), timestamps monotonic, owner-only permissions (0600), per-call `settings` recorded.
- `model`/`effort` settings: `claude --model sonnet --effort medium` accepted; `agy --model
  gemini-3.8-flash-medium` accepted (agy's own log shows `model="gemini-3.8-flash-medium"` and
  the label "Gemini 3.8 Flash (Medium)").
- **Claude reports the model it used** (`modelUsage`): `claude-sonnet-5-5`.
- Real Codex quota wording is classified as a quota error.
- Peer failure handling and the early-stop path (Run B).
- `agy` plan mode denies commands in print mode: its log shows `soft-denying tool confirmation
  "RunCommand"`. This is evidence for read-only behavior, though not a write test.

### Finding: `agy` can return an empty response after a denied tool call

Run B's Google failure is explained by `agy`'s log for that call: the model tried to run a
command (`RunCommand`, step 14), plan mode denied it, and the turn ended with no text. At the
time `agy` was on its default model (Gemini 3.8 Flash (High)); the same kind of prompt succeeded
on `gemini-3.8-flash-medium` in Run A, but one success is not proof of a fix.

Changes made in response (unverified live): every debate prompt now says to answer in plain text
from the message itself and not to stop if a tool or command is denied, and the empty-response
error now explains the likely cause and suggests retrying. If it recurs, the error and the
`agy` log (`~/.gemini/antigravity-cli/log/`) identify it quickly.

### Still not verified live

- Codex inside a debate, and `-c model_reasoning_effort="medium"` with `gpt-6-luna` (no credits).
- Whether the new prompt wording prevents the empty `agy` response, and whether the roster
  wording stops the invented "non-responding agents" line.

## v0.2 debate workflow: first live run

Run by the user in a plain terminal; the saved record (`~/.local/share/ai-orchestra/debates/`)
and the log file were inspected afterwards.

```sh
ai-orchestra debate "What is the best architecture for a high-performance Rust MCP server?" \
  --coordinator claude --rounds 2 --live --log-file /tmp/debate.jsonl
```

Result: **partial, as designed** (Codex was out of credits). Id `20261011-163016-00095a`,
5 calls, 87.8 s. The banner reported exit code 5 (the shell's exit status itself was not shown).

| Step | Agent | Settings | Started at | Duration |
|---|---|---|---|---|
| initial proposal | claude | `claude-sonnet-5-5 (requested sonnet)` · effort medium | 0 ms | 17.2 s |
| review, round 1 | codex | n/a: FAILED, `quota` | 17 188 ms | 6.3 s |
| review, round 1 | google | `gemini-3.8-flash-medium` | 17 188 ms | 40.4 s |
| cross-review, round 2 | google | `gemini-3.8-flash-medium` | 57 582 ms | 10.5 s |
| final synthesis | claude | `claude-sonnet-5-5 (requested sonnet)` · effort medium | 68 101 ms | 19.7 s |

What this verified against real agents:

- **Parallel reviews.** Both reviewers started in the same millisecond (offset 17 188 ms), and the
  cross-review began exactly when the slower reviewer finished (17 188 + 40 393 = 57 581 ms).
- **Cross-review with a real agent.** Google's round-2 answer reconsidered its round-1 review: it
  withdrew its `rmcp` concern, said no other reviewer changed its mind (Codex had no response),
  and restated five disagreements it still held.
- **No retry.** Codex was asked once; the round-2 pass asked only Google.
- **Quota failure classified correctly** (second live sample of Codex's wording, with the reset time).
- **Status semantics.** `partial`, the `PARTIAL RESULT` banner, a warning naming the failure, and
  exit code 5 (per the banner), while the synthesis was still produced and printed.
- **Synthesis.** All six required sections were present. Claude named Codex as `no response`
  instead of inventing absentees (the roster fix from the earlier run works), kept Google's
  objections attributed and marked resolved or unresolved, flagged a claim it could not verify, and
  recommended re-running with Codex after its quota resets.
- **Persistence.** The record is `schema 1`, 31.8 KB, file mode 0600 in a 0700 directory;
  `history` and `show` read it back, including per-step offsets, durations, error kind and settings.
- **Model reporting.** Claude reported `claude-sonnet-5-5`; Google's pinned model was shown as requested.

Defects found in this run, fixed afterwards (tests added, not re-verified live):

1. A raw tracing line (`WARN reviewer failed ...`) appeared in the middle of the `--live` output.
   Failures are already shown and recorded, so they are no longer logged at warn level.
2. The same two-line Codex error was printed three times (live block, the WARN line, the final
   warning). Warnings are now one line (the full text stays in the saved step), so it is shown
   twice at most: the progress line and the final warning.
3. The `history` table's ID column was narrower than the real ids (22 characters), misaligning
   the columns. It now sizes to the widest id.

Observations (no change made):

- **Agents run in the working directory and can read it.** Claude's proposal mentions "a manager
  type like `ProcessManager`", which is this project's own type, so it very likely read the local
  source. Read-only access is intended, but for a generic question it can bias the answer. Run
  from a neutral directory, or pass `--cwd <dir>`, when the question is not about the local code.
- **`--log-file` appends.** The reused `/tmp/debate.jsonl` mixed three runs, two in the previous
  event format (`peers`, roles `critique` / `final`) and one in the current one. Use a fresh file
  per run.
- Google's round-1 review was the slowest step (40.4 s), as before; it sets the pace of a debate.

Still not verified live for v0.2:

- A **complete** debate in which all three agents answer (Codex's review and cross-review).
- `--rounds 1`, and the default short progress lines (this run used `--live`).
- The Codex session markers, and `AI_ORCHESTRA_ACTIVE` against a real nested run.
- Everything listed under "Still not verified live" above (real timeout, auth wording, write test).

### Mock-run verification of the v0.2 workflow

Requirement-by-requirement, where it is tested
(`src/debate.rs` unit tests with in-process mocks; `tests/mock_cli.rs` against fake shell CLIs):

| # | Requirement | Test(s) |
|---|---|---|
| 1 | Initial proposal | `one_round_discussion_has_the_expected_shape`, `initial_proposal_failure_is_fatal_...` |
| 2 | Parallel reviewers, deterministic | `reviewers_run_in_parallel_not_in_sequence` (a barrier opens only when all reviewers are inside `invoke` at once; no timing assumptions) |
| 3 | Configurable coordinator | `any_provider_can_coordinate_with_the_same_algorithm`, e2e `every_provider_can_be_the_coordinator` (claude, codex, google, agy) |
| 4 | One-round discussion | `one_round_discussion_has_the_expected_shape` (4 calls), e2e `three_agents_debate_with_one_review_round_by_default` |
| 5 | Two-round discussion | `two_round_discussion_shares_round_one_reviews_for_cross_review` (6 calls; round 1 independent, round 2 sees all round-1 reviews), e2e `two_rounds_add_a_cross_review_...` |
| 6 | Reviewer timeout | `reviewer_timeout_makes_the_debate_partial_and_is_not_retried`, e2e `a_reviewer_timeout_is_terminated_recorded_...` (real process killed) |
| 7 | Missing provider | `missing_providers_are_skipped_with_a_reason_or_rejected`, e2e `a_missing_reviewer_is_skipped_...` |
| 8 | Malformed response | `malformed_reviewer_output_is_recorded_...`, e2e `malformed_reviewer_output_is_partial_not_a_crash` |
| 9 | Coordinator failure | proposal: fatal + saved as `failed`; synthesis: `coordinator_failure_during_synthesis_keeps_the_proposal_and_reviews`, e2e `a_coordinator_that_fails_the_synthesis_...` |
| 10 | Persisted state | `conversation_state_is_persisted_and_reads_back_identically`, `fatal_failures_are_saved_as_failed_records`, `a_failure_to_save_is_a_warning_...`; `src/history.rs` tests for atomic writes, 0600/0700, redaction, ids, prefix lookup; e2e `history_...`, `show_rejects_...` |
| 11 | Final synthesis | the six required sections, roster of reviewers, "never hide disagreement", all asserted in the one- and two-round tests |
| 12 | Cancellation | `cancellation_stops_the_debate_and_is_recorded` (cancels once a reviewer has demonstrably started), e2e `ctrl_c_cancels_the_debate_...` (a real SIGINT: exit 130, agents terminated, saved as `failed`) |
| 13 | Milestone 1 backward compatibility | all Milestone 1 `ask` / `doctor` / guard / error tests are unchanged and pass |
| | Partial failure is never reported as success | status `partial` and exit code 5 for a failed reviewer, round or synthesis: e2e `a_reviewer_out_of_quota_...exit_5...`, timeout, malformed, synthesis tests |
| | No automatic retry of quota failures | `quota_failures_are_never_retried` (the reviewer is called once even with two rounds) |
| | At most two rounds | `rounds_are_limited_to_one_or_two`, e2e `--rounds 3|0|9` rejected |
| | No nested orchestration / recursive Codex | `nested_orchestration_is_refused_unless_explicitly_allowed`, e2e `agents_are_marked_so_a_nested_orchestra_refuses_to_run`, `a_codex_session_marker_stops_codex_...` |

### Performance (mock runs)

Measured with `cargo test --release overhead -- --nocapture` on the development machine
(macOS, x86_64). Agents answer instantly, so these numbers are the orchestrator's own cost.

| Measurement | Result |
|---|---|
| In-process, 3 mock agents, 2 rounds (6 calls), 300 debates | **717 µs per debate**, 120 µs per call, slowest 1.6 ms |
| Whole process, 6 calls to 3 fake shell CLIs, 7 samples (after warm-up) | wall **min 34.8 / median 36.6 / max 39.8 ms**; engine time min 30 / median 31 / max 34 ms |

The process-level figure is dominated by spawning the fake scripts, not by orchestration. Real
agents take 5 to 40 s per call, so orchestration overhead is negligible next to them. A fresh
executable's *first* launch on macOS can take seconds (the system scans it); the end-to-end tests
launch each fake CLI once during fixture setup so that cost never lands inside a timed test.

### Test-suite findings during this work

- **ID collisions (fixed).** 500 ids generated in one second produced 499 distinct values with a
  16-bit random suffix. Ids now carry a per-process counter plus randomness, and a debate checks
  the history directory before reusing an id.
- **Date arithmetic** is hand-written (no date crate); its expected values were cross-checked
  against Python's `datetime`.
- **macOS first-exec scan** made timeout-bounded fake-CLI tests flaky until each fake binary was
  launched once during setup.

### Still to verify live

A complete three-agent debate, once Codex has credits again (from a plain terminal; 6 requests;
use a fresh log file):

```sh
./target/release/ai-orchestra debate \
  "What is the best architecture for a high-performance Rust MCP server?" \
  --coordinator claude --rounds 2 --live --log-file /tmp/debate-3agents.jsonl
```

## v0.3 MCP server: verified with mocks and a real MCP client

No real host (Claude Code, Codex CLI, Antigravity) has been connected and no subscription request
was made. Details and setup: [`mcp.md`](mcp.md).

| Requirement | Test |
|---|---|
| Initialization, capability negotiation | `initialization_negotiates_and_advertises_tools_only`; raw handshake against the release binary (negotiated 2025-06-18) |
| Tool discovery and valid JSON schemas | `tool_discovery_lists_six_tools_with_valid_schemas` (required fields, round limits in the schema, annotations) |
| Input schema validation, malformed input | `malformed_input_is_rejected_...` (wrong types, missing and unknown fields, out-of-range values, unknown tool); rejected requests never reach an agent |
| Successful agent invocation | `ask_agent_returns_a_structured_answer` (read-only, isolated, structured and text forms) |
| Provider errors preserved | `provider_failures_arrive_as_structured_errors_with_their_kind` (quota, auth, malformed, other) |
| Timeout handling | `timeouts_are_reported_as_timed_out_not_failed` (provider timeout; hard task limit stops a stuck agent) |
| Async task status | `long_calls_become_tasks_that_can_be_followed_to_completion`; task manager tests for pending, running, completed, failed, cancelled, timed_out |
| Cancellation | `cancel_task_stops_a_running_agent_...`, `debates_run_as_cancellable_tasks`, e2e SIGTERM test (agent processes are killed) |
| Debate integration | `debate_runs_the_existing_engine_...`, `a_partial_debate_is_flagged_as_incomplete`; e2e debate through the real binary |
| Conversation retrieval | `debates_can_be_retrieved_by_id_or_prefix_afterwards`; e2e: the MCP record is readable by `ai-orchestra show` |
| Recursion prevention | host never called (`ask_agent`, `debate` coordinator, reviewers), `the_host_can_be_allowed_explicitly`, `excessive_delegation_depth_is_refused`, e2e depth tests (`AI_ORCHESTRA_DEPTH`), children run with depth + 1, isolation flags reach Claude and Codex |
| Concurrent MCP requests | `concurrent_requests_respect_the_task_limits` (barrier: two tasks must be inside the agent at once; the third is rejected as `busy`), `many_parallel_requests_...` |
| stdout protocol purity | e2e: every stdout line is JSON-RPC with `-vv` and `RUST_LOG=debug`; logs appear only on stderr; refusing to start writes nothing to stdout |
| Backward compatibility | the 49 Milestone 1 and 2 end-to-end tests and all earlier unit tests pass unchanged (one unit test was updated: nesting is now detected by delegation depth) |
| Real MCP client against the real server | `tests/mcp_inprocess.rs` (in memory) and `tests/mcp_e2e.rs` (child process, `rmcp` `TokioChildProcess`) |

Current counts: 120 unit + 6 MCP end-to-end + 20 MCP in-process + 49 CLI end-to-end tests, none
contacting a provider.

### Verified facts about the host CLIs (local help output and local experiments only)

- `claude mcp add [--scope local|user|project] <name> -- <command> [args...]`; Claude's
  `--strict-mcp-config` (already used for every call) ignores all MCP configuration.
- `codex mcp add <NAME> [--env K=V] -- <COMMAND>...`; `codex exec` has `--ignore-user-config`
  ("Do not load $CODEX_HOME/config.toml; auth still uses CODEX_HOME").
- `agy mcp add [flags] <name> <commandOrUrl> [args...]` (flags before the name; `--` before arguments
  starting with `-`); `agy mcp list` reported no servers; `agy --help` has no option to skip MCP servers.
- **Tried and rejected:** `codex mcp list -c 'mcp_servers={}'` still listed a configured server (using a
  temporary `CODEX_HOME`, not the real one), so a config override is not used for isolation.

### Findings during this work

- `rmcp` 3.5.1 reports tool parameters that fail to deserialize as a tool result with `isError: true`
  and a readable message, not as a JSON-RPC error. That is what lets a model correct itself, so the
  tests accept either form.
- Two of my own tests were wrong, not the server: one waited on a barrier that also needed the test
  as a party (a hang), and one asserted a protocol error where rmcp returns a tool error.

### Live validation for v0.3, Codex as host (2026-10-11 14:30, scheduled run)

`live-tests/mcp-run.sh` registered the server with Codex (`codex mcp add ai-orchestra ...`, backup of
`~/.codex/config.toml` taken first; the registration stays) and ran the checks with Codex
(`gpt-6-luna`, medium) as host. Results: `live-tests/results/mcp-20261011-143000/results.md`.

| Test | Verdict | Seconds | What it shows |
|---|---|---|---|
| registered | PASS | 0 | `codex mcp get` shows the server |
| mcp_list | PASS | 10.1 | Codex discovered and called `list_agents`: host `codex`, callable `claude`, `google` |
| mcp_ask_google | PASS | 18.6 | `ask_agent` to Google; the answer was relayed; Google received exactly 1 request |
| mcp_host_block | PASS | 13.5 | Codex asking for itself got `host_agent_blocked`; no agent was called |
| mcp_coordinate | PASS | 25.4 | Codex consulted Claude and Google through `ask_agent` and wrote its own synthesis |
| mcp_debate | PASS | 37.0 | `debate` with Claude as coordinator and Google as reviewer: `complete`, 3 calls, Codex excluded, saved record `20261011-203114-000b93`, id relayed by Codex |
| mcp_isolation | PASS | 6.3 | with `AI_ORCHESTRA_DEPTH=1` Codex started the registered server once normally and 0 times with `--ignore-user-config` |

Notes:

- That debate finished inside the inline wait; following a task id is covered by the 16:02 run below.
- `--ignore-user-config` is now verified for Codex: the registered server was not started. The
  isolated run also sent about 260 fewer input tokens than the normal one (17,699 vs 17,959), which
  is consistent with the tool list being absent.
- The 14:00 job (below) used a frozen binary and a config copy; the MCP job used its own frozen
  MCP-capable binary. Neither touched the checkout's `ai-orchestra.toml`.

### Live validation for v0.3, Claude Code as host and task ids (2026-10-11 16:02, manual run)

`live-tests/mcp-hosts-run.sh`; results in `live-tests/results/mcp-hosts-20261011-160156/results.md`.
Claude Code (`claude -p`, sonnet) was configured per run with `--mcp-config` and
`--strict-mcp-config`, so no Claude configuration file was changed. The task-id tests used a server
config with `inline_wait_secs = 2`, which forces a pending task; for Codex this was a per-run
override of the registered server's arguments (nothing written).

| Test | Verdict | Seconds | What it shows |
|---|---|---|---|
| claude_list | PASS | 7.9 | host `claude`, callable `codex`, `google` |
| claude_ask_google | PASS | 15.3 | answer relayed; Google received 1 request |
| claude_host_block | PASS | 9.3 | asking for `claude` got `host_agent_blocked`; no agent called |
| claude_coordinate | PASS | 27.4 | Claude Code called `ask_agent` twice (Codex, Google) and wrote a synthesis |
| claude_debate_task | PASS | 41.6 | debate `20261011-220302-000d5f` (coordinator google, reviewer codex, 3 calls, complete); Claude Code got a pending task, followed it with `get_task_status` and relayed the id |
| codex_debate_task | PASS | 41.3 | debate `20261011-220346-0003e2` (coordinator google, reviewer claude, 3 calls, complete); Codex followed the task id the same way |

### Live validation for v0.3, Antigravity as host (2026-10-11 16:14, manual run)

`live-tests/mcp-agy-run.sh` (results in `live-tests/results/mcp-hosts-20261011-161408/`). `agy` has no
per-run MCP option, so the server was registered with `agy mcp add` for the test and removed again,
and a temporary rule `permissions.allow = ["mcp(ai-orchestra/list_agents)"]` was added to agy's
`settings.json` (backed up first; restored byte for byte afterwards; `agy mcp list` is empty again).
`--dangerously-skip-permissions` was not used.

- A first run without the rule (16:12) showed agy registers and starts the server but headless `agy -p`
  auto-denies the `mcp` permission (`denied_actions: CallMcpTool`).
- With the rule, `--mode accept-edits`: **PASS** (13.5 s). agy discovered and called `list_agents` and
  answered `{"host":"google","callable":["claude","codex"]}`, so host detection and the host block
  work for Antigravity too.
- With the rule, `--mode plan`: no output, because the model tried to read a file (`read_file`), which
  headless mode also auto-denies. That is the model wandering, not an MCP failure; the MCP call was
  not the denied action. Plan mode is therefore unreliable for MCP use with agy.
- Only `list_agents` was exercised through agy. `ask_agent` and `debate` through agy were not run.

### Not verified live for v0.3

- Antigravity isolation: `agy` has no flag to skip its MCP config, so only the depth limit protects it.
- `ask_agent` and `debate` called from Antigravity (only `list_agents` was).
- The hosts' own tool-permission prompts in interactive use (these runs pre-approved the
  ai-orchestra tools through per-run settings).

### Scheduled live validation of earlier milestones (2026-10-11 14:00, scheduled run)

`live-tests/results/20261011-140000/results.md`, run from a frozen copy of the binary: 8 of 8 passed.

| Test | Verdict | Seconds | What it shows |
|---|---|---|---|
| debate3 | PASS | 41.9 | complete three-agent debate, 6 calls (proposal, 2 reviews, 2 cross-reviews, synthesis), no failures. This closes the earlier "Codex answering" gap |
| debate1 | PASS | 32.6 | `--rounds 1`: complete, 4 calls, default progress lines present |
| timeout | PASS | 3.8 | real agy killed at ~3 s, exit 124, no process left behind |
| auth_claude | PASS | 1.5 | not-logged-in run classified as auth failure (exit 3) |
| auth_codex | PASS | 22.4 | 401 classified as auth failure (exit 3) |
| write_claude / write_codex / write_google | PASS | 7.8 / 7.8 / 12.2 | asked to create a file; each answered and no file was created |

The write tests show the agents did not create a file in these runs; they are not proof that each
CLI's read-only mode is enforced in every case.

## v0.4 consultation engine (Milestone 4): verified with mocks and fake CLIs only

`ai-orchestra consult`, the MCP `consult` tool, the router and the five modes are implemented and
tested with in-process mock providers and fake `claude` / `codex` / `agy` shell scripts. **No real
provider has been contacted for Milestone 4.** Everything below that says "pass" is a mock or
fake-CLI result; nothing about answer quality, real routing accuracy or real CLI behavior with these
prompts has been observed.

### What was verified, mapped to the milestone's test list

| # | Requirement | Where it is tested |
|---|---|---|
| 1 | Automatic mode selection | `consult::tests::the_mode_is_chosen_from_the_question_with_fixed_rules` (the example questions of the spec), `a_question_without_cues_defaults_to_research_and_says_so`, `routing_is_deterministic` |
| 2 | Manual mode override | `a_manual_mode_always_wins_and_is_marked_manual` (all five modes), CLI `consult_manual_settings_override_every_automatic_choice` |
| 3, 5 | Automatic coordinator and agents | `the_coordinator_is_the_configured_one_when_usable_else_the_first_usable`, `automatic_agents_are_every_usable_one_and_unusable_ones_are_reported`, CLI `consult_chooses_everything_automatically_and_explains_it` |
| 4, 6 | Manual coordinator and agents | `a_manual_coordinator_wins_and_must_be_usable_and_participating`, `manual_agents_are_honoured_and_unavailable_ones_reported` |
| 7, 8 | Automatic and manual rounds | `rounds_are_one_by_default_and_two_for_contested_or_long_questions`, `manual_rounds_are_respected_even_when_the_question_is_contested` |
| 9 | One-agent consultation | `one_agent_consultation_makes_one_call_and_says_there_was_no_review`, `a_short_lesson_is_answered_by_one_agent_without_extra_calls`, CLI `consult_with_one_agent_...` |
| 10 | Independent perspectives | `a_panel_answers_independently_then_cross_reviews_then_synthesizes` (no first prompt contains another agent's answer) |
| 11 | Parallel execution | `independent_perspectives_run_in_parallel` (a barrier that only two agents together can release, in both stages; sequential execution would deadlock), `parallel_stages_take_the_time_of_the_slowest_agent_not_the_sum` |
| 12, 13 | One and two review rounds | `a_panel_answers_independently_...` (7 calls), `two_rounds_add_a_cross_review_that_sees_the_round_one_reviews` (10 calls); CLI tests with fake agents |
| 14-17 | Decision, research, review, learn | `each_mode_uses_its_own_instructions_and_sections` (instructions, required sections, no invented sources, no numeric probabilities, "not verified externally") |
| 18 | Debate | `debate_mode_runs_the_milestone_two_engine_with_a_consultation_record`, CLI `consult_debate_mode_runs_the_debate_engine_...` |
| 19 | Unresolved disagreement | `unresolved_disagreement_is_kept_and_flagged` (and the resolved case) |
| 20 | Coordinator failure | `a_failed_coordinator_is_not_asked_again_and_the_result_is_partial`, CLI `a_failed_coordinator_leaves_a_labelled_partial_answer_not_a_synthesis` |
| 21, 22 | Reviewer failure, partial success | `a_failed_reviewer_makes_the_result_partial_...`, `a_reviewer_that_fails_in_round_two_...`, `if_nobody_answers_the_consultation_fails_...`, CLI quota/auth tests (exit code 5, no retry) |
| 23 | Timeout | CLI `a_consultation_timeout_terminates_the_agent_and_is_partial` (a real fake process killed), `the_total_time_budget_stops_everything` |
| 24 | Cancellation | `cancellation_stops_a_consultation_...` (recorded `cancelled`), CLI `ctrl_c_cancels_a_consultation_...` (a real SIGINT, exit 130), MCP `a_consultation_task_can_be_cancelled_...` |
| 25 | Persistence | `consultations_are_saved_listed_and_read_back_with_their_routing`, `plain_debate_records_and_old_files_are_unaffected`, CLI `show` / `history` |
| 26 | MCP consultation | `tests/mcp_inprocess.rs` (`consult_*`, tasks, clipping, partial) and `tests/mcp_e2e.rs::consult_through_the_real_binary_...` |
| 27 | MCP recursion prevention | `consult_never_calls_the_host_and_never_lets_it_coordinate`, `consult_with_only_the_host_available_...`, `consult_is_refused_when_delegation_is_too_deep`, the e2e test (host never started; agents read-only, isolated, depth 1) |
| 28 | Invalid CLI parameters | CLI `consult_input_errors_use_the_usage_exit_code_and_call_no_agent` (12 bad invocations, exit 2, no agent called), `mode_arguments_accept_auto_and_reject_unknown_values` |
| 29 | Provider unavailability | `unavailable_agents_are_skipped_and_reported_never_silently`, CLI `an_unavailable_agent_is_reported_...`, `consult_inside_claude_code_never_calls_claude_...` |
| 30 | Backward compatibility | the unchanged Milestone 1-3 suites (the only edit: a cancelled debate is now recorded as `cancelled` instead of `failed`; the MCP server version is 0.4.0 and it lists seven tools) |

Also checked: worst-case prompts for every mode stay below `agy`'s 128 KiB argument limit; `--plan`
makes no call and saves nothing; a mistyped subcommand never becomes a consultation; the example
config still parses.

### Performance (mock runs)

| Measurement | Result |
|---|---|
| 7 calls of 200 ms each (3 mock agents, 1 round), in-process | 636 ms wall; sequential execution would take 1.4 s. The stages are three parallel steps (answers, reviews, synthesis) |
| Real binary, 10 calls to 3 fake shell CLIs (3 agents, 2 rounds), 7 samples | process wall min 45 / median 56 / max 71 ms; engine time 36 / 44 / 55 ms |
| Same measurement for a 6-call `debate` | median 50 ms wall |

These bound the orchestration overhead (spawning, prompt building, persistence); they say nothing
about real provider latency, which dominates, and `agy` is the slowest.

Token savings, cost reductions and answer quality were **not** measured and are not claimed.

### Not verified (no real provider was used)

- That the real CLIs follow the mode prompts (section headings, "plain text even if a tool is
  denied"), and therefore how often the sections read back into the record are present.
- How accurate the keyword router is on real user questions.
- Quota use of a three-agent, two-round consultation (10 requests) on the real subscriptions.
- The `consult` tool inside real hosts (Claude Code, Codex, Antigravity).

### First live consultation test (2026-10-11 16:59-17:00, run from this session with Claude out of the plan)

Steps 0-2 of the commands below were run with `ai-orchestra consult` against the real Google
(`agy`) and Codex CLIs, saved to the real history (`20261011-225942-000c26`, `20261011-230006-0003d2`).
Six subscription requests in total (Google 3, Codex 3). Claude was not used.

| Step | Command | Result |
|---|---|---|
| 0 | `--plan` for "Which electronics microscope should I buy?" | 0 requests; decision, claude coordinating, 3 agents, 1 round, at most 7 calls |
| 1 | `consult "Explain what a resistor does" --agents google --live` | complete, exit 0, 1 call, 24.0 s. Mode `learn` routed automatically; the answer had every required section (`Lesson`, `Prerequisites`, `Corrections made after review`, agreement and disagreement marked "None: no other assistant was consulted", `Assumptions`, `Evidence limitations` saying no independent review took place) preceded by the `Understanding` note |
| 2 | `consult "In two sentences: tabs or spaces for a team of five?" --mode decision --agents codex,google --rounds 1 --live --log-file ...` | complete, exit 0, 5 calls, 38.8 s: answers 9.0 s (Codex) and 10.5 s (Google) in parallel, reviews 9.0 / 11.5 s, Codex synthesis 16.9 s. All ten decision-mode sections present; routing and manual overrides saved; `unresolved_disagreement` true; token counts reported by 5 of 5 calls (84,586 in / 2,359 out); the 12-line JSONL log was created owner-only |

What this showed:

- The workflow worked end to end on the real CLIs: independent answers in parallel, cross-review, a
  synthesis from the coordinator, a saved record that `history` (KIND column) and `show` read back.
- The cross-review did real work. Codex pointed out that Google's "Go requires tabs" overstated
  the case, Google criticised Codex for an unsupported "easier to adjust" claim and for not keeping to
  two sentences, and the synthesis kept both objections under *Significant disagreements* instead of
  smoothing them over.
- Both models followed the section list and the "no invented sources / nothing verified" wording.
- Routing text was misleading in this run: with `--agents codex,google` the plan said the configured
  coordinator "is not available here" although Claude was merely not requested. Fixed afterwards
  (the reason now says it is not among the requested agents); not re-run live.
- **A user's format request was overridden by the mode template.** "In two sentences" was honoured by
  neither agent's long analysis, and the final answer was a ten-section report. Fixed afterwards
  (mock-tested only, not re-run live): a question that asks for a short answer ("in two
  sentences", "briefly", "one line", "tl;dr", a few Spanish phrases) now gets the **brief style**
  automatically, `--length brief|full|auto` (MCP: `length`) overrides it, and it is shown in the
  routing (`length: brief (auto: ...)`). Brief drops the "Understanding" note and the long section
  list: answers keep to the length asked, reviews are at most five bullets, and the final answer has
  `Answer`, `Significant disagreements`, `Assumptions` and `Evidence limitations` (one or two
  lines each). The number of calls is unchanged. The debate engine keeps its own format and reports
  `--length brief` as ignored.
- One request per agent per stage and no retries: Google 3 calls, Codex 3 calls, as planned.
- Not yet run live: the three-agent default, two rounds, `research` / `review` / `debate` modes,
  a failure path, and the MCP `consult` tool inside a host.

### Three-agent default consultation (2026-10-11 17:03-17:46) and an incident

`consult "Which electronics microscope should I buy?" --live` (everything automatic: decision mode,
Claude coordinating, Claude + Codex + Google, 1 round, 7 calls) was started as a one-shot
`launchctl submit` job so that it ran outside the Claude Code session and could include Claude.

**Incident: the job ran about 42 times instead of once.** `launchctl submit` jobs are restarted by
launchd every time they exit, and I did not know that or check for it; my watcher only looked for
the first `exit=` line and the job rewrote its own log each run, so nothing showed it until 40
minutes in. I removed the job (`launchctl remove`) and killed the one leftover `claude` process;
nothing is running now. The cost: **42 consultations, 292 agent calls** on the real subscriptions
(about 125 Claude calls, 85 Codex and 85 Google) instead of 7. The records are in the real history
(`ai-orchestra history`, 42 rows with this question); I have not deleted them. Rule added to
`AGENTS.md`: background one-shot jobs must not auto-restart (use a LaunchAgent plist without
`KeepAlive`, as `live-tests/` does, and always check the process list after a launch).

What the repeated runs did show about `consult` on the real CLIs (40 complete, 2 partial):

- Every complete run used 7 calls: 3 parallel answers, 3 reviews, 1 Claude synthesis. Duration
  51.9 s min / 62.0 s median / 78.6 s max. Token counts were reported by 7 of 7 calls (a typical
  run: 69,225 in / 10,393 out).
- The final answers were substantive and kept to the sections: a direct answer first, an explicit
  "I can't name a specific model: I don't know your budget... and I can't check current prices", an
  alternatives table, and the electron-microscope ambiguity raised by one assistant and preserved.
- Both partial runs failed the same way, and the error message was wrong. The text said Codex's
  output "was truncated at 4194304 bytes", but the stdout it had captured was **empty**. The real
  cause was in `process.rs`: after the CLI exited, a process it had started still held the output
  pipe open, the pipe reader was abandoned after the 2-second grace period, and abandoning it threw
  away every byte already read (and reported "truncated"). So a complete Codex answer was lost.
  Fixed afterwards: captured bytes now survive an abandoned reader, `ProcessOutput.stdout_held_open`
  says what happened, the two pipes are waited for together (2 s, not 4), and a parse failure in that
  situation now says "the CLI exited but a process it started kept its output open, so the output may
  be incomplete" (the 4 MiB wording is only used when output really was cut). Tests: a shell that
  prints and leaves a background job (`process::tests`), and a fake Codex that does the same through
  `ask codex`. Why the real Codex leaves such a process is not known (it ran in a scratch directory
  next to the growing log file, which may have led it to run commands there); this was not
  reproduced with the real CLI after the fix.

### Scheduled live validation of `consult` (2026-10-11 18:51, one-shot LaunchAgent)

`live-tests/consult-live.py`, installed by `consult-install.sh` (no `KeepAlive`, no `RunAtLoad`,
guard file, 16-command cap, stops at the first quota/auth failure), run once from a frozen copy of
the binary in an empty directory. Rehearsed first against fake CLIs. launchd ran it once
(`runs = 1`), nothing was left running, and the agent removed its own schedule (I deleted the plist
it left behind). About 37 subscription requests. Results:
`live-tests/results/consult-20261011-185100/results.md`.

| Test | Verdict | Seconds | What it shows |
|---|---|---|---|
| brief | PASS | 37.2 | "In two sentences: ..." was routed to the **brief style** automatically; complete, 5 calls, none of the long report sections. The answer is `Answer` (two sentences) plus one-line disagreements/assumptions/limitations, and the disagreement kept both views ("Resolved by giving a conditional default rather than claiming spaces guarantee identical display") |
| research | PASS | 63.6 | routed to `research` automatically; complete; all four checked sections present; no URLs in the answer. The answer says plainly that the material "does not establish what the latest developments are" and lists areas to check instead of inventing results, which is the intended behavior without external research, and also shows why such a question needs Milestone 5 to be useful |
| review | PASS | 57.9 | routed to `review` automatically; complete; the three checked sections present |
| debate | PASS | 25.8 | `--mode debate`: the Milestone 2 engine, complete, 3 calls |
| two_rounds | PASS | 87.2 | `--rounds 2` with two agents: complete, 7 calls, `rounds_run` 2 |
| failure_timeout | PASS | 4.8 | `--timeout 4`: real Codex and Google calls cut at 4 s, status `failed`, exit 124, saved, no leftover process |
| host_claude | PASS | 35.8 | MCP `consult` from Claude Code (`claude -p`): complete, Codex coordinating with Google, **Claude excluded**, id relayed back |
| host_codex | PASS | 40.1 | MCP `consult` from Codex: complete, Claude coordinating with Google, **Codex excluded**, id relayed back |

Token counts were reported by every call of every run (for example 120,490 in / 8,759 out for the
two-round run). Wall time for a two-agent, one-round consultation was 26-64 s.

Still not run live: the default three-agent consultation after the process fixes (the earlier
42-run incident did cover it), `--length brief` forced on a question that does not ask for it, a
quota failure, and `consult` from Antigravity as host.

### Second scheduled live run of `consult` (2026-10-11 19:17, one-shot LaunchAgent)

`consult-live.py --extra`, installed by `consult-install-extra.sh` with the same safeguards as the
first run (no `KeepAlive`, guard file, command cap, a watcher that unloads the job if launchd ever
ran it twice). It ran once, removed its own schedule (the plist-deletion order bug found in the
first run was fixed), and left nothing running. About 19 subscription requests. Results:
`live-tests/results/consult-extra-20261011-191700/results.md`.

| Test | Verdict | Seconds | What it shows |
|---|---|---|---|
| three_agents | PASS | 63.9 | the default, everything automatic: Claude + Codex + Google, decision mode, 1 round, 7 calls, none failed, tokens 69,287 in / 9,794 out. Claude, which had been unreachable from the earlier in-session runs, took part normally |
| brief_forced | PASS | 46.6 | `--length brief` on a question that does not ask for a short answer: applied as a *manual* choice, complete, 125 words, none of the long report sections |
| host_agy | PASS | 36.8 | MCP `consult` from Antigravity (`agy -p`, `accept-edits`): complete, Claude coordinating with Codex, **Google excluded**, id relayed back. The server was registered with agy for the test, with temporary allow rules `mcp(ai-orchestra/consult)` and `mcp(ai-orchestra/get_task_status)` in agy's `settings.json` |
| host_agy_cleanup | PASS | 0 | `settings.json` restored byte for byte from its backup, `agy mcp list` empty again, nothing left registered |

**Quota failure: not tested live, on purpose.** A real quota failure cannot be provoked without
using up a subscription. What exists instead: Codex's real quota wording was seen in the first live
debate and is classified correctly (`quota`, exit 4), and the fake-CLI tests cover a quota-failing
participant in `consult` (partial result, exit 5, not retried, not asked again in later rounds).

With this, every path in the milestone has been run against the real CLIs except that one.

### First live consultation test (needs your authorization; run from a plain terminal)

```sh
cd /tmp && B=/path/to/ai-orchestra/target/release/ai-orchestra
CFG=/path/to/ai-orchestra/ai-orchestra.toml

# 0 requests: the routing the engine would use
$B --config $CFG consult "Which electronics microscope should I buy?" --plan

# 1 request (lightweight path, one agent)
$B --config $CFG consult "Explain what a resistor does" --agents google --live

# 5 requests (2 agents: 2 answers, 2 reviews, 1 synthesis)
$B --config $CFG consult "In two sentences: tabs or spaces for a team of five?" \
   --mode decision --agents codex,google --rounds 1 --live --log-file /tmp/consult.jsonl

# afterwards, no requests
$B --config $CFG history
$B --config $CFG show <id> --transcript
```

Run the 5-request test first from a plain terminal; running it inside a Claude Code session simply
leaves Claude out. Only after that, the full three-agent consultation (7 requests):
`$B --config $CFG consult "Which electronics microscope should I buy?" --live`.

## v0.5 research and fact checking (Milestone 5): verified with mocks, a local server and real public pages

Everything that involves an agent is mock-tested; the fetcher was also run against real public
websites (no subscription, no quota). Provider search was run live once, on 2026-10-11 at 21:05 (see
"First live research run" below). The design and the capability
matrix are in [`research.md`](research.md).

### Phase 2: what the installed CLIs expose for web research (looked at here; run live later, see below)

Methods, all local or documentation only, zero requests to any provider:

* `codex --help`, `codex exec --help`, `codex --search exec --help` (exit 0) versus
  `codex exec --search --help` (exit 2), `strings` on the Codex binary (web search modes
  `disabled/cached/indexed/live`, a `web_search` item type in events), and a check that
  `codex debug prompt-input` does not show tools (so the effect of `--search` in `exec` cannot be seen
  offline);
* `claude --help` (the `--tools` / `--allowedTools` options, `--restricted` removing `WebFetch`) and
  `strings` on the Claude Code binary (built-in `WebSearch` and `WebFetch`);
* `agy --help`, `agy changelog` and `strings` on the `agy` binary (`search_web`, `read_url_content`),
  and the official headless page (permission rules `action(target)`; tool steps carry `tool_info`
  with name, parameters and output in `stream-json`; the web tools' rule syntax is not documented).

| | Web search | Source URLs exposed in non-interactive output |
|---|---|---|
| Claude Code | documented (`WebSearch`, via `--allowedTools`) | **unknown** until a run is captured |
| Codex CLI | documented (`codex --search exec`; parser acceptance verified) | **unknown** |
| Antigravity CLI | not enabled (no documented headless allow rule for its web tools) | unknown |

Consequence for the design: no provider's URLs are trusted. The adapters harvest URLs from any
web-tool result without assuming field names, they are recorded as *provider-reported*, and the
program fetches every one itself.

### The fetcher against real websites (2026-10-11, no quota)

`ai-orchestra inspect` against public pages, with the unchanged default policy:

| URL | Result |
|---|---|
| `https://example.com/` | retrieved; title; no dates stated (shown as "not stated"); limitation "very little text" |
| `https://rust-lang.org/` | retrieved through a redirect; 2,780 characters |
| `https://en.wikipedia.org/wiki/Oscilloscope` | retrieved; published `2002-02-24T19:53:32Z`, updated `2026-09-13T12:31:06Z` (two different dates, kept apart); kind: reference work; language lists and navigation removed after a fix |
| `https://arxiv.org/abs/1706.03762` | retrieved; kind: preprint (not peer reviewed); published `2017-06-12` read from `citation_date` (slashed dates are normalised) and all authors read; found by inspecting the real page |
| `https://arxiv.org/pdf/1706.03762` | `unsupported` (`application/pdf`) |
| `https://example.com/definitely-missing-page` | `inaccessible` (HTTP 404) |
| `https://www.reddit.com/r/rust/` | `blocked by the site's robots.txt` |
| `https://localtest.me/` (a public name that resolves to 127.0.0.1) | `blocked`: "localtest.me (127.0.0.1) is refused: a loopback address". This is the DNS check working against a real name, and it first reported the wrong reason (robots) until fixed |
| `https://blog.rust-lang.org/2024/10/17/Rust-1.82.0.html` | retrieved, but it is a JavaScript redirect page: reported as "very little text" with the limitation, not followed |

Speed: four pages in 1.5 s wall, one large page in 1.0 s.

### Mock and local-server verification, mapped to the milestone's test list

| # | Requirement | Where |
|---|---|---|
| 1, 2 | URL validation, public URLs accepted | `net::tests` (scheme, credentials, port, length, disguised IPv4 spellings), `plan::tests`, CLI `research_option_errors_...`, the real pages above |
| 3 | Private / loopback / link-local / metadata refused | `net::tests` (30+ addresses, IPv4-in-IPv6, NAT64, 6to4), `fetch::tests::the_default_policy_refuses_local_servers_before_any_request` (0 hits reach the server), CLI `inspect_refuses_...`, MCP `inspect_sources_...` |
| 4 | Redirect safety | `fetch::tests` (metadata IP, LAN, localhost, `file:`, userinfo, loops, meta-refresh to a private address) |
| 5 | DNS rebinding | `net::tests::rebinding_is_caught...`, `fetch::tests::dns_rebinding_cannot_slip_a_private_address_past_the_check`, `a_name_that_resolves_to_a_private_address...`, robots path reports the address, live `localtest.me` |
| 6, 7, 8 | HTTP errors, timeout, size | `fetch::tests` (404/500/429 not retried, slow server, body cap including a lying `Content-Length`, text cap) |
| 9, 10, 11 | HTML extraction, metadata, missing dates | `html::tests`, `fetch::tests`, `research::...a_claim_backed_by_a_retrieved_source...` (an unstated `updated` stays `null`) |
| 12 | Claim extraction | `claims::tests` (fenced block, tolerant parsing, merging across agents without promoting status) |
| 13, 14, 15 | Supporting, contradicting, insufficient | `tests/research.rs`: `a_claim_backed_by...` (SUPPORTED), `contradictory_sources...` (MIXED_EVIDENCE + contradiction list), `a_page_that_exists_but_does_not_state_the_claim...` (INSUFFICIENT_EVIDENCE) |
| 16 | Pages repeating one origin | `quality::tests`, `claims::tests`, `tests/research.rs::pages_repeating_one_origin...` (two pages state the claim, one independent source) |
| 17, 18, 19 | Citation ids, URLs, fabricated citations | `citations::tests`, `tests/research.rs` (`[9]` removed, real URL rendered as `<url>`, unretrieved sources listed as NOT checked) |
| 20, 21 | Offline research, unavailable search | `tests/research.rs::web_off_...`, `web_on_fails_clearly...`, `plan::tests`, `discovery::tests` (search that exposes nothing / fails / is cancelled) |
| 22 | User-supplied URL research | `tests/research.rs::user_supplied_urls_are_enough...` |
| 23, 24, 25 | Depth strategies, manual depth and web overrides | `plan::tests`, `tests/research.rs::depth_controls...` (quick: 2 agents, no review, no judging; standard: 1 verifier; deep: 2 distinct verifiers and 2 rounds), CLI `plan_shows_the_research_decisions...` |
| 26 | Cancellation | `tests/research.rs::cancelling_during_research...` (recorded `cancelled`, gathered report kept), `tests/research_mcp.rs` (cancel_task) |
| 27 | Partial source failure | `tests/research.rs::a_failed_source_is_reported...`, MCP `a_partial_source_failure...` |
| 28, 29 | MCP research, recursion | `tests/research_mcp.rs` (8 tests: `research`, `verify_claim`, `inspect_sources`, `get_research_report`, validation, tasks, the host never called and never the searcher) |
| 30 | M1-M4 unchanged | the Milestone 1-4 suites pass unchanged (apart from expected tool counts and the server version 0.5.0) |

Regression fixtures for the three situations the milestone singles out (`tests/fixtures/research/`):
**model consensus without evidence** (`model_consensus_without_a_retrieved_source_is_not_verification`:
three agents assert a claim, the source is unreachable, nothing is SUPPORTED, the result is partial),
**source-backed verification** (`a_claim_backed_by_a_retrieved_source...`), and **a source that exists
but does not support the claim** (`a_page_that_exists_but_does_not_state_the_claim...`). A page that tries
to steer the agents (`injection.html`) is checked to stay inside one `<source>` element, to forge no
source, and to trigger no extra request (`text_inside_a_page_is_data...`). The largest possible
prompt with 12 large pages, a 30 KB question, 3 agents and 2 rounds stays below 128 KiB.

### Performance (mock agents, local server)

6 pages fetched, 3 agents, deep depth, 2 rounds, 12 agent calls, claims verified: 145 ms wall for the
whole research layer (fetch, extraction, passage search, prompts, record), engine time 35 ms. Real
provider latency dominates; research adds the planning call, 0 to 2 verification calls and larger
prompts, and nothing is estimated about quota.

### Not verified (needs authorization to spend subscription quota)

* (Done, see above: both run web search non-interactively and expose the URLs.) The live effect of the new source ranking.
* How good the judgements and the claims lists of the real models are with these prompts.
* The research tools from inside real hosts.
* JavaScript-rendered pages, paywalls, PDFs (unsupported by design).

### First live research run (2026-10-11 21:05, authorized)

Run by a one-shot LaunchAgent (`live-tests/research-live.py`), outside any Claude Code session.

| Step | Result |
|---|---|
| raw Claude, web on | exit 0 in 19 s; stream-json has `tool_use` blocks `WebSearch` and `WebFetch` and the result URLs: web search works non-interactively |
| raw Codex, `--search exec` | exit 0 in 13 s; `web_search` items started/completed, with the URLs |
| `claude_search`, `codex_search` (quick) | complete, 4 calls each, 9 sources each reported by the tool (`provider_tool_reported`), 3 of 9 retrieved |
| `user_url` (example.com) | consultation worked; the harness failed reading an omitted empty field (fixed) |
| `verify_true` | SUPPORTED (same harness issue) |
| `verify_false` | INSUFFICIENT_EVIDENCE: the page does not mention oscilloscopes, correct |

Problems found, and what was done:

* **Candidate selection.** Only the first N candidates were fetched. Claude's list contained stale
  Mageia advisories; Codex listed the Rust 1.99.0 post 7th and answered 1.98.0. Fix: fetch all
  candidates (cap 24), rank them (user URLs, relevance, subject site, kind, recency for "latest"
  questions) and keep the best `max_sources`; the rest are `not_selected` and reported. Covered by
  `engine::tests` (ranking, URL dates) and `tests/research.rs` (a rig where the newest page is listed
  last). Re-run live at 22:14 (`claude_search`, `codex_search` only, 8 requests): see below.
* The claim-check answer now names the passages it used, so those sources count as cited.
* Leftover processes after the run were the user's own Codex app-server, not ours.

### Re-run after the ranking fix (2026-10-11 22:14, authorized)

| Step | Result |
|---|---|
| `codex_search` | complete, 4 calls, 9 candidates (all `provider_tool_reported`), 3 kept: Rust 1.97.0, 1.98.0 and **1.99.0**; six others, including 1.94 to 1.96, were `not_selected`. The answer now says the latest stable among the retrieved announcements is **1.99.0** (before: 1.98.0), and that a newer release is not ruled out |
| `claude_search` | complete, 4 calls. Claude's search returned only stale or third-party pages (Mageia advisories 2017-2020, a Linux.org thread, a git tag of 1.85.0, a mirror of the release-process page); none names the current version. Ranking can only choose among the candidates it is given, so it kept the three most relevant of those. The answer says plainly that the evidence does not establish the latest version, that 1.85.0 is very likely out of date and how to check; no invented version |

Takeaways: the ranking fixed the case where the right page was among the candidates but listed late.
It cannot fix a search that never finds the right page; in that case the report says so instead of
guessing, which is the intended behavior. `--url https://blog.rust-lang.org/` (or the official page)
is the manual remedy. Two visible rough edges: the same page can appear twice as candidates
(`.../63426/` and `.../63426/latest`), and the candidate cap (24) ignored 18 reported URLs in the
Claude run (reported as a limitation). No processes were left behind and the job removed itself.

### How to run the first live research test (plain terminal)

Zero quota first:

```sh
B=/path/to/ai-orchestra/target/release/ai-orchestra
CFG=/path/to/ai-orchestra/ai-orchestra.toml
$B --config $CFG capabilities
$B --config $CFG inspect https://en.wikipedia.org/wiki/Oscilloscope
$B --config $CFG consult "What is the latest stable version of Rust?" --web on --depth quick --plan
```

Then, smallest first (the planning call is the one that shows how each CLI exposes its search):

```sh
# 4 requests: Claude searches (WebSearch), Codex answers, Claude synthesizes; no review, no judging
$B --config $CFG consult "What is the latest stable version of Rust?" --web on --depth quick \
   --coordinator claude --agents claude,codex --live --log-file /tmp/research-claude.jsonl
# the same with Codex as the searching agent
$B --config $CFG consult "What is the latest stable version of Rust?" --web on --depth quick \
   --coordinator codex --agents codex,google --live --log-file /tmp/research-codex.jsonl
# no search at all, only your page: 6 requests
$B --config $CFG consult "Does this page say the Model X has 100 MHz?" --url https://example.org/page \
   --agents codex,google --rounds 1 --depth standard --live
```

`show <id> --output json` then has `consultation.research.sources[*].discovery` telling whether the
URLs came from the tool output or only from the model's text.

## v0.6 smart orchestration (Milestone 6): verified with scripted agents, fake CLIs and a local server

**The Milestone 6 test suite and benchmark use no real provider and their numbers are simulated.**
Everything that involves an agent in them is a scripted in-process stand-in, a fake CLI shell script
or a fixture served from a local web server. The only real run is the single live comparison
(2026-10-12, 15 requests) recorded in [`performance-benchmarks.md`](performance-benchmarks.md#first-live-comparison-2026-10-12-0802-authorized-run-once).

The design and every rule are in [`smart-orchestration.md`](smart-orchestration.md); the numbers in
[`performance-benchmarks.md`](performance-benchmarks.md).

### Phase 1: baseline and bottlenecks (measured before any change)

The directory is not a git repository, so there was no git state to preserve; the sources were
archived before the first edit. The baseline was taken by running twelve scripted workloads
through the unchanged Milestone 4/5 engine (the `legacy` column of the benchmark reproduces it, and a
test asserts it still does): calls 1, 1, 7, 10, 1, 8, 10, 10, 8, 8, 12, 6. Findings, measured:

- the call count depended on the number of agents and rounds, not on the question;
- every stage waited for its slowest agent (383 ms against 60 ms with one agent ten times slower);
- the program's own overhead was 0 to 3 ms, 5 to 17 ms with research on local pages: not a bottleneck;
- the only limits were per-call timeouts and a 1800 s total; there was no call cap, no per-consultation
  parallel cap and no record of provider behaviour;
- already good, and kept: independent answers and each review round in parallel, bounded fetches,
  process-group cancellation.

### What was verified, mapped to the milestone's test list

| # | Requirement | Where it is tested |
|---|---|---|
| 1 | Automatic strategy selection | `engine::tests` (`auto_chooses_by_the_question_and_says_why`, `auto_uses_fast_when_only_one_agent...`, `auto_is_deterministic`), `tests/smart.rs::auto_chooses_a_strategy_from_the_question_and_records_why`, benchmark |
| 2, 3, 4 | Fast, balanced, thorough | `tests/smart.rs`: `fast_asks_one_agent_once...`, `balanced_stops_after_one_call...`, `balanced_adds_one_review...`, `thorough_runs_every_agent...`; `engine::tests` for the plans |
| 5 | Manual strategy override | `an_explicit_strategy_beats_the_configured_default`; CLI `without_a_flag_the_default_strategy_is_auto...`; MCP `the_configured_default_strategy_applies...` |
| 6, 7, 8 | Manual coordinator, agents, rounds | `a_manual_coordinator_leads...`, `named_agents_all_take_part_whatever_the_strategy...`, `explicit_rounds_are_run_in_full_even_under_fast`; `engine::tests` |
| 9 | Conflicting configuration rejected before quota | `conflicting_settings_are_rejected_before_any_agent_is_called` (no request reached any mock, nothing saved); CLI `limits_that_cannot_be_met...`; MCP `bad_strategies_and_impossible_limits...` |
| 10 | Simple question, lightweight | `balanced_stops_after_one_call...`; benchmark workloads 1 and 2 (1 call) |
| 11 | Complex-question escalation | `a_material_disagreement_escalates_once_to_an_independent_third_perspective`; `auto` sends the long multi-part decision to `thorough` |
| 12 | Contradiction-triggered review | `contradicting_evidence_triggers_a_review_even_for_a_simple_question`; `adaptive::tests` |
| 13 | Evidence-insufficient outcome | `when_no_source_can_be_retrieved_more_agents_are_not_summoned...` (1 call, `partial`, gate names the missing evidence) |
| 14 | Early stopping | the stop reasons asserted in the balanced tests; `adaptive::tests` |
| 15 | Quality gate | `quality::tests` (9); `the_quality_gate_turns_a_short_changed_review_into_an_honest_partial_result` |
| 16 | Independent review preserved | named agents answer in parallel before any review and see no other answer; the escalation perspective sees neither the lead's answer nor the review (prompt inspected) |
| 17, 18 | Parallel and sequential execution | `independent_first_round_answers_run_in_parallel` (span overlap), `the_evidence_check_and_the_review_run_at_the_same_time...` (a barrier that only opens if both calls are inside the agent together), `sequential_execution_is_used_when_the_first_result_decides_the_next_call` |
| 19, 20 | Provider history updates; missing or damaged history | `provider_latency_is_recorded_without_inventing_anything_and_then_used_to_order_agents`, `a_damaged_history_file_is_ignored...`, `profiles::tests` (8) |
| 21, 22, 23 | Budget, call limit, deadline | `budget::tests` (6), `the_call_limit_is_enforced_while_running...` (exactly the limit, `partial`), `the_global_deadline_stops_new_work_and_cancels_the_call_in_flight`; CLI `the_time_limit_stops_a_slow_agent_process...` (a real child process is terminated) |
| 24 | Cancellation | `cancelling_stops_the_adaptive_flow_and_saves_a_cancelled_record`; the Milestone 1 process-group tests |
| 25 | Provider quota failure | `a_usage_limit_is_never_retried...` (the reviewer asked once, no substitute) |
| 26 | Source deduplication | `the_same_page_given_twice_is_fetched_once...` (one server hit for three spellings); `source::tests` |
| 27 | Citation integrity | `an_invented_citation_is_removed_in_the_adaptive_flow_too...`; `quality::tests` |
| 28 | Freshness preserved | `freshness_limits_survive_every_strategy`, `a_strategy_never_changes_an_explicit_research_depth` |
| 29, 30 | MCP integration and recursion | `tests/smart_mcp.rs` (7): strategies, limits, `dry_run`, rejections, the host never called under any strategy, an explicit host coordinator refused |
| 31 | Dry run | CLI `dry_run_prints_the_smart_plan_and_calls_nothing` (no fake CLI was started, nothing saved), MCP `dry_run_returns_the_plan_and_calls_no_agent` |
| 32, 34 | Trace and persistence | `the_saved_record_carries_the_plan_the_trace_and_the_metrics_and_old_records_still_load` (including a record without the new fields), CLI `a_smart_consultation_saves_its_trace_and_show_prints_it` |
| 33 | Provider failure recovery | `a_failing_participant_makes_a_thorough_result_partial_but_keeps_the_rest`, `an_unavailable_participant_changes_nothing_for_a_strategy_that_never_asks_it` |
| 35 | Milestones 1-5 unchanged | their suites pass unchanged under `[smart] default_strategy = "off"`; `without_a_strategy_the_fixed_workflow_runs_exactly_as_before`; the benchmark's `legacy` column equals the baseline |

### Findings during this work

- **Step timings included the wait for a parallel slot**, which made calls that ran one after another
  look concurrent in the metrics. A call's span now starts when its slot is obtained, measured on one
  clock.
- **A subtle honest-status case:** when research retrieved nothing, the first version of the controller
  said "sufficient: simple and uncontested"; it now says that more deliberation cannot help, because
  no agent can supply evidence that does not exist.
- **A mock artefact, not a product bug:** the single-agent prompt contains the words the scripted agent
  used to recognise a synthesis, so the first benchmark run skipped the evidence check for `fast`;
  the script now recognises that prompt.
- The default strategy is `auto`, which changes what a plain `consult` does; the Milestone 4/5 CLI,
  MCP and library tests that were written for the fixed workflow therefore run with
  `default_strategy = "off"`, and the new behaviour has its own tests. `ConsultOptions` with no
  strategy (library use) is unchanged.

### Not verified

- Anything against a real provider: whether `balanced` gives answers as useful as `thorough` on real
  questions, how often a real reviewer says `AGREE` to an answer that deserved a challenge, real
  latency, real quota use, whether the provider history changes real choices usefully.
- Behaviour with many simultaneous consultations in one MCP server (fairness is by design, not
  measured).
- Reading the real `agy` and Claude Code processes' reaction to a deadline cancellation beyond the
  process-group termination tested with fake CLIs.

### First live comparison (2026-10-12 08:02, authorized)

fast 1 call / 19 s, balanced 4 calls / 84 s (the reviewer disagreed materially, so it escalated once),
thorough 10 calls / 99 s; all `complete`, no failures, quality gate passed, nothing left running.
Details, caveats and the repeat commands: [`performance-benchmarks.md`](performance-benchmarks.md#first-live-comparison-2026-10-12-0802-authorized-run-once).
Still unmeasured: whether the extra calls make the answer more useful, variance across runs,
behaviour with research on, and many simultaneous consultations.

## v0.7 memory, context and token optimization (Milestone 7): verified with scripted agents, fake CLIs and temporary databases

The automated checks below ran with scripted agents. They show that the right facts reach a prompt and the
wrong ones do not; they do not show better answers, lower latency or lower subscription usage. The live
runs of 2026-10-12 (Codex only, one run each) are in the section "Live runs" below.

### Baseline (before the change)

Every agent call carried only the question: a follow-up ("which of the three is the best value?") reached
the agent with no idea what "the three" were, and the only way to give it context was pasting the earlier
answer by hand. There was no store, no retrieval, no context budget and no way to inspect what an agent
was sent. No provider token counts for a recalled block exist, so none are claimed.

### What was verified, mapped to the milestone's test list

| Area | Where | Result |
|---|---|---|
| Schema, migrations (V1, V2, newer-version refusal), transactions, concurrent open/read/write, corruption and quarantine, real deletion (bytes gone from the file), file mode `0600`, expiry | `memory::db` unit tests | pass |
| FTS5 ranking, stemming/accents, metadata filters, entity boost, follow-up continuity, related user statements, linked evidence, modes | `memory::retrieval` unit tests, `tests/memory_eval.rs` | pass |
| Context assembly, budget as a ceiling, shortest-first fitting, user statements never cut, dedupe, conflicts, freshness notices, provider size cap, AI-summary reservation | `memory::context` unit tests, `tests/memory.rs`, `tests/memory_eval.rs` | pass |
| Scope isolation, project isolation, global only on request, session refusal before any agent runs | `tests/memory.rs`, CLI block of `tests/mock_cli.rs` | pass |
| Credentials never stored, personal data needs a flag and never reaches an agent, a question holding a credential is answered but not remembered | `tests/memory.rs`, `tests/mock_cli.rs`, `tests/memory_mcp.rs` | pass |
| Prompt-injection text in memory stays data (cannot close the block, markup neutralised) | `tests/memory.rs`, `tests/memory_mcp.rs` | pass |
| Independent reviewers never receive another agent's answer through memory | `tests/memory.rs`, `tests/memory_eval.rs` (scenario 8) | pass |
| Cancellation mid-run leaves no conclusion; a failed coordinator is not remembered; a corrupt database never stops a consultation | `tests/memory.rs`, `tests/mock_cli.rs` | pass |
| Concurrent consultations on one database | `tests/memory.rs` | pass |
| Memory adds no agent call; the optional summary costs exactly one and is labelled | `tests/memory.rs` | pass |
| CLI: `memory` / `session` commands, consult flags, `--plan --explain-context` calls no agent, usage errors (exit 2) before any agent | `tests/mock_cli.rs` | pass |
| MCP: six tools, bounded results, sensitive records withheld, writes need `allow_mcp_writes` **and** `user_approved`, never global, nested invocations refused | `tests/memory_mcp.rs`, `tests/mcp_e2e.rs` (tool list, stdout purity) | pass |
| Backward compatibility with Milestones 1-6 | the previous suites unchanged (memory pinned off in the CLI/MCP e2e fixtures; the library default has no memory service); records without memory keep their JSON shape | pass |
| Evaluation framework: 15 scenarios x 4 conditions, deterministic checks of essential facts, URLs, caveats, open disagreements and leaks | `tests/memory_eval.rs` (table: `target/tmp/memory-eval.md`) | pass |

### Findings made while building it

* The first context-assembly notice cost ~190 estimated tokens per call; it was shortened (~110).
* A follow-up that does not repeat the user's budget lost it: related user statements about the same subject
  as a selected conclusion are now recalled.
* Structural words of the digest ("question", "answer") matched every record; they are no longer search terms.
* The latest turns of the session were added to every question; a question that strongly matches an earlier
  conclusion now gets none, an uncued unrelated question only the very latest in its shortest form.
* A disagreement was repeated in the conclusion and as its own record; it is now said once.
* An expired record stayed visible until the next daily prune; it is now filtered at query time.
* For a one-turn session the recalled block is **larger** than resending that turn (1,431 vs 560 characters);
  for a 30-turn session it is ~1% (936 vs 93,618). Storage is ~3.8x the raw answer text (352,256 bytes for
  93,618).

### Not verified

* Answer quality with a real model, variance across runs, Claude and Antigravity as the answering agent, and
  whether a recalled block helps or distracts (one Codex run each; see "Live runs").
* Provider-native session continuation (documented by the CLIs; not used, not tried).
* That the sensitivity classifier catches every personal-data shape; encryption at rest does not exist.
* Retrieval beyond shared words (no embeddings by design).

### Live runs (2026-10-12, authorized; Codex only, `--strategy fast`)

Three runs of the same three questions in a throwaway directory with its own memory file, comparing
**A** (`--memory auto`, one session), **C** (`--memory off`) and **B** (`--memory off` with the previous answer
pasted into the question by hand). Token counts are the **input tokens Codex itself reported** (exact);
"estimated" is this program's bytes / 3. One run each; nothing here measures answer quality.

| Run | Setup | What it showed |
|---|---|---|
| 1 | `--web off` (8 calls) | Memory worked mechanically (the second question recalled the first conclusion as a continuation), but the first answer named no models (without web research Codex refused to), so there was nothing concrete to preserve. The comparison says nothing about preservation. Exact input: A2 19,020 / C2 18,151; A3 19,243 / C3 18,148; pasting by hand 18,845 / 19,271. |
| 2 | first question with `--web on` (12 calls) | **Found a bug of ours**: the personal-data check mistook a 10-digit hash inside a source URL for a phone or card number, so the research answer was *not remembered* ("memory: not remembered: ... personal data") and the follow-ups recalled nothing from it. Fixed in `memory::sensitive` (a number counts only when it stands alone or is grouped like a phone or card) with a regression test; both research answers now pass. Also fixed: the report said "-33.7% smaller" when the context was larger. The run's comparison is void. |
| 3 | same as 2, after the fix (11 calls) | The research answer was remembered (3 conclusions and 4 evidence items in the database). Question 2 recalled 5 items (~1,149 estimated tokens, 66% less than the full session history, 0 extra agent calls); question 3 recalled 6 (~1,757, 63% less). The answers cite the recalled material ("according to earlier research...", "the OWON HDS200, HDS-N and HDS120 mentioned in the earlier context"); without memory they said they did not know which three. |

Exact input tokens in run 3:

| | with memory | memory off | previous answer pasted by hand |
|---|---:|---:|---:|
| Question 2 | 18,991 | 18,153 | 21,067 |
| Question 3 | 19,374 | 18,150 | 21,749 |

* The recalled block added about 838 and 1,224 exact input tokens over the no-memory baseline; pasting the
  previous answer added 2,914 and 3,599 (memory cost about 28-34% of the extra). Every call carries about
  18,000 tokens of Codex's own base before any context.
* The estimate was 1.4-1.5 times the exact figure (1,149 vs 838; 1,757 vs 1,224): conservative, as documented.
* **Not shown**: better answers. In run 3 the agent still said it could not decide which of three was best
  value, because the research answer (question 1) had identified OWON *families* and a couple of other models,
  not three priced models; memory carried what existed. The paste-by-hand arm also failed (it found only two
  models). The research phase itself cost 97,984 input tokens with memory and 161,375 without, for the same
  question: that is run-to-run variation (2 vs 3 calls), not an effect of memory.
* A single run per arm: no variance, no quality judgement, only Codex. Quota used: 8 + 12 + 11 = 31 calls.

### How the live test was run (reproduce from a plain terminal; spends about 11-12 calls)

Nine single-agent consultations (`--strategy fast`, one call each), in a throwaway project directory, with a
private memory file so nothing touches your real memory:

```sh
mkdir -p ~/memtrial && cd ~/memtrial
export AI_ORCHESTRA_CONFIG=~/memtrial/ai-orchestra.toml
printf '[memory]\npath = "%s/memory.db"\n[debate]\nhistory_dir = "%s/debates"\n' ~/memtrial ~/memtrial > $AI_ORCHESTRA_CONFIG
Q1="Investiga tres osciloscopios portátiles para aprender electrónica y compáralos."
Q2="De los tres anteriores, ¿cuál tiene mejor relación calidad-precio?"
Q3="Ahora aumenta el presupuesto a \$8,000 MXN. ¿Cambiaría tu recomendación?"
A="--agents codex --coordinator codex --strategy fast --web off"
# Arm A: memory on, one session (3 calls)
ai-orchestra consult "$Q1" $A --memory auto
ai-orchestra consult "$Q2" $A --memory auto --explain-context
ai-orchestra consult "$Q3" $A --memory auto --explain-context
# Arm C: no memory at all (3 calls): shows the cost of no context
ai-orchestra consult "$Q1" $A --memory off
ai-orchestra consult "$Q2" $A --memory off
ai-orchestra consult "$Q3" $A --memory off
# Arm B (3 calls): memory off, the previous answer pasted into the question by hand, e.g.
#   ai-orchestra consult "$(ai-orchestra show <id-of-Q1> | sed -n '/--- Answer ---/,$p') $Q2" $A --memory off
ai-orchestra memory stats
```

Compare prompt size (the `--explain-context` report: characters and *estimated* tokens), wall time per
consultation, number of calls (`calls` in `ai-orchestra show <id> --output json`), provider-reported token
usage when present (exact, kept apart from estimates), and, by reading the answers, whether the budget, the
three products and the open disagreement survived. One run proves nothing about quality.

## v0.8 chat, presentation and quality evaluation (Milestone 8): verified with scripted agents, fake CLIs and temporary stores

The automated checks below ran with scripted agents and show the machinery behaves as specified; they say nothing
about whether several agents answer better than one. One authorized live run (six cases, three arms, Codex only, 55
calls) is reported in [`evaluation-benchmarks.md`](evaluation-benchmarks.md#3a-the-first-live-run-2026-10-12-authorized-six-cases-three-arms-codex-only): it exposed three defective checks, which were corrected and the stored answers re-scored; after that all three arms passed 6/6 objective checks, at 1x, 1.8x and 6.3x the agent calls.

### Baseline (inspected before changing anything)

* The repository is not a git repository (`git status` fails), so there was no working-tree state to preserve
  beyond the files themselves; nothing existing was removed or renamed.
* Usability: every use needed a subcommand and long flags (`consult "..." --strategy balanced --web off ...`);
  there was no way to continue a conversation except by passing `--session`; `history` could only list; the
  answer was printed as the coordinator wrote it, with the orchestration log on stderr; nothing was
  interactive, and a Ctrl-C ended the program.
* Evaluation: the data that exists is the Milestone 6 decision traces and metrics (calls, time, stages, the
  tokens CLIs reported), the Milestone 5 research records (sources, claims, citations) and the Milestone 7
  context reports. No quality measurement existed: the Milestone 6 benchmark is simulated and compares cost,
  not answers, and the one live comparison recorded calls and time only.

### What was verified, mapped to the milestone's test list

| # | Area | Where | Result |
|---|---|---|---|
| 1 | Chat startup, banner, help | `tests/ux_chat.rs`, `tests/mock_cli.rs` (a conversation from a pipe; plain `ai-orchestra` without a terminal keeps its usage error, exit 2) | pass |
| 2 | Follow-up questions in the same session | `tests/ux_chat.rs` (the second prompt carries the earlier result) | pass |
| 3-5 | Settings changes, manual overrides never changed, AUTO, precedence request > session > preference > AUTO, validation (a refused value changes nothing), unknown agents refused | `ux::settings` unit tests, `tests/ux_chat.rs` | pass |
| 6 | Session restoration (`--session`, `/resume`, `/new`); sessions of another project refused | `tests/ux_chat.rs` | pass |
| 7 | Consultation history: listing, search, reopening with a "not re-checked" note | `tests/ux_chat.rs`, `tests/mock_cli.rs`, `history::search` | pass |
| 8 | Cancellation (Ctrl-C mid-consultation terminates it, says CANCELLED, remembers no conclusion, the chat stays usable) | `tests/ux_chat.rs` (gated mock agents) | pass |
| 9 | Partial-result presentation (PARTIAL RESULT, never "complete") | `ux::present` unit tests, `tests/ux_chat.rs` | pass |
| 10-11 | Citation and source display; individual agent answers, disagreements, plan, statistics | `tests/ux_chat.rs` (a researched answer with a local server) | pass |
| 12-14 | Dataset parsing and validation (every problem listed; invented ground truth refused), deterministic offline evaluation (no provider called, same result twice) | `eval::dataset` tests, `tests/eval.rs`, `tests/mock_cli.rs` | pass |
| 15-16 | Blind ordering reproducible by seed, packet hides arms and provenance, preference recording and mapping back to the right arm whatever the display order, supersession, no self-judging, AI judgments kept apart | `eval::blind` tests, `tests/mock_cli.rs` | pass |
| 17 | Feedback persistence, listing, deletion, secret redaction, owner-only file, changes nothing | `eval::feedback` tests, `tests/ux_chat.rs`, `tests/mock_cli.rs` | pass |
| 18 | Rubric evaluation (deterministic criteria decided by checks, judgement criteria `NotAssessed`, `NotApplicable` rather than invented scores) | `eval::rubric` tests, `tests/eval.rs` | pass |
| 19-21 | Citation validity, unsupported (adversarial) citations, evidence limitations: unknown ids, unretrieved sources, links outside the source list, unbacked verification wording and its honest negations, required claims uncited | `eval::citations` tests, `tests/eval.rs` | pass |
| 22-24 | Statistics: medians, Wilson and seeded bootstrap intervals (same seed, same interval), pairs with ties and "insufficient", withheld below 10 observations | `eval::stats` tests, `tests/eval.rs` | pass |
| 25-26 | Latency and invocation comparison, and the rule that a saving is stated only with recorded calls, at least 10 shared cases and no loss of checks | `eval::report` tests, `tests/eval.rs` | pass |
| 27 | Fixture / mock / live separation: separate sections, a warning when comparing runs, no cross-class blind pairing | `tests/eval.rs`, `eval::report` tests | pass |
| 28 | Privacy and scope isolation: preferences, feedback and run files owner-only; project isolation of sessions; live evaluation runs isolated from memory and from the saved history | `tests/ux_chat.rs`, `tests/eval.rs`, `tests/mock_cli.rs` | pass |
| 29 | MCP: `get_eval_report` read-only and bounded, no tool starts an evaluation, `submit_feedback` under `allow_mcp_writes` + `user_approved`, nested invocations refused, tool list and schemas | `tests/memory_mcp.rs`, `tests/mcp_e2e.rs`, `tests/mcp_inprocess.rs` | pass |
| 30 | Backward compatibility with Milestones 1-7 | the previous suites unchanged (they still pass: 789 tests in all) | pass |
| - | The live runner: free plan, call ceiling refused before any call, cancellation keeps partial results, failures recorded and never retried, supplied sources in the prompt, history carried for follow-ups | `tests/eval.rs`, `tests/mock_cli.rs` (the plan without `--yes-run-live` calls nothing; with it exactly the planned calls) | pass |

Totals on 2026-10-12: library 486, `eval` 19, `ux_chat` 14, `memory_mcp` 15, `mock_cli` 103, `mcp_inprocess` 31,
`mcp_e2e` 8, `memory` 35, `memory_eval` 3, `research` 21, `research_mcp` 8, `smart` 37, `smart_mcp` 8, `benchmark` 1 -
789 tests, 0 failed; `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo build --release`
clean.

### Findings made while building it

* A negation window of 14 characters flagged "nothing here has been verified against ..." as unbacked
  verification wording; negations are now looked for earlier in the same sentence.
* Agent names in settings were checked against a fixed list, which also refused mock agents; the settings now
  validate only the shape of a name and the chat checks it against the registry of the running installation.
* A dataset category had a different serialised name and label; every built-in dataset is now parsed by a test.
* The live runner first did not put a case's supplied sources in the prompt, so an agent asked about "the
  supplied study" could not see it; they are now in the prompt and count as supplied evidence in the citation
  checks.
* The default call ceiling (150) is below the worst case of the 18-case comparison with three arms, so a full
  live run needs an explicit `--max-total-calls`; this was deliberate and shows in the plan.

### Second live run, harder cases (2026-10-12, authorized)

Six `hard` cases x 3 arms, 59 calls: every arm passed every check (6/6), so the cases did not discriminate; an AI
judge (labelled, not ground truth) split 1-2-3 (single, `balanced`, tie) and 2-2-2 (single, `thorough`, tie). Details
and caveats in [`evaluation-benchmarks.md`](evaluation-benchmarks.md#3c-the-harder-cases-half-pass-2026-10-12-authorized-six-cases-three-arms-codex-only).

### Not verified

* Quality differences between arms on a dataset where they fail checks (the six live cases were passed by every arm), and everything else about real models: whether `balanced` or `thorough` answer better than one agent, whether the
  keyword checks agree with a careful human reader, how consistent two judges are, how much a run varies from
  one repetition to the next, and what any of it costs in tokens or quota.
* The chat in a real terminal under real providers (it was driven with a scripted terminal and with a pipe).
* The optional browser UI: not built (assessed and deferred, see `quality-evaluation.md`).

### First live quality comparison (needs your authorization; run from a plain terminal)

The plan and the exact commands, including a six-case first run of about 60 calls and the blind comparison
steps, are in [`evaluation-benchmarks.md`](evaluation-benchmarks.md#3-the-first-live-comparison-not-run).
`ai-orchestra eval run comparison --live --arms single:codex,balanced,thorough` prints the plan for your
machine and runs nothing until `--yes-run-live` is added.

## v0.9 local web interface (Milestone 9): verified with mock agents, fake CLIs and a real browser

**No real model was called**, and no real provider CLI was started: every consultation in these tests runs
against in-process mock agents or against shell-script fake CLIs (`web/e2e/fakes/`) in a private `HOME`. The
first live consultation through the browser is left for you to authorize (see the end of
[web-ui-user-guide.md](web-ui-user-guide.md)).

What was run, and what each layer proves:

| Layer | Where | What it covers |
|---|---|---|
| Rust HTTP suite | `tests/web_api.rs` (24 tests + 1 ignored fixture generator) | the real server on an ephemeral port over mock agents and temporary stores: schema from the backend; Host rejection (DNS rebinding); token, JSON-only, Origin and no-CORS; security headers; path traversal; validation (10 invalid requests refused with the field named and **zero agent calls**); plan preview runs nothing and reports each setting's origin (request > session > saved default > auto); a full consultation's contiguous ordered events, saved record, theater = recorded steps, history search/filter, analytics; reconnect with `Last-Event-ID`/`?after=` and two clients, with no repeated call; events arrive **while** the run is in progress; a failing agent gives `partial` (never complete) with the failure kept as a failure; cancel (fast, `cancelled`, 409 on repeat); the concurrency limit (429); stopping the server mid-run leaves no running job and no half-written file; feedback and delete need confirmation (and deleting also drops the in-memory job); an old plain debate record opens and replays; saved defaults and interface preferences validate and persist; permanent memory needs `confirm` and corrections/deletes do too; sessions continue through memory and a context preview calls no agent; the provider page calls nothing and reports authentication as `unknown`; research endpoint on a record built from the real research types (retrieved vs candidate, claim status from evidence links, model-dependence note) |
| Component tests | `web/tests/` (Vitest + Testing Library, 73 tests) | Markdown safety (raw HTML shown as text, `javascript:` links inert, citations clickable only for retrieved sources); es/en dictionaries have identical keys and placeholders; the job reducer (progress only from recorded events, idempotent per `seq`, one message per recorded message, failures kept, partial/cancelled never complete); the API client (token only on state-changing requests, structured errors, unreachable server); `EventSource` de-duplication and resume; all 3 robots in all 7 states render as SVG rects with text alternatives; the settings form from the schema (every field, manual vs Auto, reset, bounds, disabled with reason); tabs/dialog keyboard and focus trap; chat (a page load starts nothing, send payload, per-question vs session vs Auto, validation errors, plan preview, cancel, session continuation, partial display, verified citation); theater (exactly the recorded messages, failures without text, replay reveals nothing before its recorded time, live message-by-message, replayed event adds nothing); history, memory (no remembering without the confirmation), reports (measured vs estimate, no invented numbers), settings, theme, language, reduced motion, navigation and shortcuts |
| Browser tests | `web/e2e/` (Playwright with the installed Chrome, 28 runs) | the **real binary** with fake CLIs: opening/reloading/browsing every page calls no agent (call log of the fakes); a three-agent consultation (live progress, answer table, one message per call and calls reported = calls received); refresh mid-run reattaches without repeating calls; follow-up in the same session; one agent; a recorded disagreement flagged; a failing agent -> partial; a timeout; cancel (no fake process left alive); validation error; plan preview; history search/replay/export/delete; session continuation; a seeded research record (retrieved vs candidate, claim evidence, citation -> source); settings persistence; theme/language persistence; provider versions with authentication unknown; memory add/correct/delete with confirmation |
| Visual and accessibility | `web/e2e/visual.spec.ts` | every screen in dark and light, desktop (1360x860) and phone (390x844): no horizontal scroll and **no serious or critical axe violations (WCAG 2 A/AA, colour contrast included)**; the images are in `docs/screenshots/` |

Findings fixed while building it (each now covered by a test): reconnecting the event stream when the
consultation id first appeared (the effect depended on it: fixed to follow the job only); deleting a saved
consultation left its finished job in memory (it kept serving the record); low-contrast secondary text, hints
inside coloured notices, scrollable tables not reachable by keyboard on a phone, form controls nested in their
labels (the accessible name included every option), server notes shown in English in the Spanish interface.

### v0.9.1 follow-up (animated stage, conversation, summary, characters, usage)

Same method as v0.9: mock agents, fake CLIs (they now also log the prompts they receive, so the tests can check
what an agent was actually told) and real Chrome. **No real model was called.** Added and run on 2026-10-12:

* Rust: 7 unit tests of `src/style.rs` (machine calls get nothing, independent answers never name the others,
  user text cannot close its block, cleaning and bounds) and 7 HTTP tests in `tests/web_api.rs` (31 in total):
  characters validate and persist; **every** call that writes prose carries the language rule; a character
  reaches only its own agent's prompts while reviewers learn the names and first answers do not; the record says
  who had a character but not the profile text; a summary is exactly one call to the coordinator, is stored with
  its author and reappears; a double click makes one call and a running consultation is refused; a client that
  goes away releases the request; usage per agent comes only from CLI-reported tokens (windows, per day, quota
  errors) and never claims a remaining quota.
* Vitest: 103 tests (30 new): where the characters stand at each stage, deterministic bounded wandering, bubbles
  carry only the real text of the agent that wrote it and a failure shows a fixed status, names and looks from
  the characters, walking only when motion is allowed, what the stage reads from live events, accessories and
  walk cycle as pixels, conversation grouping and tokens, heading translation, the summary button (nothing until
  pressed, one request, cancel, errors, participants), the characters form, the usage report.
* Playwright: 35 runs (7 new): the stage opens with the consultation, its characters walk and a bubble holds
  an agent's own words; reduced motion means nobody walks; the stage can be switched off; the conversation reads
  in groups; summary end to end (one call to the coordinator, kept in the history); characters end to end (saved,
  drawn on the stage and in bubbles with the real CLI beside the name, and written into the prompts, with first
  answers free of the other names); usage with a dash for a CLI that reports no tokens. axe stays clean
  (contrast included) on every screen at desktop and phone width with the stage present.

Findings fixed on the way: the stage list and the new stage shared a CSS class (the progress steps became 250 px
tall); a form field's accessible name included its options (fixed in v0.9); bubbles were clipped at the top of the
stage (taller stage, shorter bubble text).

Not verified: whether real agents given a personality or the conversational instruction actually write better or
more naturally, and whether the language rule removes mixed-language answers from real CLIs (it is an instruction
in the prompt; checking it needs a live run you authorise); no remaining quota can be shown because no CLI
exposes it.

### Measured cost of the interface (release build, this machine, fake CLIs; one run, not a benchmark)

| Quantity | Value |
|---|---|
| binary size (release, frontend embedded) | 26.1 MB |
| frontend transferred on first load | 358 KB uncompressed (chat bundle 340 KB / 106 KB gzip; the other pages 2-14 KB each, loaded on demand) |
| first page: DOMContentLoaded / load | 94 ms / 95 ms |
| JS heap after load | 3.6 MB |
| server idle: resident memory / CPU over 10 s | about 5 MB / 0.0 % |
| server event to browser, 35 live events | mean 1 ms, max 3 ms |
| result visible after the server finished a consultation | 147 ms (including re-reading the record) |

These are the costs of the interface and the server, **separate from provider latency**, which is whatever the
real CLIs take (seconds to minutes, see the earlier sections). The fake CLIs sleep 0.5 s per call, so the
consultation durations in the tests are not provider numbers either.

### Not verified

* No real provider was run through the browser; neither CLI authentication nor real quota failures were
  exercised.
* Browsers other than Chrome were not run; no screen reader was used (the checks are automated: axe,
  roles and names in the tests, keyboard paths).
* A phone was emulated by viewport, not run on a device.
* The event kinds `source_discovered` and `claim_verified` of the specification are not emitted (the research
  engine has no such events); research details appear when a consultation completes.

## Error handling and limits: what was checked, and how

| Behavior | Verified how | Result |
|---|---|---|
| Recursion guard | real binary, `ask claude` inside Claude Code | refused before spawning, exit 1, clear message |
| Missing executable | real binary, config pointing at `/nonexistent/agy` | `executable ... was not found`, exit 1 |
| Unknown provider | real binary, `ask gemini` | exit 2, lists known providers |
| Invalid value | real binary, `--timeout 0` | exit 2 |
| `doctor` | real binary, no prompts sent | exit 0 in 0.78 s; all three installed; Claude logged in (claude.ai plan), Codex logged in (ChatGPT) |
| Timeout, cancellation, process-group kill, output caps, bounded concurrency | automated tests (`src/process.rs`, `tests/mock_cli.rs`) using local shell scripts | pass |
| Auth failure, quota, malformed JSON, non-zero exit classification | automated tests with fake CLIs | pass |
| Prompt kept off argv, API-key variables stripped, read-only flags | automated tests with fake CLIs | pass |

**Not verified live** (each would cost an extra subscription request or needs a failing
account, so it was not done):

- (Done 2026-10-11, see the scheduled run above: a real agy timeout, one write attempt per CLI, and
  real Claude and Codex auth-failure wording.)
- That each CLI *enforces* its read-only mode (Claude `--tools Read,Grep,Glob`,
  `codex --sandbox read-only`, `agy --mode plan`). The flags are passed and unit-tested; the
  CLIs' enforcement itself was not probed.

## Automated checks

Current state: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` (832 tests in 15 suites, none contacting a provider; the Milestone 9 additions are the 31 HTTP tests of `tests/web_api.rs` and unit tests of the job log, the theater, the security helpers and the prompt style), `cargo build --release`, and for the frontend
`npm run typecheck`, `npm run lint`, `npm test` (103), `npm run build` and `npx playwright test` (35) all pass.

Note on scope: the `ask` results above predate Milestone 2. Debate and the model/effort settings
are recorded in their own section.

## Reproduce

Run from a **plain terminal** (not inside a Claude Code session), from the project root.

```sh
cargo build --release
B=./target/release/ai-orchestra

$B doctor

# Claude
/usr/bin/time -p $B ask claude "Reply with exactly: ORCHESTRA_CLAUDE_OK" --json

# Codex
/usr/bin/time -p $B ask codex "Reply with exactly: ORCHESTRA_CODEX_OK" --json

# Google
/usr/bin/time -p $B ask google "Reply with exactly: ORCHESTRA_GOOGLE_OK" --json
```

Expected (and observed for all three): exit 0, `"text": "ORCHESTRA_CLAUDE_OK"` / `"ORCHESTRA_GOOGLE_OK"`, empty stderr
(apart from `/usr/bin/time`'s own lines), `"truncated": false`.

Failure behavior to expect: exit 3 (not signed in; run the vendor CLI's own login), 4 (usage
limit reached), 124 (timeout), 1 (anything else, with the CLI's message).

Non-quota error-handling checks:

```sh
printf '[providers.google]\nexecutable = "/nonexistent/agy"\n' > /tmp/missing.toml
$B --config /tmp/missing.toml ask google hi     # exit 1, "was not found"
$B ask gemini hi                                # exit 2, unknown provider
$B ask google hi --timeout 0                    # exit 2
```
