# Contributing to AI Orchestra

Thanks for helping. This document explains how work flows through the repository, what every change must pass,
and the rules that keep the project safe. Please also read the [Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to help

- **Report a bug** or a CLI that changed under us: open an issue with the output of `ai-orchestra doctor`
  (it contains no prompts or credentials).
- **Suggest a feature**: open an issue first for anything bigger than a small fix, so we can agree on the shape
  before you spend time on it. Check the [hard rules](#hard-rules) first: some ideas (calling model APIs, writing
  files by default) are out of scope on purpose.
- **Improve the docs**, translations of the web interface (`web/src/i18n/`), or the tests.
- **Review pull requests.**

Good first issues are labelled `good first issue`.

## Workflow (GitHub flow)

`main` is the only long-lived branch. It is protected: it always builds and passes CI, and nobody pushes to it
directly.

1. **Fork** the repository (or, if you have write access, branch directly).
2. **Branch from `main`** with a short name that says what it does:
   `feat/<topic>`, `fix/<topic>`, `docs/<topic>`, `test/<topic>`, `refactor/<topic>`, `chore/<topic>`.
3. **Commit** in small, reviewable steps using [Conventional Commits](https://www.conventionalcommits.org/):
   `feat(web): add a stage curtain`, `fix(google): record why a reply was empty`, `docs: explain config order`.
4. **Run the checks locally** (below) and keep the change focused: one concern per pull request.
5. **Open a pull request** against `main` and fill in the template. Link the issue it closes.
6. **CI must be green** and a maintainer must review. Address review comments with new commits (we squash on merge,
   so history inside the PR does not matter).
7. A maintainer **squash-merges** the PR; the branch is deleted automatically. History on `main` stays linear.

Releases are cut from `main` by tagging `vX.Y.Z` ([semantic versioning](https://semver.org/)); the
[CHANGELOG](CHANGELOG.md) is updated in the pull request that changes behaviour.

## Development setup

You need a recent stable Rust toolchain (edition 2024) and, **only if you change the web interface**, Node.js 22.
You do not need any of the agent CLIs to develop or test: tests use in-process mocks and fake shell-script CLIs.

```sh
cargo build
cargo test
```

### Checks every change must pass

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# only when web/ changed:
cd web && npm ci && npm run typecheck && npm run lint && npm test && npm run build && cd ..
cargo build && (cd web && npx playwright test)   # needs Google Chrome installed
```

`web/dist` is the committed build output that `build.rs` embeds in the binary. **If you change `web/src`,
rebuild it (`npm run build`) and commit the result**; CI fails when `web/dist` is out of date.

## Hard rules

The complete list is in [AGENTS.md](AGENTS.md) and is enforced in review. The ones that matter most:

1. **No model APIs, API keys or credential handling.** Only spawn the installed CLIs; never read, copy or forward
   OAuth files or tokens.
2. **No live AI calls in tests or development.** Use `MockProvider` or fake CLIs (see `tests/mock_cli.rs`).
   Only the maintainer runs live validation, deliberately, because it spends their subscription quota.
3. **Never use a shell.** Build a `CommandSpec { program, args }`; never interpolate prompts or agent output into
   a command string, and never execute anything an agent produced.
4. **Read-only by default.** Write access needs an explicit user flag. Do not add `--dangerously-*`,
   `danger-full-access` or `bypassPermissions`.
5. **No secrets in logs or stored data.** Anything saved goes through `redact_secrets`; never log prompts, argv or
   environment values.
6. **Tests must not touch the real home directory.** Give spawned binaries a private `HOME` / `XDG_DATA_HOME`.
7. **Adapter flags must come from the CLI's own `--help` or official docs.** Record in the module comment what was
   verified and what was not.
8. **Decisions are rules, not model calls.** Routing, escalation and stopping are deterministic code.
9. **Numbers are measured.** No invented token counts, costs, probabilities or quality scores.

## Code conventions

- Errors are variants of `OrchestraError` (`thiserror`); no `unwrap`/`expect`/panics outside tests.
- Async code uses Tokio; anything that can block on a child honours a `CancellationToken` and a timeout. Child
  processes are spawned only through `ProcessManager`.
- Provider-specific logic stays in `src/providers/<name>.rs`. To add a provider: a new file, register it in
  `builtin_registry`, document its config, and add parser and argument-builder unit tests plus a fake CLI in
  `tests/mock_cli.rs`.
- The web interface is a *view*: handlers call the same services as the CLI and never add a second orchestration
  path. Every user-visible string goes through `i18n` (Spanish is the source; English must have the same keys).
  The browser tests check contrast, keyboard use and reduced motion; do not weaken them.
- Keep comments for the non-obvious *why*. Match the style of the surrounding code, prefer minimal dependencies.

## Pull request checklist

- [ ] The change is focused and the PR description says *why*.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass.
- [ ] If `web/` changed: the web checks pass and `web/dist` is rebuilt and committed.
- [ ] New behaviour has tests (mocks or fake CLIs, never a real model).
- [ ] Docs and `CHANGELOG.md` are updated when behaviour changes.
- [ ] No secrets, personal paths or real model output were added.

## Licensing of contributions

Unless you explicitly state otherwise, any contribution you intentionally submit is licensed under the same
terms as the project: **MIT OR Apache-2.0**, without additional terms or conditions.
