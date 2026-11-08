//! One HTTP client per dictation, so the correction request can reuse a
//! connection opened while the user was still speaking.
//!
//! A reqwest `Client` owns its connection pool: a request made through a new
//! client always pays DNS, TCP and TLS before its first byte. Dictations are
//! minutes apart, longer than any keep-alive, so one long-lived client would
//! not help either — and it would keep the system proxy it was built with.
//! Instead each recording builds a fresh client (reading the proxy then, as a
//! per-request client did) and opens a connection to the LLM host; the key
//! release opens it again in case a long recording outlived the keep-alive.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::{Client, Url};

use super::LLMConfig;

const WARM_TIMEOUT: Duration = Duration::from_secs(5);

static SESSION_CLIENT: Mutex<Option<Client>> = Mutex::new(None);

/// Recording started: replace the session client and connect to the LLM host
/// when correction or translation may run.
pub fn begin_session(config: Option<LLMConfig>) {
    tauri::async_runtime::spawn(async move {
        let client = Client::new();
        *SESSION_CLIENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(client.clone());
        if let Some(config) = config {
            warm(client, &config, "recording_started").await;
        }
    });
}

/// Recording stopped: the correction request follows ASR's final result, so
/// connect now, overlapping the handshake with ASR finishing. Reuses the open
/// connection, and so costs one small round trip, when it is still alive.
pub fn rewarm(config: Option<LLMConfig>) {
    let Some(config) = config else { return };
    let Some(client) = current() else { return };
    tauri::async_runtime::spawn(async move {
        warm(client, &config, "recording_stopped").await;
    });
}

/// The client the current dictation's correction should use.
pub fn session_client() -> Client {
    current().unwrap_or_default()
}

fn current() -> Option<Client> {
    SESSION_CLIENT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

async fn warm(client: Client, config: &LLMConfig, reason: &str) {
    let Some(origin) = origin(&config.base_url) else {
        return;
    };
    let started = Instant::now();
    // HEAD carries no credentials and no body, so the connection goes straight
    // back to the pool; any status, even 404, means it is open.
    match client.head(origin.clone()).timeout(WARM_TIMEOUT).send().await {
        Ok(response) => log::info!(
            "LLM connection warmed reason={} host={} status={} elapsed={}ms",
            reason,
            origin.host_str().unwrap_or_default(),
            response.status().as_u16(),
            started.elapsed().as_millis()
        ),
        Err(err) => log::info!(
            "LLM connection warm-up failed reason={} host={} elapsed={}ms: {}",
            reason,
            origin.host_str().unwrap_or_default(),
            started.elapsed().as_millis(),
            err
        ),
    }
}

fn origin(base_url: &str) -> Option<Url> {
    let url = Url::parse(base_url.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    let mut origin = url;
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    Some(origin)
}

#[cfg(test)]
mod tests {
    use super::origin;

    #[test]
    fn origin_keeps_scheme_host_and_port_only() {
        assert_eq!(
            origin("https://api.deepseek.com/v1?x=1").unwrap().as_str(),
            "https://api.deepseek.com/"
        );
        assert_eq!(
            origin(" http://localhost:8080/api/v3/ ").unwrap().as_str(),
            "http://localhost:8080/"
        );
        assert!(origin("").is_none());
        assert!(origin("ftp://example.com").is_none());
    }
}
