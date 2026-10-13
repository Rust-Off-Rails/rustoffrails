# Caching (Solid Cache / `Rails.cache` → `kernel/cache.rs`)

> **TL;DR**
> - Don't cache until it's measured or the spec requires it; log every cache in `docs/decisions.md`.
> - `state.cache.fetch(&key, ttl, || async {…})`: moka in memory (L1, only for TTL ≥ 60s) in front of the `cache_entries` table.
> - Never cache HTML containing the flash, forms or the current user; never fill a cache inside a transaction that may roll back.
> - Bound keys built from user input (hash them, short TTL).
>
> Read the rest when adding a cache, designing keys, or using HTTP caching.

**Don't cache until it's measured or the spec requires it.** Most Off Rails pages are a few indexed queries. A cache is a second source of truth with invalidation bugs attached. When you add one, log the reason in `docs/decisions.md`.

| Rails | Off Rails |
|-------|-----------|
| `Rails.cache.fetch(key, expires_in:) { … }` | `state.cache.fetch(&key, ttl, \|\| async { … }).await?` |
| `Rails.cache.delete(key)` | `state.cache.delete(&key).await?` |
| `delete_matched` | `state.cache.delete_prefix("posts/").await?` |
| `cache_key_with_version` | `format!("posts/{}-{}", post.id, post.updated_at.timestamp_micros())` |
| Solid Cache | `cache_entries` table (L2) + `moka` (L1) |
| Fragment caching (`cache @post do`) | Cache the *data* the view needs, not HTML. Askama rendering is fast |
| `fresh_when` / `stale?` | Optional ETag/Last-Modified helpers on show pages |

## Table

```
cache_entries:
  key         string pk          // ≤ 250 chars
  value       text               // serde_json
  expires_at  timestamp_with_time_zone
index cache_entries_expires_at_idx (expires_at)
```

`CleanupJob` deletes expired rows hourly.

## Kernel spec (`src/kernel/cache.rs`)

```rust
#[derive(Clone)]
pub struct Cache { l1: moka::future::Cache<String, Arc<String>>, db: DatabaseConnection }

impl Cache {
    pub fn new(db: DatabaseConnection) -> Self;          // L1: 10_000 entries, 60s TTL
    pub async fn fetch<T, F, Fut>(&self, key: &str, ttl: Duration, compute: F) -> Result<T, AppError>
    where T: Serialize + DeserializeOwned, F: FnOnce() -> Fut, Fut: Future<Output = Result<T, AppError>>;
    pub async fn read<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, AppError>;
    pub async fn write<T: Serialize>(&self, key: &str, value: &T, ttl: Duration) -> Result<(), AppError>;
    pub async fn delete(&self, key: &str) -> Result<(), AppError>;
    pub async fn delete_prefix(&self, prefix: &str) -> Result<(), AppError>;
    pub async fn delete_expired(&self) -> Result<u64, AppError>;   // CleanupJob
}
```

- `fetch`: `read`, else compute, `write`, return.
- `read`: L1 hit → return. L2 hit (not expired) → return, and copy into L1 **only if the entry lives ≥ 60s more**. Otherwise a short-TTL entry could outlive its TTL in L1.
- `write`: upsert L2 with `insert(..).on_conflict(OnConflict::column(key).update_columns([value, expires_at]))`. Fill L1 only when `ttl >= 60s` (moka's per-cache TTL is 60s).
- L1 staleness is bounded at 60s: other processes' deletes only reach L2. If a spec can't tolerate that, bypass L1 for that key (`fetch_l2_only`).
- `delete_prefix` uses `LIKE 'prefix%'` on L2 with `%`, `_` and `\` escaped and `.like(LikeExpr::new(pattern).escape('\\'))` (sea-query 1.0). Without the escape, `books_` also deletes `booksX…`. On L1 it uses `invalidate_entries_if` (enable moka's `support_invalidation_closures()`).
- `delete_expired() -> u64` deletes expired L2 rows. `CleanupJob` calls it hourly.
- Deserialization failure (a struct changed shape) counts as a miss. Recompute and overwrite.

## Key design

- **Version keys with `updated_at`**, as Rails does: `posts/42-1728740000123456`. Updating the row changes the key, so explicit invalidation is rarely needed.
- For collections: `posts/index/{max_updated_at}-{count}`, computed by one cheap aggregate query.
- Prefix every key with the resource (`posts/`, `users/`), so `delete_prefix` works.
- **Never** put per-user data in a shared key without the user id in it (`dashboards/user:7/…`).

## HTTP caching (optional)

For public, read-heavy show pages: compute an ETag from `(id, updated_at)`. If it matches `If-None-Match`, return 304. Put this in a small helper in `controllers/application.rs`, used per handler. Never use it on pages with per-user content or flash messages.

Static assets: `kernel/assets.rs` sets `Cache-Control: public, max-age=31536000, immutable` on digested URLs (see the frontend guides). Don't set long cache headers anywhere else.

## Gotchas agents get wrong

- Caching rendered HTML that contains the CSRF-relevant form, the flash, or the current user's name.
- Caching inside a transaction and then rolling back, which leaves the cache ahead of the DB. Compute cache values from committed data only, outside transactions.
- Unbounded keys from user input (search queries) filling the table. Cap them with a hash: `search/{sha256(q)[..16]}`, plus a short TTL.
- Forgetting that tests share nothing. `TestApp` gets a fresh `Cache`, so don't write tests that depend on cache state between requests unless that's what they test.

## Testing

Test the behavior the cache must preserve: after an update, the page shows the new value. That proves the versioned key or invalidation works. Kernel tests (`tests/app/kernel_test.rs`, both backends) cover hit, miss, expiry, `delete_prefix` with a `_` in the prefix, `delete_expired`, and a short-TTL entry never being served from L1 after it expired in L2.
