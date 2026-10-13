# Security policy

## Supported versions

AI Orchestra is pre-1.0. Security fixes are made on `main` and released in the next version; older versions are
not patched.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.** Use GitHub's private reporting instead:
go to the repository's **Security** tab and choose **Report a vulnerability**. Include what you found, how to
reproduce it and what you think the impact is. You will get a first answer within a few days; this is a
volunteer project, so fixes take as long as they take, but reports are treated as the priority.

## What is in scope

AI Orchestra runs on your machine and drives CLIs you installed. The things that matter most:

- the local web interface (`ai-orchestra web`): a way to reach it from a web page or a non-loopback address, bypass
  the per-run token or the `Host`/`Origin` checks, or make it expose a file, database or credential;
- the fetcher used for research: reaching local or private addresses, following redirects or DNS answers to them,
  or sending local data to a site;
- a path by which text from an agent, a web page or memory becomes an instruction, a command, a flag or a
  permission change;
- leaking secrets into logs, history, memory or reports;
- the MCP server exposing write access, or letting the host agent call itself.

## What is out of scope

Vulnerabilities in the third-party CLIs (Claude Code, Codex, Antigravity) or in model behaviour: report those to
their vendors. A prompt-injection that merely makes a model say something wrong, without crossing one of the
boundaries above, is a quality issue rather than a vulnerability, though we still want to hear about it.

The project's safety rules are listed in [AGENTS.md](AGENTS.md).
