//! Local ASR client backed by the external `qwen-asr` CLI (Qwen3-ASR).
//!
//! This is deliberately a *file-based* provider: the `qwen-asr` CLI only emits a
//! single final line even in `--stream` mode, so there is no incremental output
//! to surface. Live token streaming exists only in the `qwen-asr` Rust library,
//! which is the intended target of a later in-process integration.
//!
//! Two flags matter for quality and are always sent:
//!   - `--language`: without it the model drifts into English mid-utterance,
//!     which is far worse for an input method than a few wrong characters.
//!   - `--prompt`: the model's only hotword channel; measurably effective.

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
        check_platform_support()?;

        let program = resolve_qwen_local_command(&self.config.qwen_local_command_path)
            .map_err(AsrError::ConnectionFailed)?;

        let model_dir = self.config.qwen_local_model_dir.trim();
        if model_dir.is_empty() {
            return Err(AsrError::ConnectionFailed(
                "Qwen local ASR: model directory is not configured".to_string(),
            ));
        }
        let model_dir = expand_tilde(model_dir);
        if !model_dir.is_dir() {
            return Err(AsrError::ConnectionFailed(format!(
                "Qwen local ASR: model directory does not exist: {}",
                model_dir.display()
            )));
        }

        // VoiceX records OGG/Opus, and the probe ships an .ogg sample, but the
        // CLI reads only 16-bit PCM WAV. Decode when needed.
        let prepared = PreparedAudio::for_input(path)?;

        let args = build_args(&self.config, &model_dir, prepared.path());
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
            log::info!("QWEN_LOCAL_DIAG stderr: {stderr}");
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

/// Refuse the provider on Windows, before anything goes looking for a binary.
///
/// The settings page does not offer it there, but settings sync means a Windows
/// install can inherit the choice from a Mac. Saying why it cannot run beats
/// letting the PATH search answer "`qwen-asr` was not found": the binary is not
/// missing, it does not exist for this platform. `qwen-asr-cli` defaults to
/// `qwen-asr/vdsp`, i.e. Accelerate, and its live capture is documented as
/// macOS-only — we have never had a Windows build to point at.
#[cfg(target_os = "windows")]
fn check_platform_support() -> Result<(), AsrError> {
    Err(AsrError::ConnectionFailed(
        "Qwen local ASR is not available on Windows: the `qwen-asr` CLI it drives is \
         macOS / Linux only. Pick another ASR provider in settings."
            .to_string(),
    ))
}

#[cfg(not(target_os = "windows"))]
fn check_platform_support() -> Result<(), AsrError> {
    Ok(())
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

/// Locate the `qwen-asr` binary.
///
/// A bare PATH lookup is not enough: a bundled macOS app inherits launchd's
/// minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`), which contains neither
/// `~/.cargo/bin` (where `cargo install` puts it) nor Homebrew's prefix. Without
/// the common-location fallback this works under `tauri dev` and then fails for
/// every packaged build.
/// Returns the resolved binary, or an error explaining exactly what was tried.
pub fn resolve_qwen_local_command(configured: &str) -> Result<PathBuf, String> {
    let configured = configured.trim();

    if !configured.is_empty() {
        let path = expand_tilde(configured);

        // Pointing at the containing directory is a natural thing to type, so
        // accept it and look for the binary inside.
        if path.is_dir() {
            let candidate = path.join(exe_name());
            return if candidate.is_file() {
                Ok(candidate)
            } else {
                Err(format!(
                    "the configured command path `{}` is a directory that does not contain `{}`",
                    path.display(),
                    exe_name()
                ))
            };
        }

        if path.is_absolute() || path.components().count() > 1 {
            return if path.is_file() {
                Ok(path)
            } else {
                Err(format!(
                    "the configured command path `{}` does not exist",
                    path.display()
                ))
            };
        }

        // A bare name: search PATH for it, then fall through to the defaults.
        if let Some(found) = find_in_path(configured) {
            return Ok(found);
        }
    }

    find_in_path(exe_name())
        .or_else(find_in_common_locations)
        .ok_or_else(|| {
            format!(
                "`qwen-asr` was not found. Install it with `cargo install qwen-asr-cli`, \
                 or set an explicit command path in ASR settings. Searched PATH and: {}",
                searched_locations_hint()
            )
        })
}

/// Expand a leading `~`, which the OS does not do for us — shells expand it
/// before exec, but a path typed into a settings field never passes through one.
fn expand_tilde(raw: &str) -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match (raw.strip_prefix("~/").or(raw.strip_prefix("~")), home) {
        (Some(rest), Some(home)) if raw.starts_with('~') => {
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(raw),
    }
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(command))
        .find(|candidate| candidate.is_file())
}

fn find_in_common_locations() -> Option<PathBuf> {
    candidate_locations()
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// Where `qwen-asr` realistically lands, in priority order.
fn candidate_locations() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    // `cargo install qwen-asr-cli` — by far the most likely location.
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        candidates.push(PathBuf::from(&home).join(".cargo/bin").join(exe_name()));
    }

    #[cfg(target_os = "macos")]
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/qwen-asr"),
        PathBuf::from("/usr/local/bin/qwen-asr"),
    ]);
    #[cfg(all(unix, not(target_os = "macos")))]
    candidates.extend([
        PathBuf::from("/usr/local/bin/qwen-asr"),
        PathBuf::from("/usr/bin/qwen-asr"),
    ]);

    candidates
}

fn exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "qwen-asr.exe"
    } else {
        "qwen-asr"
    }
}

/// Human-readable list of the places we looked, for error messages.
fn searched_locations_hint() -> String {
    candidate_locations()
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
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
    use super::{
        build_args, build_prompt, candidate_locations, resolve_qwen_local_command,
        QWEN_LOCAL_MAX_PROMPT_HOTWORDS,
    };
    use crate::asr::config::AsrConfig;
    use std::path::Path;

    fn config() -> AsrConfig {
        AsrConfig {
            qwen_local_model_dir: "/models/qwen3-asr-0.6b".to_string(),
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
    fn explicit_path_is_honoured_only_when_it_exists() {
        // A configured path that does not exist must fail loudly rather than
        // silently falling back to some other binary, and must say why.
        let err = resolve_qwen_local_command("/definitely/not/here/qwen-asr").unwrap_err();
        assert!(err.contains("does not exist"), "unhelpful error: {err}");

        let this_binary = std::env::current_exe().unwrap();
        assert_eq!(
            resolve_qwen_local_command(this_binary.to_str().unwrap()).unwrap(),
            this_binary
        );
    }

    #[test]
    fn configured_directory_is_searched_for_the_binary() {
        // Users naturally type the containing directory rather than the binary.
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let err = resolve_qwen_local_command(dir.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("is a directory that does not contain"),
            "expected a directory-specific message, got: {err}"
        );
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
    fn tilde_is_expanded() {
        // `~` reaches us verbatim from a settings field: no shell expands it.
        let home = std::env::var("HOME").unwrap();
        assert_eq!(super::expand_tilde("~"), std::path::PathBuf::from(&home));
        assert_eq!(
            super::expand_tilde("~/.cargo/bin"),
            std::path::PathBuf::from(&home).join(".cargo/bin")
        );
        // Non-tilde input must pass through untouched.
        assert_eq!(
            super::expand_tilde("/abs/path"),
            std::path::PathBuf::from("/abs/path")
        );
    }

    #[test]
    fn tilde_directory_resolves_to_the_cargo_binary() {
        // Reproduces the exact value that failed in the app: a tilde-prefixed
        // directory rather than an absolute path to the executable.
        let expected = std::path::PathBuf::from(std::env::var("HOME").unwrap())
            .join(".cargo/bin")
            .join(super::exe_name());
        if !expected.is_file() {
            return; // qwen-asr not installed on this machine; nothing to assert.
        }
        assert_eq!(
            resolve_qwen_local_command("~/.cargo/bin").unwrap(),
            expected
        );
        assert_eq!(
            resolve_qwen_local_command("~/.cargo/bin/qwen-asr").unwrap(),
            expected
        );
    }

    #[test]
    fn candidates_cover_the_cargo_install_location() {
        // A packaged macOS app inherits launchd's minimal PATH, which excludes
        // ~/.cargo/bin — the default target of `cargo install qwen-asr-cli`.
        // Losing this candidate would break every bundled build.
        let candidates = candidate_locations();
        assert!(
            candidates
                .iter()
                .any(|p| p.to_string_lossy().contains(".cargo/bin")),
            "expected a ~/.cargo/bin candidate, got {candidates:?}"
        );
    }

    #[test]
    fn resolution_survives_a_gui_style_minimal_path() {
        // Simulates the packaged-app environment: PATH has no ~/.cargo/bin, so
        // resolution must come from the common-location fallback instead.
        temp_env_path("/usr/bin:/bin:/usr/sbin:/sbin", || {
            let expected = candidate_locations().into_iter().find(|p| p.is_file());
            assert_eq!(resolve_qwen_local_command("").ok(), expected);
        });
    }

    /// Swap PATH for the duration of `f`, then restore it.
    fn temp_env_path(value: &str, f: impl FnOnce()) {
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", value);
        f();
        match original {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
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
