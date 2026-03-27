use async_stream::stream;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use chrono::Utc;
use clap::Parser;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::convert::Infallible;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::broadcast;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, warn};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{EnvFilter, Registry};
use tracing_subscriber::prelude::*;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "voicex-sync-server")]
struct Cli {
    /// Bind address, e.g. 127.0.0.1:8787
    #[arg(long, env = "VOICEX_SYNC_ADDR", default_value = "127.0.0.1:8787")]
    addr: String,

    /// SQLite database path
    #[arg(long, env = "VOICEX_SYNC_DB", default_value = "./dev.db")]
    db: String,

    /// Shared secret for HMAC auth
    #[arg(long, env = "VOICEX_SYNC_SHARED_SECRET", default_value = "")]
    shared_secret: String,

    /// Log directory
    #[arg(long, env = "VOICEX_SYNC_LOG_DIR", default_value = "./logs")]
    log_dir: String,
}

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    buses: Arc<DashMap<String, broadcast::Sender<BusEvent>>>,
    server_id: String,
    shared_secret: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BusEvent {
    seq: i64,
    #[serde(rename = "type")]
    event_type: String,
    device_id: String,
    payload: Value,
}

#[derive(Clone, Debug)]
struct AuthedAccount {
    account_id: String,
}

#[derive(Clone, Debug)]
struct LoggedError;

#[derive(Debug, Error)]
enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("db error: {0}")]
    Db(#[from] sqlx::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            ApiError::Db(_) => (StatusCode::INTERNAL_SERVER_ERROR, "db error".to_string()),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();
    let addr: SocketAddr = cli.addr.parse()?;
    let shared_secret = cli.shared_secret.trim().to_string();
    if shared_secret.is_empty() {
        anyhow::bail!("VOICEX_SYNC_SHARED_SECRET is required");
    }

    fs::create_dir_all(&cli.log_dir)?;
    let file_appender = tracing_appender::rolling::never(&cli.log_dir, "voicex-sync.log");
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

    let connect_opts = SqliteConnectOptions::new()
        .filename(cli.db)
        .create_if_missing(true);

    let db = SqlitePoolOptions::new()
        // SQLite has a single-writer model; keep it small and predictable.
        .max_connections(8)
        .connect_with(connect_opts)
        .await?;

    init_db(&db).await?;
    let server_id = ensure_server_id(&db).await?;

    let state = AppState {
        db,
        buses: Arc::new(DashMap::new()),
        server_id,
        shared_secret,
    };

    let v1 = Router::new()
        .route("/account", get(get_account))
        .route("/device", put(put_device))
        .route("/events", post(post_events).get(get_events))
        .route("/subscribe", get(subscribe))
        .layer(from_fn_with_state(state.clone(), auth_middleware));

    let app = Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", v1)
        .layer(TraceLayer::new_for_http())
        .layer(from_fn(log_failed_requests))
        .fallback(not_found)
        .with_state(state);

    info!("sync-server listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn not_found(req: Request<axum::body::Body>) -> Response {
    let ip = client_ip(&req).unwrap_or_else(|| "unknown".to_string());
    let ua = client_user_agent(&req).unwrap_or_else(|| "unknown".to_string());
    let method = req.method().as_str();
    let path = req.uri().path();
    warn!(
        "not_found ip={} method={} path={} ua={}",
        ip, method, path, ua
    );
    let mut resp = (StatusCode::NOT_FOUND, "not found").into_response();
    resp.extensions_mut().insert(LoggedError);
    resp
}

async fn log_failed_requests(req: Request<axum::body::Body>, next: Next) -> Response {
    let ip = client_ip(&req).unwrap_or_else(|| "unknown".to_string());
    let ua = client_user_agent(&req).unwrap_or_else(|| "unknown".to_string());
    let method = req.method().as_str().to_string();
    let uri = req.uri().to_string();

    let response = next.run(req).await;
    let status = response.status();

    if status.is_client_error() || status.is_server_error() {
        if response.extensions().get::<LoggedError>().is_none() {
            warn!(
                "http_error ip={} method={} uri={} ua={} status={}",
                ip,
                method,
                uri,
                ua,
                status.as_u16()
            );
        }
    }

    response
}

async fn init_db(db: &SqlitePool) -> Result<(), sqlx::Error> {
    // Basic pragmas for local dev / low traffic usage.
    let _ = sqlx::query("PRAGMA journal_mode = WAL;").execute(db).await?;
    let _ = sqlx::query("PRAGMA foreign_keys = ON;").execute(db).await?;

    // NOTE: Keep schema SQLite-friendly. `payload` stored as JSON text.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS accounts (
            account_id TEXT PRIMARY KEY,
            token_hash TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            text_retention_days INTEGER NOT NULL DEFAULT 30
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS devices (
            account_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            device_name TEXT NOT NULL,
            platform TEXT,
            app_version TEXT,
            created_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            PRIMARY KEY (account_id, device_id)
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS history_records (
            account_id TEXT NOT NULL,
            record_id TEXT NOT NULL,
            source_device_id TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            text TEXT NOT NULL,
            original_text TEXT,
            ai_correction_applied INTEGER NOT NULL,
            mode TEXT NOT NULL,
            duration_ms INTEGER NOT NULL,
            is_final INTEGER NOT NULL,
            error_code INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (account_id, record_id)
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS events (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id TEXT NOT NULL,
            event_id TEXT NOT NULL UNIQUE,
            device_id TEXT NOT NULL,
            type TEXT NOT NULL,
            record_id TEXT,
            payload TEXT NOT NULL,
            created_at TEXT NOT NULL
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS usage_stats (
            account_id TEXT PRIMARY KEY,
            total_duration_ms INTEGER NOT NULL,
            total_characters INTEGER NOT NULL,
            llm_correction_count INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS server_info (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            server_id TEXT NOT NULL
        )
        "#,
    )
    .execute(db)
    .await?;

    Ok(())
}

async fn ensure_server_id(db: &SqlitePool) -> Result<String, sqlx::Error> {
    let row = sqlx::query("SELECT server_id FROM server_info WHERE id = 1")
        .fetch_optional(db)
        .await?;

    if let Some(row) = row {
        let server_id: String = row.get(0);
        return Ok(server_id);
    }

    let server_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO server_info (id, server_id) VALUES (1, ?1)")
        .bind(&server_id)
        .execute(db)
        .await?;

    Ok(server_id)
}

async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let ip = client_ip(&req).unwrap_or_else(|| "unknown".to_string());
    let ua = client_user_agent(&req).unwrap_or_else(|| "unknown".to_string());
    let method = req.method().as_str();
    let path = req.uri().path();

    let Some(bearer) = extract_bearer(req.headers()) else {
        warn!(
            "auth rejected ip={} method={} path={} ua={} reason=missing_bearer",
            ip, method, path, ua
        );
        let mut resp = (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
        resp.extensions_mut().insert(LoggedError);
        return resp;
    };
    let Some(token_hash) = verify_bearer_token(&bearer, &state.shared_secret) else {
        warn!(
            "auth rejected ip={} method={} path={} ua={} reason=invalid_bearer",
            ip, method, path, ua
        );
        let mut resp = (StatusCode::UNAUTHORIZED, "invalid bearer token").into_response();
        resp.extensions_mut().insert(LoggedError);
        return resp;
    };

    match get_or_create_account(&state.db, &token_hash).await {
        Ok(account_id) => {
            req.extensions_mut().insert(AuthedAccount { account_id });
            next.run(req).await
        }
        Err(err) => {
            warn!("auth failed: {}", err);
            (StatusCode::INTERNAL_SERVER_ERROR, "auth failed").into_response()
        }
    }
}

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?;
    let value = value.to_str().ok()?;
    let value = value.trim();
    let prefix = "Bearer ";
    if value.len() <= prefix.len() {
        return None;
    }
    if !value.starts_with(prefix) {
        return None;
    }
    Some(value[prefix.len()..].trim().to_string())
}

fn client_ip(req: &Request<axum::body::Body>) -> Option<String> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.to_string())
}

fn client_user_agent(req: &Request<axum::body::Body>) -> Option<String> {
    req.headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string())
}

fn verify_bearer_token(token: &str, shared_secret: &str) -> Option<String> {
    let mut parts = token.split('.');
    let version = parts.next()?;
    if version != "vx1" {
        return None;
    }
    let payload = parts.next()?;
    let sig = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if payload.is_empty() || sig.is_empty() {
        return None;
    }
    if payload.len() != 64 || !payload.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if !verify_hmac(shared_secret, payload, sig) {
        return None;
    }
    Some(payload.to_string())
}

type HmacSha256 = Hmac<Sha256>;

fn verify_hmac(shared_secret: &str, payload: &str, signature_hex: &str) -> bool {
    let sig = match hex::decode(signature_hex) {
        Ok(sig) => sig,
        Err(_) => return false,
    };
    let mut mac = match HmacSha256::new_from_slice(shared_secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(payload.as_bytes());
    mac.verify_slice(&sig).is_ok()
}

async fn get_or_create_account(db: &SqlitePool, token_hash: &str) -> Result<String, sqlx::Error> {
    if let Some(row) = sqlx::query("SELECT account_id FROM accounts WHERE token_hash = ?1 LIMIT 1")
        .bind(token_hash)
        .fetch_optional(db)
        .await?
    {
        return Ok(row.get::<String, _>(0));
    }

    let account_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO accounts (account_id, token_hash, created_at, text_retention_days)
         VALUES (?1, ?2, ?3, 30)",
    )
    .bind(&account_id)
    .bind(token_hash)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO usage_stats (account_id, total_duration_ms, total_characters, llm_correction_count, updated_at)
         VALUES (?1, 0, 0, 0, ?2)
         ON CONFLICT(account_id) DO NOTHING",
    )
    .bind(&account_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(account_id)
}

fn bus_sender(state: &AppState, account_id: &str) -> broadcast::Sender<BusEvent> {
    match state.buses.entry(account_id.to_string()) {
        Entry::Occupied(entry) => entry.get().clone(),
        Entry::Vacant(entry) => {
            let (tx, _rx) = broadcast::channel(1024);
            entry.insert(tx.clone());
            tx
        }
    }
}

async fn broadcast_event(state: &AppState, account_id: &str, ev: BusEvent) {
    let sender = bus_sender(state, account_id);
    let _ = sender.send(ev);
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountResponse {
    server_id: String,
    account_id: String,
    config: AccountConfig,
    usage: UsageStats,
    last_seq: Option<i64>,
    server_now: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountConfig {
    text_retention_days: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageStats {
    total_duration_ms: i64,
    total_characters: i64,
    llm_correction_count: i64,
}

async fn get_account(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedAccount>,
) -> Result<Json<AccountResponse>, ApiError> {
    let account_row = sqlx::query(
        "SELECT text_retention_days FROM accounts WHERE account_id = ?1 LIMIT 1",
    )
    .bind(&auth.account_id)
    .fetch_one(&state.db)
    .await?;

    let text_retention_days = account_row.get::<i64, _>(0);

    let usage_row = sqlx::query(
        "SELECT total_duration_ms, total_characters, llm_correction_count
         FROM usage_stats WHERE account_id = ?1 LIMIT 1",
    )
    .bind(&auth.account_id)
    .fetch_one(&state.db)
    .await?;

    let last_seq = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(seq) FROM events WHERE account_id = ?1",
    )
    .bind(&auth.account_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(AccountResponse {
        server_id: state.server_id.clone(),
        account_id: auth.account_id.clone(),
        config: AccountConfig { text_retention_days },
        usage: UsageStats {
            total_duration_ms: usage_row.get::<i64, _>(0),
            total_characters: usage_row.get::<i64, _>(1),
            llm_correction_count: usage_row.get::<i64, _>(2),
        },
        last_seq,
        server_now: Utc::now().to_rfc3339(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutDeviceRequest {
    device_id: String,
    device_name: String,
    platform: Option<String>,
    app_version: Option<String>,
}

async fn put_device(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedAccount>,
    Json(req): Json<PutDeviceRequest>,
) -> Result<Json<Value>, ApiError> {
    if req.device_id.trim().is_empty() {
        return Err(ApiError::BadRequest("deviceId is required".to_string()));
    }
    let name = req.device_name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("deviceName is required".to_string()));
    }
    if name.chars().count() > 64 {
        return Err(ApiError::BadRequest("deviceName too long (max 64)".to_string()));
    }

    let now = Utc::now().to_rfc3339();
    let payload = json!({
        "deviceId": req.device_id,
        "deviceName": name,
        "platform": req.platform,
        "appVersion": req.app_version
    });

    let mut tx = state.db.begin().await?;

    // Upsert device registry.
    sqlx::query(
        r#"
        INSERT INTO devices (account_id, device_id, device_name, platform, app_version, created_at, last_seen_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
        ON CONFLICT(account_id, device_id) DO UPDATE SET
            device_name = excluded.device_name,
            platform = excluded.platform,
            app_version = excluded.app_version,
            last_seen_at = excluded.last_seen_at
        "#,
    )
    .bind(&auth.account_id)
    .bind(payload["deviceId"].as_str().unwrap())
    .bind(payload["deviceName"].as_str().unwrap())
    .bind(payload["platform"].as_str())
    .bind(payload["appVersion"].as_str())
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    // Record an event so other devices can update their name map.
    let event_id = Uuid::new_v4().to_string();
    let (seq, ev) = insert_event(
        &mut tx,
        &auth.account_id,
        payload["deviceId"].as_str().unwrap(),
        &event_id,
        "device.updated",
        None,
        &payload,
    )
    .await?;

    tx.commit().await?;
    broadcast_event(&state, &auth.account_id, ev).await;

    info!(
        "device updated account={} device_id={} name={}",
        auth.account_id, req.device_id, name
    );
    Ok(Json(json!({ "ok": true, "seq": seq })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostEventsRequest {
    device_id: String,
    events: Vec<ClientEvent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientEvent {
    event_id: String,
    #[serde(rename = "type")]
    event_type: String,
    record: Option<HistoryRecord>,
    record_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryRecord {
    id: String,
    source_device_id: String,
    timestamp: String,
    text: String,
    original_text: Option<String>,
    ai_correction_applied: bool,
    #[serde(default)]
    llm_invoked: bool,
    mode: String,
    duration_ms: i64,
    is_final: bool,
    error_code: i32,
    #[serde(default)]
    asr_model_name: Option<String>,
    #[serde(default)]
    llm_model_name: Option<String>,
}

async fn post_events(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedAccount>,
    Json(req): Json<PostEventsRequest>,
) -> Result<Json<Value>, ApiError> {
    if req.device_id.trim().is_empty() {
        return Err(ApiError::BadRequest("deviceId is required".to_string()));
    }
    if req.events.is_empty() {
        return Ok(Json(json!({ "accepted": 0, "lastSeq": null })));
    }

    let total = req.events.len();
    let mut accepted = 0usize;
    let mut last_seq: Option<i64> = None;

    for ev in req.events {
        let seq = apply_client_event(&state, &auth.account_id, &req.device_id, ev).await?;
        if let Some(seq) = seq {
            accepted += 1;
            last_seq = Some(last_seq.unwrap_or(seq).max(seq));
        }
    }

    info!(
        "events received account={} device_id={} count={} accepted={} last_seq={:?}",
        auth.account_id, req.device_id, total, accepted, last_seq
    );
    Ok(Json(json!({ "accepted": accepted, "lastSeq": last_seq })))
}

async fn apply_client_event(
    state: &AppState,
    account_id: &str,
    request_device_id: &str,
    ev: ClientEvent,
) -> Result<Option<i64>, ApiError> {
    if ev.event_id.trim().is_empty() {
        return Err(ApiError::BadRequest("eventId is required".to_string()));
    }

    let now = Utc::now().to_rfc3339();
    let mut tx = state.db.begin().await?;

    let (maybe_seq, bus_ev) = match ev.event_type.as_str() {
        "history.upsert" => {
            let record = ev
                .record
                .ok_or_else(|| ApiError::BadRequest("record is required for history.upsert".to_string()))?;
            if record.source_device_id != request_device_id {
                return Err(ApiError::BadRequest(
                    "record.sourceDeviceId must match request deviceId".to_string(),
                ));
            }

            // Insert the event first (idempotency by event_id).
            let payload = serde_json::to_value(&record).map_err(|e| ApiError::BadRequest(e.to_string()))?;
            let inserted = insert_event_if_new(
                &mut tx,
                account_id,
                request_device_id,
                &ev.event_id,
                "history.upsert",
                Some(&record.id),
                &payload,
            )
            .await?;

            let Some((seq, bus_ev)) = inserted else {
                tx.rollback().await?;
                return Ok(None);
            };

            // Insert record (idempotent by record_id).
            let rec_inserted = sqlx::query(
                r#"
                INSERT OR IGNORE INTO history_records (
                    account_id, record_id, source_device_id, timestamp, text, original_text,
                    ai_correction_applied, mode, duration_ms, is_final, error_code, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
            )
            .bind(account_id)
            .bind(&record.id)
            .bind(&record.source_device_id)
            .bind(&record.timestamp)
            .bind(&record.text)
            .bind(&record.original_text)
            .bind(if record.ai_correction_applied { 1 } else { 0 })
            .bind(&record.mode)
            .bind(record.duration_ms)
            .bind(if record.is_final { 1 } else { 0 })
            .bind(record.error_code)
            .bind(&now)
            .execute(&mut *tx)
            .await?
            .rows_affected();

            // Increment stats only if this history record is new.
            if rec_inserted == 1 {
                let chars = record.text.chars().count() as i64;
                let llm = if record.llm_invoked { 1 } else { 0 };
                sqlx::query(
                    r#"
                    UPDATE usage_stats
                    SET total_duration_ms = total_duration_ms + ?1,
                        total_characters = total_characters + ?2,
                        llm_correction_count = llm_correction_count + ?3,
                        updated_at = ?4
                    WHERE account_id = ?5
                    "#,
                )
                .bind(record.duration_ms.max(0))
                .bind(chars.max(0))
                .bind(llm)
                .bind(&now)
                .bind(account_id)
                .execute(&mut *tx)
                .await?;
            }

            info!(
                "event accepted seq={} type=history.upsert record_id={} device_id={}",
                seq, record.id, request_device_id
            );
            (Some(seq), bus_ev)
        }
        "history.delete" => {
            let record_id = ev
                .record_id
                .ok_or_else(|| ApiError::BadRequest("recordId is required for history.delete".to_string()))?;
            let payload = json!({ "recordId": record_id });
            let inserted = insert_event_if_new(
                &mut tx,
                account_id,
                request_device_id,
                &ev.event_id,
                "history.delete",
                Some(payload["recordId"].as_str().unwrap()),
                &payload,
            )
            .await?;

            let Some((seq, bus_ev)) = inserted else {
                tx.rollback().await?;
                return Ok(None);
            };

            sqlx::query("DELETE FROM history_records WHERE account_id = ?1 AND record_id = ?2")
                .bind(account_id)
                .bind(payload["recordId"].as_str().unwrap())
                .execute(&mut *tx)
                .await?;

            info!(
                "event accepted seq={} type=history.delete record_id={} device_id={}",
                seq, record_id, request_device_id
            );
            (Some(seq), bus_ev)
        }
        other => {
            return Err(ApiError::BadRequest(format!(
                "unsupported event type: {}",
                other
            )))
        }
    };

    tx.commit().await?;
    broadcast_event(state, account_id, bus_ev).await;
    Ok(maybe_seq)
}

async fn insert_event_if_new(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    account_id: &str,
    device_id: &str,
    event_id: &str,
    event_type: &str,
    record_id: Option<&str>,
    payload: &Value,
) -> Result<Option<(i64, BusEvent)>, ApiError> {
    let payload_str =
        serde_json::to_string(payload).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let now = Utc::now().to_rfc3339();

    let result = sqlx::query(
        r#"
        INSERT OR IGNORE INTO events (account_id, event_id, device_id, type, record_id, payload, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
    )
    .bind(account_id)
    .bind(event_id)
    .bind(device_id)
    .bind(event_type)
    .bind(record_id)
    .bind(&payload_str)
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(None);
    }

    let seq = result.last_insert_rowid();
    Ok(Some((
        seq,
        BusEvent {
            seq,
            event_type: event_type.to_string(),
            device_id: device_id.to_string(),
            payload: payload.clone(),
        },
    )))
}

async fn insert_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    account_id: &str,
    device_id: &str,
    event_id: &str,
    event_type: &str,
    record_id: Option<&str>,
    payload: &Value,
) -> Result<(i64, BusEvent), ApiError> {
    let inserted = insert_event_if_new(tx, account_id, device_id, event_id, event_type, record_id, payload).await?;
    inserted.ok_or_else(|| ApiError::BadRequest("eventId already exists".to_string()))
}

#[derive(Debug, Deserialize)]
struct GetEventsQuery {
    since: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GetEventsResponse {
    events: Vec<BusEvent>,
    last_seq: i64,
}

async fn get_events(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedAccount>,
    Query(q): Query<GetEventsQuery>,
) -> Result<Json<GetEventsResponse>, ApiError> {
    let since = q.since.unwrap_or(0).max(0);
    let mut limit = q.limit.unwrap_or(200).max(1);
    limit = limit.min(1000);

    let rows = sqlx::query(
        r#"
        SELECT seq, type, device_id, payload
        FROM events
        WHERE account_id = ?1 AND seq > ?2
        ORDER BY seq ASC
        LIMIT ?3
        "#,
    )
    .bind(&auth.account_id)
    .bind(since)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    let mut events = Vec::with_capacity(rows.len());
    let mut last_seq = since;

    for row in rows {
        let seq: i64 = row.get(0);
        let event_type: String = row.get(1);
        let device_id: String = row.get(2);
        let payload_str: String = row.get(3);
        let payload: Value = serde_json::from_str(&payload_str).unwrap_or(Value::Null);
        last_seq = last_seq.max(seq);
        events.push(BusEvent {
            seq,
            event_type,
            device_id,
            payload,
        });
    }

    debug!(
        "events fetch account={} since={} returned={}",
        auth.account_id,
        since,
        events.len()
    );
    Ok(Json(GetEventsResponse { events, last_seq }))
}

#[derive(Debug, Deserialize)]
struct SubscribeQuery {
    since: Option<i64>,
}

async fn subscribe(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedAccount>,
    Query(q): Query<SubscribeQuery>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let since = q.since.unwrap_or(0).max(0);
    info!("subscribe account={} since={}", auth.account_id, since);

    // Subscribe first to avoid missing events between backlog query and stream start.
    let sender = bus_sender(&state, &auth.account_id);
    let mut rx = sender.subscribe();

    // Backlog (bounded) + live stream.
    let db = state.db.clone();
    let account_id = auth.account_id.clone();
    let mut last_sent = since;

    let out = stream! {
        // Backlog: keep it bounded; large initial sync should use GET /v1/events paging.
        let backlog_rows = sqlx::query(
            r#"
            SELECT seq, type, device_id, payload
            FROM events
            WHERE account_id = ?1 AND seq > ?2
            ORDER BY seq ASC
            LIMIT 1000
            "#,
        )
        .bind(&account_id)
        .bind(since)
        .fetch_all(&db)
        .await
        .unwrap_or_default();

        for row in backlog_rows {
            let seq: i64 = row.get(0);
            let event_type: String = row.get(1);
            let device_id: String = row.get(2);
            let payload_str: String = row.get(3);
            let payload: Value = serde_json::from_str(&payload_str).unwrap_or(Value::Null);

            if seq <= last_sent {
                continue;
            }
            last_sent = seq;

            let data = json!({
                "seq": seq,
                "type": event_type,
                "deviceId": device_id,
                "payload": payload
            });

            yield Ok(
                Event::default()
                    .id(seq.to_string())
                    .event("event")
                    .data(data.to_string())
            );
        }

        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if ev.seq <= last_sent {
                        continue;
                    }
                    last_sent = ev.seq;

                    let data = json!({
                        "seq": ev.seq,
                        "type": ev.event_type,
                        "deviceId": ev.device_id,
                        "payload": ev.payload
                    });

                    yield Ok(
                        Event::default()
                            .id(ev.seq.to_string())
                            .event("event")
                            .data(data.to_string())
                    );
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Client should reconnect with last known seq and catch up via GET /v1/events.
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Ok(Sse::new(out).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}
