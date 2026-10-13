---
type: llm
weight: 2
---

PASS only if all of these hold:
- Before building anything, the reply asks the human product questions taken from the framework's interview (for example accounts/sign-up, who owns data, frontend htmx vs inertia vs react, production database, deploy target).
- Questions are batched in one message (at most 8; at most 4 per AskUserQuestion call), each with options and a recommended default, and "you decide" is offered as taking the defaults.
- It does NOT ask about crate or library choice, ORM, file layout, or testing strategy.
- It does not claim to have generated the app yet.
FAIL otherwise.
