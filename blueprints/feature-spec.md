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

## Assumptions (agent-written)

- {{Assumed X because the spec didn't say. Change this line to change the behavior.}}

## Open questions

- {{Questions waiting on the human. The agent proceeds with the assumption above meanwhile.}}
