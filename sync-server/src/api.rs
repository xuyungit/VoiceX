use crate::db::{self, AuthedDevice, DbApiError};
use async_stream::stream;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use serde::Deserialize;
use sqlx::SqlitePool;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::warn;
use voicex_sync_protocol::{
    error_codes, ApiErrorBody, CreatePairingCodeRequest, CreatePairingCodeResponse,
    DeviceUpdateRequest, NotifyEvent, PairRedeemRequest, PushEnvelopesRequest, SyncDomain,
    MAX_PUSH_BODY_BYTES,
};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub server_id: String,
    pub buses: Arc<DashMap<String, broadcast::Sender<NotifyEvent>>>,
    pub rate_limiter: Arc<RateLimiter>,
}

impl AppState {
    pub fn new(db: SqlitePool, server_id: String) -> Self {
        Self {
            db,
            server_id,
            buses: Arc::new(DashMap::new()),
            rate_limiter: Arc::new(RateLimiter::new(120, Duration::from_secs(60))),
        }
    }

    pub fn with_rate_limit(
        db: SqlitePool,
        server_id: String,
        max_requests: usize,
        window: Duration,
    ) -> Self {
        Self {
            db,
            server_id,
            buses: Arc::new(DashMap::new()),
            rate_limiter: Arc::new(RateLimiter::new(max_requests, window)),
        }
    }

    fn bus_for(&self, account_id: &str) -> broadcast::Sender<NotifyEvent> {
        match self.buses.entry(account_id.to_string()) {
            Entry::Occupied(e) => e.get().clone(),
            Entry::Vacant(e) => {
                let (tx, _) = broadcast::channel(512);
                e.insert(tx.clone());
                tx
            }
        }
    }
}

/// Simple sliding-window in-memory rate limiter keyed by device or IP.
pub struct RateLimiter {
    max_requests: usize,
    window: Duration,
    buckets: DashMap<String, VecDeque<Instant>>,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            max_requests,
            window,
            buckets: DashMap::new(),
        }
    }

    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut entry = self.buckets.entry(key.to_string()).or_default();
        while let Some(&front) = entry.front() {
            if now.duration_since(front) >= self.window {
                entry.pop_front();
            } else {
                break;
            }
        }
        if entry.len() >= self.max_requests {
            return false;
        }
        entry.push_back(now);
        true
    }
}

#[derive(Clone, Debug)]
struct LoggedError;

fn json_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiErrorBody {
            code: code.to_string(),
            message: message.into(),
        }),
    )
        .into_response()
}

impl IntoResponse for DbApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            DbApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            DbApiError::Unauthorized | DbApiError::DeviceRevoked => StatusCode::UNAUTHORIZED,
            DbApiError::ScopeForbidden(_) => StatusCode::FORBIDDEN,
            DbApiError::PairingInvalidOrExpired(_) | DbApiError::DeviceIdConflict(_) => {
                StatusCode::CONFLICT
            }
            DbApiError::QuotaExceeded => StatusCode::INSUFFICIENT_STORAGE,
            DbApiError::Sqlx(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let code = self.error_code();
        let message = match &self {
            DbApiError::Sqlx(err) => {
                warn!("db error: {err}");
                "internal database error".to_string()
            }
            other => other.to_string(),
        };
        json_error(status, code, message)
    }
}

pub fn build_app(state: AppState) -> Router {
    let authed_v2 = Router::new()
        .route("/account", get(get_account))
        .route("/devices/pairing-code", post(post_device_pairing_code))
        .route("/devices/:device_id", put(put_device))
        .route("/envelopes", post(post_envelopes).get(get_envelopes))
        .route("/subscribe", get(subscribe))
        .layer(from_fn_with_state(state.clone(), auth_and_rate_limit_middleware));

    let public_v2 = Router::new().route("/pair/redeem", post(post_pair_redeem));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v2", public_v2.merge(authed_v2))
        .layer(RequestBodyLimitLayer::new(MAX_PUSH_BODY_BYTES))
        .layer(from_fn(map_body_too_large))
        .layer(TraceLayer::new_for_http())
        .layer(from_fn(log_failed_requests))
        .fallback(not_found)
        .with_state(state)
}

async fn map_body_too_large(req: Request<axum::body::Body>, next: Next) -> Response {
    let resp = next.run(req).await;
    if resp.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            error_codes::PAYLOAD_TOO_LARGE,
            format!("request body exceeds limit of {MAX_PUSH_BODY_BYTES} bytes"),
        );
    }
    resp
}

async fn healthz() -> &'static str {
    "ok"
}

async fn not_found(req: Request<axum::body::Body>) -> Response {
    let ip = client_ip(&req).unwrap_or_else(|| "unknown".to_string());
    warn!(
        "not_found ip={} method={} path={}",
        ip,
        req.method(),
        req.uri().path()
    );
    let mut resp = json_error(StatusCode::NOT_FOUND, "not_found", "not found");
    resp.extensions_mut().insert(LoggedError);
    resp
}

async fn log_failed_requests(req: Request<axum::body::Body>, next: Next) -> Response {
    let ip = client_ip(&req).unwrap_or_else(|| "unknown".to_string());
    let method = req.method().as_str().to_string();
    let uri = req.uri().to_string();
    let response = next.run(req).await;
    let status = response.status();
    if (status.is_client_error() || status.is_server_error())
        && response.extensions().get::<LoggedError>().is_none()
    {
        warn!("http_error ip={ip} method={method} uri={uri} status={}", status.as_u16());
    }
    response
}

async fn auth_and_rate_limit_middleware(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(bearer) = extract_bearer(req.headers()) else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            error_codes::UNAUTHORIZED,
            "missing bearer token",
        );
    };

    let auth = match db::authenticate_token(&state.db, &bearer).await {
        Ok(auth) => auth,
        Err(err) => return err.into_response(),
    };

    // SSE subscribe opens a long-lived stream once; rate-limit normal RPC endpoints.
    if req.uri().path() != "/subscribe" {
        let rl_key = format!("dev:{}:{}", auth.account_id, auth.device_id);
        if !state.rate_limiter.check(&rl_key) {
            return json_error(
                StatusCode::TOO_MANY_REQUESTS,
                error_codes::RATE_LIMITED,
                "rate limit exceeded",
            );
        }
    }

    req.extensions_mut().insert(auth);
    next.run(req).await
}

async fn post_pair_redeem(
    State(state): State<AppState>,
    req_headers: HeaderMap,
    Json(payload): Json<PairRedeemRequest>,
) -> Result<Json<voicex_sync_protocol::PairRedeemResponse>, Response> {
    let ip_key = req_headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("direct");
    if !state.rate_limiter.check(&format!("pair:{ip_key}")) {
        return Err(json_error(
            StatusCode::TOO_MANY_REQUESTS,
            error_codes::RATE_LIMITED,
            "pairing rate limit exceeded",
        ));
    }

    db::redeem_pairing_code(&state.db, &state.server_id, &payload)
        .await
        .map(Json)
        .map_err(IntoResponse::into_response)
}

async fn get_account(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedDevice>,
) -> Result<Json<voicex_sync_protocol::AccountInfoResponse>, DbApiError> {
    db::get_account_info(&state.db, &state.server_id, &auth.account_id)
        .await
        .map(Json)
}

async fn post_device_pairing_code(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedDevice>,
    payload: Option<Json<CreatePairingCodeRequest>>,
) -> Result<Json<CreatePairingCodeResponse>, DbApiError> {
    let ttl_minutes = payload
        .and_then(|Json(req)| req.ttl_minutes)
        .unwrap_or(15)
        .clamp(1, 60);
    let created = db::create_pairing_code(&state.db, &auth.account_id, ttl_minutes).await?;
    Ok(Json(CreatePairingCodeResponse {
        pairing_code: created.pairing_code,
        expires_at: created.expires_at,
    }))
}

async fn put_device(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedDevice>,
    Path(device_id): Path<String>,
    Json(payload): Json<DeviceUpdateRequest>,
) -> Result<Json<voicex_sync_protocol::DeviceInfo>, DbApiError> {
    db::update_device(&state.db, &auth, &device_id, &payload)
        .await
        .map(Json)
}

async fn post_envelopes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedDevice>,
    Json(payload): Json<PushEnvelopesRequest>,
) -> Result<Json<voicex_sync_protocol::PushEnvelopesResponse>, DbApiError> {
    let (resp, notifications) = db::push_envelopes(&state.db, &auth, &payload).await?;
    if !notifications.is_empty() {
        let bus = state.bus_for(&auth.account_id);
        for note in notifications {
            let _ = bus.send(note);
        }
    }
    Ok(Json(resp))
}

#[derive(Debug, Deserialize)]
struct PullQuery {
    domain: String,
    #[serde(default)]
    since: i64,
    limit: Option<usize>,
}

async fn get_envelopes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedDevice>,
    Query(query): Query<PullQuery>,
) -> Result<Json<voicex_sync_protocol::PullEnvelopesResponse>, DbApiError> {
    let domain = SyncDomain::from_str(query.domain.trim())
        .map_err(DbApiError::BadRequest)?;
    db::pull_envelopes(&state.db, &auth.account_id, domain, query.since, query.limit)
        .await
        .map(Json)
}

async fn subscribe(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthedDevice>,
) -> Response {
    let mut rx = state.bus_for(&auth.account_id).subscribe();
    let event_stream = stream! {
        loop {
            match rx.recv().await {
                Ok(note) => {
                    if let Ok(json) = serde_json::to_string(&note) {
                        yield Ok::<Event, Infallible>(Event::default().event("notify").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Tell client to catch up via GET /v2/envelopes across domains.
                    let note = NotifyEvent { domain: SyncDomain::History, seq: 0 };
                    if let Ok(json) = serde_json::to_string(&note) {
                        yield Ok::<Event, Infallible>(Event::default().event("notify").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    let mut resp = Sse::new(event_stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(20))
                .text("keepalive"),
        )
        .into_response();
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    resp.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    resp
}

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn client_ip(req: &Request<axum::body::Body>) -> Option<String> {
    if let Some(forwarded) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(first) = forwarded.split(',').next().map(str::trim) {
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
}
