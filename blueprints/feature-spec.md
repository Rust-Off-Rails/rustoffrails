# Feature: {{name}}

<!-- One file per feature in product/features/. Humans can write any part in plain English; agents fill gaps and mark assumptions. -->

## Why

{{The user problem this solves.}}

## Who

{{Which users can see it? Which can change it?}}

## What (data)

| Field | Meaning | Required? | Rules |
|-------|---------|-----------|-------|
| {{title}} | {{…}} | yes | {{max 200 chars}} |

## Behavior

- {{When X happens, Y.}}
- {{Side effects: emails, notifications, live updates.}}
- {{On delete: …}}

## Screens

- {{List page: shows … sorted by …}}
- {{Form: …}}

## Acceptance criteria

<!-- Each gets a stable ID `AC-<this file's name>-<n>` (never renumbered or reused; strike through dropped ones).
     Each becomes a test that cites the ID in its doc comment. Agents tick [x] when that test passes on SQLite and Postgres. -->

- [ ] AC-{{feature}}-1: {{A logged-in user can create a book with a title and author}}
- [ ] AC-{{feature}}-2: {{Creating a book without a title shows "Title can't be blank"}}
- [ ] AC-{{feature}}-3: {{Only the owner sees the Edit and Delete buttons}}

## Security (agent-written threat model)

<!-- Required for every feature. Humans may add concerns in plain English; agents keep it complete.
     Every threat needs a control AND a test. A threat without both is a finding (/offrails:security).
     A STRIDE category that can't apply here: | <Category> | n/a: <why it can't happen> | — | — |
     (offrails-check accepts "n/a: <reason>" in the Threat column; a bare "n/a" or a real threat with "—" fails). -->

- **Assets:** {{what an attacker wants here: other users' books, reviews, emails, sessions, money}}
- **Actors:** {{anonymous · user · other user · other tenant · admin · compromised account}}
- **Entry points:** {{routes, socket topics, jobs, inbound email, webhooks, uploads this feature adds}}

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | {{someone acts as another user}} | {{RequireUser + session digest}} | {{`anonymous_cannot_create_a_book`}} |
| Tampering | {{edits another user's record / mass-assigns owner_id}} | {{policy + Params without ownership fields}} | {{`other_user_gets_404_on_update`}} |
| Repudiation | {{admin action can't be traced}} | {{logged with actor id}} | {{…}} |
| Information disclosure | {{IDOR, cached private page, enumeration}} | {{owner-scoped query, no-store, same response for unknown emails}} | {{…}} |
| Denial of service | {{flooding, huge uploads}} | {{rate limit, body limit}} | {{…}} |
| Elevation of privilege | {{user reaches admin action}} | {{AdminAccess / role policy}} | {{…}} |

## Assumptions (agent-written)

- {{Assumed X because the spec didn't say. Change this line to change the behavior.}}

## Open questions

- {{Questions waiting on the human. The agent proceeds with the assumption above meanwhile.}}
