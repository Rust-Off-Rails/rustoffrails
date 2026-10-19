//! The `app` binary is `bin/rails`: serve, db …, routes, task …, health, introspect.

use std::{net::SocketAddr, process::Stdio};

use anyhow::Context;
use clap::{Parser, Subcommand};
use migration::{Migrator, MigratorTrait};
use offrails_app::{
    config::{AppEnv, Config, LogFormat},
    controllers, kernel, routes,
    state::AppState,
    tasks,
};
use tokio::net::TcpListener;
use tracing_subscriber::{Layer, Registry, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "app", about = "Offrails App")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web server (default).
    Serve,
    /// Database commands.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    /// Print the route table and rewrite docs/routes.md.
    Routes,
    /// Container healthcheck: GET /up on 127.0.0.1:$PORT, exit 0 on 200 (no curl in the image).
    Health,
    /// Development only: the app's state as JSON (routes, migrations, CSS, config redacted).
    Introspect,
    /// Run a task from src/tasks.
    Task { name: String, args: Vec<String> },
    // samples: cli commands
}

#[derive(Subcommand)]
enum DbCommand {
    /// Apply pending migrations.
    Migrate,
    /// Roll back the last migration(s).
    Rollback {
        #[arg(long, default_value_t = 1)]
        steps: u32,
    },
    /// Show migration status.
    Status,
    /// Run the seed task (idempotent).
    Seed,
    /// Migrate, then seed.
    Setup,
    /// Drop everything, migrate, seed. Refuses unless APP_ENV=development.
    Reset,
    /// Open sqlite3 or psql on DATABASE_URL.
    Console,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // The route table is a constant: no config, no database, no APP_ENV needed.
    if matches!(cli.command, Some(Command::Routes)) {
        return print_routes();
    }
    let config = Config::from_env()?;
    // WHY: a lean runtime by default (APP_WORKER_THREADS=2, APP_BLOCKING_THREADS=16) instead of
    // one worker per core and 512 blocking threads: each idle thread holds its stack and an
    // allocator arena (checked by .offrails/tools/ci/memory-budget.sh).
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.worker_threads)
        .max_blocking_threads(config.blocking_threads)
        .enable_all()
        .build()
        .context("starting the Tokio runtime")?
        .block_on(run(cli, config))
}

async fn run(cli: Cli, config: Config) -> anyhow::Result<()> {
    init_tracing(&config);
    for warning in config.boot_warnings() {
        tracing::warn!("{warning}");
    }

    match cli.command.unwrap_or(Command::Serve) {
        // samples: cli dispatch
        // WHY: the anchor heads the match: after a last arm ending in `}`, rustfmt pulls a
        // trailing comment onto that brace's line, and the next part's wiring would refuse.
        Command::Serve => serve(config).await,
        Command::Db { command } => db_command(config, command).await,
        Command::Routes => print_routes(),
        Command::Health => health(&config).await,
        Command::Task { name, args } => {
            let state = app_state(config).await?;
            tasks::run(&state, &name, args).await
        }
        Command::Introspect => introspect(config).await,
    }
}

fn init_tracing(config: &Config) {
    let directives = std::env::var("RUST_LOG").unwrap_or_else(|_| {
        match config.env {
            AppEnv::Production => "info",
            _ => "info,offrails_app=debug,tower_http=debug",
        }
        .to_owned()
    });
    // SECURITY: the filter caps targets that would log raw request URIs (tokens), whatever
    // RUST_LOG says (test: tokens_in_request_uris_never_reach_the_logs).
    let filter = kernel::redact::log_filter(&directives);
    // Layers samples add (a dashboard's log tail and traces), beside the formatter.
    let layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![
        // samples: tracing layers
    ];
    let registry = tracing_subscriber::registry()
        .with(sample_layers(layers))
        .with(filter);
    match config.log_format {
        LogFormat::Json => registry
            .with(tracing_subscriber::fmt::layer().json())
            .init(),
        LogFormat::Pretty => registry.with(tracing_subscriber::fmt::layer()).init(),
    }
}

/// The samples' layers, or none at all.
// WHY: an empty `Vec` layer answers `Interest::never()` for every callsite, which silences the
// whole subscriber (no log line at all, warnings included) in an app with no sample layers;
// `None` is a true no-op (test: an_app_without_sample_layers_still_logs).
fn sample_layers(
    layers: Vec<Box<dyn Layer<Registry> + Send + Sync>>,
) -> Option<Vec<Box<dyn Layer<Registry> + Send + Sync>>> {
    (!layers.is_empty()).then_some(layers)
}

/// Resolves on Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Connects, then builds the state (the same two steps `TestApp` takes with its own database).
async fn app_state(config: Config) -> anyhow::Result<AppState> {
    let db = kernel::db::connect(&config.database_url, config.database_pool).await?;
    AppState::new(config, db)
}

async fn serve(config: Config) -> anyhow::Result<()> {
    // `static/app.css` is built output and git-ignored: serving without it is an unstyled app.
    if let Some(problem) = kernel::assets::app_css_problem() {
        if config.is_production() {
            // The Docker image builds the CSS, so this means a broken build.
            anyhow::bail!(problem);
        }
        tracing::warn!("{problem}");
    }
    // APP_BIND: 127.0.0.1 in development (this machine only), 0.0.0.0 in production.
    let addr = SocketAddr::new(config.bind, config.port);
    let state = app_state(config).await?;
    let service =
        routes::router(state.clone())?.into_make_service_with_connect_info::<SocketAddr>();
    // samples: serve start

    // One bind for both branches: a taken port fails here, once, with a clear error.
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let drain = state.config.shutdown_drain; // APP_SHUTDOWN_DRAIN_SECS, default 5
    let (stopping_tx, mut stopping) = tokio::sync::watch::channel(false);
    let stop = async move {
        shutdown_signal().await;
        // samples: serve stop
        let _ = stopping_tx.send(true);
    };
    match state.config.tls.clone() {
        None => {
            // axum 0.8's graceful shutdown has no timeout: race it against the drain clock.
            let drained = async move {
                let _ = stopping.wait_for(|s| *s).await;
                tokio::time::sleep(drain).await
            };
            tokio::select! {
                r = axum::serve(listener, service).with_graceful_shutdown(stop).into_future() => r?,
                () = drained => tracing::warn!(?drain, "shutdown drain timed out; dropping open connections"),
            }
        }
        Some(tls) => {
            // sqlx/lettre bring ring, axum-server brings aws-lc-rs: rustls can't pick a provider alone.
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
            let cfg = axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert, &tls.key)
                .await
                .with_context(|| {
                    format!(
                        "loading APP_TLS_CERT={} / APP_TLS_KEY={}",
                        tls.cert, tls.key
                    )
                })?;
            // Development only: the same router and state on plain HTTP beside the TLS port.
            if let Some(port) = state.config.dev_http_port {
                let plain = TcpListener::bind(SocketAddr::new(state.config.bind, port))
                    .await
                    .with_context(|| format!("binding APP_DEV_HTTP_PORT={port}"))?;
                let (service, mut stopping) = (service.clone(), stopping.clone());
                tokio::spawn(async move {
                    let stopped = async move {
                        let _ = stopping.wait_for(|s| *s).await;
                    };
                    let _ = axum::serve(plain, service)
                        .with_graceful_shutdown(stopped)
                        .await;
                });
            }
            let handle = axum_server::Handle::new();
            let graceful = handle.clone();
            tokio::spawn(async move {
                stop.await;
                graceful.graceful_shutdown(Some(drain));
            });
            // Plain http:// on this port (a bare `localhost:PORT`) gets a 307 to https:// (kernel/tls.rs).
            let acceptor = axum_server::tls_rustls::RustlsAcceptor::new(cfg)
                .acceptor(kernel::tls::HttpToHttps);
            axum_server::from_tcp(listener.into_std()?)?
                .acceptor(acceptor)
                .handle(handle)
                .serve(service)
                .await?;
        }
    }
    Ok(())
}

async fn db_command(config: Config, command: DbCommand) -> anyhow::Result<()> {
    match command {
        DbCommand::Migrate => {
            let db = kernel::db::connect(&config.database_url, config.database_pool).await?;
            Migrator::up(&db, None).await?;
            tracing::info!("migrations applied");
        }
        DbCommand::Rollback { steps } => {
            let db = kernel::db::connect(&config.database_url, config.database_pool).await?;
            Migrator::down(&db, Some(steps)).await?;
            tracing::info!(steps, "rolled back");
        }
        DbCommand::Status => {
            let db = kernel::db::connect(&config.database_url, config.database_pool).await?;
            Migrator::status(&db).await?;
        }
        DbCommand::Seed => {
            let state = app_state(config).await?;
            tasks::seed::run(&state, Vec::new()).await?;
        }
        DbCommand::Setup => {
            let state = app_state(config).await?;
            Migrator::up(&state.db, None).await?;
            tasks::seed::run(&state, Vec::new()).await?;
        }
        DbCommand::Reset => {
            if config.env != AppEnv::Development {
                anyhow::bail!("db reset only runs with APP_ENV=development");
            }
            let state = app_state(config).await?;
            Migrator::fresh(&state.db).await?;
            tasks::seed::run(&state, Vec::new()).await?;
        }
        DbCommand::Console => return console(&config.database_url),
    }
    Ok(())
}

/// `rails dbconsole`: arguments are passed as a vector, never through a shell.
fn console(database_url: &str) -> anyhow::Result<()> {
    let mut command = if database_url.starts_with("sqlite:") {
        let path = database_url
            .trim_start_matches("sqlite://")
            .trim_start_matches("sqlite:")
            .split('?')
            .next()
            .unwrap_or_default();
        let mut c = std::process::Command::new("sqlite3");
        c.arg(path);
        c
    } else {
        let mut c = std::process::Command::new("psql");
        c.arg(database_url);
        c
    };
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("starting the database console (is sqlite3/psql installed?)")?;
    if !status.success() {
        anyhow::bail!("console exited with {status}");
    }
    Ok(())
}

/// `GET /up` over a plain TCP connection, so the slim runtime image needs no curl or wget. With
/// direct TLS it only checks that the port accepts a connection (a plain probe can't speak TLS).
async fn health(config: &Config) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let port = config.port;
    let tls = config.tls.is_some();
    let probe = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        if tls {
            return anyhow::Ok(b"HTTP/1.1 200".to_vec());
        }
        stream
            .write_all(b"GET /up HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut head = vec![0u8; 12];
        stream.read_exact(&mut head).await?;
        anyhow::Ok(head)
    };
    let head = tokio::time::timeout(std::time::Duration::from_secs(4), probe)
        .await
        .context("timed out")??;
    // "HTTP/1.1 200"
    if head.ends_with(b" 200") {
        Ok(())
    } else {
        anyhow::bail!("unhealthy: {}", String::from_utf8_lossy(&head))
    }
}

fn print_routes() -> anyhow::Result<()> {
    let mut md = String::from(
        "# Routes\n\n<!-- Generated by `cargo run -- routes`. Do not edit. -->\n\n| Method | Path | Handler | Access |\n|--------|------|---------|--------|\n",
    );
    let mut out = String::new();
    for (method, path, handler, access) in routes::ROUTES {
        md.push_str(&format!(
            "| {method} | `{path}` | `{handler}` | {} |\n",
            access.as_str()
        ));
        out.push_str(&format!(
            "{method:<6} {path:<44} {handler:<32} {}\n",
            access.as_str()
        ));
    }
    print!("{out}");
    std::fs::create_dir_all("docs")?;
    std::fs::write("docs/routes.md", md).context("writing docs/routes.md")?;
    Ok(())
}

/// `app introspect`: the snapshot `/dev/introspect.json` serves, without the live parts.
async fn introspect(config: Config) -> anyhow::Result<()> {
    anyhow::ensure!(
        config.is_development(),
        "app introspect is development only (APP_ENV is {:?})",
        config.env
    );
    let state = app_state(config).await?;
    let snapshot =
        kernel::introspect::snapshot(&state, controllers::dev::FRONTEND_MODE, false).await?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_without_sample_layers_still_logs() {
        let subscriber = tracing_subscriber::registry()
            .with(sample_layers(Vec::new()))
            .with(tracing_subscriber::EnvFilter::new("info"));
        tracing::subscriber::with_default(subscriber, || {
            assert!(tracing::enabled!(tracing::Level::WARN));
            assert!(!tracing::enabled!(tracing::Level::DEBUG));
        });
    }
}
