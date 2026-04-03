use std::sync::{Arc, Mutex, OnceLock};

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

static GLOBAL_DEBUG_SERVICE: OnceLock<AsrDebugService> = OnceLock::new();

#[derive(Clone, Default)]
pub struct AsrDebugService {
    inner: Arc<Mutex<AsrDebugInner>>,
}

#[derive(Default)]
struct AsrDebugInner {
    soniox_ws_override: Option<String>,
    soniox_fault_mode: Option<String>,
    mock_server: Option<SonioxMockServerHandle>,
}

struct SonioxMockServerHandle {
    scenario: SonioxMockScenario,
    url: String,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SonioxDebugHarnessStatus {
    pub ws_override: Option<String>,
    pub fault_mode: Option<String>,
    pub mock_running: bool,
    pub mock_url: Option<String>,
    pub mock_scenario: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SonioxMockScenario {
    HappyPath,
    ServerError401,
    ServerError429,
    ServerError502,
    CloseAfterHandshake,
    CloseAfterFirstAudio,
    PartialThenClose,
    StallFinalizing,
}

impl SonioxMockScenario {
    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "happy_path" => Some(Self::HappyPath),
            "server_error_401" => Some(Self::ServerError401),
            "server_error_429" => Some(Self::ServerError429),
            "server_error_502" => Some(Self::ServerError502),
            "close_after_handshake" => Some(Self::CloseAfterHandshake),
            "close_after_first_audio" => Some(Self::CloseAfterFirstAudio),
            "partial_then_close" => Some(Self::PartialThenClose),
            "stall_finalizing" => Some(Self::StallFinalizing),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HappyPath => "happy_path",
            Self::ServerError401 => "server_error_401",
            Self::ServerError429 => "server_error_429",
            Self::ServerError502 => "server_error_502",
            Self::CloseAfterHandshake => "close_after_handshake",
            Self::CloseAfterFirstAudio => "close_after_first_audio",
            Self::PartialThenClose => "partial_then_close",
            Self::StallFinalizing => "stall_finalizing",
        }
    }
}

impl AsrDebugService {
    pub fn install_global(&self) {
        let _ = GLOBAL_DEBUG_SERVICE.set(self.clone());
    }

    pub fn global() -> Option<Self> {
        GLOBAL_DEBUG_SERVICE.get().cloned()
    }

    pub fn soniox_ws_override() -> Option<String> {
        Self::global().and_then(|svc| {
            svc.inner
                .lock()
                .ok()
                .and_then(|inner| inner.soniox_ws_override.clone())
        })
    }

    pub fn soniox_fault_mode() -> Option<String> {
        Self::global().and_then(|svc| {
            svc.inner
                .lock()
                .ok()
                .and_then(|inner| inner.soniox_fault_mode.clone())
        })
    }

    pub fn status(&self) -> SonioxDebugHarnessStatus {
        let inner = self.inner.lock().expect("asr debug lock");
        SonioxDebugHarnessStatus {
            ws_override: inner.soniox_ws_override.clone(),
            fault_mode: inner.soniox_fault_mode.clone(),
            mock_running: inner.mock_server.is_some(),
            mock_url: inner.mock_server.as_ref().map(|mock| mock.url.clone()),
            mock_scenario: inner
                .mock_server
                .as_ref()
                .map(|mock| mock.scenario.as_str().to_string()),
        }
    }

    pub fn set_soniox_fault_mode(&self, fault_mode: Option<String>) -> Result<(), String> {
        let normalized = match fault_mode {
            Some(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            None => None,
        };

        let mut inner = self.inner.lock().map_err(|_| "asr debug lock poisoned")?;
        inner.soniox_fault_mode = normalized;
        Ok(())
    }

    pub fn clear_soniox_debug_overrides_now(&self) -> Result<(), String> {
        let handle = {
            let mut inner = self.inner.lock().map_err(|_| "asr debug lock poisoned")?;
            inner.soniox_fault_mode = None;
            inner.soniox_ws_override = None;
            inner.mock_server.take()
        };

        if let Some(mock) = handle {
            mock.cancel.cancel();
            mock.task.abort();
        }

        Ok(())
    }

    pub async fn start_soniox_mock_server(
        &self,
        scenario: SonioxMockScenario,
    ) -> Result<SonioxDebugHarnessStatus, String> {
        self.stop_mock_server().await?;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|err| format!("Failed to bind Soniox mock server: {err}"))?;
        let addr = listener
            .local_addr()
            .map_err(|err| format!("Failed to read Soniox mock server address: {err}"))?;
        let url = format!("ws://{}/transcribe-websocket", addr);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();

        let task = tokio::spawn(async move {
            loop {
                let accept_result = tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    incoming = listener.accept() => incoming,
                };

                let (stream, _) = match accept_result {
                    Ok(pair) => pair,
                    Err(err) => {
                        log::warn!("Soniox mock server accept failed: {}", err);
                        continue;
                    }
                };

                let connection_cancel = task_cancel.child_token();
                tokio::spawn(async move {
                    if let Err(err) =
                        handle_soniox_mock_connection(stream, scenario, connection_cancel).await
                    {
                        log::warn!("Soniox mock connection failed: {}", err);
                    }
                });
            }
        });

        {
            let mut inner = self.inner.lock().map_err(|_| "asr debug lock poisoned")?;
            inner.soniox_fault_mode = None;
            inner.soniox_ws_override = Some(url.clone());
            inner.mock_server = Some(SonioxMockServerHandle {
                scenario,
                url: url.clone(),
                cancel,
                task,
            });
        }

        Ok(self.status())
    }

    pub async fn stop_mock_server(&self) -> Result<SonioxDebugHarnessStatus, String> {
        let handle = {
            let mut inner = self.inner.lock().map_err(|_| "asr debug lock poisoned")?;
            inner.soniox_ws_override = None;
            inner.mock_server.take()
        };

        if let Some(mock) = handle {
            mock.cancel.cancel();
            mock.task.abort();
        }

        Ok(self.status())
    }

    pub async fn clear_soniox_debug_overrides(&self) -> Result<SonioxDebugHarnessStatus, String> {
        self.clear_soniox_debug_overrides_now()?;
        Ok(self.status())
    }
}

async fn handle_soniox_mock_connection(
    stream: tokio::net::TcpStream,
    scenario: SonioxMockScenario,
    cancel: CancellationToken,
) -> Result<(), String> {
    let ws_stream = accept_async(stream)
        .await
        .map_err(|err| format!("accept websocket failed: {err}"))?;
    let (mut write, mut read) = ws_stream.split();
    let mut saw_audio = false;
    let mut sent_partial = false;

    while let Some(message) = tokio::select! {
        _ = cancel.cancelled() => None,
        next = read.next() => next,
    } {
        match message {
            Ok(Message::Text(text)) => {
                if text.is_empty() {
                    match scenario {
                        SonioxMockScenario::StallFinalizing => {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(30),
                                cancel.cancelled(),
                            )
                            .await;
                            return Ok(());
                        }
                        _ => {
                            let transcript = if saw_audio {
                                "你好，请 shut up，你吵到我用语音输入法了。"
                            } else {
                                "你好，mock 测试成功。"
                            };
                            write
                                .send(Message::Text(
                                    json!({
                                        "tokens": [
                                            { "text": transcript, "is_final": true }
                                        ],
                                        "finished": true
                                    })
                                    .to_string(),
                                ))
                                .await
                                .map_err(|err| format!("send mock final failed: {err}"))?;
                            let _ = write.close().await;
                            return Ok(());
                        }
                    }
                }

                if !text.is_empty() {
                    match scenario {
                        SonioxMockScenario::ServerError401 => {
                            send_mock_error(&mut write, 401, "Mock unauthorized").await?;
                            return Ok(());
                        }
                        SonioxMockScenario::ServerError429 => {
                            send_mock_error(&mut write, 429, "Mock rate limit").await?;
                            return Ok(());
                        }
                        SonioxMockScenario::ServerError502 => {
                            send_mock_error(&mut write, 502, "Mock bad gateway").await?;
                            return Ok(());
                        }
                        SonioxMockScenario::CloseAfterHandshake => {
                            let _ = write.close().await;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
            Ok(Message::Binary(_audio)) => {
                saw_audio = true;
                match scenario {
                    SonioxMockScenario::CloseAfterFirstAudio => {
                        let _ = write.close().await;
                        return Ok(());
                    }
                    SonioxMockScenario::PartialThenClose => {
                        write
                            .send(Message::Text(
                                json!({
                                    "tokens": [
                                        { "text": "你好，请 shut up，", "is_final": false }
                                    ],
                                    "finished": false
                                })
                                .to_string(),
                            ))
                            .await
                            .map_err(|err| format!("send mock partial failed: {err}"))?;
                        let _ = write.close().await;
                        return Ok(());
                    }
                    SonioxMockScenario::HappyPath if !sent_partial => {
                        sent_partial = true;
                        write
                            .send(Message::Text(
                                json!({
                                    "tokens": [
                                        { "text": "你好，请 shut up，", "is_final": false }
                                    ],
                                    "finished": false
                                })
                                .to_string(),
                            ))
                            .await
                            .map_err(|err| format!("send mock partial failed: {err}"))?;
                    }
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(err) => return Err(format!("read websocket message failed: {err}")),
        }
    }

    Ok(())
}

async fn send_mock_error(
    write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message,
    >,
    code: u16,
    message: &str,
) -> Result<(), String> {
    write
        .send(Message::Text(
            json!({
                "error_code": code,
                "error_message": message
            })
            .to_string(),
        ))
        .await
        .map_err(|err| format!("send mock error failed: {err}"))?;
    let _ = write.close().await;
    Ok(())
}
