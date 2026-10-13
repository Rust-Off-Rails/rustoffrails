# Contributing

Rust off Rails has one founding rule: **all code is written by AI** ([doctrine §0](doctrine.md#0-all-code-is-written-by-ai)). That applies to this repository too. Contributions are **intent and evidence**, not hand-written code.

## Code policy

- **AI-written code is what we accept.** Docs, generators, skills, hooks and scripts in this repo are written by coding agents, steered by people.
- **Hand-written code is the exception.** If you had to write something by hand, explain why in the PR (what the agents got wrong, which agent and model, what you tried). It may still be accepted, but the explanation is required, and we'll usually prefer turning it into a doc fix so agents get it right next time.
- **Why no reference code (yet)?** Apps are generated from the docs, so two people with different agents can get different code for the same spec. That's expected: the docs pin the decisions (stack, structure, conventions, security, tests), not the exact lines. We tested shipping sample code and, for now, it isn't in the framework's interest; it may come later as optional, verified reference implementations.

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
