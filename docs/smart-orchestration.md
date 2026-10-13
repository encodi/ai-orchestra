# Smart orchestration (Milestone 6)

AI Orchestra decides **how much AI work a question needs**: which agents to ask, in what order,
whether to review, when to escalate and when to stop. The goal is a well-grounded answer with fewer
calls, lower latency and less use of your subscription quotas, **without** giving up the evidence
checks or any choice you made yourself.

> **Core principle.** Use the minimum amount of AI computation necessary to produce a sufficiently
> reliable answer, without sacrificing important evidence verification or user-requested depth.
> More agents are not automatically better. More rounds are not automatically better. A faster
> answer is not automatically more accurate.

Everything on this page is deterministic and explainable. **No model is asked how to orchestrate**
(routing, strategy choice, escalation and stopping are rules over structured facts), and nothing is
learned or predicted: no bandits, no reinforcement learning, no numeric "expected quality".

- [The four strategies](#the-four-strategies)
- [How `auto` chooses](#how-auto-chooses)
- [Your settings always win](#your-settings-always-win)
- [Adaptive deliberation](#adaptive-deliberation)
- [Quality safeguards](#quality-safeguards)
- [Resource budgets](#resource-budgets)
- [Provider performance profiles](#provider-performance-profiles)
- [Concurrency](#concurrency)
- [Work that is not done twice](#work-that-is-not-done-twice)
- [Research and strategies](#research-and-strategies)
- [Using it: CLI, MCP, plans and traces](#using-it-cli-mcp-plans-and-traces)
- [The same question in fast, balanced and thorough](#the-same-question-in-fast-balanced-and-thorough)
- [Why fewer agents may be chosen](#why-fewer-agents-may-be-chosen)
- [Configuration](#configuration)
- [Limits of this milestone, and known failure modes](#limits-of-this-milestone-and-known-failure-modes)

## The four strategies

| Strategy | Agents | What happens | Calls (3 usable agents, no research) |
|---|---|---|---|
| `fast` | one (the coordinator) | One agent writes the answer. No review. Essential evidence checks are kept: with research, the claims are still checked against the retrieved pages. | 1 |
| `balanced` | starts with one, up to three | The lead answers. An **independent review** is added only when the question or the evidence calls for it. A material disagreement escalates **once** to a third independent perspective. | 1 to 4 |
| `thorough` | all usable agents | Independent perspectives in parallel, evidence verification, cross-review in **two** rounds, one synthesis. Bounded: still limited by the budgets below. | 10 |
| `auto` (default) | decided per question | Picks `fast`, `balanced` or `thorough` from the question, the agents, the budgets and your settings. The choice and its reason are recorded. | depends |

A strategy sets **defaults** for whatever you did not set. It never overrides a setting you made.

The strategies run on one of four *flows*:

| Flow | Used by | Shape |
|---|---|---|
| `single` | `fast`, or any plan that ends up with one agent | one call writes the answer; with research it is followed by the evidence check |
| `adaptive` | `balanced` | lead answer → (evidence check ∥ review when the review is certain) → controller decides → at most one escalation → synthesis if needed |
| `panel` | `thorough`, or whenever you named `--agents` or `--rounds` | every participant answers independently and in parallel, then the review rounds, then the synthesis (the Milestone 4 workflow) |
| `debate` | `--mode debate` | the Milestone 2 debate engine, unchanged, now under the same budgets and trace |

## How `auto` chooses

Rules are evaluated in this order; the first that applies wins, and its reason is stored with the
consultation (`routing.smart.strategy.reason`).

1. **Only one agent can take part** (others unavailable, or the host agent excluded) → `fast`.
2. **Debate mode** → `thorough` (deliberation is the point).
3. **A call limit of 3 or less**, or **a time limit under 120 s** → `fast` (no room for a review).
4. **A claim check** (`--verify`, the MCP `verify_claim`) → `balanced`.
5. **You named `--agents` or `--rounds`** → `thorough` for three or more agents or two rounds,
   otherwise `balanced`. Your choice defines the panel; the strategy only fills in the rest.
6. **Deep research** (the wording asks to verify or contrast claims) → `thorough`.
7. **A short, uncontested question** in `learn` or `research` mode, with no high-stakes wording →
   `fast`. If it needs outside facts the evidence check still runs.
8. **A long or multi-part question** (45 words or more, three or more question marks, or over
   600 bytes) → `thorough`.
9. Otherwise → `balanced`.

The features it looks at are **wording features** of the question (length, contested or high-stakes
cues, how many parts) plus the mode the Milestone 4 router chose. They say how big the question
looks, not whether anything is true.

## Your settings always win

Precedence, highest first:

1. Safety and execution restrictions: the agents stay read-only, the host agent of an MCP server
   is never called, the delegation-depth limit, no nested orchestration.
2. Your explicit settings: `--mode`, `--coordinator`, `--agents`, `--rounds`, `--depth`,
   `--max-sources`, `--web`, `--url`, `--length`.
3. Your explicit limits: `--max-agent-calls`, `--max-duration`, `--max-parallel`.
4. The strategy's defaults.
5. The provider history, which only **orders** agents and never excludes one.

What that means in practice:

- **Named agents are never silently dropped.** With `--agents claude,codex,google` all three take
  part under every strategy, they answer independently and in parallel first, and the plan is the
  full panel. (`--strategy fast --agents a,b,c` means "fast defaults for everything else": the
  rounds default to 0, so three independent answers and a synthesis.)
- **A named coordinator leads** and writes the synthesis; it is never replaced, even when its
  history is poor.
- **Named rounds are run in full.** `--rounds 2` brings in at least a second agent (a review needs
  one), runs two complete rounds, and the quality gate downgrades the result to `partial` if the
  rounds could not happen.
- **Explicit depth, sources and web settings are kept**; a strategy only chooses a depth when you
  did not (`fast` and `balanced` upgrade `quick`, which finds passages but never judges them, to
  `standard`; `thorough` uses `deep`).
- **The host agent of an MCP server is excluded** from every strategy. Naming it as the
  coordinator fails with `host_agent_blocked`. Naming it in `agents` is reported under `skipped`
  with the reason and the other agents proceed: this is the Milestone 3/4 contract and it is kept;
  nothing is dropped silently.

**Conflicts are rejected before any quota is spent.** The plan is computed first, and these fail with
exit code 2 (or `invalid_request` over MCP) without calling an agent:

- limits that your explicit settings cannot fit (`--agents a,b,c --rounds 2 --max-agent-calls 5`
  needs 10 calls; the message gives the arithmetic and names the settings that force it);
- a duration above the configured ceiling (`debate.total_timeout_secs`), zero or negative limits,
  an unknown strategy;
- `--web on` with a strategy that left no agent able to search and no `--url`, and a debate
  that cannot fit its minimum size.

When the limits are tight but only **automatic** settings are in the way, they are lowered one at a
time, in this order, and every step is written to the plan's explanation: review rounds, extra
agents, the evidence check (only when research was not explicitly required), research itself (same
condition). A debate keeps its two agents and one round.

## Adaptive deliberation

`balanced` does not commit to an expensive fixed workflow. After each stage a **controller**
decides the next step from structured facts only (it is a pure function; it is unit-tested with
fixed inputs):

| Signal | Source |
|---|---|
| mode, contested / complex / high-stakes question | the question's wording and the Milestone 4 mode |
| retrieved sources, claim statuses `SUPPORTED`, `CONTRADICTED`, `MIXED_EVIDENCE`, `INSUFFICIENT_EVIDENCE`, `NOT_CHECKED`, verification failed | the Milestone 5 evidence records (Rust, not a model) |
| the reviewer's verdict `AGREE`, `MINOR_ISSUES`, `MATERIAL_DISAGREEMENT` | one agent's categorical judgement of one answer; **only used to decide whether to look closer** |
| calls and time left, agents not yet asked, escalations used | the budget manager |

Stages for `balanced`:

1. **The lead answers** (with the evidence if there is research; the claims are checked).
2. **Review or finish?** A review is added when the question is a decision or a review, is
   contested, long or high-stakes, **or** the evidence contradicts a claim, leaves a claim that
   needs outside evidence unsupported, or the evidence check failed. Otherwise it finishes.
3. **One other agent reviews** the lead's answer and ends with a `VERDICT:` line.
4. **After the review:** `AGREE` with no contradicting evidence finishes **without a synthesis**
   (the answer carries a generated "Independent review" section quoting the reviewer). `MINOR_ISSUES`
   or no verdict → the coordinator integrates it in a synthesis. `MATERIAL_DISAGREEMENT` →
   escalate **once**.
5. **Escalation (at most one):** a third agent gives an **independent** perspective (it sees
   neither the lead's answer nor the review), and its claims are checked against the same passages;
   claims it shares with the lead are not judged again. With no third agent, the lead reconsiders
   after reading the review. Then the synthesis.
6. **Stop.** Always bounded: one review, one escalation, one synthesis.

The reasons for every decision, and the reason for stopping, are in the decision trace.

**What it deliberately does not do**

- It does not treat a model's self-reported confidence as a probability, and does not estimate one.
- It does not treat agreement between models as correctness. A result is "sufficient" because the
  retrieved evidence supports what the answer relied on, or because the question is simple and
  nothing in the results calls for a second look: not because two agents agree.
- It does not count keywords to judge answers. Keyword-like features are used once, on the
  *question*, to decide how big the question looks; the escalation itself reads evidence records and
  categorical verdicts.
- It does not force more rounds when the evidence is missing. If research was part of the task and
  **no source could be retrieved**, more agents cannot supply evidence, so the controller stops and
  says so (`more deliberation cannot help: ...`); the result is `partial` and the quality gate
  names the missing evidence.
- It does not retry. A reviewer that hit a usage limit is not replaced by another agent; the result
  is `partial` with the lead's answer and a warning.

### Early stopping, in full

Deliberation ends, with the reason recorded, when:

- the question is simple and uncontested and nothing in the result calls for a second look;
- every claim the answer relied on is `SUPPORTED` by retrieved sources and the question is not contested;
- the independent review found no material issue;
- no source could be retrieved (evidence cannot improve);
- no other agent is left, or the call/time budget cannot cover the next stage;
- the single allowed escalation has been used.

## Quality safeguards

Performance is never the only objective. The **quality gate** runs on every consultation made with
a strategy and checks structural properties of the result and of its evidence records:

| Check | Fails when | Blocking |
|---|---|---|
| `required_evidence_present` | research was required (`--web on`, `--url`, a claim check) and no source was retrieved | yes |
| `citation_references_valid` | the agents' text cites a number that is not a retrieved source | yes |
| `claims_checked` | verification was expected, claims were listed, and none was checked | yes |
| `contradictions_visible` | a `CONTRADICTED` or `MIXED_EVIDENCE` claim is missing from the answer | yes |
| `requested_review_satisfied` | you asked for `--rounds N` and fewer than N ran | yes |
| `requested_agents_consulted` | an agent you named was neither called nor reported as unavailable | yes |
| `limitations_disclosed` | calls failed or a limit stopped the work, but the result is not marked partial with warnings | yes |
| `completeness_classified` | the status is `complete` although a call failed, a limit stopped the work or planned rounds did not run | yes |
| `independent_review` | a single agent wrote the answer and nobody reviewed it | no (advisory) |

A failed blocking check turns `complete` into `partial`, adds a warning and a short "Quality notes"
section at the end of the answer. A result that fails nothing is **not certified correct**: the
gate verifies that required pieces are present and honestly labelled, not that the answer is true.
Models can share a blind spot; a passed check means a required piece exists, nothing more.

Other safeguards that hold in every strategy: a factual claim is never "verified" because several
models agree; required source checks are never skipped silently (a fast or balanced run upgrades
`quick` depth to `standard`; dropping the check to fit a call limit happens only when you did not
require research, and is written in the plan and in the answer); explicit review requirements are
never removed; a partial failure is never reported as consensus; important contradictions stay
visible in the answer.

## Resource budgets

One **budget manager** is shared by every agent call of a consultation (the Milestone 2 debate
engine included). Limits, all optional:

| Limit | Flag / MCP parameter / config | Notes |
|---|---|---|
| total wall-clock time | `--max-duration SECS` / `max_duration_seconds` / `smart.max_duration_secs` | never above `debate.total_timeout_secs`, the single absolute ceiling |
| agent calls | `--max-agent-calls N` / `max_agent_calls` / `smart.max_agent_calls` | planning, verification, reviews and synthesis all count |
| concurrent calls | `--max-parallel N` / `max_parallel` / `smart.max_parallel` | per consultation, on top of the global `general.max_concurrent_agents` |
| per-call time | `--timeout` (as before) | clipped to what is left of the total |
| agent output | `smart.max_response_chars` | total characters accepted |
| research sources, verification effort | `--max-sources`, `--depth` (as before) | part of the plan's call count |

**One clock.** There is no second timeout system: a call's own timeout is clipped to the time left,
research is clipped to the time left, and the old `debate.total_timeout_secs` stays as the
absolute ceiling that a `--max-duration` can lower but not raise.

**When a limit is reached** the consultation stops launching new work, cancels the call in flight
if the deadline is what ended it (the whole process group is terminated), keeps everything that
finished, marks the missing reviews and the unverified claims, and returns a clearly labelled
**partial** result. A subscription limit hit by an agent is **never retried**.

The budget is tracked, not estimated: calls started, calls answered from an identical earlier call,
peak parallelism, time spent waiting for a slot, characters sent and received.

## Provider performance profiles

A small local file (`profiles.json`, next to the saved debates, owner-only permissions) keeps what
was **observed** per provider: calls, successes, failures by kind (timeout, usage limit,
authentication, other), the latency of successful calls as an exponentially weighted moving
average per kind of call (answer, review, verification, synthesis, planning), characters per
reply, the token counts a CLI itself reported (summed only over the calls that reported them), when
it was last used and whether it was available.

- Nothing is invented: no token counts or costs are estimated, and startup overhead is not recorded
  because it cannot be separated from the call.
- Latency is **never** read as a measure of intelligence or answer quality.
- It holds no prompts, answers, argument lists, environment values or credentials.
- It is perishable: history older than `smart.profile_max_age_days` (14) is ignored, and latency is
  used only after `smart.profile_min_samples` (3) recent successful answers.
- A provider that failed `smart.profile_degraded_after_failures` (3) calls in a row, the last one
  within a day, is **tried last** (and an automatic coordinator is replaced by a healthier agent).
  It is never excluded, and an explicit coordinator or `--agents` list is never changed.
- Cancellations, calls refused by a budget, a missing executable and nested-orchestration refusals
  say nothing about a provider and are not recorded.
- `--no-save` skips it. With no usable history (none, too old, too few calls, an unreadable file)
  agents are used in registry order, and the plan says so.

## Concurrency

- Genuinely independent work runs concurrently: the independent answers of a panel; the evidence
  check and the review in `balanced` **when the review is certain** (the question alone calls for
  it, so neither depends on the other's result); the judges of a deep verification; page fetches.
- Work whose later steps might be avoided runs in sequence: `balanced` waits for the lead's answer
  and the evidence check before deciding whether a review is worth a call.
- Concurrency is bounded twice: by the process manager for the whole process, and per consultation
  by `--max-parallel`. A consultation that only ever has that many calls waiting should interleave
  fairly with other consultations in an MCP server (the process queue is first-in first-out); that
  is the design, and it has not been measured under load.
- Independent first-round analysis is preserved: panel answers are requested in parallel and none
  sees another's work; the `balanced` escalation perspective sees neither the lead's answer nor
  the review. (The reviewer, by design, reads the lead's answer: it is a review, not a second
  opinion.)
- Cancellation reaches the child processes through the process manager as before. All work is
  cancellable; a deadline cancels the call in flight.

## Work that is not done twice

Within one consultation only; nothing is persisted (persistent memory is Milestone 7).

- **Pages:** the same page under another spelling (fragment, tracking parameters, trailing slash,
  host case) is one source and is fetched once. The count is reported (`sources_merged`).
- **Claims:** a claim a further agent shares with one already judged is not sent to a judge again;
  only new claims are judged, against the same retrieved passages (`claims_reused`).
- **Agent calls:** two identical calls (same agent, settings and prompt) are answered once.
  A failed call is not shared as if it were a result and is never re-asked on anyone's behalf.
- **Context:** a review sees only the answer it reviews, an escalation perspective sees only the
  question and the evidence; the synthesis sees only what was gathered. Prompt size is measured
  (`prompt_chars`).
- **Evidence:** the evidence is gathered once and reused by every stage. It is not reused across
  consultations, so freshness requirements are always checked against pages fetched now.

An AI conclusion is never cached as a verified fact. Native CLI session continuation is **not**
used: there is no verified, tested, officially supported way to continue a session that would not
mean handling provider session state, which this project does not touch.

## Research and strategies

| | `fast` | `balanced` | `thorough` |
|---|---|---|---|
| default depth (when you set none) | `standard`, or the router's `deep` | `standard`, or the router's `deep` | `deep` |
| evidence check | one judge (the one agent) | one judge at `standard` depth, two at `deep` | two distinct judges |
| who searches the web | the first participant that can; with one agent, an automatic coordinator that cannot search is replaced by one that can | the first participant that can | the first participant that can |
| contradicting evidence | shown in the answer (claim table) | triggers the review | cross-examined in two rounds |

`--freshness-days`, `--max-sources`, `--url` and `--web` work as in Milestone 5 under every strategy
(the tests check the freshness limit under each). The sources an agent reports are only candidates;
a source counts when this program fetched it.

## Using it: CLI, MCP, plans and traces

```sh
ai-orchestra consult "Explain electrical resistance" --strategy fast
ai-orchestra consult "Which oscilloscope should I buy?" --strategy balanced
ai-orchestra consult "Analyze this scientific claim" --strategy thorough
ai-orchestra consult "Compare these products" --strategy auto --explain-plan
ai-orchestra consult "Help me choose a microscope" --coordinator claude --agents claude,codex,google --rounds 2
ai-orchestra consult "..." --max-agent-calls 4 --max-duration 120 --max-parallel 2
ai-orchestra consult "..." --dry-run          # the plan, nothing is called or fetched (same as --plan)
```

| Flag | Meaning |
|---|---|
| `--strategy auto\|fast\|balanced\|thorough` | default `auto`; `smart.default_strategy` in the config changes the default (`off` keeps the fixed Milestone 4/5 workflow) |
| `--max-agent-calls N` | at most N agent calls |
| `--max-duration SECS` | wall-clock limit for the whole consultation |
| `--max-parallel N` | at most N of this consultation's calls at once |
| `--explain-plan` | print the plan and the reasons before running, even with `--quiet` |
| `--dry-run` / `--plan` | print the plan and exit; no agent, no fetch, nothing saved |

The MCP `consult`, `research` and `verify_claim` tools take `strategy`, `max_agent_calls`,
`max_duration_seconds`, `max_parallel` and `dry_run`, and use the same engine; there is no second
copy of the logic. The result carries `strategy` (value, source, reason) and `orchestration`
(stop reason, agents used, calls, reused calls, peak concurrency, time breakdown, quality gate
verdict, the first 40 trace events). `get_debate` returns them for a saved consultation.

**Two views.** The *plan* is made before anything runs: the strategy, the flow, the agents in the
order they would be asked, the rounds, the expected and maximum number of calls, the limits and
the reasons. An adaptive plan can add steps while it runs; those decisions are not in the plan but in
the *trace*, which `ai-orchestra show <id>` prints under "Orchestration" and which is stored in the
record (`consultation.orchestration`):

```
execution:    4 call(s) in 59 ms (agents busy 57 ms, outside agent calls 2 ms), at most 1 at once
budget:       4 call(s) started, 0 reused, 0 ms waiting for a slot
stopped:      escalation limit: one escalation is the maximum
quality gate: 1 check(s) passed
  +     0 ms  planned: balanced strategy, adaptive flow, 3 agent(s), at most 4 call(s), limits: no limit set ...
  +     0 ms  the lead lead answers first
  +    13 ms  after the lead's answer: review: a decision question: the criteria and trade-offs deserve an independent check; the question is contested
  +    13 ms  beta reviews the answer independently
  +    29 ms  beta reviewed the answer: MATERIAL_DISAGREEMENT
  +    29 ms  after the review: independent_perspective: the reviewer disagrees materially with the answer; a third, independent perspective helps tell whose view holds
  +    29 ms  gamma gives an independent perspective, without seeing the others' work
  +    44 ms  after the escalation: synthesize (stop: escalation limit: ...): the bounded escalation is complete; everything gathered is integrated
  +    44 ms  lead writes the final answer from 3 input(s)
```

(Scripted agents; the timings are simulated.) Traces hold counts, names and reasons; they never
hold the question, a prompt, an answer, an argument list or an environment value.

## The same question in fast, balanced and thorough

`ai-orchestra consult "Should I buy a budget or professional oscilloscope?" --strategy S --dry-run`
on a machine with Codex and Antigravity usable (run inside Claude Code, so Claude is excluded; no agent was called):

| | `fast` | `balanced` | `thorough` |
|---|---|---|---|
| participants | `codex` | `codex, google` (the second only if asked) | `codex, google` |
| rounds | 0 | 1 (a second only if the reviewer disagrees materially and no agent is left) | 2 |
| flow | single | adaptive | panel |
| calls | 1 expected, at most 1 | 3 expected, at most 4 | 7 |
| note | "single-agent consultation: no independent review" | | |

What each did on the scripted workload "agents disagree" (3 agents, two of which hold the opposite
position; **simulated**): `fast` 1 call, no review; `balanced` 4 calls (lead, a review that says
`MATERIAL_DISAGREEMENT`, one independent third perspective, the synthesis); `thorough` 10 calls
(3 answers, 2 rounds of 3 reviews, the synthesis). The full comparison is in
[`performance-benchmarks.md`](performance-benchmarks.md). Which one is *better* is a question for a
live comparison of answer usefulness, not for latency; see the live A/B plan there.

## Why fewer agents may be chosen

- Asking more agents costs quota and time on **every** question, but changes the answer on few of
  them. A short, uncontested question or a claim the retrieved sources support does not gain from a
  second opinion; a contested decision, a contradiction or a material disagreement does.
- Independent reviews are kept where they pay: decisions and product evaluations, contested or
  long questions, contradicting evidence, and anything you ask for by name.
- Fewer agents also means fewer failure surfaces: in the scripted "provider failure" workload the
  fixed workflow was `partial` because one agent failed in every stage, while `fast` and `balanced`
  never asked it. (They would have been `partial` had they asked it: no strategy retries.)
- The plan always says what was left out and why (`not used: ...`), and `--agents` or `--strategy
  thorough` brings any of it back.

## Configuration

```toml
[smart]
default_strategy = "auto"          # auto | fast | balanced | thorough | off (the fixed M4/M5 workflow)
# max_agent_calls = 8
# max_duration_secs = 600          # never above debate.total_timeout_secs
# max_parallel = 3
max_response_chars = 2000000
profiles = true                    # the local provider history
# profiles_path = "~/.local/share/ai-orchestra/profiles.json"
profile_max_age_days = 14
profile_min_samples = 3
profile_ewma_alpha = 0.3
profile_degraded_after_failures = 3
```

A flag or MCP parameter always beats the configuration.

## Limits of this milestone, and known failure modes

- **Barely live-validated.** Everything here is tested with scripted agents, fake CLIs and a local web
  server. The only real run is one comparison of `fast`, `balanced` and `thorough` on one question
  (see [`performance-benchmarks.md`](performance-benchmarks.md#first-live-comparison-2026-10-12-0802-authorized-run-once)): it confirmed the plans, call
  counts, trace and parallelism on the real CLIs and says nothing about which answer is better.
- **The simulated benchmarks say nothing about answer quality, real token use or real quota savings.**
  They show *orchestration-level* differences: how many calls, in what shape, with what overhead.
- **The reviewer's verdict is a model's judgement.** A reviewer that is wrong, lazy or aligned with
  the lead's blind spot says `AGREE` and ends the consultation early. Use `thorough` (or `--agents`
  and `--rounds`) when that risk matters; the answer from a single reviewed run states that the
  verdict is "one model's judgement of one answer, not proof".
- **Wording features are blunt.** A short question can be high stakes and a long one trivial; `auto`
  can pick `fast` for a question that deserved more. The plan states why it chose, and
  `--strategy` overrides it.
- **No failover.** If the lead or the only reviewer fails, nobody takes its place (that would be a
  retry in disguise). A failed lead fails the consultation; a failed reviewer makes it `partial`.
- **A review is not a second opinion.** In `balanced` the reviewer reads the lead's answer, so it
  can be anchored by it; an independent perspective is added only on a material disagreement.
- **Missing critical criteria are not detected** automatically: the lead is asked to state its
  assumptions and information gaps, but they are not parsed into an escalation signal.
- **The history is small and perishable** and written without cross-process locking: two
  processes finishing at the same moment can lose one update. It never affects a result.
- **Queue time** is measured per consultation (the wait for its own parallel slots). The wait for the
  process manager's global slots is not separately recorded.
- **Session reuse** (continuing a CLI's own session to avoid resending context) is not implemented.
- Over MCP, a call limit set by the host applies to the whole consultation, not per task.
