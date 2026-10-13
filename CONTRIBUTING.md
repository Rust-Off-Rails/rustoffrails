# Contributing

Rust off Rails has one founding rule: **all code is written by AI** ([doctrine §0](doctrine.md#0-all-code-is-written-by-ai)). That applies to this repository too. Contributions are **intent and evidence**, not hand-written code.

## Great contributions

- **A doc bug found by building something.** You followed the framework, an agent got stuck, improvised or produced something wrong. Open an issue with the app type, the step, what happened, and (ideally) the agent transcript excerpt. This is the most valuable thing you can send.
- **A verified doc fix.** Have your agent fix the guide, *prove it* by building or changing a real app from the corrected docs, and open a PR that includes:
  - what you built and with which agent;
  - the Definition of Done results (both databases);
  - the crate and package versions it was verified against;
  - a `CHANGELOG.md` entry under `## Unreleased` with reconcile-style **App migration** steps if existing apps are affected.
- **New eval cases** in `evals/` that catch agents doing the wrong thing.
- **Ideas** for features (as specs: the problem, who it's for, what Rails/Phoenix/others do).

## House rules

- Every code snippet in the docs must match code that compiled. Don't guess APIs; check the resolved version.
- Keep the voice: terse, opinionated, written for agents.
- Never weaken a security rule without a security review in the PR.
- Run before opening a PR: `claude plugin validate . --strict`, and make sure every Markdown link resolves (CI checks both).

## Security issues

See [SECURITY.md](SECURITY.md). Please don't report vulnerabilities in public issues.
