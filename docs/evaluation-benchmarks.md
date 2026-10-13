# Evaluation benchmarks (Milestone 8)

What was measured for the evaluation system itself, what the offline example shows, and the plan for the first
real comparison. **Read [quality-evaluation.md](quality-evaluation.md) first for what a number here can and
cannot mean.**

## 1. What has been measured, and what it is

| Kind | Where it comes from | What it can support |
|---|---|---|
| Automated tests | `tests/eval.rs`, `tests/ux_chat.rs`, the `eval::*` and `ux::*` unit tests, the Milestone 8 block of `tests/mock_cli.rs`, `tests/memory_mcp.rs` | that the machinery behaves as specified |
| Offline fixture run | `ai-orchestra eval run starter --offline` | that the checks find the planted flaws; **nothing about any model** |
| Live run | one, 18 consultations, 55 calls (section 3a) | cost and time of these arms on six cases; correctness on cases where every arm passes |

No subscription-consuming call was made by any test. The one live run (section 3a) was authorized and its calls are counted there.

## 2. The offline fixture run (the `starter` dataset)

Twelve cases, one per category; four arms of **hand-written fixture responses** (`single:claude`,
`single:codex`, `balanced`, `thorough`), some deliberately wrong, with planted citation defects. 48 responses
are evaluated in one command (about 0.8 s wall-clock for the whole process in a release build on the
development machine, one run, which includes starting the program and writing the run file; no provider is
called).

Result of that run (deterministic: running it again gives the same table):

| Arm (fixture) | Objective checks passed | Citations structurally valid | Required claims cited to the supplied evidence | Links outside the source list |
|---|---:|---:|---:|---:|
| `balanced` | 11 / 11 | 8 / 8 | 5 / 5 | 0 |
| `thorough` | 11 / 11 | 9 / 10 | 5 / 5 | 0 |
| `single:codex` | 7 / 11 | 8 / 8 | 5 / 5 | 0 |
| `single:claude` | 5 / 11 | 5 / 6 | 2 / 5 | 1 |

How to read it: the table shows that the evaluator **notices** what the fixtures were built to contain - a wrong
current (48 A for 12 V over 4 ohm), a forgotten constraint, asserting a 2030 price, accepting a false premise,
an invented citation `[4]`, a link that is not in the source list - and does not penalise the fixtures that
were written correctly. It does **not** show that `thorough` is better than `single:codex`: the numbers
describe sentences a person wrote to produce them. The report says so in its first lines (`READ THIS FIRST`),
and no cost, token or time comparison is made from it because the fixtures record none (call counts and
latencies are left empty on purpose, so the quality-against-efficiency section says "agent calls were not
recorded for both" and refuses to claim a saving).

Findings made while building it, each now covered by a test:

* a negation window of 14 characters flagged "nothing here has been verified against ..." as an unbacked
  verification claim; negations are now looked for earlier in the same sentence;
* a model-name check was too static (it refused mock agents); names are now checked against the registry
  of the running installation, and the settings validate only their shape;
* a category name differed between the serialised form and its label; the dataset validator caught it
  because every built-in dataset is parsed by a test.

## 3. The first live comparison

The plan, then (3a) what the first step produced.

**Question:** does AI Orchestra give better answers than one assistant, or only longer and slower ones?

**Design**
* Dataset: `comparison` - 18 synthetic cases across the 12 categories, no recorded answers. Reference facts are
  supplied in the case or follow from arithmetic and textbook definitions, and supplied sources are put in the
  prompt. This tests reasoning, instruction-following, use of supplied evidence, citation hygiene and
  calibration - **not** how accurately an assistant researches the real web.
* Arms: one single agent (`single:codex`), `balanced`, `thorough`. (Add `single:claude` / `single:google` /
  `auto` from a plain terminal if you want them; Claude Code cannot be called from inside its own session.)
* Each consultation runs alone with memory off, one run per case and arm, nothing retried.
* Measured automatically: the objective checks, citation validity, evidence coverage, calls, wall time,
  provider-reported input tokens when present.
* Measured by people: blind pairwise preferences between arms, with the length and position bias shown.
* Report: counts first; intervals only from 10 decided comparisons; no claim of "better" from a single run.

**Cost** (printed by `eval run ... --live` before anything runs; from the plan made on the development
machine, from inside Claude Code, so only Codex and Google were usable): 54 consultations, about 178 agent
calls typically and at most 216 for the three arms above. From a plain terminal with all three agents,
`thorough` makes more calls (up to 10 per case), so read the plan on your machine. The default ceiling is 150
calls, so the full run needs `--max-total-calls`.

**Commands** (run from a plain terminal; nothing below runs until the `--yes-run-live` ones):

```sh
# 0. free: validate the dataset and see the exact plan for your machine (no provider is called)
ai-orchestra eval validate comparison
ai-orchestra eval run comparison --live --arms single:codex,balanced,thorough

# 1. a small first run: six cases (one per kind of question) = about 60 calls
CASES=fact-ohm,science-claim,product-scopex,conflict-kestrel,uncertain-price,followup-channels
ai-orchestra eval run comparison --live --arms single:codex,balanced,thorough --cases $CASES --yes-run-live

# 2. the full run once step 1 looks sane (about 180-260 calls; check the plan first)
ai-orchestra eval run comparison --live --arms single:codex,balanced,thorough --max-total-calls 260 --yes-run-live

# 3. blind comparison of the newest run: single agent against each strategy
RUN=$(ai-orchestra eval list --output json | python3 -c "import json,sys;print(json.load(sys.stdin)['runs'][0]['id'])")
ai-orchestra eval blind $RUN --pairs single-codex:balanced,single-codex:thorough --seed 7 > packet.txt
#    read packet.txt without opening the run file; for each item:
ai-orchestra eval judge $RUN item-001 --choice first --evaluator you --why "which one and why"
#    ... until every item is judged

# 4. the report (the identities are revealed here)
ai-orchestra eval report $RUN
```

Repeat step 2 at least twice before drawing a conclusion: live runs are stochastic and one run per arm cannot
separate a real difference from variation.

## 3a. The first live run (2026-10-12, authorized): six cases, three arms, Codex only

Step 1 of the plan below, run from inside Claude Code (so only Codex and Google could take part): six cases
(`fact-ohm`, `science-claim`, `product-scopex`, `conflict-kestrel`, `uncertain-price`, `followup-channels`) x
`single:codex`, `balanced`, `thorough` = 18 consultations, **55 agent calls** (plan: about 62, at most 72),
about 20 minutes, one run per arm, memory off, nothing retried. Nothing was left running.

**What the first scoring said, and why it was wrong.** The run was scored with version 1.0.0 of the checks:
`balanced` 4/6, `single:codex` 3/6, `thorough` 3/6 objective checks passed. Reading the failed answers showed the
failures were defects of the *checks*, not of the answers: a forbidden phrase ("will cost") matched the answer
restating the question ("what a Scopex B will cost in 2030"); another ("is supported") matched "whether the claim is
supported"; a required fact ("4 channels") missed the paraphrase "has more channels: 4, compared with ...". All three
arms had answered those cases correctly. The checks were corrected (dataset 1.0.1, listed in its provenance notes; no
question or fixture changed) and the **stored answers were scored again with `eval rescore`**, which calls no
provider. This is a post-hoc change made after seeing the answers; it is disclosed here and in the run's notes, the
original run is kept next to the re-scored one, and from now on the checks are frozen for this dataset version. A
regression test uses the three real answers.

**Result, scored with the corrected checks (one run per arm, 6 cases - counts, not a comparison):**

| Arm | Objective checks passed | Agent calls | Median time per case | Median input tokens (reported by the CLI) | Median words |
|---|---:|---:|---:|---:|---:|
| `single:codex` | 6 / 6 | 6 | 9.6 s | 18,262 | 323 |
| `balanced` | 6 / 6 | 11 | 63.4 s | 31,574 | 427 |
| `thorough` | 6 / 6 | 38 | 156.7 s | 124,262 | 437 |

What this does and does not show:

* On these six cases the three arms passed the same objective checks: **no difference in correctness was
  detected, and six easy cases with checks every arm passes cannot detect one.** The multi-agent arms cost 1.8x
  (`balanced`) and 6.3x (`thorough`) the agent calls, took 6.6x and 16x the time, used 1.7x and 6.8x the input
  tokens, and wrote 32-35% more words. Those cost figures are measurements of this run (the token counts are
  what the CLI reported, one number per call, summed per consultation).
* Whether the extra work produced answers people prefer is **not known**: the blind comparison has not been
  judged. A longer answer is not evidence of a better one; the report will show how often the longer answer
  wins.
* One `thorough` consultation (`uncertain-price`) finished *partial* (3 of its 7 planned calls); the first version
  of the run record did not keep the warning that said why, so the cause is unknown (the record now keeps it).
* Citations: all 57 citations in the answers were structurally valid (the sources were the ones supplied); 7 cited
  sentences share little vocabulary with their passage (a lexical flag for a reader, not a verdict).
* Claude Code cannot be called from inside its own session, so Claude was not an arm. Google took part only inside
  `balanced` and `thorough`.

**Blind judgments (one human judge, 12 comparisons, seed 7, 2026-10-12).** `single:codex` against `balanced` and
against `thorough`, six cases each, judged without knowing which answer was which (run
`eval-20261012-191556-0004a3`):

| Comparison | Prefers the single agent | Prefers the strategy | Tie | Longer answer won | First-shown answer won |
|---|---:|---:|---:|---:|---:|
| `single:codex` vs `balanced` | 2 | 2 | 2 | 2 of 4 | 2 of 4 |
| `single:codex` vs `thorough` | 0 | 6 | 0 | 2 of 3 | 2 of 6 |

Reading it honestly: with six decided comparisons per row the report gives no interval and no verdict. The
`balanced` row is even. The `thorough` row is lopsided - six out of six would be a 1-in-32 outcome if the judge
were choosing at random - but it is **one judge and six cases that every arm answered correctly**, so it can mean "this judge preferred `thorough`'s
fuller answers" and cannot mean "`thorough` answers better" in general. The length check is mixed (the longer
answer won in 2 of the 3 `thorough` comparisons where the lengths differed by 10% or more), and in one case the
judge chose the longer `thorough` answer although the case's instruction said to prefer the shorter one when both
are correct (`fact-ohm`), so the preference is not explained by the case criteria alone. One comparison
(`uncertain-price`, item-001) was recorded twice by the same judge with the same choice; the later record
supersedes and both stay in the file. No judge reasons were written except in one, so *why* is not recorded.

## 3b. Harder cases (built; half of them run in 3c)

The first run could not tell the arms apart because the cases were easy. The `hard` dataset has 12 cases that
a careless answer gets wrong: forgetting the 2 pi in an RC cutoff, adding the amp-hours of a series pack, using
the power formula for a voltage gain, ignoring an LED's drop, a resistor exactly at its rated power, a question
with a false premise, a question the supplied datasheet cannot answer, constraints no product satisfies, a
preference that changed mid-conversation (the later one must win), a newer source that supersedes an older one,
and two studies that disagree. Each reference is supplied in the case or follows from arithmetic, and each
case's checks are tested (`tests/eval.rs`) against plausible correct answers - including ones that restate the
question or paraphrase - and the classic wrong ones **before** any model has answered, so the defects found in
the first dataset's checks are not repeated. That the cases are hard is a hypothesis, not a result.

Plan for your machine, from inside Claude Code (Codex and Google only): 36 consultations, about **120 agent
calls, at most 144** for `single:codex`, `balanced` and `thorough` - under the default ceiling of 150. Commands
(nothing runs until the `--yes-run-live` ones):

```sh
export AI_ORCHESTRA_CONFIG=~/evaltrial/ai-orchestra.toml
ai-orchestra eval validate hard
ai-orchestra eval run hard --live --arms single:codex,balanced,thorough          # prints the plan, runs nothing

# a half-size first pass (6 cases, about 60 calls):
CASES=hard-rc-cutoff,hard-battery-runtime,hard-missing-input-voltage,hard-no-feasible-option,hard-changed-preference,hard-mixed-studies
ai-orchestra eval run hard --live --arms single:codex,balanced,thorough --cases $CASES --yes-run-live

# or the whole set (about 120 calls):
ai-orchestra eval run hard --live --arms single:codex,balanced,thorough --yes-run-live
```

If the checks still do not tell the arms apart, the report says so; if they do, the blind comparison
(`eval blind` / `eval judge`) then adds the human view. More than one run and more than one judge are needed
before any conclusion.

## 3c. The harder cases, half pass (2026-10-12, authorized): six cases, three arms, Codex only

Six of the twelve `hard` cases (`hard-rc-cutoff`, `hard-battery-runtime`, `hard-missing-input-voltage`,
`hard-no-feasible-option`, `hard-changed-preference`, `hard-mixed-studies`) x `single:codex`, `balanced`,
`thorough` = 18 consultations, **59 agent calls** (plan: about 60), about 22 minutes, one run per arm, memory off,
nothing retried, nothing left running. Run `eval-20261012-202656-0128bb`. The checks were not touched after the
run (they had been tested against plausible answers beforehand).

| Arm | Objective checks passed | Agent calls | Median time per case | Median words |
|---|---:|---:|---:|---:|
| `single:codex` | 6 / 6 | 6 | 14.7 s | 398 |
| `balanced` | 6 / 6 | 11 | 51.7 s | 558 |
| `thorough` | 6 / 6 | 42 | 148.0 s | 498 |

**The cases did not discriminate: every arm passed every check, including the ones built to catch the classic
slips** (forgetting the 2 pi, adding the amp-hours, forcing a pick when no model fits, treating a superseded budget as
current). So the hypothesis that these cases are hard for these models was wrong, for this size and these arms.
Correctness cannot tell the arms apart here either, and the multi-agent arms again cost more: 1.8x (`balanced`) and
7x (`thorough`) the agent calls, 3.5x and 10x the time.

**Blind judgments by an AI judge, 12 comparisons (labelled `ai`, not ground truth).** The user asked me (Claude) to
judge, so these are AI-assisted judgments, kept apart from the human ones in the report. The judge produced
none of the answers (`codex` and `google` did), judged from the blind packet (seed 11) without knowing which answer
was which, and wrote a reason for each. Its limits: it is one model, it wrote the cases and their instructions, it
knew the hypothesis, and it is not independent of the project. It also revised one judgment while judging
(`item-001`) after noticing in `item-005` that one answer attributed to the user a requirement they never stated;
only the final judgment was recorded.

| Comparison | Prefers the single agent | Prefers the strategy | Tie |
|---|---:|---:|---:|
| `single:codex` vs `balanced` | 1 | 2 | 3 |
| `single:codex` vs `thorough` | 2 | 2 | 2 |

Nothing here separates the arms: with six comparisons per row the report gives no interval, the counts are about
even, and most differences the judge saw were things the cases did not ask for - a shorter, direct answer versus a
long one that pastes in the agents' discussion, a useful one-line safety caution in one answer, an answer that
weighed the two studies' sample sizes explicitly (the strategies did, the single agent did not), and one answer
that put a requirement in the user's mouth (the single agent did). Together with the earlier human judgments
(`thorough` preferred 6 of 6 on easy cases, one judge) the picture is: **on these question types the single agent
is as correct as the strategies, the strategies sometimes add a missing nuance or a caution, they write more and
cost several times more calls and time, and the human and AI judges do not agree on whether that is better.**
Nothing here supports "multi-agent answers are better" and nothing supports "they are not": the sample, the
cases and the judges are too small, and the cases too easy.

## 4. What would count as an answer

* If `balanced` or `thorough` pass **more objective checks** than the single agent over the same cases, and
  the blind preferences (at least 10 decided comparisons, interval excluding 50%) agree, that is evidence for
  this dataset.
* If they pass the same checks and the preferences favour the longer answers, the report's length-bias line is
  the first thing to read: that is what "longer, not better" looks like.
* If a strategy matches the single agent on the checks and uses fewer calls than `thorough` on at least 10
  shared cases, the report will say so in one sentence, and say it does not carry over to other questions.
* Anything else is reported as counts, with the reason no comparison is made.

## 5. Not measured

* Any real model, any real web research, token or money cost, variance across runs, other languages, long
  conversations, the quality of the Spanish-language routing, or human inter-rater agreement.
