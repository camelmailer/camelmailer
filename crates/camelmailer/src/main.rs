//! The `camelmailer` CLI — the Rust port of `bin/postal`, dispatching the
//! process roles from a single binary.

use camelmailer_api::{
    build_auth_router, build_oidc_router, build_router, build_saml_router, build_scim_router,
    build_server_router, cors_layer, tracking_router, ApiState, TrackingState,
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
        Some("make-user") => run_async(make_user(args[2..].to_vec())),
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

/// `make-user <email> <first-name> <last-name> [--admin]` — create a user
/// account. The password comes from `CAMELMAILER_USER_PASSWORD` or is
/// generated and printed.
async fn make_user(args: Vec<String>) -> std::io::Result<()> {
    let config = load_config();
    if !postgres_enabled(&config) {
        return Err(std::io::Error::other(
            "make-user requires PostgreSQL (postgres.enabled: true or DATABASE_URL)",
        ));
    }
    let admin = args.iter().any(|a| a == "--admin");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let Some(email) = positional.first().filter(|e| e.contains('@')) else {
        return Err(std::io::Error::other(
            "usage: camelmailer make-user <email> [first-name] [last-name] [--admin]",
        ));
    };
    let first_name = positional.get(1).cloned().cloned().unwrap_or_default();
    let last_name = positional.get(2).cloned().cloned().unwrap_or_default();

    let (password, generated) = match std::env::var("CAMELMAILER_USER_PASSWORD") {
        Ok(password) if !password.is_empty() => (password, false),
        _ => (camelmailer_core::token::generate_key(), true),
    };
    if (password.len() as u32) < config.auth.minimum_password_length {
        return Err(std::io::Error::other(format!(
            "password must be at least {} characters",
            config.auth.minimum_password_length
        )));
    }
    let digest = camelmailer_core::auth::hash_password(&password).map_err(std::io::Error::other)?;

    let store = connect_pg(&config).await?;
    use camelmailer_core::{AdminStore, AuthStore};
    let user = store
        .create_user(camelmailer_core::NewUser {
            email_address: email.to_string(),
            first_name,
            last_name,
            admin,
        })
        .await
        .map_err(std::io::Error::other)?;
    store
        .set_password_digest(user.id, &digest)
        .await
        .map_err(std::io::Error::other)?;

    println!(
        "User '{}' created{}.",
        user.email_address,
        if admin { " (global admin)" } else { "" }
    );
    if generated {
        println!("Generated password: {password}");
        println!("(set CAMELMAILER_USER_PASSWORD to choose one; change it after first login)");
    }
    Ok(())
}

async fn web_server() -> std::io::Result<()> {
    let config = load_config();
    let global_key = config.camelmailer.admin_api_key.clone();

    let (state, tracking) = if postgres_enabled(&config) {
        // One Postgres store, shared as the admin store, the tenant-scoped
        // server store, the account store, and the tracking store.
        let pg = Arc::new(connect_pg(&config).await?);
        let state = ApiState::full(
            pg.clone(),
            Some(pg.clone()),
            Some(pg.clone()),
            global_key,
            config.clone(),
        );
        let tracking: Arc<dyn TrackingStore> = pg;
        (state, Some(tracking))
    } else {
        tracing::warn!("postgres is not enabled; using in-memory storage (non-persistent)");
        let memory = Arc::new(MemoryStore::new());
        let state = ApiState::full(
            memory.clone(),
            Some(memory.clone()),
            Some(memory),
            global_key,
            config.clone(),
        );
        (state, None)
    };

    let mut router = build_router(state.clone())
        .merge(build_server_router(state.clone()))
        .merge(build_auth_router(state.clone()))
        .merge(build_oidc_router(state.clone()))
        .merge(build_saml_router(state.clone()))
        .merge(build_scim_router(state));
    if let Some(tracking) = tracking {
        router = router.merge(tracking_router(std::sync::Arc::new(TrackingState {
            store: tracking,
        })));
    }
    if let Some(cors) = cors_layer(&config.web_server.cors_origins) {
        tracing::info!(origins = ?config.web_server.cors_origins, "CORS enabled");
        router = router.layer(cors);
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
    println!(" * make-user <email> [first] [last] [--admin] - create a user account");
    println!();
    println!("Other tools:");
    println!();
    println!(" * version - show the current CamelMailer version");
}
