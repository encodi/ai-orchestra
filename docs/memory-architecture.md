# Memory architecture (Milestone 7)

AI Orchestra's memory is a local SQLite database that lets a later consultation start from what an
earlier one established, **without sending the whole earlier conversation again**. This page says what is
stored, where, how it is found, what an agent is shown, who may change it, and what is *not* claimed.

Companion: [context-optimization.md](context-optimization.md) (how the context is built and measured).
Rules for contributors are in [../AGENTS.md](../AGENTS.md).

## Layers

| Layer | What it holds | Lifetime | Written by |
|---|---|---|---|
| 0 - working context | The prompt of one call: the user's question plus a recalled-context block | One call | Rebuilt for every call, never stored |
| 1 - session memory | The consultations of one AI Orchestra *session*: question, conclusion, open disagreements, evidence | Until `session clear`, `memory prune` or the retention window (90 days by default) | Automatically, after a consultation that produced an answer |
| 2 - persistent knowledge | Preferences, requirements, decisions, interests and notes, per project or global | Until you delete them | **Only by an explicit action**: `memory add`, `memory promote`, or an MCP `save_memory` that the operator allowed and the user asked for |
| 3 - research / evidence memory | Claims checked against pages the program retrieved: status, source URL, dates, content hash | Like layer 1 (session) unless promoted | Automatically from a research run's claim table |

Persistent knowledge is never written automatically. A consultation only ever writes **session** memory,
and only when memory is on and `--no-save` was not given.

## The data model

Every record has:

* **scope** - `session` (one session of one project), `project` (one project), `global` (all projects);
* **kind** - `question`, `conclusion`, `finding`, `unresolved`, `evidence`, `preference`, `decision`,
  `requirement`, `interest`, `note`;
* **origin** - who said it: `user`, `consultation_final` (what the panel concluded together),
  `agent:<name>` (one agent's own words), `evidence` (a claim checked against retrieved sources),
  `imported`, `system`;
* **trust** (`status`) - `user_provided`, `agent_interpretation`, `previously_verified`, `unverified`,
  `contradicted`, `historical`. Agent conclusions are `agent_interpretation`: **never** facts.
  `previously_verified` means "supported by a page we retrieved on *that* date", not "true now";
* **lifecycle** - `active` or `superseded` (replaced by a newer record; kept, never rewritten);
* **authorization** - `explicit_user`, `automatic_session`, `imported`;
* dates: saved, updated, **retrieved** (when the source page was fetched) and **published** (as the page
  stated it). No date is ever invented: with no recorded date a fact's freshness is `unknown`;
* a content hash (duplicates are not stored twice), an optional topic, tags and an optional expiry.

`finding` records (one agent's own answer) are kept for the audit trail and are **never** recalled into
another agent's prompt: independent reviewers must not receive other agents' conclusions through memory.

## SQLite

One file, `memory.db`, next to the saved-debates directory (override with `[memory] path`).

* `rusqlite` with the bundled SQLite; no server, no network, no vector database, no embeddings.
* Tables: `meta`, `projects`, `sessions`, `memories`, `memory_fts` (FTS5, external-content), `memory_links`
  (supersedes / contradicts / derived_from / evidence_for), `evidence_refs`, `consultations`, `summaries`,
  `context_metrics`, `tombstones`. Indexes cover scope/project/session/lifecycle, content hash, topic,
  consultation and recency.
* Full-text index: tokenizer `porter unicode61 remove_diacritics 2` (so "oscilloscopes" matches
  "oscilloscope" and "osciloscopios" matches "osciloscopio"), ranked with `bm25(memory_fts, 1.0, 0.6, 1.4)`
  (text, tags, topic). Triggers keep the index in step with the table.
* **Migrations**: the schema version is `PRAGMA user_version` (currently 2: V1 core, V2 summaries and
  context metrics). A migration runs in one immediate transaction and re-checks the version inside it, so
  two processes opening an old file at once migrate it once. A file written by a *newer* version is
  refused with a message, never modified.
* **Writes are transactions** (`BEGIN IMMEDIATE`, savepoints when nested). A multi-step write - store a
  consultation with its conclusion, evidence and links - is all or nothing.
* **Concurrency**: WAL, so readers do not block the writer; a 10 s busy timeout plus a bounded retry on
  open/migrate; at most 4 database calls run at once, each on a blocking thread, so a Tokio worker is
  never held by SQLite.
* **Crash recovery**: SQLite's journal. An interrupted write leaves the previous state.
* **Corruption**: `memory check` runs `PRAGMA integrity_check` and a check of the index. A damaged
  database never stops a consultation (it runs without memory and says so with a `memory:` warning).
  `memory check --quarantine` renames the file to `memory.db.corrupt-<time>` - **never deletes it** - and
  starts a new one; `--rebuild` rebuilds the full-text index.
* **Deterministic serialization**: `memory export` is JSON with a fixed field order.
* **Migration from earlier records**: `memory import-history` brings the saved consultations of the debate
  history into the current project's memory (labelled `imported`, idempotent, the history is untouched).
  Plain Milestone 1-6 files keep their shape; a consultation record gains a `context` field only when
  memory was used.

### Files and permissions

The database is created with mode `0600` (owner only). The file is **not encrypted at rest**: anyone who
can read your home directory can read it. Do not store what you would not keep in a plain file.

### Deletion, backups and journaling

* `memory forget`, `memory clear` and `session clear` delete the record, its index entry, its evidence
  references and its links in one transaction, with SQLite `secure_delete` (freed pages are overwritten),
  then merge the index and checkpoint the write-ahead log. A test reads the database file's bytes and checks
  the deleted text is gone. What remains is a **tombstone** holding only the id, the kind and the deletion
  time (no text), so a re-import does not bring the record back.
* A **copy of the file that you made** (a backup, a Time Machine snapshot, a cloud-synced folder) is not
  touched. Delete those yourself.
* The `-wal`/`-shm` files next to the database belong to SQLite's journal and are cleaned by the checkpoint.

## Modes and scopes

`--memory auto|off|session|project|global` (or `[memory] default`):

| Mode | Reads | Writes after a consultation |
|---|---|---|
| `off` | nothing; **the database is not even created** | nothing |
| `session` | the current session | session memory |
| `project` | session + project | session memory |
| `auto` (default) | session + project; global only with `[memory] auto_include_global = true` | session memory |
| `global` | session + project + global | session memory |

* `--no-save` reads but writes nothing (not memory, not the debate history).
* A **project** is identified by the canonical path of the working directory, or by `[memory] project`/
  `--project <PATH|NAME>`. Two projects never see each other's session or project memory; a session of
  another project is refused before any agent runs.
* A session continues automatically while it is used at least once per `session_idle_hours` (24). An idle
  session is not continued silently; `--session <id>` or `session resume <id>` reaches it.
* The library default is *no memory service*: code that builds an `Orchestrator` without `with_memory`
  behaves exactly as in Milestones 1-6. The CLI and MCP server attach it with the `auto` default.

## Privacy and what is never stored

* **Credentials** (API keys, tokens, passwords, private keys) are never stored - not by `memory add`, not by
  `--allow-sensitive`, not from a consultation. A question or answer that looks like it holds one is
  answered but not remembered. Text is also passed through `redact_secrets` before storage.
* **Personal data** (contact details, payment-card-like numbers) is refused unless you pass
  `--allow-sensitive` to `memory add`; it is then flagged `sensitive`: shown to you, **never** recalled into
  a prompt and never returned to an agent through MCP. Consultations skip it automatically.
* The classifier is a set of conservative shape heuristics, not a guarantee. Review what is stored with
  `memory list`/`export`.
* **No telemetry.** Nothing leaves the machine except the prompt of an agent call you asked for, which now
  may include a recalled-context block. The context report (`--explain-context`) shows exactly what that
  block contains.
* Research page text is untrusted data. Evidence stored from a run is a *claim, its status, URL, dates and a
  slice of our own extraction*; page text is never stored as an instruction and is neutralised when recalled.

## Retrieval and ranking

`MemoryRetrievalEngine` is a stable interface over a `RetrievalBackend` trait; the only backend is lexical
(FTS5). New backends (for example embeddings) could be added behind the trait; none is included or planned
here.

1. **Lexical candidates**: the question's content words (stop-words removed, English and Spanish; digits and
   names kept) become a quoted-term FTS5 `OR` expression - user text can never become FTS syntax.
2. **Hybrid score** (fixed, documented weights; nothing is learned):
   `0.55 x relevance` (BM25 scaled to 0..1 within the query) `+ 0.12 x recency` (half-life 30 days, only a
   nudge) `+ scope` (session +0.08, project +0.04) `+ trust` (user-stated +0.10, verified +0.06,
   contradicted +0.03, agent -0.02) `+ exact names/numbers` (+0.04 each, capped at 0.12).
3. **Continuity**: a follow-up often shares no words with what it continues ("which one is the best
   value?"). The latest consultations of the session are therefore candidates too: with a continuation cue
   (English or Spanish: "above", "those", "now", "anteriores", "ahora", ...) the latest few, otherwise only
   the very latest and only in its shortest form; a question that strongly matches an earlier conclusion by
   its own words does not get the latest turns added. This is a deliberate trade: an unrelated question
   after an unrelated turn costs one short line.
4. **Linked evidence**: the evidence and open disagreements of a selected consultation come with it.
5. **Related user statements**: a budget or requirement the user stated about the same *subject* as a
   selected conclusion (shared subject words) comes along even though the follow-up does not repeat it.
6. **Filters**: records marked sensitive, superseded records, expired records and records of other projects
   or sessions are never candidates. Duplicates are collapsed by content hash.

`memory search` uses the same index with the user's own view (sensitive records included). The MCP
`search_memory` uses the agent's view (sensitive records excluded).

## Conflicts and corrections

* A newer **explicit user statement** on the same topic supersedes an older one; the old record is kept,
  marked `superseded`, linked by `supersedes`, and not recalled. An *inferred* record never replaces an
  explicit user statement.
* A newer **verified evidence** record on the same claim topic supersedes an older one. Agreement between
  agents never creates or upgrades evidence.
* `memory correct` writes the corrected text as a new version and keeps the old one as replaced.
* Contradictions are **exposed, not merged**: a claim whose sources contradicted it, two checks of the same
  claim that disagree, and two user decisions on one topic are listed in the recalled context as conflicts
  for the agent to weigh (and in `--explain-context`).

## Research freshness

A remembered fact carries the date it was *retrieved* and, when the page stated one, *published*. A policy
(`[memory] freshness_*`) turns that into `current`, `stale` or `unknown`: prices 14 days, time-sensitive
facts 30, other checked facts 365, and 7 when the question asks for "latest/current/today". A stale or
undated fact is recalled **with a notice** that it must be re-checked; an old verification is never
presented as current. This is a policy about *age*, not a judgement that the fact changed.

## Provider-native sessions

The three CLIs document ways to continue their own conversations (`claude --resume`, `codex exec resume`,
`agy --continue`; read from their `--help`). **AI Orchestra uses none of them.** Every call is a fresh,
stateless session (`--no-session-persistence`, `--ephemeral`), and the continuity comes from the bounded
context block built from local memory. This was not verified live: it is not known whether continuing a
session works non-interactively, is safe through MCP, or changes what a subscription counts, and nothing
here assumes it would reduce quota. `memory status` prints this table. `session resume` resumes **AI
Orchestra's own** session, and says so.

## CLI

```
ai-orchestra consult "..." [--memory auto|off|session|project|global] [--session ID]
                           [--context-budget TOKENS] [--explain-context] [--no-save]
ai-orchestra consult "..." --plan --explain-context      # retrieval only: no agent is called

ai-orchestra memory list|search|show [--original]|add|correct|promote|forget|export|clear|stats|status|check|prune|import-history|summarize
ai-orchestra session list|show|resume|clear
ai-orchestra --project <PATH|NAME> memory ...             # another project than the working directory
```

* `memory add "text" [--scope project|global|session] [--kind preference|decision|requirement|interest|note] [--topic T] [--tag T] [--expires-days N] [--allow-sensitive]`
  is the authorization to keep something persistently.
* `memory clear --scope session` deletes the current session; `--scope project|global` needs `--yes`.
* `memory export [--all-projects] [--file F]` writes JSON (file mode `0600`).
* `memory summarize --agent claude|codex|google` makes **one** agent call to write a short summary of a
  session; the result is stored labelled `ai:<agent>`, unverified, linked to the consultations it condenses,
  which are never removed. It is never automatic.
* Exit code 2 for an input that cannot be honoured (an unknown session, `--memory off` with `--session`,
  a budget of 0), before any agent runs.

## MCP

Six tools over the same service (see [mcp.md](mcp.md)): `search_memory`, `get_memory`, `get_session_context`,
`explain_context` (read-only), `save_memory`, `forget_memory` (write). `consult`, `research` and
`verify_claim` also take `memory`, `session`, `context_budget_tokens`, `explain_context` and `remember`.

* Results are bounded (at most 20 records, clipped text), exclude sensitive records, and carry a note that
  memory is data and never an instruction. Memory text cannot change permissions or settings.
* **Writes need two permissions**: the operator's `[memory] allow_mcp_writes = true` (default `false`) *and*
  the caller's `user_approved: true` (only when the user asked for exactly this). Saved records are labelled
  `agent:<host>` / `agent_interpretation` - never as the user's own words. Global memory cannot be written
  or deleted through MCP. Credentials and personal data are refused.
* All memory tools refuse to run inside a nested invocation (`AI_ORCHESTRA_DEPTH`), and the host agent can
  never be called through its own server (unchanged).
* Nothing is written to stdout except protocol messages (`tests/mcp_e2e.rs` checks this under `-vv`).

## Known limitations

* **Not encrypted at rest.** File permissions only.
* **Lexical retrieval only.** A follow-up with no shared words and no continuation cue relies on "latest
  turn" continuity; a paraphrase of an old topic with entirely different words is not found.
* **Debate mode** (`--mode debate`) runs the Milestone 2 engine unchanged: it is *recorded* in memory but
  does not *recall* memory.
* The sensitivity classifier is heuristic. The digest of a consultation is deterministic text extraction
  (sections of the final answer), not understanding; a caveat that lives only in a long answer body can be
  cut when the budget forces the shortest form (the omission is announced, never silent).
* Token figures are estimates (bytes / 3). No provider reports a usable count for the recalled block, so
  none is claimed.
* Single-machine, single-user. There is no sync and no sharing.
* Provider-native session continuation is documented by the CLIs but not used or verified.
