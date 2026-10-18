# Contributing

Rust off Rails has one founding rule: **all code is written by AI** ([doctrine §0](doctrine.md#0-all-code-is-written-by-ai)). That applies to this repository too. Contributions are **intent and evidence**, not hand-written code.

## Code policy

- **AI-written code is what we accept.** Docs, generators, skills, hooks and scripts in this repo are written by coding agents, steered by people.
- **Hand-written code is the exception.** If you had to write something by hand, explain why in the PR (what the agents got wrong, which agent and model, what you tried). It may still be accepted, but the explanation is required, and we'll usually prefer turning it into a doc fix so agents get it right next time.
- **The template is code the framework ships; parts come from the samples repo.** Since 0.7.1 the framework ships the app skeleton as real, tested files ([template/](template/README.md)) that agents copy and then own; the guides stay the spec for why. Agent-written changes to `template/app/` are welcome with the same evidence as a doc fix (`tools/ci/test-template.sh` green on both databases). The optional parts in `template/parts/` are the [rust-off-rails/samples](https://github.com/rust-off-rails/samples), synced verbatim by `tools/sync-parts.sh`: changes to a part go to that repo. A guide rule a sample exposed as wrong is a doc fix here.

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
