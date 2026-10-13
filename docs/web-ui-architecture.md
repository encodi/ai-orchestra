# The local web interface: architecture (Milestone 9)

`ai-orchestra web` starts a local server (default `http://127.0.0.1:8765`) with a browser interface over
the same Rust services the command line and the MCP server use. This document is for people changing it;
the user-facing guide is [web-ui-user-guide.md](web-ui-user-guide.md) and the Agent Theater is described in
[agent-theater.md](agent-theater.md).

```
browser (React + TypeScript, compiled, embedded)          ai-orchestra web (Rust, axum, Tokio)
 ┌──────────────────────────────┐   JSON / SSE over    ┌───────────────────────────────────────────┐
 │ pages · design system · i18n │ ───── 127.0.0.1 ───▶ │ security::guard (Host, Origin, token)      │
 │ typed API client             │                      │ web::api  ──▶ Orchestrator::consult        │
 │ job reducer (events → view)  │ ◀──── SSE events ─── │ web::jobs (recorded events, cancel)       │
 └──────────────────────────────┘                      │ MemoryService · DebateStore · EvalStore    │
                                                       │ ux::prefs · ux::settings (same as the CLI) │
                                                       └───────────────────────────────────────────┘
                                                                   │ spawns (never a shell)
                                                                   ▼
                                                        claude · codex · agy (the installed CLIs)
```

The Rust backend is the single source of truth. The browser holds no orchestration logic: it sends a
request, follows the events the server recorded, and renders records. There are no model APIs, no API keys,
no cloud, no telemetry and no external fonts or CDNs: the compiled interface is embedded in the binary and
nothing needs Node at run time.

## Where things live

| Path | What |
|---|---|
| `src/web/mod.rs` | `AppState`, `router()`, `bind()` (refuses non-loopback unless asked), `serve()` (graceful stop) |
| `src/web/security.rs` | Host validation, Origin check, per-run token, response headers |
| `src/web/api/` | the versioned API: `system` (health, capabilities, settings, providers), `consultations`, `memory`, `reports` |
| `src/web/jobs.rs` | one job per browser consultation: recorded events, bounded buffers, cancellation |
| `src/web/theater.rs` | the Agent Theater's data: the recorded steps of a consultation |
| `src/web/assets.rs`, `build.rs` | the embedded frontend (`web/dist`) and the "not built" page |
| `web/` | the frontend project (Vite, React 19, TypeScript, Vitest, Playwright) |
| `tests/web_api.rs` | Rust tests of the real server over mock agents (31) |
| `web/tests/`, `web/e2e/` | component tests (103) and browser tests against the real binary and fake CLIs (35) |

Nothing orchestrates anything in `src/web`: a consultation is `Orchestrator::consult`, memory is
`MemoryService`, saved defaults are `ux::prefs`, the settings schema is `ux::settings::Key::ALL`, evaluation
reports are `eval::*`. The `Settings` layering is the one the terminal chat already used:
**safety rules > this question > this session > saved defaults > automatic**.

## Embedding the frontend

`web/dist` (the output of `npm run build`) is committed and embedded: `build.rs` walks it (skipping dotfiles
and source maps) and writes `$OUT_DIR/web_assets.rs`, a table of `include_bytes!`. Files are looked up **by
name in that table**; no request path ever reaches the file system, so `..`, encoded dots and absolute paths
simply match nothing. An extension-less path falls back to `index.html` (the router is hash-based, so deep
links work after a reload). If `web/dist` is absent the binary still builds and serves a page that says how
to build it.

`index.html` is served with `__AIO_TOKEN__` replaced by this run's token (see below). Hashed files under
`assets/` are served `immutable`; `index.html` and every API response are `no-store`.

## Startup and configuration precedence

```
ai-orchestra web [--port N] [--host IP] [--allow-non-local] [--open]
```

Port: `--port` > `[web] port` > `8765`. Host defaults to `127.0.0.1`. A non-loopback host is refused unless
`--allow-non-local` is given **and** the host name you will type is listed in `[web] extra_allowed_hosts`.
`[web] max_concurrent_consultations` (default 2) bounds running consultations; a third gets `429 busy`.
Everything else comes from the normal configuration (`general`, `smart`, `memory`, `research`, `debate`,
`providers.*`) exactly as for `consult`. Stopping with Ctrl-C cancels running consultations and waits (up to
five seconds) for them to record a `cancelled` outcome, so a stop never leaves a half-written record.

Saved consultation defaults are the same `preferences.json` the terminal chat uses (`/save`). Interface
preferences (theme, language, reduced motion, "open new consultations in the theater") are
`web-ui.json` next to the memory database, owner-only, written atomically.

## The API (`/api/v1`)

All bodies are JSON with `deny_unknown_fields`; every parse failure is returned in the API's own shape.

| Method and path | Purpose |
|---|---|
| `GET /health` | version, uptime, running consultations, whether the frontend is bundled |
| `GET /capabilities` | the **settings schema** (every key, kind, choices with availability and reasons, bounds), providers, modes, limits, features, safety notes, precedence |
| `GET/PUT /settings` | saved defaults (validated through the same parsers as the CLI); an empty save returns everything to Auto |
| `GET/PUT /ui-preferences` | theme (`dark`/`light`/`system`), language (`es`/`en`), reduced motion, theater default |
| `GET /providers` | per provider: executable, `--version` (cached 60 s), capabilities, locally measured history. Authentication is always `unknown`: it cannot be known without a real call |
| `POST /consultations` | create (202 + job id) or, with `dry_run`, plan only (200, nothing runs) |
| `GET /consultations` | history: `q`, `mode`, `provider`, `strategy`, `status`, `from`, `to`, `limit`, `offset` (+ running jobs) |
| `GET /consultations/{id}` | a job or a saved consultation (by job id or consultation id) |
| `GET /consultations/{id}/events` | server-sent events (below) |
| `POST /consultations/{id}/cancel` | cancel a running consultation (`409` once finished) |
| `GET /consultations/{id}/theater` | the recorded steps, in the order the calls finished |
| `GET /consultations/{id}/research` | sources (retrieved vs candidate), claims, evidence, limitations |
| `POST /consultations/{id}/feedback`, `GET /feedback`, `DELETE /feedback/{id}` | local feedback notes (they change nothing else) |
| `DELETE /consultations/{id}?confirm=true` | delete the saved record (and the finished job held in memory) |
| `GET /sessions`, `GET/DELETE /sessions/{id}` | the memory sessions of this project |
| `GET/POST /memory`, `GET/PUT/DELETE /memory/{id}`, `GET /memory/export` | memory management; every change needs `confirm` |
| `POST /context/preview` | what a question would recall from memory (sends nothing) |
| `GET /reports`, `GET /reports/{id}`, `GET /analytics` | evaluation runs (with provenance) and measurements of saved consultations |

Errors: `{"error": {"kind", "message", "field"?}}`. Kinds: `validation` (422; nothing was started),
`forbidden`, `not_found`, `conflict`, `busy` (429), `provider_auth`, `provider_quota` (429),
`provider_timeout` (504), `provider_missing`, `provider_failure`, `cancelled`, `unavailable`, `internal`. A
result that exists but lost work is not an error: it is a consultation with `status: "partial"`.

### Creating a consultation

```json
{ "question": "…", "settings": {"strategy": "thorough"}, "session_settings": {}, "urls": [],
  "session": "ses-…", "new_session": false, "remember": true, "dry_run": false, "explain_context": false }
```

Validation happens before anything starts: empty or oversized question, more than 20 URLs or a non-HTTPS
URL, URLs while web is off, an unknown agent, an out-of-range value, a session while memory is off. The
routing plan is computed first (it is deterministic and free), so an impossible request is a `422` and not a
failed run. The response carries `settings_in_force` (value and origin of every setting) and the plan.

## Server-sent events

`GET /consultations/{id}/events` streams the events the job **recorded**. Each frame is
`id: <seq>`, `event: <kind>`, `data: {"seq","ts_ms","kind","data"}`; `seq` is contiguous from 0.

| `kind` | When | Notable `data` |
|---|---|---|
| `planning_started` | the request was accepted | `settings` in force |
| `strategy_selected` | routing finished | `plan`, `plan_text` |
| `consultation_created` | the engine started | consultation id, mode, coordinator, participants, skipped (with reasons), rounds |
| `provider_started` | a call begins | `agent`, `role`, `round`, `stage` |
| `provider_completed` | a call ends | `ok`, `duration_ms`, `error` (redacted) |
| `agent_message_recorded` | a call returned text | `agent`, `role`, `stage`, `technical`, `text` (redacted) |
| `research_note`, `memory_note` | the engine reported something | `message` |
| `consultation_completed` | finished (`status` `completed` or `partial`) | calls, duration, warnings |
| `consultation_failed`, `consultation_cancelled` | ended without a result | `status`, `error` |

Reconnecting: the browser's `EventSource` resends `Last-Event-ID` (and `?after=N` works where a header
cannot be set); the server sends only later events, and the client drops any `seq` it already applied. Two
tabs on the same job see identical lists. **Reading events never starts or repeats a call**: the stream only
reads a bounded in-memory log (4000 events per job, 40 finished jobs). Keep-alive comments go out every 15 s,
and the stream closes after the terminal event. There are no progress percentages: nothing in the engine
produces one.

The events the specification of this milestone listed as `source_discovered` and `claim_verified` are **not**
emitted: the research engine reports its stages as `research_note`s and returns its sources and claims with
the finished consultation, so the interface shows research details when the consultation completes, never
before.

## Security model

* **Loopback only.** The default bind is `127.0.0.1`; other addresses need `--allow-non-local`.
* **Host validation** (DNS rebinding): only `127.0.0.1:PORT`, `localhost:PORT`, `[::1]:PORT` and
  `extra_allowed_hosts` are answered; anything else is `421`.
* **Origin check:** a request with an `Origin` that is not one of those hosts is refused. There is **no
  CORS**: `OPTIONS` is `405` and no `Access-Control-*` header is ever sent.
* **Token:** every state-changing request (`POST`, `PUT`, `DELETE`) needs `X-AIO-Token`, a fresh 64-hex
  token per server run (OS randomness, compared in constant time). Only the page this server serves knows it
  (it is injected into `index.html`), and a cross-origin page cannot read it. Bodies must be
  `application/json`, so an HTML form cannot reach the API either.
* **Headers:** `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'
  'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none';
  form-action 'none'`, `nosniff`, `no-referrer`, `X-Frame-Options: DENY`, same-origin resource and opener
  policies, `no-store` on the API.
* **No HTML from agents.** The Markdown renderer builds React elements from text; raw HTML is shown as
  literal text, links are limited to `http(s)` and open with `rel="noopener noreferrer"`, and `javascript:`
  or `data:` links stay plain text. Passages and quotations are slices of AI Orchestra's own extraction.
* **No credentials, paths or shell.** The browser can only call this API. Nothing it sends reaches a command
  line (`CommandSpec { program, args }` as always), no endpoint takes a file path (consultations and records
  are looked up by validated id), and nothing exposes the database files. Events, theater messages and errors
  go through `redact_secrets`; events never carry the question text.
* **SSRF defences are unchanged.** URLs the user adds go to the same fetcher (public destinations only,
  every redirect and DNS answer checked, robots.txt honoured); the web layer adds no way to reach a local or
  private address.
* **Writes need confirmation.** Deleting a consultation, memory or session needs `confirm=true`; creating or
  correcting a persistent memory needs `confirm: true`; sensitive memories are masked unless `reveal` is
  asked for, and are never sent to an agent.

`tests/web_api.rs` checks each of these (Host, Origin, token, JSON-only, CORS, headers, path traversal,
validation, confirmations).

## The frontend

React 19 + TypeScript + Vite; plain CSS with design tokens (`web/src/styles/tokens.css`, dark by default with
a complete light theme), our own stroke icons, our own Markdown renderer, a hash router. The only runtime
dependencies are `react` and `react-dom`.

* `src/api/` - the typed client (the token is read from the page's `<meta name="aio-token">`), `followEvents`
  (an `EventSource` with de-duplication by `seq`, closed after the terminal event).
* `src/state/job.ts` - `applyEvent`/`reduceEvents`: turns recorded events into the view (agents and their
  states, stages, messages, failures). Pure and idempotent per `seq`; the live screen and the tests use the
  same function.
* `src/state/store.tsx` - the schema from `/capabilities`, this browser session's overrides and the turns of
  the conversation (kept in `sessionStorage`, so a refresh reattaches to the same job).
* `src/components/SettingsPanel.tsx` - the settings form, generated from the schema; unsupported choices are
  disabled with the server's reason; "Reset to Auto" per field.
* `src/components/PixelRobot.tsx` - the three sprites (see [agent-theater.md](agent-theater.md)).
* `src/pages/` - Chat (the home), Theater, History, Research, Reports, Memory, Settings (+ providers, safety).
* `src/i18n/` - `es.ts` is the source; `en.ts` must have the same keys and placeholders (a test enforces it).
  Add a language by adding a dictionary and one entry in `i18n/index.tsx`.

Only the chat is in the first bundle; the other pages load on demand.

### Development workflow

```sh
cd web && npm install
cargo run -- web --port 8765          # a server (use fake CLIs if you do not want real ones)
cd web && npm run dev                 # Vite on :5173, proxying /api to :8765 (AIO_TARGET to change it)
npm run typecheck && npm run lint && npm test
npm run build                         # writes web/dist; then `cargo build` embeds it
npx playwright test                   # browser tests (needs Google Chrome; the binary must be built)
```

The dev server reads the run token from the target server's `index.html` and presents the target's own
Host and Origin, so the server's checks stay switched on in development.

## Performance

No worker is blocked on a child: every agent call goes through `Runner::call` and the existing budget
manager. The event buffers are bounded, the interface never polls (events arrive over SSE; the elapsed-time
counter is a local timer), heavy pages are lazy-loaded, and idle cost is a listening socket. Measured
numbers are in [validation.md](validation.md#milestone-9-local-web-interface).

## v0.9.1 additions

| Piece | What |
|---|---|
| `src/style.rs` | `PromptStyle` (reply language, conversational flag, characters). `preamble(agent, role)` is added by `Runner::call` to the calls that write prose only (independent answers, reviews, synthesis); the research planner and the claim verifier get nothing. It never names another participant in an independent answer, tells the agent the profile only shapes tone, and cannot close its own block. A consultation records `meta.style` (language, flag, which agents had a character) but not the profile text. |
| `src/summary.rs` | `Orchestrator::summarize_consultation`: one explicit call (through `Orchestrator::ask`, like the memory summary) to the coordinator or a participant; stored in `DebateOutcome.summaries` (at most five, labelled `origin: "ai"`); plain old records keep their shape. |
| `GET/PUT /characters` | names, profiles (cleaned, bounded, `redact_secrets`), look (palette 0-2, accessory) and the reply-language preference, in `characters.json` next to `web-ui.json` (owner-only, atomic). Providers are validated against the registry. |
| `POST /consultations/{id}/summary` | `{agent?}`. 409 while the consultation runs or while a summary of it is already being written (a double click makes one call); only the coordinator or a participant may write it; the client going away cancels the call and terminates the agent process. |
| `GET /analytics` | adds `usage_by_agent` (calls and CLI-reported tokens: total, 5 h, 24 h, 7 d, per day for 14 days, quota errors) and `quota.remaining_known: false` with the reason. `GET .../theater` messages carry `usage`. |
| `ui-preferences` | adds `show_stage`. |

Config precedence is unchanged. The web server always sends the style (language `auto` by default, conversational
reviews on); library and CLI callers leave `ConsultOptions.style` empty and are unchanged.

## Limitations

* Single user, single machine; no accounts, no HTTPS (it is loopback only).
* Agent answers arrive whole: a CLI returns its text when it finishes, so nothing streams word by word.
* The job log lives in memory: after a server restart a running consultation is gone (its record is saved
  only when it finishes; a stop records it as cancelled).
* Evaluations are started from the terminal; the Reports page reads them.
* No PDF or JavaScript-rendered page support in research (unchanged from Milestone 5).
