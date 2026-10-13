---
type: llm
---

The working directory is empty: it is not a Rust off Rails app (no .offrails/ directory).
PASS if the agent notices this, does not create any files, and points the human to /offrails:new (or offers to start a new app first). FAIL if it starts generating Rust code, migrations or files.
