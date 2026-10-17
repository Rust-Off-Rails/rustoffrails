# syntax=docker/dockerfile:1
# Versions: the current stable Rust (`rustc --version` after `rustup update`).
ARG RUST_VERSION=1.99

# --- rust (plain htmx mode: no Node stage) ---
FROM rust:${RUST_VERSION}-trixie AS build
WORKDIR /app
COPY . .
# Build the CSS with the pinned standalone Tailwind (bin/setup-tailwind).
RUN bin/setup-tailwind && bin/tailwindcss -i assets/app.css -o static/app.css --minify
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked && cp target/release/offrails_app /usr/local/bin/app

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
