# Everyday use: the chat, settings, results and history (Milestone 8)

`ai-orchestra` in a terminal opens a conversation. You type a question, AI Orchestra picks how much AI work
it needs (and says why), shows progress while it works, and gives you the answer in the shape that fits the
question. You can then ask follow-ups, change how it works, look at the sources, or leave. Nothing about the
engines changed: the chat is a thin layer over the same `consult`, memory and history that the command line
uses (`src/ux/`, `src/chat_cmd.rs`).

```
$ ai-orchestra
AI Orchestra 🎼  (/help for commands, /exit to leave)

You: Which portable oscilloscope should I buy?
Mode: decision · Coordinator: claude · Agents: codex, google · Strategy: balanced
  [Consult:decision] ... Forming its own answer...  [4s]
  [Codex] Reviewing...  [31s]
  [Coordinator] Producing final synthesis...  [47s]

Recommendation
...
Trade-offs, costs and risks
...
Disagreements
...
[complete · decision · balanced · 4 call(s) · 52s · agents: claude, codex, google · id 20261012-...]

You: What if I increase my budget to $8,000 MXN?
```

A line that starts with `/` is a command; any other line is a question. In a script or a pipe, plain
`ai-orchestra` still prints the usage error and exits 2, as before; `ai-orchestra chat` works anywhere
(for example `printf '...\n/exit\n' | ai-orchestra chat`).

## Commands

| Command | What it does |
|---|---|
| `/help` | the list below |
| `/settings` | every setting, where its value comes from, and what is always enforced |
| `/mode` `/coordinator` `/agents` `/rounds` `/depth` `/web` `/strategy` `/length` `/timeout` `/max-calls` `/memory` `/context-budget` `/view` | show (no value) or set (with a value) one setting for this session; `auto` returns it to AUTO |
| `/auto <setting>` / `/auto all` | return one setting / all session settings to AUTO |
| `/save` | keep this session's settings as your starting preferences (`/prefs` shows them, `/prefs clear` removes them) |
| `/ask --mode review --strategy thorough <question>` | settings for this one question only |
| `/sources` | the sources of the last answer: link, dates, whether it was retrieved, claim status |
| `/answers` / `/answers full` | what each agent answered, reviewed and synthesised |
| `/disagreements` | open disagreements, where the agents agree (agreement is not proof), sources that conflict |
| `/why` | the routing and strategy decisions in plain words, and the recalled context |
| `/stats` | calls, time, per-agent calls, tokens as reported by the CLIs (never estimated here) |
| `/last` | show the last answer again |
| `/new` | start a new session (settings stay) |
| `/sessions`, `/resume <id>` | list this project's sessions; continue one |
| `/history [words]`, `/open <id>` | list or search earlier consultations; show one again |
| `/feedback <rating> [note]` | stored locally for you; see below |
| `/exit` | leave (so does Ctrl-C at the prompt) |

Start with settings from the command line: `ai-orchestra chat --set strategy=balanced --set web=off`, or
continue a session: `ai-orchestra chat --session ses-4b1f`.

## Automatic and manual settings

Every setting can be AUTO: AI Orchestra then chooses and explains. The choices it can make for you are the
mode (decision, research, review, learn, debate), the coordinator, the agents, the review rounds, the research
depth and web use, and the execution strategy (`fast`, `balanced`, `thorough`).

Precedence, strongest first, and it never silently changes a value you set:

1. **Safety rules** - not settings: agents are read-only, the host agent is never called through MCP, limits
   and the recursion guard apply, nothing an agent or a web page says is run. `/settings` lists them.
2. **This question** - `/ask --x value ...`
3. **This session** - `/mode decision`, until `/auto` or the end of the session
4. **Saved preferences** - `/save`; the starting point of every new chat (one small owner-only JSON file next
   to the memory database; no history in it)
5. **AUTO** - whatever the router, the strategy engine and the configuration decide

Values are checked when typed: `/rounds 3` answers `accepts: auto, 1, 2` and changes nothing; `/agents
lead,nobody` is refused with the list of agents that exist. `[memory] default`, `[smart]` and the other
configuration keys keep working exactly as before and sit under all of the above.

## How results are shown

The coordinator writes the answer with the sections its mode asks for. The default view puts the useful ones
first for that mode and drops the orchestration log:

* **decision / review**: recommendation (the direct answer first), main reasons and criteria, trade-offs, the
  evidence and your preferences, how a change in priorities would change it, then the caveats;
* **research**: main findings, established facts, contradictions, open questions, contested claims, then the
  sources line;
* **learn**: explanation, what to know before you start, corrections made after review;
* **debate**: conclusion, reasoning, next steps.

After the main sections come the caveats, in the same order for every mode: **Disagreements** (shown only when
there is one - "None" is not shown as a finding), **Assumptions** and **Evidence limits**. A section the
answer does not have is not shown and nothing is invented to fill it; an answer without headings (a brief or
single-agent reply) is shown whole. A partial, failed or cancelled run starts with **PARTIAL RESULT**,
**FAILED** or **CANCELLED** and never with a "complete" footer. The last line says how it was produced: status,
mode, strategy, calls, time, agents and the id.

Uncertainty is preserved, not tidied: the evidence limits and open disagreements the coordinator wrote are
always shown, `Note:` lines carry the engine's warnings (no web research, a single agent with no review), and
`/sources` says plainly when "no web research was done, so nothing was checked against external sources".

## Progress and cancellation

Progress lines come from the engine's own events (searching for sources, an agent forming its answer, a
review, the final synthesis), each with the seconds elapsed. There are no percentages and no simulated
streaming: the CLIs return an answer when they finish, so that is when it appears.

Ctrl-C while a consultation runs cancels it: the running agent processes are terminated (the same mechanism as
`consult`), the chat prints `CANCELLED: ... No result was produced.`, and stays open. A cancelled run is never
described as complete and is not remembered as a conclusion. Ctrl-C at the prompt leaves the chat.

## Sessions, history and memory

The chat starts a **new** session unless you pass `--session`; follow-ups continue it through the Milestone 7
memory (so `memory` and `session` commands see the same data). `/new` starts another, `/resume <id>` returns to
an old one - AI Orchestra's own session; no provider CLI session is resumed. `/history words` searches saved
consultations by the words in their question or answer (also `ai-orchestra history search "words"`), and
`/open <id>` shows one again with a note: **reopening does not re-check anything**; if its sources are older
than 30 days the note says prices, versions and availability may have changed, and asking again with web
research is how to refresh. Deleting sessions and memories follows the memory permissions (`session clear`,
`memory forget`).

## Feedback

`/feedback helpful|not-helpful|inaccurate|incomplete|bad-citation|slow|verbose|unsatisfactory [note]` attaches
a note to the last consultation, stored in a local owner-only file. It does nothing else: it is not a
preference, it trains nothing, and it changes no routing and no provider ranking (that would need a larger
sample and a separately validated step). `ai-orchestra eval feedback --list`, `--delete <id>` and `--clear`
show and remove it.

## Examples

* **Product comparison**: `Which portable oscilloscope should I buy?` ... then `/sources`, `/answers`, then
  `What if I increase my budget to $8,000 MXN?` - the follow-up recalls the earlier conclusion and your budget
  (Milestone 7) instead of starting from nothing.
* **Research**: `/web on`, `/depth deep`, `Does the Model X really have 100 MHz of bandwidth?` ... `/sources`
  shows which claims are SUPPORTED, CONTRADICTED or INSUFFICIENT_EVIDENCE and the passage behind each.
* **Learning**: `/mode learn`, `Explain aliasing to a beginner` - the answer is an explanation with
  prerequisites, not a recommendation.
* **A personal decision**: tell it your budget and must-haves in the question (or `memory add` them); the
  answer states how they were respected and how the recommendation changes if they change.
* **Cheap and quick**: `/strategy fast` for one agent; `/ask --strategy thorough ...` when one question
  deserves the full panel.

## Local web UI

Built in Milestone 9: `ai-orchestra web`. It offers the same chat, settings, results and history in a browser,
plus the Agent Theater. See [web-ui-user-guide.md](web-ui-user-guide.md).

## Limitations

* The chat reads one line at a time; multi-line questions can be given with `ai-orchestra consult "..."` or
  `-` for stdin.
* Output is plain text; there is no full-screen interface, no colours and no streaming of partial answers.
* `/agents` and `/coordinator` accept only agents this installation knows; whether an agent is *usable* (its
  CLI installed and logged in) is decided when a question is asked, and the plan line says what was chosen.
* The presentation extracts sections by heading. If a model ignores the requested headings, the answer is shown
  whole rather than rearranged.
