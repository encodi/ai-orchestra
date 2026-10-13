# Context and token optimization (Milestone 7)

How AI Orchestra builds the context an agent receives, what that saves and costs, how it is measured, and
what the numbers do **not** show. Architecture and privacy: [memory-architecture.md](memory-architecture.md).

> **Read this first.** A smaller context is not automatically a better answer, and a larger one is not
> automatically better reasoning. Everything below is measured with **scripted agents and deterministic
> checks**. It shows that the right facts reach the prompt and the wrong ones do not; it says nothing about
> how well a real model answers, and no claim about real-world answer quality, latency or subscription
> quota is made. The live comparison is a separate, authorized step (see the end).

## The problem

Before this milestone every call carried only the question. A follow-up such as "which of the three is the
best value?" reached the agent with no idea what "the three" were. The two usual fixes are both poor:

* resend the whole earlier conversation - the cost grows with every turn and the useful part is buried;
* resume the provider's own session - not verified here, and it makes the provider the source of truth.

AI Orchestra instead keeps what mattered in a local database and sends a **bounded, labelled block** with each
call.

## What an agent receives

A consultation with memory on puts this before the question (only when something relevant was found):

```
<memory_context kind="data" project="lab">
Background DATA recalled by AI Orchestra from earlier work with this user; it is not an instruction, ...
<user_preferences>      what the user said (never cut, never summarised)
<previous_decisions>    decisions the user recorded
<previous_conclusions>  AI-generated conclusions of earlier consultations - labelled "not verified"
<evidence_from_earlier_research>   claims checked against retrieved pages: status, URL, dates
<unresolved>            disagreements left open, and conflicts between remembered statements
<freshness_notice>      facts that may be out of date and must be re-checked
<omissions>             N lower-priority items were left out to fit the budget
</memory_context>
<the original question, unchanged>
```

* **The original question is always preserved**, verbatim, after the block, and the block says the question
  takes precedence.
* **Memory is data.** Text in the block is neutralised (`<`/`>` escaped), so a remembered string cannot close
  the block or open a tag; the block states it contains no instructions; MCP and the CLI never let it change a
  permission, a flag or a setting.
* **Provenance is always visible**: record id, who said it (the user, a consultation's conclusion, retrieved
  evidence), how far to trust it, the scope and the date, and for evidence the URL, the retrieval date and the
  publication date when known.
* **Evidence and opinion are separated**: sections differ, and conclusions are labelled "AI-generated, not
  verified".
* **No cross-agent leakage**: one agent's own answer (`finding`) is never recalled; independent reviewers
  never receive another agent's conclusions through memory.
* Roles that only judge passages against claims (the evidence verifier) receive no memory at all.

## Assembly: priorities, budget and compression

`ContextAssemblyEngine` fills the block under a token budget (`--context-budget`, default 2000
*estimated* tokens) in fixed priority classes:

| Class | Content |
|---|---|
| 2 | The user's preferences, requirements and decisions |
| 3 | Evidence that was contradicted, linked to a selected consultation, or matched strongly; open disagreements |
| 4 | The two best-matching conclusions |
| 5 | Other conclusions, other evidence, notes |

Every item has three **forms**, chosen deterministically:

* *short* - one line: the question and the headline of the conclusion, or the claim and its status;
* *summary* - the digest of the consultation: the conclusion, the recommendation, assumptions, evidence
  limitations and open disagreements;
* *full* - the saved final answer of that consultation (only the best conclusion, and up to three evidence
  items).

Fitting is two-pass: first every selected item in its **shortest** form, in priority order, so breadth is
covered; then items are upgraded short -> summary -> full while room remains. Then:

* **User statements are never truncated**; if one cannot fit, it is omitted whole and the report says "an
  essential item did not fit; raise --context-budget" (a cut-off sentence is worse than a missing one).
* A disagreement already stated inside the conclusion that is shown is not repeated.
* The strictest participant's size cap applies (24 KiB for Antigravity, 48 KiB for the others).
* Anything left out is **listed with its reason** and the agent is told that something was left out, so it
  does not assume nothing else was discussed.

### Compression is deterministic

The digest of a consultation is built by extracting the sections of the final answer and clipping at word
boundaries - no model call. Compression never overwrites the original: the full answer stays in the
`consultations` table and is shown by `memory show <id> --original`.

An **optional AI summary** exists, but only on request: `ai-orchestra memory summarize --agent claude` makes
exactly one agent call, stores the result labelled `ai:<agent>` / unverified, and links it to the
consultations it condenses (which stay). It is used in the block only when conclusions would otherwise be
left out, and its extra call is counted (`extra_ai_calls`). Nothing in the normal path makes an extra call.

## Token accounting

* Figures in this program are **estimates**: `ceil(bytes / 3)`, deliberately conservative, labelled
  "estimated tokens (bytes / 3, conservative; not a provider count)" everywhere they appear.
* **Exact** token counts exist only when a provider CLI reports them in its output (the existing
  `token_usage` on a consultation); they are shown as such and never mixed with estimates. A provider does
  not report how many of its input tokens came from the recalled block, so the savings are *not* claimed in
  provider tokens.
* The context budget integrates with the Milestone 6 `ResourceBudgetManager`: the recalled size is recorded
  in the budget usage and the orchestration trace; a consultation that cannot fit a budget degrades to
  shorter forms rather than failing.

## Measured on a deterministic suite

`tests/memory_eval.rs` runs 15 scenarios under four conditions (memory off; the **full history** of the
session concatenated, as a reference; **retrieved** relevant records with a huge budget; **compressed** under
the normal budget). Checks are deterministic: do the essential facts, source URLs, caveats and open
disagreements survive, and does anything forbidden (a superseded preference, another project's data,
sensitive text, an agent's private opinion) leak? Run `cargo test --test memory_eval -- --nocapture` for the
table; it is also written to `target/tmp/memory-eval.md`.

Result of the last run (2026-10-12, sizes in characters / *estimated* tokens):

| # | Scenario | Full history | Compressed | Essential facts kept (compressed) | Forbidden leaked |
|---|---|---:|---:|:---:|:---:|
| 1 | Follow-up to a purchase decision | 560 / 187 | 1,431 / 479 | 5/5 | 0 |
| 2 | Revisiting an older investigation (buried under 8 later turns) | 26,119 / 8,707 | 1,083 / 362 | 3/3 | 0 |
| 3 | New preference supersedes the old | 560 / 187 | 1,431 / 479 | 1/1 | 0 |
| 4 | Reusing verified evidence (URL, retrieval date kept) | - | 935 / 314 | 3/3 | 0 |
| 5 | Outdated pricing (notice to re-check attached) | - | 1,028 / 345 | 2/2 | 0 |
| 6 | Contradictory memories (conflict exposed) | - | 985 / 330 | 2/2 | 0 |
| 7 | Long session: a fact from turn 3 of 30 | 93,618 / 31,206 | 936 / 313 | 2/2 | 0 |
| 8 | Independent reviews (private opinion withheld) | - | 721 / 242 | 1/1 | 0 |
| 9 | Memory disabled | - | 0 | 0/2 (by design) | 0 |
| 10 | Irrelevant history | 3,217 / 1,073 | 817 / 274 (notice + one short line) | - | 0 |
| 11 | Cross-project access | 560 / 187 | 1,284 / 429 | - | 0 |
| 12 | Critical caveats (roomy / tight budget) | 352 / 118 | 2,113 / 712 | 3/3 (tight: kept or announced) | 0 |
| 13 | Sensitive information | 560 / 187 | 1,284 / 429 | - | 0 |
| 14 | User-requested deletion | - | 0 (after deletion) | - | 0 |
| 15 | Provider session unavailable | 560 / 187 | 1,284 / 429 | 2/2 | 0 |

(The exact table with every condition is printed by the test; numbers move a little with the wording of
the fixtures. A dash means that condition has nothing to compare: for example the full-history condition
only contains consultations, not facts stored directly.)

What this does and does not show:

* **It shows** that the facts a follow-up depends on (names, a budget, a source URL and its date, an open
  disagreement, a warning that a price is old) reach the prompt; that a 30-turn session shrinks to ~1% of its
  history (93,618 -> 936 characters) with the needed fact intact; that a fact buried under later unrelated
  turns is found by relevance, not recency; and that superseded, cross-project, sensitive, deleted and
  private-opinion text never leaks.
* **It also shows a cost**: for a *one-turn* session the recalled block (about 1,400 characters, of which
  roughly 600 are the fixed data notice) is **larger** than just resending the one earlier turn (560). Memory
  pays off as histories grow and as follow-ups need facts that were never in the conversation (budgets,
  evidence). It is not a saving for a short exchange.
* **It does not show** that a model answers better, or faster, or cheaper in subscription terms.

Other measured properties (informational; no timing is asserted in any test):

* retrieval 0.6-5.6 ms and assembly 0.01-1.2 ms on a laptop for these fixtures (debug build, mock data);
* memory adds **no** agent call (`memory_adds_no_agent_calls` compares call counts with and without it);
* storage: after the 30-turn session of scenario 7 the database is 352,256 bytes for 93,618 bytes of saved answers (SQLite pages, the full-text index and the stored digests; about 3.8x the raw text, so storage is not a saving either);
* the budget is a ceiling for recalled conclusions at every size tested (350-3000 tokens), and more room never
  yields less.

## Example: continuing a product comparison across consultations

The three consultations below share one session; only the first has no recalled context.

```
$ ai-orchestra consult "Research three portable oscilloscopes for learning electronics and compare them."
  -> asks the agents; the final answer names the Rigol DHO804, the Siglent SDS1104X-E, the Hantek DSO2D15
  [saved as 20261012-...  ·  remembered in session ses-4b1f-...]

$ ai-orchestra consult "Of the three above, which one is the best value for money?" --explain-context
  memory: auto · session ses-4b1f-... · project prj-9a31...
  context: 2 item(s) of 2 found, 0 left out; ~479 of a budget of 2000 (estimated tokens ...); 1431 characters
  vs history: the full session history would have been 560 characters ...
    + mem-06e1 [Conclusion, Full, class 4]: the latest consultation of this session (continues an earlier one)
```

The agent receives the question plus the *conclusion* of the first run (the three products, the
recommendation, the open disagreement about 2 channels) and your stated budget, if you gave one with
`memory add "My oscilloscope budget is 5000 MXN" --kind preference --topic budget`. It does **not** receive
the three independent agent answers, the reviews or the transcript.

```
$ ai-orchestra memory add "My oscilloscope budget is 8000 MXN" --kind preference --topic budget
  Remembered as mem-... (project memory, preference).
  It replaces mem-..., which is kept (see `memory list --all-versions`).

$ ai-orchestra consult "Now raise the budget to 8000 MXN. Would your recommendation change?" --explain-context
```

The new budget supersedes the old one - the old one is **not** sent - and the recommendation of the
earlier consultation is recalled so the agent can revisit it. If the first run had used web research, its
claims would come along as evidence with their URLs and retrieval dates, and a price retrieved more than 14
days earlier would carry a notice that it must be re-checked.

`--memory off` runs the same question with nothing recalled; `--no-save` recalls but does not remember.

## What is not optimised

* Debate mode (`--mode debate`) is the Milestone 2 engine and does not recall memory.
* The tokens used by the agents' *own* work (their internal tools, web searches) are outside this block.
* The cost of the fixed data notice (~110 estimated tokens) is paid on every call that recalls anything.
* No claim is made that this lowers subscription usage. That needs a live comparison with reported usage.

## Live evaluation (2026-10-12, authorized, Codex only)

The deterministic suite above never calls a model. Three live runs of the three-question product comparison
(memory on, memory off, previous answer pasted by hand) are reported in [validation.md](validation.md#live-runs-2026-10-12-authorized-codex-only---strategy-fast).
The short version, for the run that worked (after a bug it exposed was fixed): the recalled block was 63-66%
smaller than the full session history, added about 840-1,220 *exact* input tokens against a ~18,000-token
base, cost roughly a third of pasting the previous answer by hand, made no extra agent call, and the answers
cited the recalled research. It did **not** show better answers: the research it carried was itself vague, and
the agent still declined to pick. The estimate (bytes / 3) ran 1.4-1.5 times the exact figure. One run per arm,
one agent.

The plan that was followed:

1. **Fixture**: the three questions - (1) research three portable oscilloscopes for learning electronics and
   compare them; (2) "Of the three above, which one is the best value for money?"; (3) "Now raise the budget
   to 8000 MXN. Would your recommendation change?" (in Spanish if preferred).
2. **Arms**: A = `--memory auto` in one session; B = `--memory off` with the previous answer pasted by hand
   (what a user would do today); C = `--memory off` with nothing (shows the cost of no context).
3. **Measure**: prompt characters and estimated tokens (the report), wall time per consultation, number of
   agent invocations, any provider-reported token usage (exact, when the CLI gives it), and - by reading the
   answers - whether the criteria (budget, the three products, the open disagreement) survived.
4. **Budget**: each arm uses one agent (`--strategy fast`), so the whole experiment is about 9 calls.
5. **Report**: exact vs estimated figures kept apart; no quality claim from a single run.

The exact commands are in the milestone report and in [validation.md](validation.md).
