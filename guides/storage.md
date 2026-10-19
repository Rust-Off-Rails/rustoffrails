# Storage (Active Storage → `object_store` + `blobs`)

> **TL;DR**
> - Active Storage equivalent: `object_store` (local disk in dev, memory in tests, S3-compatible in prod) plus a `blobs` table; added with the first `attachment` field.
> - Storage keys are always random (never user filenames); validate size and type; raise `DefaultBodyLimit` on upload routes; an empty file input isn't an upload.
> - Purge blobs in a job after commit, never inside the request transaction; serve with authorization and a safe `Content-Disposition`.
> - object_store 0.14: import `ObjectStoreExt` for `put`/`get`/`delete`.
>
> Read the rest when adding uploads, serving files, or listing attachments without N+1.

> Verified on 2026-10-12 in the reference app (book covers): object_store 0.14.2 (`aws` feature), axum 0.8.9 `Multipart`, tests on SQLite and Postgres, and an upload through the real browser form.

| Rails | Off Rails |
|-------|-----------|
| `has_one_attached :cover` | `cover_blob_id: Option<i64>` FK to `blobs` (`on_delete SetNull`, indexed) |
| `has_many_attached :images` | Join table `post_images(post_id, blob_id, position)` |
| `ActiveStorage::Blob` | `models/blob.rs` (entity for `blobs`) |
| `storage.yml` services | `kernel/storage.rs`: local disk, S3-compatible, or memory (tests) |
| `rails_blob_path(blob)` | `paths::blob(&blob.key)` → `GET /blobs/{key}` |
| Direct uploads | Only if a spec needs files > ~25 MB (presigned PUT, log decision) |
| Variants (`image_processing`) | Only if a spec needs them (`image` crate, log decision) |
| `purge_later` | `blob::purge_later(&txn, blob_id)` enqueues `PurgeBlobJob` |

There's no polymorphic `attachments` table. Attachments are ordinary FKs, so the schema says exactly what owns what.

## Adding storage (first `attachment` field)

1. `cargo add object_store --features aws` and `cargo add bytes`.
2. **TLS:** `object_store` (via `reqwest` 0.13) brings rustls's `aws-lc-rs` provider next to the `ring` provider that `lettre`/`sqlx` use. **Checked 2026-10-12: no `install_default` is needed.** With both compiled in, real TLS handshakes to S3 (`s3.us-east-1.amazonaws.com`) and SMTPS (`smtp.gmail.com:465`) worked, because each crate passes its provider explicitly. If a future version panics with "no process-level CryptoProvider", add `rustls` (`--no-default-features --features ring`) and call `rustls::crypto::ring::default_provider().install_default()` first in `main` and `TestApp::new`.
3. Config: `StorageConfig` in `config.rs` (below), with `STORAGE_*` in `.env.example`.
4. Migration `create_blobs`, then this guide's kernel, model, job, controller and route.

## Table

```
blobs:
  id            big_pk_auto
  key           string, unique blobs_key_key   // random 32 bytes base64url; the object's path
  filename      string                          // sanitized display name only
  content_type  string                          // verified from magic bytes
  byte_size     big_integer
  checksum      string                          // sha256 hex
  created_at    timestamp_with_time_zone
```

Owner FKs to `blobs` use `on_delete(SetNull)` for single attachments and `Cascade` on join rows. Deleting the stored object is `PurgeBlobJob`'s job, never the database's or the request's.

## Config (`config.rs`)

```rust
pub enum StorageConfig {
    Local { path: String },                                   // STORAGE_SERVICE=local (default), STORAGE_PATH (default storage/files)
    S3 { bucket: String, endpoint: Option<String>, region: Option<String> },   // STORAGE_SERVICE=s3, STORAGE_BUCKET (required),
                                                              // STORAGE_ENDPOINT (R2/Tigris/MinIO), STORAGE_REGION, AWS_* credentials
    Memory,                                                   // Config::test()
}
```

Production on a single server with SQLite may use `local` on the persistent volume. Anything with more than one machine uses `s3`.

## Kernel spec (`src/kernel/storage.rs`)

```rust
use object_store::{GetResult, ObjectStore, ObjectStoreExt, aws::AmazonS3Builder,
                   local::LocalFileSystem, memory::InMemory, path::Path};

#[derive(Clone, Debug)]
pub struct Storage { store: Arc<dyn ObjectStore> }

impl Storage {
    pub fn from_config(c: &StorageConfig) -> anyhow::Result<Self>;   // Local: create_dir_all + LocalFileSystem::new_with_prefix
                                                                    // S3: AmazonS3Builder::from_env().with_bucket_name(..)[.with_endpoint/.with_region].build()
    pub async fn put(&self, key: &str, bytes: Bytes) -> anyhow::Result<()>;   // store.put(&Path::from(key), bytes.into())
    pub async fn get(&self, key: &str) -> anyhow::Result<GetResult>;          // stream it: .into_stream()
    pub async fn delete(&self, key: &str) -> anyhow::Result<()>;              // NotFound is Ok (purges retry)
    pub async fn exists(&self, key: &str) -> bool;                           // store.head(..).is_ok()
}

pub struct Upload { pub filename: String, pub content_type: String, pub bytes: Bytes }
pub struct MultipartForm<P> { pub params: P, pub files: Vec<(String, Upload)> }   // + take_file(field)
pub async fn read_multipart<P: DeserializeOwned>(m: Multipart) -> Result<MultipartForm<P>, AppError>;

pub struct Rules { pub max_bytes: usize, pub types: &'static [&'static str] }
pub const IMAGES: &[&str] = &["image/png", "image/jpeg", "image/webp"];
pub fn sniff(bytes: &[u8]) -> Option<&'static str>;            // PNG 89 50 4E 47 0D 0A 1A 0A, JPEG FF D8 FF, WebP RIFF....WEBP, PDF %PDF-
pub fn check(u: &Upload, rules: Rules) -> Result<&'static str, &'static str>;   // "is too large" | "has the wrong type"
pub fn sanitize_filename(name: &str) -> String;
```

- **object_store 0.14 moved `put`/`get`/`delete` onto the extension trait `ObjectStoreExt`**, which has a blanket impl for every `ObjectStore`, `dyn` included. Import it, or the calls don't resolve. `put` takes a `PutPayload` (`bytes.into()`). Check `Cargo.lock` before writing this module; these APIs move between minors.
- `read_multipart` reads every part with `field.bytes()`, which is bounded by the route's body limit.
  - Text fields are re-encoded with `serde_html_form` and deserialized into the same `Params` struct a plain `Form<P>` would use, so `Params` stays the permit list and stray fields (`owner_id`) are ignored.
  - An empty file input (no file chosen: empty filename and no bytes) is **not** an upload. Saving an edit form without picking a file keeps the existing attachment.
  - A part over the body limit → `AppError::PayloadTooLarge` (413). Malformed → 422.
- `check` requires the claimed `Content-Type` to be in the allowlist **and** equal to what the magic bytes say. A PNG claimed as JPEG and an SVG claimed as an image are both "the wrong type". Write the sniffing here, without a crate.
- `sanitize_filename` keeps the basename (splitting on `/` and `\`), replaces anything outside `[A-Za-z0-9._-]` with `_`, strips leading dots, and caps the result at 100 characters (`"file"` if empty). It's for display and `Content-Disposition` only, never a path.

## The model (`models/blob.rs`)

```rust
pub struct VerifiedUpload { filename: String, content_type: &'static str, bytes: Bytes }   // fields private: only check() makes one

pub fn check(upload: Upload, rules: Rules) -> Result<VerifiedUpload, &'static str>;        // pure
pub async fn create(db: &impl ConnectionTrait, storage: &Storage, u: VerifiedUpload) -> Result<Model, AppError>;
pub async fn find_by_key(db, key) -> Result<Model, AppError>;
pub async fn by_ids(db, ids) -> Result<HashMap<i64, Model>, AppError>;   // one is_in query for list pages
pub async fn purge_later(db, blob_id) -> Result<(), AppError>;          // enqueue PurgeBlobJob { blob_id }
pub async fn purge(db, storage, blob_id) -> Result<(), AppError>;       // delete object, then row; idempotent
```

- **Validation runs with the form's other fields.** The owner's `Params::validate(&self, cover: Option<Upload>)` calls `blob::check` and adds field errors ("Cover must be a PNG, JPEG or WebP image", "Cover is too large (maximum is 5 MB)"), so a bad title and a bad file show together on one re-render.
- `create`: random key (`kernel::session::generate_token`), sha256, `storage.put`, then insert the row on the caller's transaction. If the insert fails, delete the object. If the caller's transaction rolls back later, the object is orphaned. That's harmless; a sweep task can remove keys with no row.
- **Replacing** an attachment: create the new blob, point the FK at it, and `purge_later(&txn, old_id)`, all in one transaction. **Deleting** the owner: delete the row (children cascade), then `purge_later` its blobs in the same transaction.

## Uploading (controller)

```rust
pub async fn create(State(s): State<AppState>, ctx: Ctx, RequireUser(user): RequireUser, multipart: Multipart)
    -> Result<Response, AppError>
{
    let mut form = storage::read_multipart::<BookParams>(multipart).await?;
    let cover = form.take_file("cover");
    match form.params.validate(cover) { /* Ok(new) → book::create(&s.db, &s.storage, user.id, new); Err → 422 re-render with form.params */ }
}
```

- `Multipart` is the **last** argument (body extractor).
- **Raise the body limit on the upload routes only:** `.route("/books", get(index).post(create).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)))`. Set the limit a little above the largest allowed file plus the text fields (5 MB cover → 6 MB). The router-wide `DefaultBodyLimit` (2 MB) stays. The per-route layer is inner, so it wins (verified). Between 5 and 6 MB the user sees the field message; above 6 MB, 413.
- The form needs `enctype="multipart/form-data"`. htmx-boosted forms send it as `FormData` (verified).

## Serving

**Default: serve from the app origin via `GET /blobs/{key}`** (`controllers/blobs.rs`, `Access::User` in `ROUTES`). Every download goes through an authorization check, and the CSP stays `img-src 'self' data:`.

1. `RequireUser` (logged out → login redirect), then `blob::find_by_key`.
2. Find the owner through every model that references blobs (`book::with_cover(db, blob.id)`, …) and check its policy (`books::can_view`). A missing owner or no permission → **404**, so existence doesn't leak.
3. Stream it: `(headers, Body::from_stream(storage.get(&key).await?.into_stream()))`, with these headers:
   - `Content-Type` from the row;
   - `Content-Disposition: inline; filename="…"` for images, `attachment` otherwise;
   - `X-Content-Type-Options: nosniff`;
   - `Cache-Control: no-store`. The header middleware sets `no-store` on every response to a request with a session, and only fills headers a handler left unset, so a handler that writes `private, max-age=3600` here would override it and keep a private file in the browser cache for an hour after logout or revocation. Public, unauthenticated media is the exception: `public, max-age=…` is fine when the route needs no login.

Use **S3 presigned URLs** only when the interview or a spec calls for it (large or public media, offloading bandwidth). Then:
- add `presigned_get(key, ttl)` to `Storage` (object_store's `Signer`, S3 only) and redirect (303) to it after the same authorization. `ttl` is **minutes, not days** (5 by default): the URL is a bearer credential for that object, and nothing revokes it before it expires. Pass `response-content-disposition` (`attachment; filename="…"` unless it's an image) and `response-content-type` from the row, so S3 doesn't serve a client-chosen type;
- set `STORAGE_PUBLIC_HOST`, and add that host to the CSP `img-src` (and `media-src` for video) in `security.md`'s header layer;
- log the decision in `docs/decisions.md`.

## List pages (no N+1)

Load the page's owners, then their blobs in **one** query: `blob::by_ids(db, rows.iter().filter_map(|b| b.cover_blob_id))`, then map ids to `paths::blob(&key)` in the handler. The N+1 guard test (`assert_query_count_stable`) covers it.

## Gotchas agents get wrong

- Forgetting `use object_store::ObjectStoreExt` (0.14). `put`/`get`/`delete` aren't on `ObjectStore` itself any more.
- Using the uploaded filename (or anything user-supplied) as the storage key: path traversal and collisions. Keys are always random.
- Forgetting `DefaultBodyLimit` on the upload route, so uploads fail with 413 at axum's 2 MB default.
- Putting `Multipart` before other extractors (compile error) or reading the whole stream without a size cap.
- Treating an empty file input as an upload, which replaces or clears the existing attachment on every edit.
- Deleting the object inside the request transaction. If the transaction rolls back, the row points at nothing. Purge in a job after commit.
- Trusting `content_type` from the browser.
- `TestResponse.body` is lossy UTF-8. Compare binary downloads with `res.bytes`.

## Testing

- `TestApp::post_multipart(path, fields, files: &[(field, filename, content_type, bytes)])`. Tests use `StorageConfig::Memory`, and `app.state.storage.exists(key)` checks purges.
- Fixtures: a valid 1×1 PNG and JPEG magic bytes as `const`s in `tests/app/factories.rs`.
- Cover:
  - the happy path, where the page `<img src>` round-trips the same bytes, with `nosniff` and `inline`;
  - a sanitized filename;
  - SVG, a mismatched claimed type and PDF → 422 with the message, and nothing stored;
  - just over the spec's limit → 422; over the route limit → 413;
  - logged out → login redirect;
  - an unknown key or ownerless blob → 404;
  - replacing purges the old blob after `drain_jobs`;
  - deleting the owner purges, and the object is still there before the drain and gone after.
