//! Exercise the real batch/retranscribe and history paths with an isolated DB.
use super::*;
use crate::commands::retranscribe::{re_transcribe, ReTranscribeRequest};
use crate::commands::settings::AppSettings;
use std::io::{Read, Write};
use std::path::Path;

#[test]
fn short_batch_recordings_and_diagnostics_survive_failure() {
    const CHILD: &str = "VOICEX_ASR_REGRESSION_DIR";
    if let Some(directory) = std::env::var_os(CHILD) {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(run_regressions(Path::new(&directory)));
        return;
    }
    // DB is a process-wide OnceLock. A subprocess prevents this test's settings
    // from contaminating other tests and never opens the user's database.
    let directory = std::env::temp_dir().join(format!("voicex-asr-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "session::asr_regression_tests::short_batch_recordings_and_diagnostics_survive_failure",
            "--nocapture",
        ])
        .env(CHILD, &directory)
        .env("RUST_LOG", "warn")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .output()
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn run_regressions(directory: &Path) {
    // Use the production sink, so the assertions check persisted diagnostics.
    crate::logging::init(directory).unwrap();
    let log_path = directory.join("voicex.log");

    // Settings load failure takes the explicit startup-error path and releases
    // recording state, rather than disappearing before on_finished.
    let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(1);
    let reason = crate::services::asr_manager::AsrManager::new()
        .stream(
            16_000,
            1,
            audio_rx,
            tokio_util::sync::CancellationToken::new(),
            |_| panic!("settings failure cannot emit text"),
            |_| panic!("settings failure is returned to the session"),
        )
        .await
        .unwrap_err();
    assert!(audio_tx.is_closed());
    let controller = SessionController::default();
    let mut state = AppState::new();
    state.handle_hotkey_pressed();
    controller.handle_message(&mut state, SessionMessage::AsrStartupFailed { reason });
    assert!(!state.is_recording);
    assert!(state
        .terminal_error_message
        .as_deref()
        .unwrap()
        .contains("无法加载识别设置"));
    controller.on_error_display_done_state(&mut state);

    crate::storage::init_database(Path::new(":memory:")).unwrap();
    let mut settings = AppSettings::default();
    settings.asr_provider_type = "openai".into();
    settings.openai_asr_mode = "batch".into();
    settings.openai_asr_model = "whisper-1".into();
    settings.openai_asr_api_key = "private-test-credential".into();
    let raw_body =
        r#"{"request_id":"abc413def","message":"upstream unavailable","trace":"raw-detail-xyz"}"#;
    for (index, duration_ms, status, body, expected) in [
        (1, 300, "200 OK", r#"{"text":""}"#, "未识别到有效语音"),
        (2, 700, "503 Service Unavailable", raw_body, "当前服务异常"),
    ] {
        let (endpoint, server) = mock_transcription(status, body);
        settings.openai_asr_base_url = endpoint;
        crate::storage::save_settings(&settings).unwrap();
        let path = directory.join(format!("short_{duration_ms}.wav"));
        write_wav(&path, duration_ms);
        let controller = SessionController::default();
        let (tx, mut rx) = unbounded_channel();
        controller.attach_sender(tx);
        let mut state = AppState::new();
        state.transition_to_finalizing();
        // Start at the capture-complete boundary; this invokes the actual HTTP
        // client, batch completion handler, async DB writer, and cleanup.
        controller.handle_audio_stopped_state(
            &mut state,
            Some(path.to_string_lossy().into()),
            None,
            Some(duration_ms),
        );
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, SessionMessage::BatchAsrFailed { .. }));
        controller.handle_message(&mut state, event);
        server.join().unwrap();
        assert!(state
            .terminal_error_message
            .as_deref()
            .unwrap()
            .contains(expected));
        assert!(!state.final_injected && !state.is_recording);
        let record = wait_for_history(index)
            .await
            .into_iter()
            .find(|row| row.duration_ms == duration_ms as i64)
            .unwrap();
        assert_eq!(
            record.error_code,
            crate::services::history_service::HISTORY_ERROR_ASR_FAILED
        );
        assert_eq!(record.audio_path.as_deref(), path.to_str());
        assert!(record.error_message.as_deref().unwrap().contains(expected));
        assert!(!record
            .error_message
            .as_deref()
            .unwrap()
            .contains("raw-detail-xyz"));
        controller.on_error_display_done_state(&mut state);
        assert!(
            path.exists(),
            "failed recording must remain available for retry"
        );
    }

    // Local validation is visible and persists its actionable explanation.
    settings.openai_asr_api_key.clear();
    crate::storage::save_settings(&settings).unwrap();
    let path = directory.join("short_500.wav");
    write_wav(&path, 500);
    let controller = SessionController::default();
    let mut state = AppState::new();
    state.transition_to_finalizing();
    controller.handle_audio_stopped_state(
        &mut state,
        Some(path.to_string_lossy().into()),
        None,
        Some(500),
    );
    assert!(state
        .terminal_error_message
        .as_deref()
        .unwrap()
        .contains("API Key"));
    let rows = wait_for_history(3).await;
    assert!(rows
        .iter()
        .any(|row| row.duration_ms == 500
            && row.error_message.as_deref().unwrap().contains("API Key")));
    let error = re_transcribe(request(&path, "openai")).await.unwrap_err();
    assert!(error.contains("API Key"));
    controller.on_error_display_done_state(&mut state);
    assert!(path.exists());

    // A broken local OGG never reaches the network or blames provider capability.
    settings.qwen_asr_api_key = "private-test-credential".into();
    crate::storage::save_settings(&settings).unwrap();
    let broken = directory.join("500_test.ogg");
    std::fs::write(&broken, b"invalid ogg container").unwrap();
    let error = re_transcribe(request(&broken, "qwen")).await.unwrap_err();
    assert!(error.contains("解码"), "{error}");
    assert!(!error.contains("Qwen"));

    let logs = std::fs::read_to_string(log_path).unwrap();
    assert!(
        logs.contains(raw_body),
        "original server body must survive friendly mapping"
    );
    assert!(logs.contains("Database not initialized"));
    assert!(logs.contains("Re-transcribe configuration validation failed"));
    assert!(logs.contains("openai_asr_api_key"));
    assert!(logs.contains("Failed to read OGG OpusHead packet"));
    assert!(logs.contains("Batch ASR returned empty result"));
    assert!(!logs.contains("private-test-credential"));
}

fn request(path: &Path, provider: &str) -> ReTranscribeRequest {
    ReTranscribeRequest {
        audio_path: path.to_string_lossy().into(),
        asr_provider: provider.into(),
        enable_llm: false,
        llm_provider: None,
        history_mode: None,
    }
}

async fn wait_for_history(count: usize) -> Vec<crate::storage::HistoryRecord> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = crate::storage::get_history(20, 0).unwrap();
            if rows.len() >= count {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

fn write_wav(path: &Path, duration_ms: u64) {
    let data_len = duration_ms as u32 * 32; // 16 kHz, mono i16
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(b"\x02\0\x10\0data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(44 + data_len as usize, 0);
    std::fs::write(path, wav).unwrap();
}

fn mock_transcription(
    status: &'static str,
    body: &'static str,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0u8; 4096];
            let n = stream.read(&mut buffer).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let size: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                if request.len() >= end + 4 + size {
                    break;
                }
            }
        }
        assert!(request.starts_with(b"POST /v1/audio/transcriptions "));
        assert!(
            request.windows(4).any(|w| w == b"RIFF"),
            "short recording must be uploaded"
        );
        write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    (endpoint, thread)
}
