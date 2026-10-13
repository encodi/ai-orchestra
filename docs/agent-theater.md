# Agent Theater (Milestone 9)

The Agent Theater shows a consultation as a conversation between the agents, with a pixel-art robot for each.
It is a **view of the record**, not a performance. Its rules:

* **Every message is a real, recorded message.** A bubble is the text an agent's CLI actually returned,
  attributed to the agent that returned it, with the time it finished (relative to the start of the
  consultation) and how long the call took. Nothing is written, completed, summarised or reordered to tell a
  better story, and there is no dialogue invented to be entertaining.
* **No hidden reasoning.** Only what a CLI returned is shown; no chain of thought is recorded, so none is
  displayed. Technical messages (a search plan, claim verification) are shown folded.
* **Failures are shown as failures** - the recorded error, the agent, the stage - and never as a reply.
* **No simulated streaming.** The CLIs return a whole answer, so a message appears when its call finishes.
  "Thinking" is shown only while a call is actually running (between `provider_started` and
  `provider_completed`).
* **Disagreement is not inferred from tone.** The indicator "the coordinator left disagreements unresolved"
  comes from the synthesis the engine already parsed (`unresolved_disagreement`) and quotes its text;
  agreement between models is never presented as proof.
* **It changes nothing about the deliberation.** The theater reads events and records; independent first
  analyses stay independent because nothing here is sent to any agent.

## Data provenance

| What you see | Where it comes from |
|---|---|
| live bubbles | `agent_message_recorded` events of the running job (`GET /consultations/{id}/events`) |
| robot states | `provider_started` / `provider_completed` events (thinking / reviewing / speaking by stage; finished; failed) |
| a saved or finished consultation, replay | `GET /consultations/{id}/theater`: the steps stored in the consultation record, in the order the calls finished |
| the timeline | the same events (live) or the start and finish offsets of each recorded step (saved) |
| stage headings | the role of each call: independent analysis, review, cross-review, synthesis |
| disagreement indicator | `meta.unresolved_disagreement` and `meta.disagreements` of the record |

The live view and the replay come from the same record, so what you watched and what you replay later are the
same messages (`tests/web_api.rs` checks that the number and text of the live `agent_message_recorded`
events equal the theater timeline).

## Using it

* **Teatro / Transcripción** toggles between the conversation with avatars and a plain list.
* **Live**: messages are appended as they are recorded; the feed follows them. If you scroll up, following
  pauses and a "back to the latest message" button resumes it (or untick *Seguir los mensajes nuevos*).
* **Timeline** (right): planning, each call's start and finish, each message (a link jumps to it), research
  and memory notes, and the final event.
* **Replay** of a saved consultation: play/pause, next message, restart, speed (1x-16x), a position slider and
  "skip long waits". Replay reveals the **real** messages when their recorded finish time is reached; it only
  compresses time, and the screen says so. Nothing is called.
* Expand a bubble for the full Markdown; citations in it behave as in the chat.

## The robots

Three original sprites, drawn as SVG rectangles on a 14 x 14 grid with `shape-rendering: crispEdges` (no image
files, no external assets), in `web/src/components/PixelRobot.tsx` with colours from the design tokens:

| Agent | Sprite | Palette |
|---|---|---|
| Claude Code | **Amber Bot**: rounded head, antenna, two square eyes | warm amber |
| OpenAI Codex | **Circuit Bot**: angular head, dark visor with lit eyes, circuit pixels | teal |
| Google Antigravity | **Prism Bot**: compact, two-tone blue/violet body, tall eyes of different colours, a small prism on top | blue and violet |

Any other agent (for example a mock) gets a neutral grey bot. States - idle, thinking (dots), speaking
(mouth), reviewing (a lens), waiting (eyes half closed), finished (a spark) and error (crossed eyes) - change
the face and a small motion drawn with `steps()` to keep the pixel feel. The robots are **decorative**: the
agent's real name is always written beside them and the accessible label names the agent and the state
(`aria-hidden` when the name is already there). With reduced motion (system setting or Ajustes) every
animation stops.

Colour is never the only cue: each robot has a different shape, and the name, role and state are text.

## The animated stage in the chat (v0.9.1)

Above the chat, while a consultation runs (and after it), a small pixel-art stage shows the characters walking
on the platform of a theatre (proscenium arch, painted backdrop, wooden boards, footlights, the audience's
heads at the bottom). The same rules apply, applied to motion:

* **Where they stand is decoration derived from the real stage.** Independent analysis: each at its own desk.
  Review and cross-review: around the table. Synthesis: the coordinator steps to the front, the others stay at
  their desks. Finished: everyone lines up at the front. Before anything has happened they stroll (a decoration
  with no meaning, only when motion is allowed). Cancelled or failed runs stand still.
* **A speech bubble is a recorded message.** It carries the first words of the text an agent actually returned,
  next to the agent that wrote it, and only the last two messages have one, so the bubbles move on as the
  conversation does. A failed call shows a fixed "could not answer" status, never the error text or any words.
  Technical messages (search plans, claim checks) never get a bubble.
* **The curtains follow the show.** They are closed before anything happens and while the plan and the web
  research are prepared, open when the agents start working and talking, and close when the consultation
  finishes (a cancelled or failed run also ends with them closed). A saved record opened in the Agent Theater
  page is shown with the curtains open so the cast is visible; replaying it opens and closes them like a live
  one. Bubbles never show behind a closed curtain. A soft light on the boards follows whoever spoke last.
* **The name is below the character** (the character name the user chose, else the CLI's name).
* **Motion:** characters swap two leg poses while they move and face the direction they walk; with reduced
  motion (system setting or Settings) there is no walking, wandering or bubble animation, and they simply
  stand where the stage puts them.
* The scene is `aria-hidden`: it is a picture of the conversation, which is in the chat below with the same
  messages. It can be hidden from the chat or switched off in Settings -> Appearance.
* The Agent Theater page uses the same stage in place of the old row of avatars, driven by the live events or,
  when replaying, by the replay cursor.

Implementation: `web/src/components/Stage.tsx` (positions from `spotFor`, a pure function of the stage, the
agent's place in the cast and a tick), `PixelRobot.tsx` (walk cycle, accessories, palettes).
