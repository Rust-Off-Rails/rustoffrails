# Security policy

Security is Rust off Rails' gold standard. The framework tells agents to build apps with strict CSP and security headers, authorization on every route and socket join, hashed tokens, rate limits and attack tests ([guides/security.md](guides/security.md)). A flaw in that guidance spreads to every app generated from it, so we treat **doc vulnerabilities as real vulnerabilities**.

## Reporting a vulnerability

**Please don't open a public issue.** Report privately through GitHub: **Security → Report a vulnerability** on this repository ([private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)).

Useful reports include:
- the guide, generator, skill or hook involved (file and section);
- what an app built from it is exposed to (a concrete exploit scenario);
- the fix you'd suggest, if any.

We aim to acknowledge reports within 3 business days and to ship a fix (with a `CHANGELOG.md` entry and upgrade steps for existing apps) as fast as the severity demands.

## In scope

- Guidance in `guides/`, `generators/`, `blueprints/` and `stack.md` that leads agents to write vulnerable code.
- The Claude Code plugin: `skills/` and `hooks/` (e.g. a hook that can be made to run arbitrary commands, or a check that a pattern bypasses).
- `tools/` scripts.

## Out of scope

Vulnerabilities in a specific generated app caused by deviating from the framework (check its `docs/decisions.md`), and vulnerabilities in third-party crates and packages: report those upstream, then tell us if the framework should ban or pin something.

## Supported versions

The latest release in `CHANGELOG.md`. Apps upgrade with `/offrails:upgrade`.
