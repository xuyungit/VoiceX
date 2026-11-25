//! Sync v2 transport abstraction and self-hosted HTTP/SSE client implementation.
//!
//! Decoupling `SyncTransport` from the sync engine keeps Phase 3 object backends
//! (S3 / WebDAV) pluggable without touching core encryption, outbox, or merge code.

use reqwest::StatusCode;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use thiserror::Error;
use voicex_sync_protocol::{
    error_codes, AccountInfoResponse, ApiErrorBody, CreatePairingCodeRequest,
    CreatePairingCodeResponse, DeviceInfo, DeviceUpdateRequest, PairRedeemRequest,
    PairRedeemResponse, PullEnvelopesResponse, PushEnvelopesRequest, PushEnvelopesResponse,
    SyncDomain,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("device revoked: {0}")]
    DeviceRevoked(String),
    #[error("scope forbidden: {0}")]
    ScopeForbidden(String),
    #[error("pairing code invalid or expired: {0}")]
    PairingInvalidOrExpired(String),
    #[error("payload too large: {0}")]
    PayloadTooLarge(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("storage quota exceeded: {0}")]
    QuotaExceeded(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected server status {status}: {message}")]
    HttpStatus { status: u16, message: String },
}

pub trait SyncTransport: Send + Sync {
    fn pair_redeem<'a>(
        &'a self,
        req: &'a PairRedeemRequest,
    ) -> BoxFuture<'a, Result<PairRedeemResponse, TransportError>>;

    fn create_pairing_code<'a>(
        &'a self,
        device_token: &'a str,
        ttl_minutes: Option<i64>,
    ) -> BoxFuture<'a, Result<CreatePairingCodeResponse, TransportError>>;

    fn get_account<'a>(
        &'a self,
        device_token: &'a str,
    ) -> BoxFuture<'a, Result<AccountInfoResponse, TransportError>>;

    fn update_device<'a>(
        &'a self,
        device_token: &'a str,
        device_id: &'a str,
        req: &'a DeviceUpdateRequest,
    ) -> BoxFuture<'a, Result<DeviceInfo, TransportError>>;

    fn push_envelopes<'a>(
        &'a self,
        device_token: &'a str,
        req: &'a PushEnvelopesRequest,
    ) -> BoxFuture<'a, Result<PushEnvelopesResponse, TransportError>>;

    fn pull_envelopes<'a>(
        &'a self,
        device_token: &'a str,
        domain: SyncDomain,
        since: i64,
        limit: usize,
    ) -> BoxFuture<'a, Result<PullEnvelopesResponse, TransportError>>;
}

#[derive(Clone)]
pub struct SelfHostTransport {
    base_url: String,
    client: reqwest::Client,
    sse_client: reqwest::Client,
}

impl SelfHostTransport {
    pub fn new(server_url: &str) -> Result<Self, TransportError> {
        let base_url = server_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(TransportError::BadRequest("server URL is empty".into()));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| TransportError::Network(e.to_string()))?;
        let sse_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| TransportError::Network(e.to_string()))?;
        Ok(Self {
            base_url,
            client,
            sse_client,
        })
    }

    pub async fn subscribe_sse(
        &self,
        device_token: &str,
    ) -> Result<reqwest::Response, TransportError> {
        let url = format!("{}/v2/subscribe", self.base_url);
        let resp = self
            .sse_client
            .get(&url)
            .bearer_auth(device_token)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .map_err(|e| TransportError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(parse_error_response(resp).await);
        }
        Ok(resp)
    }
}

async fn parse_json_or_error<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, TransportError> {
    if !resp.status().is_success() {
        return Err(parse_error_response(resp).await);
    }
    resp.json::<T>()
        .await
        .map_err(|e| TransportError::Network(format!("invalid server JSON response: {e}")))
}

async fn parse_error_response(resp: reqwest::Response) -> TransportError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if let Ok(parsed) = serde_json::from_str::<ApiErrorBody>(&body) {
        return match parsed.code.as_str() {
            error_codes::DEVICE_REVOKED => TransportError::DeviceRevoked(parsed.message),
            error_codes::UNAUTHORIZED => TransportError::Unauthorized(parsed.message),
            error_codes::SCOPE_FORBIDDEN => TransportError::ScopeForbidden(parsed.message),
            error_codes::PAIRING_INVALID_OR_EXPIRED => {
                TransportError::PairingInvalidOrExpired(parsed.message)
            }
            error_codes::PAYLOAD_TOO_LARGE => TransportError::PayloadTooLarge(parsed.message),
            error_codes::RATE_LIMITED => TransportError::RateLimited(parsed.message),
            error_codes::QUOTA_EXCEEDED => TransportError::QuotaExceeded(parsed.message),
            error_codes::BAD_REQUEST => TransportError::BadRequest(parsed.message),
            _ => TransportError::HttpStatus {
                status: status.as_u16(),
                message: parsed.message,
            },
        };
    }

    let message = if body.trim().is_empty() {
        status.canonical_reason().unwrap_or("request failed").to_string()
    } else {
        body
    };
    match status {
        StatusCode::UNAUTHORIZED => TransportError::Unauthorized(message),
        StatusCode::FORBIDDEN => TransportError::ScopeForbidden(message),
        StatusCode::CONFLICT => TransportError::PairingInvalidOrExpired(message),
        StatusCode::PAYLOAD_TOO_LARGE => TransportError::PayloadTooLarge(message),
        StatusCode::TOO_MANY_REQUESTS => TransportError::RateLimited(message),
        StatusCode::INSUFFICIENT_STORAGE => TransportError::QuotaExceeded(message),
        StatusCode::BAD_REQUEST => TransportError::BadRequest(message),
        _ => TransportError::HttpStatus {
            status: status.as_u16(),
            message,
        },
    }
}

impl SyncTransport for SelfHostTransport {
    fn pair_redeem<'a>(
        &'a self,
        req: &'a PairRedeemRequest,
    ) -> BoxFuture<'a, Result<PairRedeemResponse, TransportError>> {
        Box::pin(async move {
            let url = format!("{}/v2/pair/redeem", self.base_url);
            let resp = self
                .client
                .post(&url)
                .json(req)
                .send()
                .await
                .map_err(|e| TransportError::Network(e.to_string()))?;
            parse_json_or_error(resp).await
        })
    }

    fn create_pairing_code<'a>(
        &'a self,
        device_token: &'a str,
        ttl_minutes: Option<i64>,
    ) -> BoxFuture<'a, Result<CreatePairingCodeResponse, TransportError>> {
        Box::pin(async move {
            let url = format!("{}/v2/devices/pairing-code", self.base_url);
            let resp = self
                .client
                .post(&url)
                .bearer_auth(device_token)
                .json(&CreatePairingCodeRequest { ttl_minutes })
                .send()
                .await
                .map_err(|e| TransportError::Network(e.to_string()))?;
            parse_json_or_error(resp).await
        })
    }

    fn get_account<'a>(
        &'a self,
        device_token: &'a str,
    ) -> BoxFuture<'a, Result<AccountInfoResponse, TransportError>> {
        Box::pin(async move {
            let url = format!("{}/v2/account", self.base_url);
            let resp = self
                .client
                .get(&url)
                .bearer_auth(device_token)
                .send()
                .await
                .map_err(|e| TransportError::Network(e.to_string()))?;
            parse_json_or_error(resp).await
        })
    }

    fn update_device<'a>(
        &'a self,
        device_token: &'a str,
        device_id: &'a str,
        req: &'a DeviceUpdateRequest,
    ) -> BoxFuture<'a, Result<DeviceInfo, TransportError>> {
        Box::pin(async move {
            let url = format!(
                "{}/v2/devices/{}",
                self.base_url,
                urlencoding::encode(device_id)
            );
            let resp = self
                .client
                .put(&url)
                .bearer_auth(device_token)
                .json(req)
                .send()
                .await
                .map_err(|e| TransportError::Network(e.to_string()))?;
            parse_json_or_error(resp).await
        })
    }

    fn push_envelopes<'a>(
        &'a self,
        device_token: &'a str,
        req: &'a PushEnvelopesRequest,
    ) -> BoxFuture<'a, Result<PushEnvelopesResponse, TransportError>> {
        Box::pin(async move {
            let url = format!("{}/v2/envelopes", self.base_url);
            let resp = self
                .client
                .post(&url)
                .bearer_auth(device_token)
                .json(req)
                .send()
                .await
                .map_err(|e| TransportError::Network(e.to_string()))?;
            parse_json_or_error(resp).await
        })
    }

    fn pull_envelopes<'a>(
        &'a self,
        device_token: &'a str,
        domain: SyncDomain,
        since: i64,
        limit: usize,
    ) -> BoxFuture<'a, Result<PullEnvelopesResponse, TransportError>> {
        Box::pin(async move {
            let url = format!(
                "{}/v2/envelopes?domain={}&since={}&limit={}",
                self.base_url,
                domain.as_str(),
                since.max(0),
                limit.clamp(1, 200)
            );
            let resp = self
                .client
                .get(&url)
                .bearer_auth(device_token)
                .send()
                .await
                .map_err(|e| TransportError::Network(e.to_string()))?;
            parse_json_or_error(resp).await
        })
    }
}
