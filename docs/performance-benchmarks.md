# Orchestration benchmarks (Milestone 6)

This page documents how the orchestration strategies were measured, what the numbers are, and what
they cannot tell you.

> **Every number below is simulated.** The agents are scripted in-process stand-ins with fixed
> artificial delays, and the web pages come from a local test server. The benchmark shows *how much
> orchestration work each strategy does* (calls, shape, overlap, overhead). It says **nothing** about
> answer quality, real token use or real subscription savings, and it must not be quoted as if it did.
> The simulated benchmark used no real model, website or subscription. The only real measurements are
> the single live run in [First live comparison](#first-live-comparison-2026-10-12-0802-authorized-run-once).

## Baseline: what the fixed Milestone 4/5 workflow did

Measured before any Milestone 6 change, with the same scripted workloads (the `legacy` column
below reproduces it exactly, and a test asserts that it still does):

- **A fixed fan-out.** With two or more usable agents, every question cost `N + R*N + 1` calls
  (N agents, R review rounds) whatever it was: 7 calls for a two-product comparison, 10 for a
  decision, 12 for deep research. Nothing looked at the first answers to decide whether the rest
  was worth a call.
- **Stage barriers.** Each stage waited for its slowest agent. In the slow-provider workload one agent
  that took 120 ms instead of 12 ms put 383 ms on the critical path, three stages in a row.
- **Few limits.** A per-call timeout and a 1800 s total. No cap on the number of calls, no cap on
  how many of one consultation's calls run together, no record of how the providers had behaved.
- **Concurrency was already good** where it applies: independent answers and each review round run in
  parallel, page fetches are bounded, cancellation terminates process groups. That is kept.
- **Overhead of this program** (time outside any agent call) was 0 to 3 ms without research and
  5 to 17 ms with one to five local pages: it is not the bottleneck; the agents are.

## What was measured, and how

`cargo test --test benchmark -- --nocapture` runs twelve workloads through the real consultation
engine for each of five settings: `legacy` (the fixed workflow, `strategy: None`), `fast`,
`balanced`, `thorough` and `auto`. The scripted agents (`tests/common/scripted.rs`) recognise what
they are asked from the prompt and answer by rule: they take a position, can end their answers
with a claims block, can be slow, and can fail; a fake judge reads the passages in its prompt and
says whether each contains the number the claim states. An ordinary call takes 12 ms, the slow
provider 120 ms.

| # | Workload | What it exercises |
|---|---|---|
| 1 | simple factual question | the cheapest possible path |
| 2 | educational explanation | a teaching question |
| 3 | two-product comparison | a review that benefits from a second look |
| 4 | complex purchasing decision | a long, multi-part decision |
| 5 | scientific claim verification | the claim check against a page |
| 6 | conflicting evidence | two pages that contradict each other |
| 7 | agents disagree | two of three agents take the opposite position |
| 8 | slow provider | one agent is ten times slower |
| 9 | provider failure | one agent fails every call |
| 10 | rate-limit failure | one agent hits a usage limit on its first call |
| 11 | deep research, several sources | five pages, three of them listed again under other spellings |
| 12 | explicit 3-agent 2-round debate | everything set by hand |

For each run it records, **from the saved record** (not from the test): wall-clock time, agent
calls (and failed ones), the sum of the agents' call durations, the time at least one agent was
working ("busy"), the time outside any agent call ("overhead"), time waiting for a parallel slot
("queue"), peak concurrency, page fetches, work avoided (identical calls answered once, claims not
judged again, pages merged), characters sent to the agents, quality-gate violations and the reason
deliberation stopped.

The asserts are only about deterministic properties (counts, statuses, which strategy `auto`
picked, that the legacy column did not change); none depends on a duration.

**Real versus simulated.** The delays are simulated. The *overhead* column is the real elapsed time
of this program's own work (building prompts, parsing, fetching local pages, extraction, passage search,
bookkeeping) on a development machine in a debug build; it excludes real networks and real
providers, so it is a lower bound for any real run, not a prediction.

## Results (simulated)

### Agent calls

| # | Workload | legacy | fast | balanced | thorough | auto |
|---|---|---|---|---|---|---|
| 1 | simple factual question | 1 | 1 | 1 | 10 | 1 → fast |
| 2 | educational explanation | 1 | 1 | 1 | 10 | 1 → fast |
| 3 | two-product comparison | 7 | 1 | 2 | 10 | 2 → balanced |
| 4 | complex purchasing decision | 10 | 1 | 2 | 10 | 10 → thorough |
| 5 | scientific claim verification | 1 | 1 | 1 | 1 | 1 → balanced |
| 6 | conflicting evidence | 8 | 2 | 4 | 12 | 2 → fast |
| 7 | agents disagree | 10 | 1 | 4 | 10 | 4 → balanced |
| 8 | slow provider | 10 | 1 | 2 | 10 | 2 → balanced |
| 9 | provider failure | 8 (partial) | 1 | 2 | 8 (partial) | 2 → balanced |
| 10 | rate-limit failure | 8 (partial) | 1 | 2 (partial) | 8 (partial) | 2 (partial) → balanced |
| 11 | deep research, several sources | 12 | 2 | 5 | 12 | 12 → thorough |
| 12 | explicit 3-agent 2-round debate | 6 | 6 | 6 | 6 | 6 → thorough |

### Wall-clock milliseconds (simulated delays; one run, expect a few ms of jitter)

| # | Workload | legacy | fast | balanced | thorough | auto |
|---|---|---|---|---|---|---|
| 1 | simple factual question | 15 | 15 | 15 | 60 | 16 |
| 2 | educational explanation | 14 | 14 | 14 | 60 | 14 |
| 3 | two-product comparison | 44 | 14 | 30 | 64 | 32 |
| 4 | complex purchasing decision | 60 | 15 | 30 | 62 | 60 |
| 5 | scientific claim verification | 18 | 19 | 17 | 17 | 17 |
| 6 | conflicting evidence | 64 | 35 | 65 | 83 | 35 |
| 7 | agents disagree | 59 | 14 | 57 | 59 | 60 |
| 8 | slow provider | 385 | 15 | 138 | 393 | 140 |
| 9 | provider failure | 61 | 15 | 32 | 62 | 31 |
| 10 | rate-limit failure | 60 | 15 | 29 | 61 | 29 |
| 11 | deep research, several sources | 92 | 44 | 72 | 95 | 94 |
| 12 | explicit 3-agent 2-round debate | 59 | 56 | 60 | 59 | 63 |

### Characters sent to the agents (measured by the budget manager)

`legacy` is not measured: the fixed workflow has no budget manager, so it does not count.

| # | Workload | fast | balanced | thorough | auto |
|---|---|---|---|---|---|
| 1 | simple factual question | 2,389 | 2,103 | 18,470 | 2,389 |
| 3 | two-product comparison | 2,670 | 4,590 | 21,433 | 4,590 |
| 4 | complex purchasing decision | 2,914 | 5,078 | 23,873 | 23,873 |
| 6 | conflicting evidence | 6,450 | 15,493 | 45,878 | 6,450 |
| 7 | agents disagree | 2,663 | 9,005 | 21,891 | 9,005 |
| 11 | deep research, several sources | 8,408 | 22,543 | 61,892 | 61,892 |

(Characters, not tokens. Token counts are only ever recorded when a CLI reports them; no conversion
is made. The scripted agents report none.)

### The provider history in use (workload 8, balanced)

| | wall ms | reviewer |
|---|---|---|
| no history | 139 | the slow agent (registry order) |
| after three runs of the same workload | 36 | the faster agent (ordered by its recorded latency) |

The ordering uses typical answer latency only; it says nothing about which agent answers better.

### What the numbers do show

- **The fixed workflow's cost was independent of the question**; the strategies' cost is not. On the
  scripted workloads `balanced` made 1 to 5 calls where the fixed workflow made 1 to 12, and `fast`
  made 1 or 2 (6 for the debate, which you set by hand).
- **Escalation is rare and local**: `balanced` made 4 or 5 calls only in the workloads that call for
  it (7: the agents disagree; 6: the evidence contradicts a claim; 11: deep research with a contested
  claim) and 2 calls for a comparison its reviewer agrees with.
- **Everything you set is respected**: workload 12 costs 6 calls under every strategy.
- **`auto` chose** `fast` for the two short questions and the contradiction case, `balanced` for the
  comparison, the disagreement and the claim check, and `thorough` for the long multi-part decision
  and the deep research.
- **`thorough` costs more than the fixed workflow in workloads 1 and 2** (10 calls against 1): the
  fixed workflow had a lightweight shortcut for short teaching questions that `thorough` does not
  take, because it was asked to be thorough.
- **Failures:** `fast` and `balanced` never asked the agent that always fails (workload 9) so they
  finished `complete`; `thorough` and the fixed workflow asked it and were `partial`. `balanced`
  asked the agent that hit a usage limit (workload 10) as the reviewer, was `partial`, and did not
  retry or substitute another agent; `fast` did not ask it.
- **Parallelism:** `thorough` ran three calls at once (peak 3); `balanced` and `fast` ran one at a
  time (peak 1), except in workload 11, where the deep evidence check uses two judges at once (peak 2).
  (The check-and-review overlap of `balanced` needs a decision or review question with research; it
  is covered by a test with a barrier, not by these workloads.)
- **Quality gate:** no violations in any of the 60 runs of the five settings.

### What they do not show

- That any strategy gives a **better or worse answer**. `fast` is not shown to be as good as
  `thorough`; it is shown to cost less. The scripted agents give the same quality of answer to every
  question by construction.
- Real latency, real queueing at the process manager, real token use or real quota savings.
- How often a real reviewer says `AGREE` to an answer that deserved a challenge. In the scripts the
  verdict is a deterministic comparison of stances.
- Behaviour under many simultaneous consultations.

## Reproduce

```sh
cargo test --test benchmark -- --nocapture                      # the table
AI_ORCHESTRA_BENCH_OUT=/tmp/bench.json cargo test --test benchmark   # also as JSON
AI_ORCHESTRA_BENCH_SHOW=7:balanced cargo test --test benchmark -- --nocapture   # one run's answer, plan and trace
```

## First live comparison (2026-10-12 08:02, authorized, run once)

**Real measurements, one run per strategy: an anecdote, not a benchmark.** Run by a one-shot
LaunchAgent outside Claude Code (`live-tests/smart-live.py`, results in
`live-tests/results/smart-20261012-080200/`), with Claude, Codex and Antigravity all usable, on
"Should I buy a budget or professional oscilloscope?" (no research; each strategy once).

| | fast | balanced | thorough |
|---|---|---|---|
| plan (dry run, nothing sent) | single, 1 call | adaptive, 3 expected, at most 4 | panel, 10 calls |
| agent calls made | 1 | 4 | 10 |
| wall clock | 19.2 s | 83.8 s | 98.6 s |
| peak concurrency | 1 | 1 | 3 |
| characters sent to agents | n/a (see tokens) | 32,575 | 180,759 |
| tokens reported by the CLIs (input / output) | 2 / 1,979 | 32,555 / 6,801 | 109,911 / 13,447 |
| answer length | 6,095 chars | 6,113 chars | 6,761 chars |
| status | complete | complete | complete |

What happened in `balanced`: Claude answered (24.7 s); Codex reviewed it and ended with
`VERDICT: MATERIAL_DISAGREEMENT` (14.9 s; it called the answer's bandwidth and safety guidance too
unqualified for a question that gave no use case); Google gave an independent perspective (26.6 s);
Claude wrote the synthesis (17.6 s). The trace recorded each decision and the stop reason
("escalation limit: one escalation is the maximum"). `thorough` ran the three answers in parallel,
two rounds of three reviews and the synthesis. No limit was reached, no call failed, nothing was
retried, no quality-gate check failed, and no process was left running.

What this does and does not say:

- It confirms the plans, the call counts, the trace and the parallelism on the real CLIs.
- `balanced` escalated on a question that gave the agents no context; a reviewer will often disagree
  materially with an answer to an under-specified question, so for such questions `balanced` can cost
  4 calls and nearly as much wall time as `thorough` (84 s against 99 s), while sending about a fifth of
  the text. Whether that escalation improved the answer is not measured here.
- The three answers are of similar length and all say the choice depends on use case and budget. Whether
  the extra calls of `balanced` and `thorough` made the answer *more useful* is a judgement for you
  to make from the three `final_answer` fields, ideally blind; this run does not decide it.
- `fast` shows 2 input tokens because that is what the Claude CLI reported (cached input is not
  counted there); token counts are reported by the CLIs, never estimated, and are not comparable
  between providers.
- One run each, one question: no variance is known.

## How to repeat the live comparison

**It spends subscription quota and needs your explicit authorization each time.** The aim is to
compare latency, number of invocations, evidence gathered and the usefulness of the answers for the
same question. Latency alone cannot say which strategy is better.

Zero-quota first (these send nothing):

```sh
B=./target/release/ai-orchestra
Q="Should I buy a budget or professional oscilloscope?"
for s in fast balanced thorough; do $B consult "$Q" --strategy $s --dry-run; done
```

Then, from a plain terminal (not inside Claude Code, which refuses nested orchestration), one run
per strategy, smallest first, each saved with its own trace:

```sh
$B consult "$Q" --strategy fast     --quiet --output json > fast.json       # 1 request
$B consult "$Q" --strategy balanced --quiet --output json > balanced.json   # 2 to 4 requests
$B consult "$Q" --strategy thorough --quiet --output json > thorough.json   # 7 requests (2 agents) or 10 (3)
```

Compare, from each file: `jq '.calls, .duration_ms, .consultation.orchestration.metrics,
.consultation.orchestration.stop_reason, .consultation.orchestration.quality.checks' *.json`, and
`jq -r .final_answer` side by side. Judge the three answers blind if you can (hide which strategy
wrote which), on usefulness for the decision: criteria named, trade-offs, what is missing.
Repeat a few times before concluding anything; one run per strategy is an anecdote. For an
evidence-backed comparison add `--web on --depth standard` to all three (the planning and
verification calls are added to each count).

To bound the spend, add `--max-agent-calls 4` or `--max-duration 180` to any command; a limit that
cannot be met fails before anything is sent.
