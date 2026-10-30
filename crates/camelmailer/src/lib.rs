//! The `camelmailer` CLI as a library: the process roles of the single
//! binary (`web-server`, `smtp-server`, `worker`, `initialize`, ...), plus
//! the extension points a custom build uses.
//!
//! The open-source binary is `main.rs`, which calls
//! [`main_with`]`(Extensions::default())`. A separate crate that wants to
//! ship its own billing provider (or extra HTTP routes) depends on this
//! crate and the API crate and builds its own binary:
//!
//! ```no_run
//! use std::sync::Arc;
//! use camelmailer::Extensions;
//! use camelmailer_api::{BillingProvider, NoBilling};
//!
//! fn main() -> std::process::ExitCode {
//!     camelmailer::main_with(Extensions {
//!         billing: Some(Arc::new(|config: &camelmailer_config::Config| {
//!             // read config.billing.options here
//!             let _ = config;
//!             Ok(Arc::new(NoBilling) as Arc<dyn BillingProvider>)
//!         })),
//!         ..Extensions::default()
//!     })
//! }
//! ```

use camelmailer_api::{build_app, run_campaign_scheduler, ApiState, BillingProvider};
use camelmailer_core::{AdminStore, MemorySink, MemoryStore, MessageSink, Store, TrackingStore};
use camelmailer_db::{PgMessageSink, PgStore};
use camelmailer_smtp::SmtpServer;
use camelmailer_worker::Worker;
use std::process::ExitCode;
use std::sync::Arc;

pub use camelmailer_api;
pub use camelmailer_config;
pub use camelmailer_core;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Builds the billing provider from the loaded configuration (typically
/// from `billing.options`). An `Err` stops the web server at startup.
pub type BillingFactory = Arc<
    dyn Fn(&camelmailer_config::Config) -> Result<Arc<dyn BillingProvider>, String> + Send + Sync,
>;

/// Adds routes to the web server's application, with access to the state.
pub type RouterHook = Arc<dyn Fn(Arc<ApiState>) -> axum::Router + Send + Sync>;

/// Runs once when the web server starts, after the state is built and
/// before it begins to listen. It is called inside the Tokio runtime, so it
/// may `tokio::spawn` background tasks (periodic syncs and the like); it
/// must not block.
pub type StartHook = Arc<dyn Fn(Arc<ApiState>) + Send + Sync>;

/// What a custom build plugs into the stock process roles. The default is
/// the open-source behaviour: no billing provider, no extra routes, no
/// start hook.
#[derive(Clone, Default)]
pub struct Extensions {
    /// The billing provider. Used only when `billing.enabled` is set;
    /// without it billing stays off ([`camelmailer_api::NoBilling`]).
    pub billing: Option<BillingFactory>,
    /// Extra routes merged into the web server's router.
    pub routes: Option<RouterHook>,
    /// Called once in the `web-server` role with the built state, e.g. to
    /// spawn background tasks. Only the web server runs it, so a task
    /// started here runs once per web-server process.
    pub on_start: Option<StartHook>,
}

/// The CLI entry point of the stock binary: [`main_with`] without
/// extensions.
pub fn main() -> ExitCode {
    main_with(Extensions::default())
}

/// The CLI entry point with extensions: parses `std::env::args()` and runs
/// the requested role.
pub fn main_with(extensions: Extensions) -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("web-server") => run_async(web_server_with(extensions)),
        Some("smtp-server") => run_async(smtp_server()),
        Some("worker") => run_async(worker()),
        Some("initialize") | Some("update") | Some("upgrade") => run_async(initialize()),
        Some("make-admin-api-key") => run_async(make_admin_api_key(args[2..].to_vec())),
        Some("make-user") => run_async(make_user(args[2..].to_vec())),
        Some("version") => {
            println!("Camelmailer v{VERSION}");
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

pub async fn initialize() -> std::io::Result<()> {
    let config = load_config();
    let store = connect_pg(&config).await?;
    camelmailer_db::migrate(store.pool())
        .await
        .map_err(std::io::Error::other)?;
    println!("Database is up to date.");
    Ok(())
}

/// `make-admin-api-key [name] [--org <permalink>] [--server <permalink>]` —
/// create an admin API key, optionally scoped to an organization or, more
/// narrowly, to one server inside it. Unscoped keys have full access.
async fn make_admin_api_key(args: Vec<String>) -> std::io::Result<()> {
    let config = load_config();
    if !postgres_enabled(&config) {
        return Err(std::io::Error::other(
            "make-admin-api-key requires PostgreSQL (postgres.enabled: true or DATABASE_URL)",
        ));
    }
    let flag_value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let org_permalink = flag_value("--org");
    let server_permalink = flag_value("--server");
    // the first positional argument that is not a flag or a flag's value
    let mut name = "cli".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--org" | "--server" => index += 2,
            value => {
                name = value.to_string();
                break;
            }
        }
    }

    let store = connect_pg(&config).await?;
    let (organization_id, server_id) = match (&org_permalink, &server_permalink) {
        (None, Some(_)) => {
            return Err(std::io::Error::other(
                "--server also needs --org <permalink>",
            ))
        }
        (None, None) => (None, None),
        (Some(org), server) => {
            let organization = store
                .organization_by_permalink(org)
                .await
                .map_err(std::io::Error::other)?
                .ok_or_else(|| std::io::Error::other(format!("organization {org:?} not found")))?;
            let server_id = match server {
                Some(permalink) => Some(
                    store
                        .server_by_permalink(organization.id, permalink)
                        .await
                        .map_err(std::io::Error::other)?
                        .ok_or_else(|| {
                            std::io::Error::other(format!(
                                "server {permalink:?} not found in organization {org:?}"
                            ))
                        })?
                        .id,
                ),
                None => None,
            };
            (Some(organization.id), server_id)
        }
    };
    let key = camelmailer_core::token::generate_key();
    store
        .create_admin_api_key_record(&name, &key, organization_id, server_id)
        .await
        .map_err(std::io::Error::other)?;
    let scope = match (&org_permalink, &server_permalink) {
        (Some(org), Some(server)) => format!(" (scoped to {org}/{server})"),
        (Some(org), None) => format!(" (scoped to {org})"),
        _ => String::new(),
    };
    println!("Admin API key '{name}'{scope} created:");
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

/// The installed billing provider: the extension's, when billing is
/// enabled and a factory is given; [`camelmailer_api::NoBilling`] otherwise.
pub fn billing_provider(
    config: &camelmailer_config::Config,
    extensions: &Extensions,
) -> std::io::Result<Arc<dyn BillingProvider>> {
    match (&extensions.billing, config.billing.enabled) {
        (Some(factory), true) => {
            let provider = factory(config).map_err(|error| {
                std::io::Error::other(format!("billing provider failed to start: {error}"))
            })?;
            tracing::info!(provider = provider.name(), "billing enabled");
            Ok(provider)
        }
        (None, true) => {
            tracing::warn!(
                "billing.enabled is set, but this build has no billing provider; billing stays off"
            );
            Ok(Arc::new(camelmailer_api::NoBilling))
        }
        _ => Ok(Arc::new(camelmailer_api::NoBilling)),
    }
}

/// Build the web server's state from configuration: Postgres when enabled
/// (else in-memory), the SPF resolver, per-organization SSO storage and the
/// billing provider. Returns the tracking store alongside (Postgres only).
pub async fn build_state(
    config: &camelmailer_config::Config,
    extensions: &Extensions,
) -> std::io::Result<(Arc<ApiState>, Option<Arc<dyn TrackingStore>>)> {
    let global_key = config.camelmailer.admin_api_key.clone();
    let billing = billing_provider(config, extensions)?;

    // The domain health check evaluates a sending domain's SPF against this
    // installation's sending addresses, which needs TXT, A/AAAA and MX rather
    // than the TXT-only verification resolver.
    let spf_resolver: Arc<dyn camelmailer_core::SpfResolver> =
        Arc::new(camelmailer_api::dns::HickorySpfResolver::new());

    if postgres_enabled(config) {
        // One Postgres store, shared as the admin store, the tenant-scoped
        // server store, the account store, and the tracking store.
        let pg = Arc::new(connect_pg(config).await?);
        let state = ApiState::full_with_billing(
            pg.clone(),
            Some(pg.clone()),
            Some(pg.clone()),
            global_key,
            config.clone(),
            Some(billing),
        )
        .with_org_sso_store(pg.clone())
        .with_spf_resolver(spf_resolver);
        let tracking: Arc<dyn TrackingStore> = pg;
        Ok((state, Some(tracking)))
    } else {
        tracing::warn!("postgres is not enabled; using in-memory storage (non-persistent)");
        let memory = Arc::new(MemoryStore::new());
        let state = ApiState::full_with_billing(
            memory.clone(),
            Some(memory.clone()),
            Some(memory.clone()),
            global_key,
            config.clone(),
            Some(billing),
        )
        .with_org_sso_store(memory)
        .with_spf_resolver(spf_resolver);
        Ok((state, None))
    }
}

/// The `web-server` role without extensions.
pub async fn web_server() -> std::io::Result<()> {
    web_server_with(Extensions::default()).await
}

/// The `web-server` role: build the state and the application, start the
/// campaign scheduler, and serve until the process ends.
pub async fn web_server_with(extensions: Extensions) -> std::io::Result<()> {
    let config = load_config();
    for warning in config.deprecation_warnings() {
        tracing::warn!("{warning}");
    }
    let (state, tracking) = build_state(&config, &extensions).await?;

    // The in-process campaign scheduler: auto-sends scheduled campaigns when
    // due, so a local install needs no separate worker for planned sends. Only
    // started in the web-server role.
    tokio::spawn(run_campaign_scheduler(state.clone()));

    let mut router = build_app(state.clone(), tracking);
    if let Some(routes) = &extensions.routes {
        router = router.merge(routes(state.clone()));
    }
    if let Some(on_start) = &extensions.on_start {
        on_start(state);
    }
    if !config.web_server.cors_origins.is_empty() {
        tracing::info!(origins = ?config.web_server.cors_origins, "CORS enabled");
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

pub async fn worker() -> std::io::Result<()> {
    let config = load_config();
    if !postgres_enabled(&config) {
        return Err(std::io::Error::other(
            "the worker requires PostgreSQL (postgres.enabled: true or DATABASE_URL)",
        ));
    }
    let settings = config
        .worker
        .with_env_overrides()
        .map_err(std::io::Error::other)?;
    let store = connect_pg(&config).await?;
    let filter = worker_queue_filter(&store, &settings)
        .await
        .map_err(std::io::Error::other)?;
    tracing::info!(
        servers = ?settings.servers,
        exclude_servers = ?settings.exclude_servers,
        deliver = ?settings.deliver,
        "worker delivery scope"
    );
    Arc::new(Worker::new(&config, store).with_delivery(filter, settings.threads))
        .run()
        .await
        .map_err(std::io::Error::other)
}

/// The part of the queue this worker serves, from `worker.servers`,
/// `worker.exclude_servers` and `worker.deliver`.
async fn worker_queue_filter(
    store: &camelmailer_db::PgStore,
    settings: &camelmailer_config::Worker,
) -> Result<camelmailer_db::QueueFilter, String> {
    let servers = if settings.servers.is_empty() {
        None
    } else {
        Some(camelmailer_db::resolve_server_permalinks(store.pool(), &settings.servers).await?)
    };
    Ok(camelmailer_db::QueueFilter {
        servers,
        exclude_servers: camelmailer_db::resolve_server_permalinks(
            store.pool(),
            &settings.exclude_servers,
        )
        .await?,
        kind: match settings.deliver {
            camelmailer_config::WorkerDeliver::All => camelmailer_db::QueueKind::All,
            camelmailer_config::WorkerDeliver::Transactional => {
                camelmailer_db::QueueKind::Transactional
            }
            camelmailer_config::WorkerDeliver::Bulk => camelmailer_db::QueueKind::Bulk,
        },
    })
}

pub async fn smtp_server() -> std::io::Result<()> {
    let config = load_config();
    let (store, sink): (Arc<dyn Store>, Arc<dyn MessageSink>) = if postgres_enabled(&config) {
        let store = connect_pg(&config).await?;
        (Arc::new(store.clone()), Arc::new(PgMessageSink::new(store)))
    } else {
        tracing::warn!("postgres is not enabled; using in-memory storage (non-persistent)");
        (Arc::new(MemoryStore::new()), Arc::new(MemorySink::new()))
    };
    if config.smtp_server.tls_enabled && !config.smtp_server.auth_requires_tls {
        // Say it on every boot. The setting is a migration window, and a
        // window nobody is reminded of stays open.
        tracing::warn!(
            "smtp_server.auth_requires_tls is off: SMTP AUTH is accepted on \
             unencrypted sessions. Intended only while clients move to \
             STARTTLS; each such AUTH is logged"
        );
    }
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
    println!(" * make-admin-api-key [name] [--org <permalink>] [--server <permalink>] - create an Admin API key (optionally scoped)");
    println!(" * make-user <email> [first] [last] [--admin] - create a user account");
    println!();
    println!("Other tools:");
    println!();
    println!(" * version - show the current Camelmailer version");
}
