# Quality evaluation: does asking several AIs give better answers? (Milestone 8)

AI Orchestra's premise is that independent assistants that review each other produce better answers than one
assistant. That is a hypothesis. This milestone builds the machinery to test it and, just as importantly, to
**refuse to answer it without data**: a longer answer, an answer several models agree on, or an answer that
sounds more careful is not thereby a better answer.

Nothing in this milestone shows that multi-agent consultation is better, and nothing in it claims so. The
offline results are produced from hand-written fixture responses and describe the fixtures. The one live run so
far (six easy cases, Codex only) is reported in [evaluation-benchmarks.md](evaluation-benchmarks.md): every arm
passed every corrected objective check, so it found no difference in correctness and, on cases that easy, could
not have; the multi-agent arms cost more calls, time and tokens, and one human judge, blind, preferred
`thorough` in 6 of 6 comparisons and split evenly on `balanced`, which is too little to conclude anything.

## The pieces (`src/eval/`)

| Piece | What it does |
|---|---|
| `dataset` | cases with their own reference evidence, versioned, with provenance |
| `rubric` | structured criteria per consultation type; deterministic where possible, left to people otherwise |
| `citations` | mechanical citation checks, kept apart from semantic support |
| `run` | the record of an evaluation run (JSON, owner-only) and the offline runner |
| `blind` | reproducible blind pairwise comparison; judgments appended, never edited |
| `stats` | medians, rates with Wilson intervals, seeded bootstrap intervals, minimum sample sizes |
| `report` | reports that keep fixture / mock / recorded / live results apart |
| `live` | the live runner: plan first, explicit authorization, ceilings, isolation |
| `feedback` | explicit user feedback, stored locally |

## What kinds of result exist, and why they are never merged

Every response in a run carries a **provenance class**, and a report summarises each class on its own, with a
sentence saying what it means:

* `fixture` - hand-written to exercise the checks. Says nothing about any model.
* `mock` - produced by scripted in-process agents. A test of the machinery.
* `recorded_live` - real provider output recorded in an earlier authorized run.
* `live` - real provider output from an authorized live run.

On top of that come two more distinctions the numbers must not blur: **independent factual checks** (the
dataset's own reference evidence) are not **human subjective preferences** (blind judgments), and a preference
rate is never shown as an accuracy. Comparing two runs of different datasets or different kinds of results
prints a warning instead of a verdict.

## Datasets

A dataset is a JSON file (`format: ai-orchestra-eval-dataset`, `format_version: 1`) with `id`,
`dataset_version`, `provenance` (authors, creation date, description, whether it is synthetic) and cases. Each
case has: `id`, `category`, `question`, evaluation `instructions`, whether it is `objective`, the
`reference_basis` that makes an objective case checkable, optional earlier `history` (follow-up cases),
`expect` (required facts, forbidden claims, constraints, whether uncertainty is the right answer, citation
requirements), supplied `evidence` passages, the rubric `criteria` that apply, and optional `recorded`
responses per arm.

**Ground truth is never invented.** An objective case must say where its reference comes from (a supplied
passage, arithmetic that follows from the question, a textbook definition); validation refuses one that does
not, and refuses checks with nothing to look for, duplicate ids, unknown criteria or arms, and a required claim
that cites evidence the case does not supply. `eval validate <dataset>` lists every problem, not only the first.
A subjective case (a personal decision with stated preferences) is marked as such and judged by people.

The twelve categories: simple facts, multi-step reasoning, technical explanations, scientific claim
verification, product comparisons, personal decisions (synthetic preferences), multi-source research,
conflicting evidence, questions whose right answer is uncertainty, misleading premises, follow-ups that need
memory, and questions where fewer AI invocations should be enough. Three datasets ship:

* `starter` - twelve small cases, one per category, **with hand-written fixture responses** for four arms
  (some deliberately wrong, with planted citation defects), to exercise the offline evaluator;
* `comparison` - eighteen cases for the live experiment, **without** recorded responses (see
  [evaluation-benchmarks.md](evaluation-benchmarks.md)). The first live run showed these are easy: every arm
  passed every corrected check;
* `hard` - twelve cases chosen because a careless answer fails them (a classic arithmetic slip, a resistor exactly
  at its rating, a false premise, a question the datasheet cannot answer, constraints that cannot all be met, a
  changed preference, a newer source superseding an older one, two studies that disagree). Written **before**
  any model answered it, and its checks are tested against plausible correct and wrong answers first, because
  the first dataset's checks had to be corrected after a live run. Whether it discriminates is a hypothesis the
  first run will test; a case every arm passes says so in the report.

Every product, study and page in them is invented (`.invalid` domains); nothing is a claim about the real
world. Private conversations are never turned into cases.

## Rubrics

Criteria are grouped as general (factual correctness, evidence quality, citation accuracy, instruction
adherence, completeness, logical consistency, uncertainty calibration, decision usefulness, clarity) plus a
set for each of decision, research, review, learn and debate (for example fair comparison of alternatives, user
priorities respected, source provenance, contradiction handling, conceptual accuracy, constructive criticism).
Each criterion has a method:

* **deterministic** - decided by checks the dataset defines: required facts present and no forbidden claim
  (factual correctness), constraints respected, required claims made and cited to the supplied evidence,
  citations structurally valid, uncertainty stated where it is the right answer, sources with dates;
* **judgement** - fairness, clarity, usefulness, whether a citation really supports a sentence: not scored by
  the program. They come out `NotAssessed`, waiting for a person (the blind comparison).

The outcome of a criterion is `Pass`, `Fail`, `NotApplicable` or `NotAssessed`. There is **no total and no
0-5 score**: a rubric result is a set of judgments, not a probability that the answer is true, and a criterion
that does not apply is `NotApplicable` rather than scored to fill a cell.

Keyword checks are crude. A required fact is present if the answer contains any of its listed alternatives
(case-insensitive); a correct paraphrase can be missed, so a failing check is a thing to look at, not proof of
a wrong answer, and `eval inspect` shows the answer next to every check.

## Citations: valid is not supported

Two different questions, kept apart (`citations.rs`):

* **Mechanical validity** (exact): does `[n]` name a source of this answer; was that source retrieved (only
  pages the program fetched are evidence); are links in the text in the source list; are publication and
  retrieval dates recorded; does the answer say "verified"/"SUPPORTED" with nothing retrieved to verify against
  (honest negations such as "nothing here has been verified" are not flagged); does it make a claim the case
  requires without citing the evidence for it.
* **Support** (not answerable by this module): does the cited passage say what the sentence says? A citation to
  a real, retrieved, dated page can still be irrelevant. The report gives the *lexical overlap* between the
  citing sentence and the passage held for the source, labelled as such, and keeps the passage so a person can
  read it. It is a flag, never "semantically verified"; the semantic judgement made by the research layer's
  verifier stays labelled model-dependent.

The starter dataset contains adversarial fixtures for this: a believable citation whose source is real but
unrelated, an invented `[4]` next to "verified against the manufacturer's own site", a link outside the source
list.

## Blind pairwise comparison

`eval blind <run> --pairs single-codex:thorough --seed 5` prepares items: for each case where both arms have a
usable response of the same provenance class (a failed response is not compared), the two answers are shown as
"Answer 1" and "Answer 2" in an order drawn from a seeded generator, and the items themselves are shuffled. The
packet contains the question, the evaluation instructions (the same for both answers) and the two texts; no
provider, strategy, provenance or arm. The same seed gives the same packet.

`eval judge <run> <item> --choice first|second|tie|insufficient --evaluator <name> [--kind human|ai] --why
"..."` records a preference, a tie or "not enough information", with the reason. Judgments are appended and
never edited; a later judgment by the same evaluator supersedes the earlier for the statistics while both stay
in the file. Identities are revealed only by `eval report`.

* Human evaluation is the default. An **AI judge** can be recorded (`--kind ai`), but is labelled, summarised
  separately and never treated as ground truth, and **a model that produced one of the answers cannot judge
  it** (a pair involving `thorough` involves every agent that took part). AI Orchestra does not run an AI judge
  for you; that would be a live call and a design of its own.
* **Biases are measured next to the result**: how often the *longer* answer won (among decided items where the
  lengths differ by 10% or more) and how often the answer shown *first* won. Blinding hides the label; it does
  not hide style, and polish and length tend to win regardless of quality.

## Statistics (`stats.rs`)

Medians; rates with a Wilson 95% interval; for paired preferences the mean of {win = 1, tie = 0.5, loss = 0}
over decided items with a **seeded percentile bootstrap** (2,000 resamples, same seed and data give the same
interval); "insufficient information" is reported separately and is not a vote. **No interval is reported below
10 observations**, and the report then says the sample is too small to compare ("read the counts, not the
rate"). When the interval includes 50% it says the sample does not separate the two; when it excludes it, it
says that is a preference in this sample, not a measure of accuracy and not causal.

## Quality against efficiency

For each pair of arms of the same provenance class the report compares, on the cases both answered: agent calls
(only if recorded for every case), objective checks passed (only if available on the same cases), and then
generates **one sentence, and only the one the numbers support**:

* fewer than 10 shared cases: counts only, "too few to say whether the two are alike or one is better";
* a saving ("X used fewer agent calls than Y while passing at least as many objective checks on this dataset")
  only with recorded calls, at least 10 shared cases and no loss of checks, and always followed by "this does
  not carry over to other questions";
* otherwise "no saving can be claimed from these numbers".

Latency and call counts are measurements of the run. **Tokens** appear only when the CLIs reported them
(exact input tokens), never estimated; no money or quota figure is produced.

## User feedback

`/feedback` in the chat or `ai-orchestra eval feedback <consultation-id> --rating inaccurate --note "..."`
stores a note against a consultation in a local owner-only file (secrets in the note are redacted). It is
inspectable (`--list`), deletable (`--delete <id>`, `--clear`), and does nothing else: no preference, no
retraining, no change of routing or provider ranking. Using feedback to tune anything belongs to a later,
separately validated step, with enough data.

## Correcting the checks of a dataset: `eval rescore`

A check can be wrong (matching the question's own words, missing a paraphrase). `ai-orchestra eval rescore <run>
--dataset <name>` applies a dataset's checks again to the answers a run already holds, **without calling any
provider**, and saves a new run that says in its notes which run it was re-scored from and the two dataset hashes;
the original stays. A check changed after seeing the answers is a post-hoc change: it must be disclosed (the dataset
version and its provenance notes say what changed, and the first live run is described in
[evaluation-benchmarks.md](evaluation-benchmarks.md#3a-the-first-live-run-2026-10-12-authorized-six-cases-three-arms-codex-only))
and the checks are then frozen for that version.

## Live evaluation (spends quota)

`eval run <dataset> --live --arms ... [--cases ...]` first **prints a plan**: every case and arm, the calls
each will make typically and at most (routed exactly as a real run would, calling nothing), and the limits. It
then stops. Nothing runs without `--yes-run-live`, and a run whose worst case exceeds `--max-total-calls`
(default 150) is refused before any call. Each consultation runs alone with memory off so arms are comparable,
is bounded by `--max-calls` (default 10 per consultation) and an optional `--timeout`, nothing is retried, a
failure is recorded as a failed arm, and Ctrl-C stops the run and keeps what finished, marked partial. Supplied
sources are put in the prompt and checked as supplied evidence. Results are stored with provenance `live`.
Consultations of a live evaluation are not added to your saved history.

The MCP server can read a report (`get_eval_report`) and store the user's feedback (`submit_feedback`, under
the same two permissions as memory writes); it cannot start an evaluation, and there is no tool that does.

## Status of the optional browser UI

Built in Milestone 9 (`ai-orchestra web`, see [web-ui-architecture.md](web-ui-architecture.md)) with the
constraints this section asked for: localhost only, no provider request on page load, state-changing
endpoints defended (token, Origin, Host, JSON only), and no credentials or session files exposed. The Reports
page reads evaluation runs; evaluations are still started from the terminal.

## Limitations

* Deterministic checks are keyword-based and see only what the dataset anticipated.
* Objective cases here test reasoning, instruction-following and use of *supplied* evidence; they do not test
  how accurately an assistant researches the real web. That needs human-judged cases or externally verified
  facts, and a researcher willing to check them.
* Blind judging is only as good as the judges; one judge is one opinion, and the report says how many
  judgments there are.
* Live runs are stochastic: a single run per arm cannot separate a real difference from run-to-run variation.
  Repeat runs before drawing a conclusion.
* The starter dataset's recorded responses are fixtures; numbers computed from them are numbers about the
  fixtures.
