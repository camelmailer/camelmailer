//! The `camelmailer` CLI — the Rust port of `bin/postal`, dispatching the
//! process roles from a single binary.

use camelmailer_api::{
    build_router, build_server_router, tracking_router, ApiState, TrackingState,
};
use camelmailer_core::{AdminStore, MemorySink, MemoryStore, MessageSink, Store, TrackingStore};
use camelmailer_db::{PgMessageSink, PgStore};
use camelmailer_smtp::SmtpServer;
use camelmailer_worker::Worker;
use std::process::ExitCode;
use std::sync::Arc;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("web-server") => run_async(web_server()),
        Some("smtp-server") => run_async(smtp_server()),
        Some("worker") => run_async(worker()),
        Some("initialize") | Some("update") | Some("upgrade") => run_async(initialize()),
        Some("make-admin-api-key") => {
            let name = args.get(2).cloned().unwrap_or_else(|| "cli".to_string());
            run_async(make_admin_api_key(name))
        }
        Some("version") => {
            println!("CamelMailer v{VERSION}");
            ExitCode::SUCCESS
        }
        _ => {
            print_usage();
            ExitCode::SUCCESS
        }
    }
}

fn run_async(future: impl std::future::Future<Output = std::io::Result<()>>) -> ExitCode {
    let runtime = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    match runtime.block_on(future) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn load_config() -> camelmailer_config::Config {
    match camelmailer_config::Config::load_from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error}");
            std::process::exit(1);
        }
    }
}

fn postgres_enabled(config: &camelmailer_config::Config) -> bool {
    config.postgres.enabled || std::env::var("DATABASE_URL").is_ok()
}

async fn connect_pg(config: &camelmailer_config::Config) -> std::io::Result<PgStore> {
    let url = config.postgres.url();
    let pool = camelmailer_db::connect(&url, config.postgres.pool_size)
        .await
        .map_err(std::io::Error::other)?;
    Ok(PgStore::new(pool))
}

async fn initialize() -> std::io::Result<()> {
    let config = load_config();
    let store = connect_pg(&config).await?;
    camelmailer_db::migrate(store.pool())
        .await
        .map_err(std::io::Error::other)?;
    println!("Database is up to date.");
    Ok(())
}

async fn make_admin_api_key(name: String) -> std::io::Result<()> {
    let config = load_config();
    if !postgres_enabled(&config) {
        return Err(std::io::Error::other(
            "make-admin-api-key requires PostgreSQL (postgres.enabled: true or DATABASE_URL)",
        ));
    }
    let store = connect_pg(&config).await?;
    let key = camelmailer_core::token::generate_key();
    store
        .create_admin_api_key(&name, &key)
        .await
        .map_err(std::io::Error::other)?;
    println!("Admin API key '{name}' created:");
    println!("{key}");
    Ok(())
}

async fn web_server() -> std::io::Result<()> {
    let config = load_config();
    let global_key = config.camelmailer.admin_api_key.clone();

    let (state, tracking) = if postgres_enabled(&config) {
        // One Postgres store, shared as the admin store, the tenant-scoped
        // server store, and the tracking store.
        let pg = Arc::new(connect_pg(&config).await?);
        let state = ApiState::with_server_store(pg.clone(), pg.clone(), global_key);
        let tracking: Arc<dyn TrackingStore> = pg;
        (state, Some(tracking))
    } else {
        tracing::warn!("postgres is not enabled; using in-memory storage (non-persistent)");
        let memory = Arc::new(MemoryStore::new());
        let state = ApiState::with_server_store(memory.clone(), memory, global_key);
        (state, None)
    };

    let mut router = build_router(state.clone()).merge(build_server_router(state));
    if let Some(tracking) = tracking {
        router = router.merge(tracking_router(std::sync::Arc::new(TrackingState {
            store: tracking,
        })));
    }

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(config.web_server.default_port);
    let bind_address = std::env::var("BIND_ADDRESS")
        .unwrap_or_else(|_| config.web_server.default_bind_address.clone());

    let listener = tokio::net::TcpListener::bind((bind_address.as_str(), port)).await?;
    tracing::info!(%bind_address, port, "camelmailer web server listening");
    axum::serve(listener, router).await
}

async fn worker() -> std::io::Result<()> {
    let config = load_config();
    if !postgres_enabled(&config) {
        return Err(std::io::Error::other(
            "the worker requires PostgreSQL (postgres.enabled: true or DATABASE_URL)",
        ));
    }
    let store = connect_pg(&config).await?;
    Worker::new(&config, store)
        .run()
        .await
        .map_err(std::io::Error::other)
}

async fn smtp_server() -> std::io::Result<()> {
    let config = load_config();
    let (store, sink): (Arc<dyn Store>, Arc<dyn MessageSink>) = if postgres_enabled(&config) {
        let store = connect_pg(&config).await?;
        (Arc::new(store.clone()), Arc::new(PgMessageSink::new(store)))
    } else {
        tracing::warn!("postgres is not enabled; using in-memory storage (non-persistent)");
        (Arc::new(MemoryStore::new()), Arc::new(MemorySink::new()))
    };
    SmtpServer::new(config, store, sink).run().await
}

fn print_usage() {
    println!("Usage: camelmailer [command]");
    println!();
    println!("Server components:");
    println!();
    println!(" * web-server - run the web server (Admin API)");
    println!(" * smtp-server - run the SMTP server");
    println!(" * worker - run the delivery worker");
    println!();
    println!("Setup/upgrade tools:");
    println!();
    println!(" * initialize - create/upgrade the PostgreSQL schema");
    println!(" * make-admin-api-key [name] - create an Admin API key");
    println!();
    println!("Other tools:");
    println!();
    println!(" * version - show the current CamelMailer version");
}
