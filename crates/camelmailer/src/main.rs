//! The `camelmailer` CLI — the Rust port of `bin/postal`, dispatching the
//! process roles from a single binary.

use camelmailer_api::{build_router, ApiState};
use camelmailer_core::{MemorySink, MemoryStore};
use camelmailer_smtp::SmtpServer;
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

async fn web_server() -> std::io::Result<()> {
    let config = load_config();
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::new(store, config.camelmailer.admin_api_key.clone());
    let router = build_router(state);

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

async fn smtp_server() -> std::io::Result<()> {
    let config = load_config();
    let store = Arc::new(MemoryStore::new());
    let sink = Arc::new(MemorySink::new());
    SmtpServer::new(config, store, sink).run().await
}

fn print_usage() {
    println!("Usage: camelmailer [command]");
    println!();
    println!("Server components:");
    println!();
    println!(" * web-server - run the web server (Admin API)");
    println!(" * smtp-server - run the SMTP server");
    println!();
    println!("Other tools:");
    println!();
    println!(" * version - show the current CamelMailer version");
}
