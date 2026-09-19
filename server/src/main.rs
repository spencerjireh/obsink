//! `obsink-server` binary: `serve` (default) plus admin subcommands.

use std::sync::Arc;

use clap::{Parser, Subcommand};
use obsink_server::{
    auth::email::{Mailer, NoMailer, SmtpMailer},
    auth::invites::{self, INVITE_TTL_SECS},
    config::{self, Config},
    db, retention, router, AppState,
};

#[derive(Parser)]
#[command(name = "obsink-server", about = "Self-hosted ObSink sync server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server (default).
    Serve,
    /// Apply pending database migrations and exit.
    Migrate,
    /// Mint invite codes for new accounts.
    Invite {
        #[arg(long, default_value_t = 1)]
        count: u32,
        #[arg(long, default_value_t = 7)]
        expires_days: u64,
    },
    /// Run one retention pass (versions, trash, expired sessions/codes, orphans).
    Retention,
    /// Print a fresh base64 value for OBSINK_SERVER_KEY.
    Keygen,
    /// GET /healthz on the local listener (for container health checks).
    Healthcheck,
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("obsink_server=info,tower_http=info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

#[tokio::main]
async fn main() {
    init_tracing();
    if let Err(error) = run(Cli::parse().command.unwrap_or(Command::Serve)).await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Keygen => {
            println!("{}", config::encode_key(&config::generate_key()));
            Ok(())
        }
        Command::Healthcheck => {
            let listen =
                std::env::var("OBSINK_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
            let port = listen.rsplit(':').next().unwrap_or("8080");
            let url = format!("http://127.0.0.1:{port}/healthz");
            let response = reqwest::get(&url)
                .await
                .map_err(|error| format!("{url}: {error}"))?;
            if response.status().is_success() {
                Ok(())
            } else {
                Err(format!("{url}: HTTP {}", response.status()))
            }
        }
        Command::Migrate => {
            let config = Config::from_env()?;
            let pool = db::connect(&config.database_url)
                .await
                .map_err(|e| e.to_string())?;
            db::migrate(&pool).await.map_err(|e| e.to_string())?;
            println!("migrations applied");
            Ok(())
        }
        Command::Invite {
            count,
            expires_days,
        } => {
            let config = Config::from_env()?;
            let pool = db::connect(&config.database_url)
                .await
                .map_err(|e| e.to_string())?;
            let mut conn = pool.acquire().await.map_err(|e| e.to_string())?;
            let ttl = if expires_days == 0 {
                INVITE_TTL_SECS
            } else {
                expires_days * 24 * 60 * 60
            };
            for _ in 0..count.max(1) {
                let invite = invites::create(&mut conn, None, db::now(), ttl)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                println!("{}  (expires {})", invite.code, invite.expires);
            }
            Ok(())
        }
        Command::Retention => {
            let (state, _) = build_state().await?;
            let report = retention::run_once(&state, db::now())
                .await
                .map_err(|e| format!("{e:?}"))?;
            println!("{report}");
            Ok(())
        }
        Command::Serve => serve().await,
    }
}

async fn build_state() -> Result<(AppState, Config), String> {
    let config = Config::from_env()?;
    let master_key = config::load_or_create_server_key(&config)?;
    let pool = db::connect(&config.database_url)
        .await
        .map_err(|error| format!("database: {error}"))?;
    if config.migrate_on_start {
        db::migrate(&pool)
            .await
            .map_err(|error| format!("migrate: {error}"))?;
    }
    let mailer: Arc<dyn Mailer> = match &config.smtp {
        Some(smtp) => Arc::new(SmtpMailer::new(smtp)?),
        None => Arc::new(NoMailer),
    };
    let state = AppState::new(config.clone(), pool, master_key, mailer);
    Ok((state, config))
}

async fn serve() -> Result<(), String> {
    let (state, config) = build_state().await?;
    retention::spawn(state.clone());
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .map_err(|error| format!("bind {}: {error}", config.listen))?;
    tracing::info!(
        listen = %config.listen,
        email = config.email_enabled(),
        apple = config.apple_enabled(),
        api_key = config.api_key.is_some(),
        "obsink-server listening"
    );
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| error.to_string())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
