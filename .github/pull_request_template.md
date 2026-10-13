## What and why

<!-- What does this change, and why? Link the issue it closes: "Closes #123". -->

## How it was tested

<!-- Mocks / fake CLIs / browser tests. Never a real model. -->

## Checklist

- [ ] Focused change; the description says *why*
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass
- [ ] If `web/` changed: typecheck, lint, tests pass and `web/dist` is rebuilt and committed
- [ ] New behaviour has tests (mocks or fake CLIs, no live AI calls)
- [ ] Docs and `CHANGELOG.md` updated if behaviour changed
- [ ] No secrets, personal paths or real model output added
- [ ] Respects the hard rules in `AGENTS.md` (no model APIs, read-only by default, no shell)
