//! Translate a history row's final text for immediate use, without persisting it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::commands::settings::AppSettings;
use crate::llm::{correction_timeout_for_text, LLMClient, PromptBuildOptions};
use crate::services::{
    history_service::HistoryService, llm_service::build_llm_config_from_settings,
};

#[derive(Default)]
pub struct HistoryTranslationState(Mutex<HashMap<String, CancellationToken>>);

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "code", content = "message", rename_all = "snake_case")]
pub enum TranslationError {
    EmptyText,
    NotConfigured,
    EmptyPrompt,
    Failed(String),
    Timeout,
    Cancelled,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationResult {
    pub text: String,
    pub model_name: Option<String>,
}

// Removing the task on drop also covers a dropped IPC future, so an abandoned
// dialog cannot leave its cancellation handle behind indefinitely.
struct TranslationTask<'a> {
    state: &'a HistoryTranslationState,
    request_id: String,
    cancel: CancellationToken,
}

impl HistoryTranslationState {
    fn start(&self, request_id: String) -> Result<TranslationTask<'_>, TranslationError> {
        let mut tasks = self
            .0
            .lock()
            .map_err(|err| TranslationError::Failed(err.to_string()))?;
        if request_id.trim().is_empty() || tasks.contains_key(&request_id) {
            return Err(TranslationError::Failed(
                "Invalid translation request ID".into(),
            ));
        }
        let cancel = CancellationToken::new();
        tasks.insert(request_id.clone(), cancel.clone());
        Ok(TranslationTask {
            state: self,
            request_id,
            cancel,
        })
    }

    fn cancel(&self, request_id: &str) -> Result<(), TranslationError> {
        let tasks = self
            .0
            .lock()
            .map_err(|err| TranslationError::Failed(err.to_string()))?;
        if let Some(cancel) = tasks.get(request_id) {
            cancel.cancel();
        }
        Ok(())
    }
}

impl Drop for TranslationTask<'_> {
    fn drop(&mut self) {
        self.cancel.cancel();
        match self.state.0.lock() {
            Ok(mut tasks) => {
                tasks.remove(&self.request_id);
            }
            Err(err) => log::error!("Failed to release history translation task: {}", err),
        }
    }
}

#[tauri::command]
pub async fn translate_history_text(
    text: String,
    request_id: String,
    state: State<'_, HistoryTranslationState>,
) -> Result<TranslationResult, TranslationError> {
    let task = state.start(request_id)?;
    let settings =
        crate::storage::get_settings().map_err(|err| TranslationError::Failed(err.to_string()))?;
    let timeout = correction_timeout_for_text(&text);
    translate_text(&text, &settings, task.cancel.clone(), timeout).await
}

#[tauri::command]
pub fn cancel_history_translation(
    request_id: String,
    state: State<'_, HistoryTranslationState>,
) -> Result<(), TranslationError> {
    state.cancel(&request_id)
}

async fn translate_text(
    text: &str,
    settings: &AppSettings,
    cancel: CancellationToken,
    timeout: Duration,
) -> Result<TranslationResult, TranslationError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(TranslationError::EmptyText);
    }
    let config = build_llm_config_from_settings(settings);
    if !config.missing_request_fields().is_empty() {
        return Err(TranslationError::NotConfigured);
    }
    if settings.translation_prompt_template.trim().is_empty() {
        return Err(TranslationError::EmptyPrompt);
    }

    // This explicit action works even when recording's translation trigger or
    // AI correction is disabled. It shares their provider and translation prompt.
    let client = LLMClient::new(config);
    let request = client.correct(
        text,
        &settings.translation_prompt_template,
        "",
        None,
        PromptBuildOptions {
            append_dictionary_if_missing: false,
            append_history_if_missing: false,
        },
    );
    let translated = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(TranslationError::Cancelled),
        result = tokio::time::timeout(timeout, request) => {
            result.map_err(|_| TranslationError::Timeout)?
                .map_err(|err| TranslationError::Failed(err.to_string()))?
        }
    };
    if translated.trim().is_empty() {
        return Err(TranslationError::Failed(
            "The model returned an empty translation".into(),
        ));
    }
    Ok(TranslationResult {
        text: translated,
        model_name: HistoryService::resolve_llm_model_name(settings),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    async fn mock_llm(
        status: &str,
        body: Option<String>,
    ) -> (AppSettings, oneshot::Receiver<Value>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sent, received) = oneshot::channel();
        let status = status.to_string();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            let header_end = loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            assert!(headers.starts_with("POST /chat/completions "));
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            while request.len() < header_end + length {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            sent.send(serde_json::from_slice(&request[header_end..header_end + length]).unwrap())
                .unwrap();
            if let Some(body) = body {
                let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            } else {
                // Keep the provider pending independently of the client future.
                // Cancelling locally cannot undo a request already sent to it.
                let _ = socket.read(&mut buffer).await;
            }
        });
        let mut settings = AppSettings::default();
        settings.llm_provider_type = "openai".into();
        settings.llm_openai_base_url = format!("http://{address}");
        settings.llm_openai_api_key = "test-key".into();
        settings.llm_openai_model = "test-model".into();
        (settings, received, server)
    }

    #[test]
    fn cancellation_is_scoped_and_tasks_are_removed_on_drop() {
        let state = HistoryTranslationState::default();
        let first = state.start("first".into()).unwrap();
        let next = state.start("next".into()).unwrap();
        assert!(state.start("next".into()).is_err());
        state.cancel("first").unwrap();
        assert!(first.cancel.is_cancelled());
        assert!(!next.cancel.is_cancelled());
        drop(first);
        state.cancel("first").unwrap();
        assert!(!next.cancel.is_cancelled());
        drop(next);
        assert!(state.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_empty_input_and_missing_configuration_before_requesting() {
        let mut settings = AppSettings::default();
        let run = |text: &'static str, settings: AppSettings| async move {
            translate_text(
                text,
                &settings,
                CancellationToken::new(),
                Duration::from_secs(1),
            )
            .await
            .unwrap_err()
        };
        assert_eq!(
            run(" \n ", settings.clone()).await,
            TranslationError::EmptyText
        );
        assert_eq!(
            run("你好", settings.clone()).await,
            TranslationError::NotConfigured
        );
        settings.llm_volcengine_api_key = "test-key".into();
        settings.translation_prompt_template.clear();
        assert_eq!(run("你好", settings).await, TranslationError::EmptyPrompt);
    }

    #[tokio::test]
    async fn translates_the_final_text_with_the_current_model_and_translation_prompt() {
        let reply = json!({"choices":[{"message":{"content":"Hello, bridge."}}]}).to_string();
        let (mut settings, received, server) = mock_llm("200 OK", Some(reply)).await;
        settings.translation_prompt_template = "Translate to English; keep the meaning.".into();
        settings.dictionary_text = "A dictionary that must not be appended".into();
        settings.translation_enabled = false;
        settings.enable_llm_correction = false;
        let result = translate_text(
            "你好，桥梁。",
            &settings,
            CancellationToken::new(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(result.text, "Hello, bridge.");
        assert_eq!(result.model_name.as_deref(), Some("OpenAI / test-model"));
        let request = received.await.unwrap();
        assert_eq!(request["model"], "test-model");
        assert_eq!(
            request["messages"][0]["content"],
            settings.translation_prompt_template
        );
        assert_eq!(request["messages"][1]["content"], "原文：\n你好，桥梁。");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn provider_errors_and_empty_replies_are_failures_instead_of_source_text() {
        for (status, reply) in [
            (
                "429 Too Many Requests",
                json!({"error":{"message":"rate limit"}}),
            ),
            ("200 OK", json!({"choices":[{"message":{"content":"  "}}]})),
        ] {
            let (settings, received, server) = mock_llm(status, Some(reply.to_string())).await;
            let result = translate_text(
                "你好",
                &settings,
                CancellationToken::new(),
                Duration::from_secs(2),
            )
            .await;
            assert!(
                matches!(result, Err(TranslationError::Failed(_))),
                "{result:?}"
            );
            received.await.unwrap();
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_and_timeout_return_while_the_provider_is_pending() {
        for should_cancel in [true, false] {
            let (settings, received, server) = mock_llm("200 OK", None).await;
            let cancel = CancellationToken::new();
            let request_timeout = if should_cancel {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(100)
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(
                    translate_text("你好", &settings, cancel.clone(), request_timeout),
                    async {
                        received.await.unwrap();
                        if should_cancel {
                            cancel.cancel();
                        }
                    }
                )
            })
            .await
            .unwrap();
            assert_eq!(
                result.unwrap_err(),
                if should_cancel {
                    TranslationError::Cancelled
                } else {
                    TranslationError::Timeout
                }
            );
            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
        }
    }
}
