---
name: security
description: Deep security review of a Rust off Rails app or change: threat model, the full security checklist, scanners (cargo-deny, secret scan, ZAP baseline), header/CSP verification and WebSocket checks. Use before shipping, after auth/payments/uploads/realtime/API changes, or when the user asks about security, hardening or a pentest-style review.
argument-hint: "[feature or area, e.g. 'channels' or 'uploads'; empty = whole app]"
context: fork
---

# /offrails:security: security is the gold standard

Only for Rust off Rails apps (a `.offrails/` directory exists). Scope: $ARGUMENTS (empty = the whole app).

You are the app's adversary. Assume every input is hostile, every client is scripted, and every proxy lies. **Security controls are never weakened to make something work.** If a fix would weaken one, stop and ask the human.

1. **Load the rules:** `.offrails/guides/security.md` (the whole thing, including the checklist), `.offrails/guides/test-standards.md` §3, and the app's `AGENTS.md` (mode, accounts, tenancy, API, deploy target).
2. **Threat model the scope.** For each feature spec in scope, fill or update its `## Security` section (see `.offrails/blueprints/feature-spec.md`):
   - assets;
   - actors (anonymous, user, other user, other tenant, admin, a compromised account);
   - entry points (routes, sockets, jobs, inbound email, webhooks, uploads);
   - STRIDE threats, with the control for each and the test that proves it.

   Any threat without a control *and* a test is a finding.
3. **Run the scanners** (install them if missing; report versions):
   - `cargo deny check` for advisories, licenses, bans and duplicate sources;
   - a secret scan of the working tree and git history (`gitleaks detect`, or `trufflehog`);
   - `pnpm audit --prod` in TS modes;
   - the **ZAP baseline** against the release build running locally (`docker run … zaproxy … zap-baseline.py -t http://host.docker.internal:$PORT`). Treat every WARN as a finding until it's triaged.
4. **Verify the response hardening live** (curl every response class: HTML page, JSON, SSE, WebSocket upgrade, kernel page, error page, static asset, redirect):
   - a strict CSP with no `unsafe-inline` or `unsafe-eval`, `frame-ancestors 'none'`, `base-uri 'self'` and `form-action 'self'`;
   - HSTS in production (`max-age=63072000; includeSubDomains`), `X-Content-Type-Options: nosniff`, `Referrer-Policy`, `Permissions-Policy`, COOP `same-origin` and CORP `same-origin`;
   - `Cache-Control: no-store` on authenticated and kernel pages;
   - cookies `HttpOnly; Secure (prod); SameSite=Lax`, with the `__Host-` prefix where possible.
5. **Walk the checklist** in `security.md` item by item against the code, with `file:line` evidence for each verdict. Go hard on:
   - authorization on every route **and** every socket join and push;
   - tenant scoping on every query;
   - IDOR via guessable ids;
   - mass assignment;
   - CSRF and Origin checks (including the WebSocket upgrade);
   - rate limits (login, magic link, API tokens, socket frames);
   - upload validation;
   - SSRF on any outbound fetch;
   - open redirects (`return_to`);
   - timing-safe token comparison;
   - token storage (digests only) and expiry;
   - sessions revoked on logout and password change, **including open sockets and streams**;
   - secrets in logs, job args or error pages;
   - dependencies pinned.
6. **Attack it.** For each finding you suspect, write the failing test first (`tests/app/security_test.rs` or the feature's test file), then fix it, then confirm the test passes on SQLite and Postgres.
7. **Report:** a findings table (severity, `file:line`, exploit scenario, fix, test name), scanner results, header matrix, and what was out of scope. Severity: critical / high / medium / low / info. Never call the app "secure". Say what was checked.
