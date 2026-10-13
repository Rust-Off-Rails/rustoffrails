# GraphQL (`API: graphql | both`)

> **TL;DR**
> - One endpoint, `POST /graphql`, built with async-graphql 7.x + async-graphql-axum; GraphiQL at `/dev/graphiql` in development only.
> - A `DataLoader` per request for every association; depth limit ≥ 13 (or introspection breaks) and a complexity limit.
> - Validation errors go in the mutation payload (`errors: [UserError]`); auth, not-found and internal errors are top-level with `extensions.code`; never leak SQL.
> - The SDL is committed as `docs/schema.graphql`, with a drift test; policies still apply to every field and mutation.
>
> Read the rest when adding types, queries or mutations, or writing GraphQL tests.

One schema, one endpoint: `POST /graphql`. Built with **async-graphql** + **async-graphql-axum** (verified 2026-10-13 against 7.2.1; 8.0 is still a release candidate, so `cargo add` resolves 7.x, which is what you want). Reference implementation: `offrails-api-demo` ([api](api.md)).

```sh
cargo add async-graphql --features chrono,dataloader
cargo add async-graphql-axum
```

| Rails / graphql-ruby | Off Rails |
|----------------------|-----------|
| `app/graphql/types/*` | `src/graphql/types.rs`: `#[derive(SimpleObject)]` built `From<Model>` (like `views/`) |
| `QueryType` / `MutationType` | `src/graphql/query.rs`, `mutation.rs` |
| `GraphQL::Dataloader` / `graphql-batch` | `src/graphql/loaders.rs`: one `Loader` per association, a fresh `DataLoader` per request |
| `max_depth`, `max_complexity` | `Schema::build(..).limit_depth(MAX_DEPTH).limit_complexity(MAX_COMPLEXITY)` |
| `context[:current_user]` | `Viewer(Option<ApiUser>)` in the request data |
| graphiql-rails | `/dev/graphiql`, development only |
| `rake graphql:schema:dump` | `docs/schema.graphql`, committed, with a drift test |

## Layout

```
src/graphql/
  mod.rs       AppSchema, schema(), MAX_DEPTH/MAX_COMPLEXITY, Viewer, viewer()/viewer_with(), gql_error(), OrGql
  query.rs     struct Query + #[Object] impl
  mutation.rs  struct Mutation + #[Object] impl
  types.rs     NoteObject (name = "Note"), UserObject (name = "User"), inputs, payloads, UserError
  loaders.rs   UserLoader(DatabaseConnection) implements Loader<i64>
src/controllers/graphql.rs   execute (POST /graphql), graphiql (GET /dev/graphiql)
```

## The handler

```rust
pub async fn execute(
    State(s): State<AppState>,
    Extension(schema): Extension<AppSchema>,
    OptionalApiUser(caller): OptionalApiUser,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let req = req.into_inner()
        .data(graphql::loaders(&s))   // DataLoader per request: no cross-request cache
        .data(Viewer(caller))
        .data(s);
    schema.execute(req).await.into()
}
```

Routed as `.route("/graphql", post(gql::execute).layer(Extension(graphql::schema())))`. The schema is built once at boot. **POST only**: no GraphQL over GET (queries in URLs end up in logs and caches). Anonymous requests reach the endpoint (`ROUTES` access `Public`); resolvers decide.

## Conventions

1. **Auth per resolver, from the same token as REST.** `viewer(ctx)?` → `UNAUTHENTICATED`; `viewer_with(ctx, Scope::Write)?` → `FORBIDDEN`. Then the same `policies::*` functions REST uses, and owner-scoped queries (`note::find_owned`), so another user's record is `NOT_FOUND`.
2. **Two kinds of errors:**
   - *The request can't be served* (auth, missing record, rate limit, internal): a top-level error from `gql_error(AppError)` with a stable `extensions.code`: `UNAUTHENTICATED`, `FORBIDDEN`, `NOT_FOUND`, `TOO_MANY_REQUESTS`, `BAD_USER_INPUT`, `INTERNAL`. The message is fixed text; internals are logged, never returned.
   - *The input is invalid*: the mutation succeeds at the GraphQL level and its payload carries `errors: [UserError!]!` (`{field, message}`, built from `models::Errors`). Clients render these next to fields, so they're data, not exceptions.
3. **Mutations return payloads**: `createNote(input: NoteInput!): NotePayload!` with `note: Note` (null on validation failure) and `errors`. `deleteNote` returns `deletedId`. Inputs map to the model's `Params` (`impl From<NoteInput> for NoteParams`), so validation is the model's, shared with REST.
4. **Every association through a DataLoader.** A `#[ComplexObject]` field loads via `ctx.data_unchecked::<DataLoader<UserLoader>>().load_one(id)`. The loader does one `WHERE id IN (…)` per batch (`user::find_many`). A test proves 1 and 20 rows cost the same number of queries.
5. **Limits.** `MAX_DEPTH = 15`: it must stay **at least 13**, because the standard introspection query (GraphiQL, codegen tools) nests that deep. With 8, GraphiQL loads with no schema, while the HTTP tests stay green. `MAX_COMPLEXITY = 500`, with list fields costed by page size: `#[graphql(complexity = "(per.clamp(1, 100) as usize) * child_complexity")]`, so many aliased big pages are rejected.
6. **IDs are `ID` strings** (`i64.into()`); parse them back with `parse_id`, where garbage is `NOT_FOUND`, not a parse error.
7. **Names**: Rust structs carry an `Object` suffix; expose them with `#[graphql(name = "Note")]`. Fields are camelCase on the wire (async-graphql's default), snake_case in Rust.
8. **Subscriptions**: not in v1 of this guide (`EmptySubscription`). Live updates use the realtime layer ([realtime](realtime.md)): clients refetch on a signal.

## Schema snapshot

`docs/schema.graphql` is the committed SDL. A test compares `graphql::schema().sdl()` with the file:

```rust
#[test]
fn schema_snapshot_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/schema.graphql");
    let sdl = offrails_api_demo::graphql::schema().sdl();
    if std::env::var("UPDATE_SCHEMAS").is_ok() { std::fs::write(&path, &sdl).unwrap(); }
    assert!(std::fs::read_to_string(&path).unwrap_or_default() == sdl,
            "docs/schema.graphql is stale: run UPDATE_SCHEMAS=1 cargo nextest run schema_snapshot");
}
```

A removed or retyped field is a breaking change: use `#[graphql(deprecation = "use x")]` first, log the decision, tell the human.

## GraphiQL (development only)

`GraphiQLSource::build().endpoint("/graphql").version(GRAPHIQL_VERSION)` at `/dev/graphiql`, with `GRAPHIQL_VERSION` pinned (verified: `4.1.2`). It loads React and GraphiQL from unpkg and runs an inline script, so this one dev-only route sets its own CSP: `script-src 'self' 'unsafe-inline' https://unpkg.com; style-src 'self' 'unsafe-inline' https://unpkg.com; img-src 'self' data: https:; connect-src 'self'`. Never mounted in production (the dev-routes test covers it). Send a token from the Headers tab: `{"Authorization": "Bearer …"}`.

## Required tests (GraphQL archetype)

- Every query and mutation anonymously → `UNAUTHENTICATED`, with no data.
- Another user's record → `NOT_FOUND`; a read-only token on any mutation → `FORBIDDEN`.
- Each mutation's validation failure → payload `errors`, and no top-level `errors`.
- REST and GraphQL return the same values for the same record (`API: both`).
- The N+1 guard for every association.
- Depth and complexity limits reject abusive queries, **and the standard introspection query passes** (`tests/app/introspection.graphql`).
- The schema snapshot.
- GET `/graphql` is 405.

## Gotchas agents get wrong

- A depth limit under 13: introspection breaks silently (only GraphiQL shows it).
- One `DataLoader` for the whole app (cached across requests and users). Create it per request.
- Returning `async_graphql::Error::new(e.to_string())` from a DB error: leaks SQL. Use `.gql()`/`gql_error`.
- Validation errors as top-level errors: clients can't tie them to fields. Use the payload.
- Skipping the policy because "the query is already scoped": scopes (`read`/`write`) still need checking.
- A `u64` argument with a default, sent `null` from a variable without a default (`query($per: Int)`): async-graphql rejects it. Give the variable a default (`$per: Int = 25`).
