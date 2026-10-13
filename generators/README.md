# Generators (`rails generate` → recipes)

A generator here is a recipe you execute: a list of files to create and edit, in order, ending in a green Definition of Done. Read the whole recipe before starting it.

| Rails command | Recipe | Produces |
|---------------|--------|----------|
| `rails app:update` | [upgrade.md](upgrade.md) | Re-vendors `.offrails/` to a newer release and migrates the app (reconcile, never re-create) |
| `rails new` | [new.md](new.md) | The app skeleton, kernel, CI, Dockerfile, smoke tests |
| `rails g authentication` | [authentication.md](authentication.md) | Users, sessions, login/logout, password reset, (sign-up) |
| `rails g scaffold` | [scaffold.md](scaffold.md) | Migration, model, controller, routes, views/pages, tests |
| `rails g resource` | [scaffold.md#resource](scaffold.md#resource) | Same, without views (handlers stubbed to the spec) |
| `rails g model` | [scaffold.md#model](scaffold.md#model) | Migration + model + factory + model tests |
| `rails g migration` | `sea-orm-cli migrate generate <name>` + [guides/migrations.md](../guides/migrations.md) | One migration |
| `rails g job` | [job.md](job.md) | Job struct, dispatcher entry, test |
| `rails g mailer` | [mailer.md](mailer.md) | Mailer fns, templates, preview, test |
| `rails g channel` | [channel.md](channel.md) | Realtime stream + client wiring |
| `rails g task` | [job.md#tasks](job.md#tasks) | `src/tasks/<name>.rs` + registration |

## Universal generator rules

1. **Check `FRONTEND_MODE`** in the app's `AGENTS.md` and follow only that mode's branch.
2. **Order:** migration → entity/model → factory → failing tests → controller → routes → views/pages → wiring (`mod.rs`, `ROUTES`, nav links) → Definition of Done.
3. **Register everything:** new modules in `mod.rs`, routes in `routes.rs` + `ROUTES` + `paths::`, jobs in the dispatcher, migrations in `migration/src/lib.rs` (sea-orm-cli does this), and the nav link in the layout if the resource is top-level.
4. **Finish green.** A generator that leaves the tree red isn't finished.
