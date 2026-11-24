use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::Router;
use clap::{Args, Parser, Subcommand};
use std::fs;
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{debug, info, warn};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Registry};
use voicex_sync_server::api::{build_app, AppState};
use voicex_sync_server::db;

#[derive(Parser, Debug)]
#[command(name = "voicex-sync-server", about = "VoiceX Sync v2 server and admin CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Bind address when invoked without a subcommand (systemd-compatible)
    #[arg(long, env = "VOICEX_SYNC_ADDR", default_value = "127.0.0.1:8787")]
    addr: String,

    /// SQLite database path when invoked without a subcommand
    #[arg(long, env = "VOICEX_SYNC_DB", default_value = "./sync-v2.db")]
    db: String,

    /// Log directory when invoked without a subcommand
    #[arg(long, env = "VOICEX_SYNC_LOG_DIR", default_value = "./logs")]
    log_dir: String,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start the HTTP/SSE Sync v2 server
    Serve(ServeArgs),
    /// Manage accounts, pairing codes, and devices directly against the SQLite DB
    Admin {
        #[command(subcommand)]
        action: AdminCommand,
    },
}

#[derive(Args, Debug)]
struct ServeArgs {
    #[arg(long, env = "VOICEX_SYNC_ADDR", default_value = "127.0.0.1:8787")]
    addr: String,
    #[arg(long, env = "VOICEX_SYNC_DB", default_value = "./sync-v2.db")]
    db: String,
    #[arg(long, env = "VOICEX_SYNC_LOG_DIR", default_value = "./logs")]
    log_dir: String,
}

#[derive(Subcommand, Debug)]
enum AdminCommand {
    /// Create a new isolated sync account
    CreateAccount {
        #[arg(long, env = "VOICEX_SYNC_DB", default_value = "./sync-v2.db")]
        db: String,
        #[arg(long, default_value = "")]
        label: String,
        #[arg(long, default_value_t = 512)]
        quota_mb: i64,
    },
    /// Issue a short-lived single-use pairing code for an account
    CreatePairingCode {
        #[arg(long, env = "VOICEX_SYNC_DB", default_value = "./sync-v2.db")]
        db: String,
        #[arg(long)]
        account: String,
        #[arg(long, default_value_t = 15)]
        ttl_minutes: i64,
    },
    /// List all devices registered under an account
    ListDevices {
        #[arg(long, env = "VOICEX_SYNC_DB", default_value = "./sync-v2.db")]
        db: String,
        #[arg(long)]
        account: String,
    },
    /// Revoke a device's credential so all future requests fail with 401 device_revoked
    RevokeDevice {
        #[arg(long, env = "VOICEX_SYNC_DB", default_value = "./sync-v2.db")]
        db: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        device: String,
    },
}

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(60);

const TCP_KEEPALIVE: socket2::TcpKeepalive = socket2::TcpKeepalive::new()
    .with_time(Duration::from_secs(60))
    .with_interval(Duration::from_secs(15))
    .with_retries(4);

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Serve(args)) => run_server(&args.addr, &args.db, &args.log_dir).await,
        None => run_server(&cli.addr, &cli.db, &cli.log_dir).await,
        Some(Command::Admin { action }) => run_admin(action).await,
    }
}

async fn run_admin(action: AdminCommand) -> Result<(), anyhow::Error> {
    match action {
        AdminCommand::CreateAccount {
            db: db_path,
            label,
            quota_mb,
        } => {
            let pool = db::open_pool(&db_path).await?;
            let _ = db::ensure_server_id(&pool).await?;
            let quota_bytes = quota_mb.max(1) * 1024 * 1024;
            let created = db::create_account(&pool, &label, quota_bytes).await?;
            println!("{}", serde_json::to_string_pretty(&created)?);
        }
        AdminCommand::CreatePairingCode {
            db: db_path,
            account,
            ttl_minutes,
        } => {
            let pool = db::open_pool(&db_path).await?;
            let code = db::create_pairing_code(&pool, &account, ttl_minutes).await?;
            println!("{}", serde_json::to_string_pretty(&code)?);
        }
        AdminCommand::ListDevices {
            db: db_path,
            account,
        } => {
            let pool = db::open_pool(&db_path).await?;
            let devices = db::list_devices(&pool, &account).await?;
            println!("{}", serde_json::to_string_pretty(&devices)?);
        }
        AdminCommand::RevokeDevice {
            db: db_path,
            account,
            device,
        } => {
            let pool = db::open_pool(&db_path).await?;
            let revoked = db::revoke_device(&pool, &account, &device).await?;
            if !revoked {
                anyhow::bail!("device {device} not found under account {account}");
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "accountId": account,
                    "deviceId": device,
                    "revoked": true
                }))?
            );
        }
    }
    Ok(())
}

async fn run_server(addr_str: &str, db_path: &str, log_dir: &str) -> Result<(), anyhow::Error> {
    let addr: SocketAddr = addr_str.parse()?;

    fs::create_dir_all(log_dir)?;
    let file_appender = tracing_appender::rolling::never(log_dir, "voicex-sync-v2.log");
    let (file_writer, _file_guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::from_default_env().add_directive("info".parse()?);
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_filter(env_filter);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_filter(LevelFilter::WARN);

    Registry::default()
        .with(stdout_layer)
        .with(file_layer)
        .init();

    let pool = db::open_pool(db_path).await?;
    let server_id = db::ensure_server_id(&pool).await?;
    let state = AppState::new(pool, server_id);
    let app = build_app(state);

    info!("voicex-sync-server v2 listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve(listener, app).await
}

/// Accept loop preserving TCP keepalive (~2 min dead-peer detection) and a 60 s
/// HTTP/1.1 header-read timeout without interrupting active SSE streams.
async fn serve(listener: tokio::net::TcpListener, app: Router) -> Result<(), anyhow::Error> {
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
    use hyper_util::server::conn::auto::Builder;
    use tower::ServiceExt as _;

    loop {
        let (stream, remote_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(err) => {
                warn!("accept failed: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        if let Err(err) = socket2::SockRef::from(&stream).set_tcp_keepalive(&TCP_KEEPALIVE) {
            warn!("failed to set TCP keepalive on {remote_addr}: {err}");
        }

        let app = app.clone();
        tokio::spawn(async move {
            let service =
                hyper::service::service_fn(move |mut req: Request<hyper::body::Incoming>| {
                    req.extensions_mut().insert(ConnectInfo(remote_addr));
                    app.clone().oneshot(req.map(axum::body::Body::new))
                });

            let mut builder = Builder::new(TokioExecutor::new()).http1_only();
            builder
                .http1()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_READ_TIMEOUT);

            if let Err(err) = builder
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                debug!("connection {remote_addr} closed: {err}");
            }
        });
    }
}
