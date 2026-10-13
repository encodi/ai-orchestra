# Research and fact checking (Milestone 5, v0.5)

AI Orchestra can back an answer with real sources instead of model agreement.

> Three assistants agreeing is **not** verification. They can share the same mistake. In research
> mode the program retrieves pages itself, cuts passages out of them, has a model judge *only those
> passages*, computes each claim's status from that, and writes the citations and the list of
> sources from its own records. A model never supplies a URL that is shown as a source, a date, or a
> quotation.

Everything is automatic by default and every choice can be made by hand.

## What it does, in order

```
question
  │  A. plan + discover   an agent with web search finds candidate pages        (1 call, optional)
  │                       your --url pages are added                            (no call)
  │  B. retrieve          THIS PROGRAM fetches the pages (safe fetcher)         (no agent)
  │  C. independent       every participant answers from the retrieved evidence (parallel)
  │     answers           and lists the claims it relies on
  │  D. verify claims     passages are found in the pages; other agents judge   (0-2 calls)
  │                       only those passages; status is computed
  │  E. cross-review      1-2 bounded rounds; reviewers see the claim checks    (as in consult)
  │  F. synthesis         the coordinator answers with [n] citations
  ▼
final answer  +  generated claim table  +  generated Sources list  +  limitations
```

* A URL a model reports is a **candidate**. It becomes a source only when the fetcher retrieves it.
  A URL a model wrote from memory that leads nowhere is listed under *Consulted but not retrieved
  (these were NOT checked)*.
* Page text reaches an agent only inside `<source>` tags, marked as data, with `<` and `>`
  neutralised so a page cannot close a tag or forge another source.
* The analysts do not browse: they receive the evidence. Only the planning call has web tools.

## Research capabilities of the installed CLIs

This table is the result of looking at the installed CLIs (Claude Code 2.1.296, Codex 0.160.1,
Antigravity `agy` 1.3.3) with their own `--help`, binaries and changelog, and at the official
documentation. **No search was run against any subscription to produce it.** Print it any time:

```sh
ai-orchestra capabilities           # or --output json
```

| | Web search | Web fetch | Source URLs in the output | Enabled in non-interactive mode by |
|---|---|---|---|---|
| **Claude Code** | documented | documented | unverified | `--tools Read,Grep,Glob,WebSearch,WebFetch --allowedTools WebSearch,WebFetch`, output `stream-json --verbose` |
| **Codex CLI** | documented | unverified | unverified | `codex --search exec …` (global flag) |
| **Antigravity CLI** | unsupported (not enabled) | unsupported | unverified | print mode soft-denies tools that need approval |

What each entry rests on:

* **Claude Code.** `WebSearch` and `WebFetch` are built-in tools named in the installed binary
  (alongside Read, Grep, Glob, Bash, Edit, Write) and in the documentation of the headless mode
  (`-p`, `--allowedTools`, `--output-format stream-json`). `--help` says `--restricted` removes
  `WebFetch`, which confirms the tool exists. *Not observed:* a search running here (whether the
  subscription allows it), and where the source URLs appear in the stream-json events. The adapter
  therefore harvests every URL from the result of a `WebSearch`/`WebFetch` call, whatever its shape.
  `WebFetch` returns a *model-written answer about a page*, so it is never used as evidence.
* **Codex.** `codex --help` documents `--search` ("the native Responses `web_search` tool is
  available to the model, no per-call approval"). Checked locally: `codex --search exec --help`
  is accepted and `codex exec --search` is rejected, so the flag goes before `exec`. The binary lists
  the modes `disabled`, `cached`, `indexed` and `live` for the `web_search` config key and a
  `web_search` item type in its events. *Not observed:* whether `exec --json` emits those items and
  whether they carry source URLs; the adapter harvests URLs from any `web_search` item.
* **Antigravity.** The CLI has `search_web` and `read_url_content` tools (named in its binary and
  changelog). In print mode "tools that need approval are denied unless a matching allow rule
  exists" (`permissions.allow` in `~/.gemini/antigravity-cli/settings.json`); the official headless
  page documents the `action(target)` rule form for shell and file actions but not for these web
  tools, and says tool steps carry `tool_info` (name, parameters, output) in `stream-json`.
  `--dangerously-skip-permissions` is **never** used, so agy does not search; it takes part as an
  analyst of retrieved evidence. (From the earlier MCP work: headless agy needs an allow rule even for
  MCP tools.)

Because the shapes are unverified, a provider's search is treated conservatively: its URLs are
candidates, each is fetched and checked independently, and the report says when the CLI did not
expose its search results (the URLs were only written in its reply).

## Source discovery

A small interface (`SourceDiscovery`) with the method recorded on every source:

| Method | Recorded as | Trust |
|---|---|---|
| URLs you pass with `--url` | `UserProvided` | fetched like any other |
| URLs in the output of a provider's own search tool | `ProviderToolReported` | candidate; must be fetched |
| URLs a provider wrote in its answer | `ProviderTextReported` | weakest; a model can invent one |

No search engine is scraped and no search API or key is needed. If nothing can search and no `--url`
is given, the result says so ("live web discovery is unavailable") and the answer carries the
limitation; `--web on` fails instead of degrading.

## The fetcher and its safety rules

`SourceFetcher` retrieves public HTML and plain-text pages. Each rule is tested.

* **HTTPS only** (plain `http://` only with `research.allow_http = true`); standard ports 80/443;
  no credentials in URLs; URLs longer than 2048 bytes refused.
* **SSRF.** Refused, for the URL *and every redirect hop and every DNS answer*: loopback, private
  (10/8, 172.16/12, 192.168/16), link-local (169.254/16, which includes cloud metadata), CGNAT,
  multicast, reserved, documentation and benchmarking ranges; IPv6 loopback, unique-local,
  link-local, site-local, multicast, and IPv4 hidden inside IPv4-mapped, NAT64 and 6to4 addresses;
  internal host names (`localhost`, `*.local`, `*.internal`, names without a dot, metadata hosts).
  The URL parser normalises disguised spellings (`2130706433`, `0x7f.1`, `[::ffff:7f00:1]`).
* **DNS rebinding.** Names are resolved by this program's own resolver, which refuses a name if *any*
  of its addresses is non-public, and the HTTP client connects to exactly the addresses returned
  there; connections are not reused, so every request resolves and is checked again. Proxies from the
  environment are ignored (a proxy would resolve names itself).
* **Redirects** are followed by hand, at most 5, each hop re-validated; a tiny page whose only job is
  `<meta http-equiv="refresh">` is followed as one more hop (a redirect done in JavaScript is not).
* **Bounds.** 15 s per page, a streaming cap of 2 MiB on the body (also against a lying
  `Content-Length`, and against decompression bombs), 200 KB of extracted text, 4 pages at a time.
* **Content.** HTML, XHTML and plain text only; PDFs and everything else are reported as
  unsupported. Scripts, styles, forms, navigation, hidden elements and common wiki/CMS furniture are
  dropped; nothing is executed. Only `ld+json` data is read as data.
* **robots.txt** is honored (RFC 9309 subset: groups, longest match, `*`, `$`); if it cannot be read
  because of a server error or timeout, the site is treated as off limits and the reason says so.
* **Privacy.** A fetch is a plain GET with a fixed `User-Agent`; nothing about you (question, files,
  history, environment) is ever sent to a site. Provider-reported URLs over 512 bytes are ignored.
* **No paywall or login bypass.** A page that cannot be retrieved is *inaccessible*, never
  *verified*.

None of the relaxations used by the test suite (a local test server) can be switched on from the
config file or the command line.

## What a source record contains

URL as given, final URL after redirects, canonical URL if the page states one, title, domain, author,
**published**, **updated** and **retrieved** dates (three different things, never substituted for each
other; a date the page does not state stays absent), the way it was discovered, HTTP status, content
type, SHA-256 of the retrieved bytes, status (`retrieved`, `inaccessible`, `blocked`, `unsupported`,
`not_attempted`), the error if any, the number of characters of extracted text, a 500-character
preview, the kind of source and why, an origin group, short excerpts used as evidence, and the
limitations observed (very little text, no date stated, text cut). Whole pages are not stored.

## Claims and verification

* The agents end their answers with a machine-readable block of the claims they rely on (at most 8
  each). Near-identical claims from several agents are merged and ranked; being made by more agents
  raises the priority of checking a claim, **not** its status. At most 4 / 8 / 12 claims are checked
  (quick / standard / deep).
* For each claim the program finds the passages of the retrieved pages that bear on it (keyword and
  number overlap, deterministic). The passages are slices of the program's own extraction, so a model
  cannot invent a quotation.
* An agent that did not make the claim, where possible, judges **only those passages**: `supports`,
  `contradicts`, `unrelated` or `unclear`. A judgement that names a claim or passage that was not
  offered is dropped. The judgement is semantic and **model-dependent**, and the output says so. At
  deep depth two agents judge, and a passage they disagree on counts as `unclear`.
* The status is **computed**:

| Status | When |
|---|---|
| `SUPPORTED` | at least one retrieved passage supports it and none contradicts it |
| `CONTRADICTED` | passages contradict it and none supports it |
| `MIXED_EVIDENCE` | some support it and others contradict it |
| `INSUFFICIENT_EVIDENCE` | verification ran and no retrieved passage supports or contradicts it |
| `NOT_CHECKED` | verification did not run (no sources, or quick depth) |

A page that exists, or answered HTTP 200, is not support. A summary written by a model is not
evidence. Several models repeating a claim is not support.

### Source quality, independence and freshness (qualitative)

There are no numeric scores or probabilities. Each source gets a *kind* from its domain and path
(government or regulatory, scholarly, preprint, reference work, documentation page, news,
community, unclassified), always described as a heuristic. Whether a kind suits a claim depends on
the claim: a manufacturer page is strong for an **advertised specification** and weak for
**real-world performance**; a forum is strong for **user experience** and weak for a **scientific
finding**; a preprint is moderate for a finding because it is not peer reviewed. Each evidence link
records that suitability and the reason.

Pages that repeat one origin are **one** independent source. Two pages are one origin when their
texts are near-copies, when one states the other as canonical, or when the *passages used as
evidence* are near-copies of each other (a news site repeating the maker's specification paragraph).
A claim supported by three copies of one paragraph is reported as supported by 1 independent source.

With `--freshness-days N`, a source whose newest stated date (updated, else published) is older than N
days is flagged stale; a page with no date is "unknown", never assumed current.

## Citations

The synthesis cites `[n]`. Afterwards the program checks every marker: a number that names no
recorded source is removed (`[unsupported citation removed]`), a marker naming a source that could not
be retrieved is removed, and a note says so. The **Sources** section is generated from the records:
`[n] Title — <full URL> (published …, updated …, retrieved …)`, using Markdown autolinks so the full
URL is visible in a terminal and clickable in Markdown. Sources that were consulted but not retrieved
are listed separately as *not checked*. The JSON output has machine-readable citation metadata (id,
URL, dates, status, whether the answer cites it, which claims it supports or contradicts).

## Using it

```sh
ai-orchestra consult "What changed recently in quantum computing?" --mode research --depth deep
ai-orchestra consult "Compare two digital oscilloscopes" --mode review --web on
ai-orchestra consult "Summarize and verify this study" --url https://example.org/study
ai-orchestra consult "Which microscope should I buy?" --mode decision --depth standard --coordinator claude
ai-orchestra consult "The Model X has 100 MHz of bandwidth" --verify --url https://maker.example/spec
ai-orchestra consult "Explain this topic" --mode learn --web on --rounds 2
ai-orchestra consult "..." --plan            # shows the research decisions and the call budget; calls nothing
ai-orchestra inspect https://example.org/a   # fetch pages like research does; no agent, no quota
ai-orchestra capabilities                    # the matrix above
```

| Flag | Meaning |
|---|---|
| `--web auto\|on\|off` | `auto` (default): research only when the question needs outside facts (recent events, prices, versions, studies, specifications, "is it true", claims, a product review) or `--url` is given; a basic educational question never browses. `on`: fail with a clear message if nothing can search and no `--url` was given. `off`: no network use at all; the answer says nothing was checked externally. |
| `--depth auto\|quick\|standard\|deep` | see below; `auto` looks at the question |
| `--max-sources N` | 1–20 (default per depth): how many retrieved pages are used as evidence. More candidates (up to 24) are fetched, then ranked, and the rest are listed as `not_selected` |
| `--url HTTPS-URL` | repeatable, at most 20; fetched by this program; contradicts `--web off` |
| `--freshness-days N` | 1–3650 |
| `--verify` | treat the text as one claim to verify instead of a question |
| all of M4 | `--mode`, `--coordinator`, `--agents`, `--rounds`, `--timeout`, `--output`, `--length` … |

| Depth | Sources | Agents | Review rounds | Claims checked | Verification calls |
|---|---|---|---|---|---|
| quick | 3 | 2 | 0 (answers go straight to the synthesis) | 4 (passages found, not judged: `NOT_CHECKED`) | 0 |
| standard | 6 | all usable | 1 | 8 | 1 |
| deep | 12 | all usable | 2 | 12 | 2 (different agents) |

**Which sources are kept.** Search tools list their results in no useful order (the first live Codex
run listed the newest Rust release 7th). So every candidate is fetched (at most 24), and then the
`--max-sources` best are kept as evidence, ranked by: pages you supplied with `--url` first, overlap
between the page text and the question, a site whose name contains a word of the question, the kind of
source (community pages rank lower), and, only when the question asks for the latest/current/new
information, recency. A date in the URL (`/2026/10/01/`) is used for that ranking only: it is never
shown as the publication date. Pages that were fetched but not kept are listed under "Retrieved but not
used as evidence" with status `not_selected`, are never cited and never count as checked, and the report
carries a limitation saying how many were set aside and why.

**Strategies (Milestone 6).** `--strategy` chooses the research depth only when you did not: `fast`
and `balanced` keep the evidence check (they upgrade `quick`, which finds passages but never judges
them, to `standard`; the router's `deep` is kept), and `thorough` uses `deep`. `fast` costs a planning
call, one answer and one judge (3 calls) instead of the fixed workflow's 8 or more. An explicit
`--depth`, `--max-sources`, `--url`, `--freshness-days` or `--web` is never changed. A call limit
that cannot hold the research you required is rejected before anything runs; when the research was
only automatic, it may be lowered or dropped to fit, and the answer says so. See
[`smart-orchestration.md`](smart-orchestration.md).

Manual `--agents`, `--rounds`, `--max-sources` always win over what the depth would choose. Extra calls
research adds to a consultation: one planning call (when an agent searches) plus the verification
calls. `--plan` prints the total.

Exit code 5 (partial) is also used when research was part of the task and nothing could be retrieved,
or when a research step failed.

## MCP tools

`research` (a consultation with the web on by default), `verify_claim` (one claim; only the verification
is done: no answer panel), `inspect_sources` (fetch pages, no agent), `get_research_report` (the
saved sources, claims, citations and limitations), and `consult` with the same research options. They
reuse the same Rust code, run as tasks when they outlast the host's request timeout, never call the
host agent, and treat all page text as untrusted data. See [`mcp.md`](mcp.md).

## Limits and honest caveats

* **Correlated errors remain.** Different assistants can be wrong in the same way; a judge can
  misread a passage. The output labels the judgement model-dependent. Retrieval makes errors
  *checkable*; it does not make them impossible.
* **Search through the provider CLIs was run once (2026-10-11).** Claude Code and Codex both ran web
  search in non-interactive mode and exposed the URLs in their output (recorded as
  `provider_tool_reported`); Antigravity is unsupported. That run showed that taking the first
  candidates gives stale answers, which is why sources are now ranked (see above); the ranking itself
  was re-run live: Codex then answered with Rust 1.99.0; Claude's search found only stale pages and the
  report said so. See [`validation.md`](validation.md).
* **Pages that need JavaScript, logins, cookies or payment** are not read; a page that redirects with
  JavaScript is reported as a page with very little text. PDFs are not read.
* **Passage retrieval is lexical.** A claim phrased very differently from the page may find no
  passage and be `INSUFFICIENT_EVIDENCE` even though the page covers it.
* **The kind of source is a guess from the domain.** It is shown as such and is not a score.
* **Independence detection is textual.** Two pages that restate one origin in different words look
  independent.
* **A consultation costs more with research**: up to the M4 call count plus 1 to 3 calls, and the
  prompts are larger (evidence is capped at 28 KiB so the largest prompt stays under agy's 128 KiB
  argument limit).

## Troubleshooting

| Symptom | Cause / action |
|---|---|
| `--web on needs a way to find or fetch sources` | no participating agent can search (see `ai-orchestra capabilities`) and no `--url`; add `--url`, include claude or codex, or use `--web off` |
| source `blocked`: "a loopback address", "cloud metadata" | the safety rules refused it; there is no setting to relax them |
| source `blocked`: "blocked by the site's robots.txt" / "robots.txt could not be read" | the site forbids or may forbid fetching; use another source |
| source `unsupported` (application/pdf) | only HTML and plain text are read |
| "the CLI of X did not expose its search results" | the search agent only wrote URLs in its reply; they were fetched and checked, but their origin is the weakest kind |
| every claim `INSUFFICIENT_EVIDENCE` | no retrieved passage bears on the claims; try `--max-sources`, more specific `--url`s, or `--depth deep` |
| claims `NOT_CHECKED` | quick depth, or no sources retrieved |
