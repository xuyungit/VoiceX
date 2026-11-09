//! File-based Qwen3-ASR using only the bundled engine and VoiceX-managed models.
//!
//! VoiceX records the utterance first and invokes the CLI with a complete WAV
//! file. Live capture/streaming offered by newer CLI versions is not used here.
//!
//! Two flags matter for quality and are always sent:
//!   - `--language`: without it the model drifts into English mid-utterance,
//!     which is far worse for an input method than a few wrong characters.
//!   - `--prompt`: the model's only hotword channel; measurably effective.

use crate::logging::preview;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::config::AsrConfig;
use super::protocol::AsrError;

/// Keep the child from flashing a console window. `tokio::process::Command`
/// carries `creation_flags` itself on Windows, so no extension trait is needed
/// — importing `std::os::windows::process::CommandExt` for it only produced an
/// unused-import warning in the Windows build.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Generous enough for a long utterance on a slow machine, short enough that a
/// wedged child process cannot hang the hotkey pipeline forever.
const QWEN_LOCAL_TIMEOUT_MS: u64 = 120_000;

/// Cap the prompt so a large dictionary cannot crowd out the audio context.
const QWEN_LOCAL_MAX_PROMPT_HOTWORDS: usize = 60;

pub struct QwenLocalAsrClient {
    config: AsrConfig,
}

impl QwenLocalAsrClient {
    pub fn new(config: AsrConfig) -> Self {
        Self { config }
    }

    pub async fn transcribe_file(&self, path: &Path) -> Result<String, AsrError> {
        let models =
            super::managed_models::ManagedModels::global().map_err(AsrError::ConnectionFailed)?;
        let local_error = |error: String| {
            AsrError::ConnectionFailed(super::managed_models::recognition_error_message(&error))
        };
        let program = models.engine().map_err(local_error)?;
        let lease = models
            .lease(&self.config.qwen_local_model)
            .map_err(local_error)?;
        let model_dir = &lease.path;

        // VoiceX records OGG/Opus, and the probe ships an .ogg sample, but the
        // CLI reads only 16-bit PCM WAV. Decode when needed.
        let prepared = PreparedAudio::for_input(path)?;

        let args = build_args(&self.config, model_dir, prepared.path());
        log::info!(
            "Starting Qwen local ASR ({} {})",
            program.display(),
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        );

        let mut command = Command::new(&program);
        command
            .kill_on_drop(true)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        command.creation_flags(CREATE_NO_WINDOW);

        let output = tokio::time::timeout(
            Duration::from_millis(QWEN_LOCAL_TIMEOUT_MS),
            command.output(),
        )
        .await
        .map_err(|_| {
            AsrError::ConnectionFailed(format!(
                "Qwen local ASR timed out after {QWEN_LOCAL_TIMEOUT_MS} ms"
            ))
        })?
        .map_err(|e| {
            AsrError::ConnectionFailed(format!("Failed to run `{}`: {e}", program.display()))
        })?;

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if !output.status.success() {
            // Surface the child's own diagnostics rather than a generic failure.
            return Err(AsrError::ServerError(format!(
                "`qwen-asr` exited with {}: {}",
                output.status,
                if stderr.is_empty() {
                    "<no stderr>"
                } else {
                    stderr.as_str()
                }
            )));
        }

        if self.config.enable_diagnostics && !stderr.is_empty() {
            // The child's own output can quote the recognized line, so an info
            // line shows only a prefix; the whole text stays on the debug one.
            log::info!("QWEN_LOCAL_DIAG stderr: {}", preview(&stderr));
            log::debug!("QWEN_LOCAL_DIAG stderr (full): {stderr}");
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err(AsrError::ServerError(format!(
                "`qwen-asr` produced no transcript{}",
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(" (stderr: {stderr})")
                }
            )));
        }

        Ok(text)
    }
}

/// Audio handed to the CLI, in the one format it accepts.
///
/// VoiceX records OGG/Opus (and the connectivity probe ships an `.ogg` sample),
/// but `qwen-asr` reads only 16-bit PCM WAV. Anything else is decoded into a
/// temporary WAV that is removed once the child process has run.
enum PreparedAudio {
    /// Already a WAV; hand the caller's file straight through.
    Passthrough(PathBuf),
    /// Decoded copy we own and must clean up.
    Temporary(PathBuf),
}

impl PreparedAudio {
    fn for_input(path: &Path) -> Result<Self, AsrError> {
        if is_riff_wave(path)? {
            return Ok(Self::Passthrough(path.to_path_buf()));
        }

        let pcm = super::ogg_decoder::decode_ogg_opus_to_pcm16k(path).map_err(|e| {
            AsrError::ProtocolError(format!(
                "Qwen local ASR: could not decode `{}` into PCM for the CLI: {e}",
                path.display()
            ))
        })?;

        let temp = crate::audio::write_temp_wav(16_000, 1, &pcm).map_err(|e| {
            AsrError::ConnectionFailed(format!(
                "Qwen local ASR: failed to stage a temporary WAV: {e}"
            ))
        })?;

        log::debug!(
            "Qwen local ASR: decoded {} into {} ({} PCM bytes)",
            path.display(),
            temp.display(),
            pcm.len()
        );
        Ok(Self::Temporary(temp))
    }

    fn path(&self) -> &Path {
        match self {
            Self::Passthrough(p) | Self::Temporary(p) => p,
        }
    }
}

impl Drop for PreparedAudio {
    fn drop(&mut self) {
        if let Self::Temporary(path) = self {
            if let Err(e) = std::fs::remove_file(&path) {
                log::warn!(
                    "Qwen local ASR: failed to remove temporary WAV {}: {e}",
                    path.display()
                );
            }
        }
    }
}

/// Cheap format sniff: a WAV starts with `RIFF....WAVE`.
fn is_riff_wave(path: &Path) -> Result<bool, AsrError> {
    use std::io::Read;

    let mut header = [0u8; 12];
    let mut file = std::fs::File::open(path).map_err(|e| {
        AsrError::ConnectionFailed(format!(
            "Qwen local ASR: cannot open audio file `{}`: {e}",
            path.display()
        ))
    })?;

    match file.read_exact(&mut header) {
        Ok(()) => Ok(&header[0..4] == b"RIFF" && &header[8..12] == b"WAVE"),
        // Too short to be a WAV; let the decoder produce the real diagnosis.
        Err(_) => Ok(false),
    }
}

fn build_args(config: &AsrConfig, model_dir: &Path, path: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "-d".into(),
        model_dir.as_os_str().to_owned(),
        "-i".into(),
        path.as_os_str().to_owned(),
        // Keep stdout to the transcript alone so parsing stays trivial.
        "--silent".into(),
    ];

    let language = config.qwen_local_language.trim();
    if !language.is_empty() {
        args.push("--language".into());
        args.push(language.into());
    }

    if let Some(prompt) = build_prompt(config) {
        args.push("--prompt".into());
        args.push(prompt.into());
    }

    args
}

/// Compose the biasing prompt from the user's dictionary.
///
/// Returns `None` when there is nothing to bias with, so we don't pass an empty
/// prompt that would just waste context.
fn build_prompt(config: &AsrConfig) -> Option<String> {
    if !config.qwen_local_use_dictionary {
        return None;
    }

    let mut words: Vec<&str> = Vec::new();
    for hotword in &config.hotwords {
        let word = hotword.trim();
        if word.is_empty() || words.contains(&word) {
            continue;
        }
        if words.len() >= QWEN_LOCAL_MAX_PROMPT_HOTWORDS {
            log::warn!(
                "Qwen local ASR: dictionary truncated to {} entries for the biasing prompt",
                QWEN_LOCAL_MAX_PROMPT_HOTWORDS
            );
            break;
        }
        words.push(word);
    }

    if words.is_empty() {
        return None;
    }

    Some(format!(
        "Technical terms that may appear: {}",
        words.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::{build_args, build_prompt, QWEN_LOCAL_MAX_PROMPT_HOTWORDS};
    use crate::asr::config::AsrConfig;
    use std::path::Path;

    fn config() -> AsrConfig {
        AsrConfig {
            qwen_local_language: "Chinese".to_string(),
            qwen_local_use_dictionary: true,
            hotwords: vec!["Tauri".to_string(), "Pinia".to_string()],
            ..Default::default()
        }
    }

    fn joined(args: &[std::ffi::OsString]) -> String {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn wav_input_is_passed_through_but_ogg_is_decoded() {
        use super::PreparedAudio;

        let dir = std::env::temp_dir().join("voicex-qwen-local-test");
        std::fs::create_dir_all(&dir).unwrap();

        // A real WAV must reach the CLI untouched — no needless re-encode.
        let wav = dir.join("passthrough.wav");
        let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
        bytes.extend_from_slice(&[0u8; 32]);
        std::fs::write(&wav, &bytes).unwrap();
        let prepared = PreparedAudio::for_input(&wav).unwrap();
        assert_eq!(prepared.path(), wav.as_path());
        drop(prepared);
        assert!(
            wav.is_file(),
            "passthrough must not delete the caller's file"
        );

        // Something that is neither WAV nor decodable must report the reason
        // rather than silently handing garbage to the CLI.
        let bogus = dir.join("bogus.ogg");
        std::fs::write(&bogus, b"definitely not audio").unwrap();
        assert!(PreparedAudio::for_input(&bogus).is_err());

        std::fs::remove_file(&wav).ok();
        std::fs::remove_file(&bogus).ok();
    }

    #[test]
    fn temporary_audio_is_cleaned_up() {
        use super::PreparedAudio;

        let temp = crate::audio::write_temp_wav(16_000, 1, &[0u8; 3200]).unwrap();
        let path = temp.clone();
        assert!(path.is_file());
        drop(PreparedAudio::Temporary(temp));
        assert!(!path.is_file(), "temporary WAV must be removed after use");
    }

    #[test]
    fn args_always_carry_model_and_language() {
        let model_dir = Path::new("/models/qwen3-asr-0.6b");
        let args = joined(&build_args(&config(), model_dir, Path::new("/tmp/a.wav")));
        assert!(args.contains("-d /models/qwen3-asr-0.6b"));
        assert!(args.contains("-i /tmp/a.wav"));
        assert!(args.contains("--silent"));
        // Language forcing is what stops the model drifting into English.
        assert!(args.contains("--language Chinese"));
        assert!(args.contains("--prompt"));
    }

    #[test]
    fn language_is_omitted_when_blank() {
        let mut c = config();
        c.qwen_local_language = "  ".to_string();
        let model_dir = Path::new("/models/qwen3-asr-0.6b");
        assert!(!joined(&build_args(&c, model_dir, Path::new("/tmp/a.wav"))).contains("--language"));
    }

    #[test]
    fn prompt_reflects_dictionary_toggle_and_dedupes() {
        let mut c = config();
        c.hotwords = vec!["Tauri".into(), " Tauri ".into(), "".into(), "Pinia".into()];
        assert_eq!(
            build_prompt(&c).unwrap(),
            "Technical terms that may appear: Tauri, Pinia"
        );

        c.qwen_local_use_dictionary = false;
        assert!(build_prompt(&c).is_none());

        let mut empty = config();
        empty.hotwords = vec![];
        assert!(build_prompt(&empty).is_none());
    }

    #[test]
    fn prompt_is_capped() {
        let mut c = config();
        c.hotwords = (0..200).map(|i| format!("word{i}")).collect();
        let prompt = build_prompt(&c).unwrap();
        assert_eq!(
            prompt.matches(", ").count(),
            QWEN_LOCAL_MAX_PROMPT_HOTWORDS - 1
        );
    }
}
