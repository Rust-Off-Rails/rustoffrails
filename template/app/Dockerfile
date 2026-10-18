# syntax=docker/dockerfile:1
# Versions: the current stable Rust (`rustc --version` after `rustup update`).
ARG RUST_VERSION=1.99

# --- rust (plain htmx mode: no Node stage) ---
FROM rust:${RUST_VERSION}-trixie AS build
WORKDIR /app
# The dependencies first, in a layer of their own: it is rebuilt only when Cargo.toml or
# Cargo.lock change, and CI's layer cache (cache-to: type=gha) keeps it, so a code change
# compiles only the app. (A `--mount=type=cache` target is not in that cache: every CI run
# compiled every dependency again.)
COPY Cargo.toml Cargo.lock ./
# The build settings (.cargo/config.toml: rustflags, profiles) too: without them this layer and the
# final build use different flags, and every dependency compiles twice.
COPY .cargo .cargo
COPY migration/Cargo.toml migration/
RUN mkdir -p src migration/src \
    && echo 'fn main() {}' > src/main.rs && touch src/lib.rs migration/src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src migration/src
COPY . .
# Build the CSS with the pinned standalone Tailwind (bin/setup-tailwind).
RUN bin/setup-tailwind && bin/tailwindcss -i assets/app.css -o static/app.css --minify
# touch: the copied sources can be older than the placeholder build, and cargo goes by mtime.
RUN touch src/main.rs src/lib.rs migration/src/lib.rs \
    && cargo build --release --locked && cp target/release/offrails_app /usr/local/bin/app

# --- runtime ---
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates sqlite3 postgresql-client \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --create-home --uid 1000 app
WORKDIR /app
COPY --from=build /usr/local/bin/app /usr/local/bin/app
COPY --from=build /app/static ./static
COPY bin/docker-entrypoint /usr/local/bin/docker-entrypoint
RUN mkdir -p /app/storage /data && chown -R app:app /app /data
USER app
ENV APP_ENV=production PORT=3000 LOG_FORMAT=json
EXPOSE 3000
ENTRYPOINT ["docker-entrypoint"]
CMD ["app", "serve"]
