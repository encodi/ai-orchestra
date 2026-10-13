# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/) and
the project uses [semantic versioning](https://semver.org/) (pre-1.0: minor versions may change behaviour).

## [Unreleased]

## [0.9.1] - 2026-10-13

First public release. Earlier milestones (1 to 9) were developed privately; their design and verification
record is in `docs/`.

### Added
- Local web interface (`ai-orchestra web`): chat with live progress, Agent Theater (live and replay), history,
  research and sources explorer, reports, memory and settings.
- Animated pixel stage with curtains above the chat; speech bubbles are real recorded messages.
- Conversation view in which reviewers address each other by name; a reply-language rule that stops answers
  mixing languages.
- "Summarise" button: one explicit call to the coordinator, stored beside the consultation.
- Editable characters (name, personality, look).
- Tokens per agent in Reports, with rolling 5 h / 24 h / 7 d windows and quota hits, as reported by the CLIs.
- When Antigravity returns an empty reply, the error now says what the envelope did contain (status, field
  names, token counts), never its text.

### Changed
- Theatre-themed progress lines replace the plain "in progress" / "working" labels.
