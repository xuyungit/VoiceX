//! Local ASR client backed by the external `coli` CLI.

use std::{
    collections::VecDeque,
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::mpsc::Receiver,
};

use super::audio_utils::resample_to_16k;
use super::config::AsrConfig;
use super::protocol::{AsrError, AsrEvent};

const COLI_DEFAULT_INTERVAL_MS: u32 = 1000;
const COLI_EXIT_GRACE_MS: u64 = 10_000;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;
const COLI_SENSEVOICE_DIR: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17";
const COLI_SENSEVOICE_CHECK_FILE: &str = "model.int8.onnx";
const COLI_WHISPER_DIR: &str = "sherpa-onnx-whisper-tiny.en";
const COLI_WHISPER_CHECK_FILE: &str = "tiny.en-encoder.int8.onnx";
const COLI_VAD_CHECK_FILE: &str = "silero_vad.onnx";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColiRefinementMode {
    Off,
    SenseVoice,
    Whisper,
}

impl Default for ColiRefinementMode {
    fn default() -> Self {
        Self::Off
    }
}

impl ColiRefinementMode {
    pub fn from_str(value: &str) -> Self {
        match value {
            "sensevoice" => Self::SenseVoice,
            "whisper" => Self::Whisper,
            _ => Self::Off,
        }
    }

    pub fn cli_model_name(&self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::SenseVoice => Some("sensevoice"),
            Self::Whisper => Some("whisper"),
        }
    }

    pub fn display_name(&self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::SenseVoice => Some("sensevoice-small"),
            Self::Whisper => Some("whisper-tiny.en"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColiAsrStatus {
    pub available: bool,
    pub configured_command: String,
    pub resolved_path: Option<String>,
    pub ffmpeg_available: bool,
    pub models_dir: Option<String>,
    pub sensevoice_installed: bool,
    pub whisper_installed: bool,
    pub vad_installed: bool,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ColiStreamLine {
    text: String,
    #[serde(default)]
    is_final: bool,
}

#[derive(Debug, Deserialize)]
struct ColiFileAsrOutput {
    text: String,
}

#[derive(Debug, Clone)]
struct ColiCommandInvocation {
    program: PathBuf,
    prefix_args: Vec<OsString>,
    extra_path_dirs: Vec<PathBuf>,
}

impl ColiCommandInvocation {
    fn display_command(&self) -> String {
        let mut parts = vec![self.program.display().to_string()];
        parts.extend(
            self.prefix_args
                .iter()
                .map(|arg| PathBuf::from(arg).display().to_string()),
        );
        parts.join(" ")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        for arg in &self.prefix_args {
            command.arg(arg);
        }
        apply_runtime_path(&mut command, &self.extra_path_dirs);
        #[cfg(target_os = "windows")]
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
}

#[derive(Debug, Default)]
struct TranscriptAccumulator {
    accumulated_final: String,
    pending_partial: String,
    last_emitted_final: String,
}

impl TranscriptAccumulator {
    fn push_partial(&mut self, text: &str) -> Option<String> {
        let incoming = text.trim();
        if incoming.is_empty() {
            return None;
        }

        let combined = merge_transcript(&self.accumulated_final, incoming);
        self.pending_partial = combined
            .strip_prefix(&self.accumulated_final)
            .unwrap_or(incoming)
            .to_string();

        Some(combined)
    }

    fn push_final(&mut self, text: &str) -> Option<String> {
        let incoming = text.trim();
        if incoming.is_empty() {
            return self.flush_pending();
        }

        let combined = merge_transcript(&self.accumulated_final, incoming);
        self.pending_partial.clear();

        if combined == self.last_emitted_final {
            self.accumulated_final = combined;
            return None;
        }

        self.accumulated_final = combined.clone();
        self.last_emitted_final = combined.clone();
        Some(combined)
    }

    fn flush_pending(&mut self) -> Option<String> {
        if self.pending_partial.is_empty() {
            return None;
        }

        let combined = format!("{}{}", self.accumulated_final, self.pending_partial);
        self.pending_partial.clear();

        if combined == self.last_emitted_final {
            self.accumulated_final = combined;
            return None;
        }

        self.accumulated_final = combined.clone();
        self.last_emitted_final = combined.clone();
        Some(combined)
    }
}

fn merge_transcript(accumulated: &str, incoming: &str) -> String {
    if accumulated.is_empty() {
        return incoming.to_string();
    }
    if incoming.starts_with(accumulated) {
        return incoming.to_string();
    }
    if accumulated.ends_with(incoming) {
        return accumulated.to_string();
    }
    format!("{}{}", accumulated, incoming)
}

pub struct ColiAsrClient {
    config: AsrConfig,
}

impl ColiAsrClient {
    pub fn new(config: AsrConfig) -> Self {
        Self { config }
    }

    pub async fn stream_session<F>(
        &self,
        sample_rate: u32,
        channels: u16,
        mut audio_rx: Receiver<Vec<u8>>,
        cancel: tokio_util::sync::CancellationToken,
        _history: Vec<String>,
        on_event: F,
    ) -> Result<(), AsrError>
    where
        F: Fn(AsrEvent) + Send + Sync + 'static,
    {
        let invocation =
            resolve_coli_invocation(&self.config.coli_command_path).ok_or_else(|| {
                AsrError::ConnectionFailed(
                    "Local ASR provider selected, but `coli` or its runtime dependencies were not found".to_string(),
                )
            })?;

        let mut command = invocation.command();
        command
            .arg("asr-stream")
            .arg("--json")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if self.config.coli_use_vad {
            command.arg("--vad");
        } else {
            command.arg("--asr-interval-ms").arg(
                self.config
                    .coli_asr_interval_ms
                    .max(COLI_DEFAULT_INTERVAL_MS / 10)
                    .to_string(),
            );
        }

        if channels != 1 {
            log::warn!(
                "Coli ASR expects mono PCM; capture is reporting {} channels. Capture pipeline should already be downmixing to mono.",
                channels
            );
        }

        log::info!(
            "Starting ASR stream [coli] (cmd: {}, capture {} Hz -> stream 16000 Hz, {} ch, vad={}, interval_ms={})",
            invocation.display_command(),
            sample_rate,
            channels,
            self.config.coli_use_vad,
            self.config.coli_asr_interval_ms,
        );

        let mut child = command.spawn().map_err(|e| {
            AsrError::ConnectionFailed(format!("Failed to start `coli asr-stream`: {}", e))
        })?;

        let mut stdin = child.stdin.take().ok_or_else(|| {
            AsrError::ConnectionFailed("Failed to open stdin for `coli`".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AsrError::ConnectionFailed("Failed to open stdout for `coli`".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AsrError::ConnectionFailed("Failed to open stderr for `coli`".to_string())
        })?;

        let diagnostics_enabled = self.config.enable_diagnostics;
        let on_event = Arc::new(on_event);
        let stdout_on_event = on_event.clone();
        let saw_transcript = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_transcript_reader = saw_transcript.clone();
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(8)));
        let stderr_tail_reader = stderr_tail.clone();

        let stdout_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut transcript = TranscriptAccumulator::default();
            while let Some(line) = lines.next_line().await? {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                match serde_json::from_str::<ColiStreamLine>(trimmed) {
                    Ok(evt) => {
                        let text = evt.text.trim();
                        if text.is_empty() {
                            continue;
                        }
                        let combined = if evt.is_final {
                            transcript.push_final(text)
                        } else {
                            transcript.push_partial(text)
                        };
                        if let Some(text) = combined {
                            saw_transcript_reader.store(true, std::sync::atomic::Ordering::SeqCst);
                            stdout_on_event(AsrEvent {
                                text,
                                is_final: evt.is_final,
                                prefetch: false,
                                definite: evt.is_final,
                                confidence: None,
                            });
                        }
                    }
                    Err(_) => {
                        if diagnostics_enabled {
                            log::info!("COLI_DIAG stdout {}", trimmed);
                        } else {
                            log::debug!("coli stdout: {}", trimmed);
                        }
                    }
                }
            }

            if let Some(text) = transcript.flush_pending() {
                saw_transcript_reader.store(true, std::sync::atomic::Ordering::SeqCst);
                stdout_on_event(AsrEvent {
                    text,
                    is_final: true,
                    prefetch: false,
                    definite: true,
                    confidence: None,
                });
            }

            Ok::<(), std::io::Error>(())
        });

        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Some(line) = lines.next_line().await? {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                push_tail_line(&stderr_tail_reader, trimmed);
                if diagnostics_enabled {
                    log::info!("COLI_DIAG stderr {}", trimmed);
                } else {
                    log::debug!("coli stderr: {}", trimmed);
                }
            }

            Ok::<(), std::io::Error>(())
        });

        while let Some(chunk) = tokio::select! {
            _ = cancel.cancelled() => None,
            next = audio_rx.recv() => next,
        } {
            let pcm = if sample_rate == 16_000 {
                chunk
            } else {
                resample_to_16k(&chunk, sample_rate)
            };

            if pcm.is_empty() {
                continue;
            }

            stdin.write_all(&pcm).await.map_err(|e| {
                AsrError::ConnectionFailed(format!("Failed to write audio into `coli`: {}", e))
            })?;
        }

        drop(stdin);

        let mut exit_timed_out = false;
        let exit_status = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                None
            }
            status = tokio::time::timeout(Duration::from_millis(COLI_EXIT_GRACE_MS), child.wait()) => {
                match status {
                    Ok(status) => Some(
                        status.map_err(|e| {
                            AsrError::ConnectionFailed(format!("Failed waiting for `coli` to exit: {}", e))
                        })?
                    ),
                    Err(_) => {
                        exit_timed_out = true;
                        log::warn!(
                            "Timed out waiting {} ms for `coli` to exit after stdin closed; terminating process",
                            COLI_EXIT_GRACE_MS
                        );
                        let _ = child.kill().await;
                        Some(child.wait().await.map_err(|e| {
                            AsrError::ConnectionFailed(format!(
                                "Failed waiting for terminated `coli` to exit: {}",
                                e
                            ))
                        })?)
                    }
                }
            }
        };

        stdout_task
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("`coli` stdout task failed: {}", e)))?
            .map_err(|e| {
                AsrError::ConnectionFailed(format!("Failed reading `coli` stdout: {}", e))
            })?;
        stderr_task
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("`coli` stderr task failed: {}", e)))?
            .map_err(|e| {
                AsrError::ConnectionFailed(format!("Failed reading `coli` stderr: {}", e))
            })?;

        if cancel.is_cancelled() {
            log::debug!("coli ASR cancelled");
            return Ok(());
        }

        if exit_timed_out {
            let saw_transcript = saw_transcript.load(std::sync::atomic::Ordering::SeqCst);
            let stderr_tail = render_tail(&stderr_tail);
            if saw_transcript {
                log::warn!(
                    "`coli` required forced termination after EOF on Windows; proceeding with streamed transcript"
                );
                if !stderr_tail.is_empty() {
                    log::warn!(
                        "`coli` stderr tail before forced termination: {}",
                        stderr_tail
                    );
                }
                return Ok(());
            }

            return Err(AsrError::ConnectionFailed(if stderr_tail.is_empty() {
                format!(
                    "`coli` did not exit within {} ms after stdin closed",
                    COLI_EXIT_GRACE_MS
                )
            } else {
                format!(
                    "`coli` did not exit within {} ms after stdin closed: {}",
                    COLI_EXIT_GRACE_MS, stderr_tail
                )
            }));
        }

        let status = exit_status
            .ok_or_else(|| AsrError::ConnectionFailed("`coli` exited unexpectedly".to_string()))?;

        if status.success() {
            Ok(())
        } else {
            let stderr_tail = render_tail(&stderr_tail);
            Err(AsrError::ServerError(if stderr_tail.is_empty() {
                format!("`coli` exited with status {}", status)
            } else {
                format!("`coli` exited with status {}: {}", status, stderr_tail)
            }))
        }
    }

    pub async fn refine_file(
        &self,
        audio_path: &Path,
    ) -> Result<Option<(String, String)>, AsrError> {
        let mode = self.config.coli_final_refinement_mode.clone();
        let Some(model) = mode.cli_model_name() else {
            return Ok(None);
        };

        let invocation =
            resolve_coli_invocation(&self.config.coli_command_path).ok_or_else(|| {
                AsrError::ConnectionFailed(
                    "Local ASR refinement requested, but `coli` or its runtime dependencies were not found".to_string(),
                )
            })?;
        log::info!(
            "COLI_REFINE cli_invocation cmd={} model={} audio_path={}",
            invocation.display_command(),
            model,
            audio_path.display()
        );

        if !audio_path.is_file() {
            return Err(AsrError::ConnectionFailed(format!(
                "Local ASR refinement audio file not found: {}",
                audio_path.display()
            )));
        }

        let output = invocation
            .command()
            .arg("asr")
            .arg("--json")
            .arg("--model")
            .arg(model)
            .arg(audio_path)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!(
                    "Failed to run `coli asr` for final refinement: {}",
                    e
                ))
            })?;
        log::info!(
            "COLI_REFINE cli_exit status={} stdout_bytes={} stderr_bytes={}",
            output.status,
            output.stdout.len(),
            output.stderr.len()
        );

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(AsrError::ServerError(if stderr.is_empty() {
                format!("`coli asr` exited with status {}", output.status)
            } else {
                format!(
                    "`coli asr` exited with status {}: {}",
                    output.status, stderr
                )
            }));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: ColiFileAsrOutput = parse_json_suffix(&stdout).ok_or_else(|| {
            AsrError::ProtocolError(
                "Failed to parse JSON output from `coli asr --json`".to_string(),
            )
        })?;

        let text = parsed.text.trim().to_string();
        log::info!("COLI_REFINE cli_parsed_text_len={}", text.chars().count());
        if text.is_empty() {
            return Ok(None);
        }

        Ok(Some((
            text,
            mode.display_name().unwrap_or(model).to_string(),
        )))
    }
}

pub fn probe_coli_status(configured_command: Option<&str>) -> ColiAsrStatus {
    let configured_command = configured_command.unwrap_or("").trim().to_string();
    let resolved_path = resolve_coli_command(&configured_command);
    let invocation = resolve_coli_invocation(&configured_command);
    let ffmpeg_available = is_ffmpeg_available();
    let models_dir = coli_models_dir();
    let sensevoice_installed = models_dir
        .as_ref()
        .map(|dir| {
            dir.join(COLI_SENSEVOICE_DIR)
                .join(COLI_SENSEVOICE_CHECK_FILE)
                .exists()
        })
        .unwrap_or(false);
    let whisper_installed = models_dir
        .as_ref()
        .map(|dir| {
            dir.join(COLI_WHISPER_DIR)
                .join(COLI_WHISPER_CHECK_FILE)
                .exists()
        })
        .unwrap_or(false);
    let vad_installed = models_dir
        .as_ref()
        .map(|dir| dir.join(COLI_VAD_CHECK_FILE).exists())
        .unwrap_or(false);

    let message = match (&resolved_path, &invocation, configured_command.is_empty()) {
        (Some(_), Some(_), true) => {
            "Detected local `coli` automatically. VoiceX can use it for offline ASR.".to_string()
        }
        (Some(_), Some(_), false) => {
            "Configured `coli` command is available. VoiceX can use it for offline ASR.".to_string()
        }
        (Some(path), None, _)
            if resolve_node_script_entry(path).is_some() || is_windows_command_wrapper(path) =>
        {
            "Local `coli` was found, but `node` was not found. Install Node.js or provide a runnable binary path.".to_string()
        }
        (Some(_), None, _) => {
            "Local `coli` was found, but VoiceX could not build a runnable invocation for it.".to_string()
        }
        (None, _, true) => {
            "Local `coli` was not found. Install `@marswave/coli`, or provide a custom command path.".to_string()
        }
        (None, _, false) => {
            "Configured `coli` command path was not found. Check the path or leave it empty for auto-detect.".to_string()
        }
    };

    ColiAsrStatus {
        available: invocation.is_some(),
        configured_command,
        resolved_path: resolved_path.map(|path| path.display().to_string()),
        ffmpeg_available,
        models_dir: models_dir.map(|path| path.display().to_string()),
        sensevoice_installed,
        whisper_installed,
        vad_installed,
        message,
    }
}

pub fn resolve_coli_command(configured_command: &str) -> Option<PathBuf> {
    let configured_command = configured_command.trim();

    if !configured_command.is_empty() {
        let configured_path = PathBuf::from(configured_command);
        if looks_like_path(&configured_path) {
            return existing_path(configured_path);
        }

        if let Some(found) = find_in_path(configured_command) {
            return Some(found);
        }

        return existing_path(configured_path);
    }

    find_in_path("coli").or_else(find_in_common_locations)
}

fn resolve_coli_invocation(configured_command: &str) -> Option<ColiCommandInvocation> {
    let resolved_command_path = resolve_coli_command(configured_command)?;

    if let Some(script_path) = resolve_node_script_entry(&resolved_command_path) {
        let node_path = find_node_binary()?;
        let node_dir = node_path.parent().map(Path::to_path_buf);
        return Some(ColiCommandInvocation {
            program: node_path,
            prefix_args: vec![script_path.clone().into_os_string()],
            extra_path_dirs: collect_runtime_dirs([
                resolved_command_path.parent(),
                script_path.parent(),
                node_dir.as_deref(),
            ]),
        });
    }

    if is_windows_command_wrapper(&resolved_command_path) {
        let shell = find_cmd_shell()?;
        let node_binary = find_node_binary();
        let node_dir = node_binary.as_deref().and_then(Path::parent);
        return Some(ColiCommandInvocation {
            program: shell,
            prefix_args: vec![
                OsString::from("/d"),
                OsString::from("/s"),
                OsString::from("/c"),
                resolved_command_path.clone().into_os_string(),
            ],
            extra_path_dirs: collect_runtime_dirs([resolved_command_path.parent(), node_dir]),
        });
    }

    Some(ColiCommandInvocation {
        program: resolved_command_path.clone(),
        prefix_args: Vec::new(),
        extra_path_dirs: collect_runtime_dirs([resolved_command_path.parent()]),
    })
}

fn looks_like_path(path: &Path) -> bool {
    path.is_absolute() || path.components().count() > 1
}

fn existing_path(path: PathBuf) -> Option<PathBuf> {
    if path.is_file() {
        Some(normalize_existing_path(
            path.canonicalize().ok().unwrap_or(path),
        ))
    } else {
        None
    }
}

fn normalize_existing_path(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let text = path.as_os_str().to_string_lossy();
        if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{}", stripped));
        }
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }

    path
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        for candidate in executable_names(command) {
            let full_path = dir.join(&candidate);
            if let Some(path) = existing_path(full_path) {
                return Some(path);
            }
        }
    }
    None
}

fn find_in_common_locations() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let candidates = ["/opt/homebrew/bin/coli", "/usr/local/bin/coli"];
    #[cfg(target_os = "windows")]
    let candidates = windows_coli_candidates();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let candidates = ["/usr/local/bin/coli", "/usr/bin/coli"];

    candidates
        .iter()
        .find_map(|candidate| existing_path(PathBuf::from(candidate)))
}

fn find_node_binary() -> Option<PathBuf> {
    find_in_path("node").or_else(|| {
        #[cfg(target_os = "macos")]
        let candidates = [
            "/opt/homebrew/bin/node",
            "/usr/local/bin/node",
            "/usr/bin/node",
        ];
        #[cfg(target_os = "windows")]
        let candidates = windows_node_candidates();
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        let candidates = ["/usr/local/bin/node", "/usr/bin/node"];

        candidates
            .iter()
            .find_map(|candidate| existing_path(PathBuf::from(candidate)))
    })
}

fn requires_node_interpreter(path: &Path) -> bool {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("js"))
    {
        return true;
    }

    let shebang = std::fs::read(path)
        .ok()
        .and_then(|bytes| {
            bytes
                .split(|b| *b == b'\n')
                .next()
                .map(|line| String::from_utf8_lossy(line).to_string())
        })
        .unwrap_or_default();

    shebang.starts_with("#!") && shebang.to_ascii_lowercase().contains("node")
}

fn resolve_node_script_entry(path: &Path) -> Option<PathBuf> {
    if requires_node_interpreter(path) {
        return Some(path.to_path_buf());
    }

    resolve_windows_command_shim_script(path)
}

fn is_windows_command_wrapper(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
}

fn resolve_windows_command_shim_script(path: &Path) -> Option<PathBuf> {
    if !is_windows_command_wrapper(path) {
        return None;
    }

    let contents = std::fs::read_to_string(path).ok()?;
    extract_windows_command_shim_script(&contents, path)
}

fn extract_windows_command_shim_script(contents: &str, shim_path: &Path) -> Option<PathBuf> {
    for quoted in contents.split('"').skip(1).step_by(2) {
        let Some(candidate) = normalize_windows_shim_script_path(quoted, shim_path) else {
            continue;
        };
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

fn normalize_windows_shim_script_path(raw: &str, shim_path: &Path) -> Option<PathBuf> {
    let trimmed = raw.trim().trim_matches('\'');
    if !trimmed.to_ascii_lowercase().ends_with(".js") {
        return None;
    }

    let shim_dir = shim_path.parent()?;
    let lower = trimmed.to_ascii_lowercase();
    let relative = if lower.starts_with("%dp0%\\") {
        &trimmed[6..]
    } else if lower.starts_with("%~dp0\\") {
        &trimmed[7..]
    } else if Path::new(trimmed).is_absolute() {
        return Some(PathBuf::from(trimmed));
    } else if trimmed.contains('%') {
        return None;
    } else {
        trimmed
    };

    Some(shim_dir.join(relative.replace('\\', std::path::MAIN_SEPARATOR_STR)))
}

fn find_cmd_shell() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        env::var_os("COMSPEC")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| {
                [
                    "C:\\Windows\\System32\\cmd.exe",
                    "C:\\WINNT\\System32\\cmd.exe",
                ]
                .iter()
                .find_map(|candidate| existing_path(PathBuf::from(candidate)))
            })
    }

    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

fn apply_runtime_path(command: &mut Command, extra_dirs: &[PathBuf]) {
    if let Some(path) = stable_runtime_path(extra_dirs) {
        command.env("PATH", path);
    }
}

fn stable_runtime_path(extra_dirs: &[PathBuf]) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|value| env::split_paths(&value).collect())
        .unwrap_or_default();

    #[cfg(target_os = "macos")]
    let extras = [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ];
    #[cfg(target_os = "windows")]
    let extras = windows_runtime_dirs();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let extras = ["/usr/local/bin", "/usr/bin", "/bin"];

    for extra in extras {
        let path = PathBuf::from(extra);
        if !dirs.iter().any(|existing| existing == &path) {
            dirs.push(path);
        }
    }

    for extra in extra_dirs {
        if !dirs.iter().any(|existing| existing == extra) {
            dirs.push(extra.clone());
        }
    }

    env::join_paths(dirs).ok()
}

fn executable_names(command: &str) -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        let lower = command.to_ascii_lowercase();
        if lower.ends_with(".exe") || lower.ends_with(".cmd") || lower.ends_with(".bat") {
            return vec![command.to_string()];
        }

        return vec![
            format!("{}.cmd", command),
            format!("{}.exe", command),
            format!("{}.bat", command),
            command.to_string(),
        ];
    }

    #[cfg(not(target_os = "windows"))]
    {
        vec![command.to_string()]
    }
}

fn collect_runtime_dirs<'a>(dirs: impl IntoIterator<Item = Option<&'a Path>>) -> Vec<PathBuf> {
    let mut collected = Vec::new();
    for dir in dirs.into_iter().flatten() {
        let path = dir.to_path_buf();
        if !collected.iter().any(|existing| existing == &path) {
            collected.push(path);
        }
    }
    collected
}

#[cfg(target_os = "windows")]
fn windows_coli_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(app_data) = env::var_os("APPDATA") {
        let npm_dir = PathBuf::from(app_data).join("npm");
        candidates.push(npm_dir.join("coli.cmd"));
        candidates.push(npm_dir.join("coli.exe"));
        candidates.push(npm_dir.join("coli.bat"));
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let node_dir = PathBuf::from(local_app_data)
            .join("Programs")
            .join("nodejs");
        candidates.push(node_dir.join("coli.cmd"));
        candidates.push(node_dir.join("coli.exe"));
    }

    if let Some(program_files) = env::var_os("ProgramFiles") {
        let node_dir = PathBuf::from(program_files).join("nodejs");
        candidates.push(node_dir.join("coli.cmd"));
        candidates.push(node_dir.join("coli.exe"));
    }

    if let Some(program_files_x86) = env::var_os("ProgramFiles(x86)") {
        let node_dir = PathBuf::from(program_files_x86).join("nodejs");
        candidates.push(node_dir.join("coli.cmd"));
        candidates.push(node_dir.join("coli.exe"));
    }

    candidates
}

#[cfg(target_os = "windows")]
fn windows_node_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(nvm_symlink) = env::var_os("NVM_SYMLINK") {
        candidates.push(PathBuf::from(nvm_symlink).join("node.exe"));
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local_app_data)
                .join("Programs")
                .join("nodejs")
                .join("node.exe"),
        );
    }

    if let Some(program_files) = env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(program_files).join("nodejs").join("node.exe"));
    }

    if let Some(program_files_x86) = env::var_os("ProgramFiles(x86)") {
        candidates.push(
            PathBuf::from(program_files_x86)
                .join("nodejs")
                .join("node.exe"),
        );
    }

    candidates
}

#[cfg(target_os = "windows")]
fn windows_runtime_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(app_data) = env::var_os("APPDATA") {
        dirs.push(PathBuf::from(app_data).join("npm"));
    }

    if let Some(nvm_symlink) = env::var_os("NVM_SYMLINK") {
        dirs.push(PathBuf::from(nvm_symlink));
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        dirs.push(
            PathBuf::from(local_app_data)
                .join("Programs")
                .join("nodejs"),
        );
    }

    if let Some(program_files) = env::var_os("ProgramFiles") {
        dirs.push(PathBuf::from(program_files).join("nodejs"));
    }

    if let Some(program_files_x86) = env::var_os("ProgramFiles(x86)") {
        dirs.push(PathBuf::from(program_files_x86).join("nodejs"));
    }

    dirs
}

fn coli_models_dir() -> Option<PathBuf> {
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".coli").join("models"))
}

pub fn is_ffmpeg_available() -> bool {
    find_in_path("ffmpeg")
        .or_else(|| {
            #[cfg(target_os = "macos")]
            let candidates = [
                "/opt/homebrew/bin/ffmpeg",
                "/usr/local/bin/ffmpeg",
                "/usr/bin/ffmpeg",
            ];
            #[cfg(target_os = "windows")]
            let candidates = ["C:\\Program Files\\ffmpeg\\bin\\ffmpeg.exe"];
            #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
            let candidates = ["/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"];

            candidates
                .iter()
                .find_map(|candidate| existing_path(PathBuf::from(candidate)))
        })
        .is_some()
}

fn push_tail_line(buffer: &Arc<Mutex<VecDeque<String>>>, line: &str) {
    if let Ok(mut buffer) = buffer.lock() {
        if buffer.len() == 8 {
            buffer.pop_front();
        }
        buffer.push_back(line.to_string());
    }
}

fn render_tail(buffer: &Arc<Mutex<VecDeque<String>>>) -> String {
    buffer
        .lock()
        .ok()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_default()
}

fn parse_json_suffix<T: DeserializeOwned>(text: &str) -> Option<T> {
    let trimmed = text.trim();
    for (idx, ch) in trimmed.char_indices().rev() {
        if ch != '{' {
            continue;
        }
        if let Ok(parsed) = serde_json::from_str::<T>(&trimmed[idx..]) {
            return Some(parsed);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use super::normalize_existing_path;
    use super::{
        extract_windows_command_shim_script, parse_json_suffix, ColiFileAsrOutput,
        ColiRefinementMode, TranscriptAccumulator,
    };
    use std::{fs, path::PathBuf};

    #[test]
    fn parses_json_suffix_after_logs() {
        let sample = "Downloading model...\nready.\n{\n  \"text\": \"hello world\"\n}\n";
        let parsed: ColiFileAsrOutput = parse_json_suffix(sample).expect("json suffix");
        assert_eq!(parsed.text, "hello world");
    }

    #[test]
    fn refinement_mode_parses_expected_values() {
        assert_eq!(
            ColiRefinementMode::from_str("sensevoice"),
            ColiRefinementMode::SenseVoice
        );
        assert_eq!(
            ColiRefinementMode::from_str("whisper"),
            ColiRefinementMode::Whisper
        );
        assert_eq!(
            ColiRefinementMode::from_str("unknown"),
            ColiRefinementMode::Off
        );
    }

    #[test]
    fn stream_finals_are_accumulated() {
        let mut accumulator = TranscriptAccumulator::default();

        assert_eq!(accumulator.push_final("第一句").as_deref(), Some("第一句"));
        assert_eq!(
            accumulator.push_final("第二句").as_deref(),
            Some("第一句第二句")
        );
        assert_eq!(
            accumulator.push_final("第三句").as_deref(),
            Some("第一句第二句第三句")
        );
    }

    #[test]
    fn pending_partial_flushes_as_final() {
        let mut accumulator = TranscriptAccumulator::default();

        assert_eq!(accumulator.push_final("第一句").as_deref(), Some("第一句"));
        assert_eq!(
            accumulator.push_partial("第二句前半段").as_deref(),
            Some("第一句第二句前半段")
        );
        assert_eq!(
            accumulator.flush_pending().as_deref(),
            Some("第一句第二句前半段")
        );
    }

    #[test]
    fn windows_cmd_shim_resolves_cli_script() {
        let root = std::env::temp_dir().join(format!("voicex-coli-test-{}", rand::random::<u32>()));
        let shim_path = root.join("coli.cmd");
        let script_path = root
            .join("node_modules")
            .join("@marswave")
            .join("coli")
            .join("distribution")
            .join("cli.js");

        fs::create_dir_all(script_path.parent().expect("script dir")).expect("create script dir");
        fs::write(&script_path, "console.log('ok');").expect("write script");
        fs::write(
            &shim_path,
            "@IF EXIST \"%dp0%\\node.exe\" (\r\n  \"%dp0%\\node.exe\" \"%dp0%\\node_modules\\@marswave\\coli\\distribution\\cli.js\" %*\r\n) ELSE (\r\n  node \"%dp0%\\node_modules\\@marswave\\coli\\distribution\\cli.js\" %*\r\n)\r\n",
        )
        .expect("write shim");

        let parsed = extract_windows_command_shim_script(
            &fs::read_to_string(&shim_path).expect("read shim"),
            &shim_path,
        )
        .expect("parse shim");

        assert_eq!(parsed, script_path);

        let _ = fs::remove_dir_all(PathBuf::from(&root));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn normalizes_windows_verbatim_drive_paths() {
        assert_eq!(
            normalize_existing_path(PathBuf::from(r"\\?\D:\tools\coli.cmd")),
            PathBuf::from(r"D:\tools\coli.cmd")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn normalizes_windows_verbatim_unc_paths() {
        assert_eq!(
            normalize_existing_path(PathBuf::from(r"\\?\UNC\server\share\coli.cmd")),
            PathBuf::from(r"\\server\share\coli.cmd")
        );
    }
}
