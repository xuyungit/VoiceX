//! `voicex-tts`: convert a text or Markdown file into a spoken `.mp3` file
//! using VoiceX's reading preprocessing prompt and TTS providers.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use voicex_lib::commands::settings::AppSettings;
use voicex_lib::storage::load_settings_from_path;
use voicex_lib::tts::render::{
    build_render_request, default_db_path, default_output_path, optimize_text_for_speech,
    resolve_llm_stage, synthesize_to_writer, LlmMode, RenderEvent, RenderOptions,
};

#[derive(Debug, Default)]
struct CliArgs {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    db: Option<PathBuf>,
    tts_provider: Option<String>,
    voice: Option<String>,
    aliyun_model: Option<String>,
    speed: Option<f32>,
    llm_flag: LlmFlag,
    llm_provider: Option<String>,
    llm_model: Option<String>,
    translate: bool,
    source_language: Option<String>,
    target_language: Option<String>,
    save_text: Option<PathBuf>,
    quiet: bool,
    help: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum LlmFlag {
    #[default]
    Auto,
    On,
    Off,
}

const USAGE: &str = "\
Usage: voicex-tts [OPTIONS] <INPUT_FILE>

Optimize a text or Markdown file for speech and synthesize it to an MP3 file.
Reads LLM and TTS credentials/defaults from the VoiceX desktop database unless
overridden on the command line.

Arguments:
  <INPUT_FILE>                 Input text or Markdown file

Options:
  -o, --output <FILE>          Output MP3 path [default: <INPUT_FILE>.mp3]
      --tts <PROVIDER>         TTS provider: edge, volcengine, aliyun, mimo, azure
      --voice <VOICE>          Voice / speaker ID for the selected TTS provider
      --tts-model <MODEL>      Aliyun TTS model (e.g. qwen3-tts-flash, qwen-audio-3.1-tts-flash)
      --speed <MULTIPLIER>     Speaking speed multiplier in 0.5..=2.0 (1.0 = normal)
      --llm <PROVIDER|off>     LLM provider for speech cleanup: follow, volcengine, openai,
                               qwen, deepseek, gemini, anthropic, custom:<id>, or 'off'
      --no-llm                 Skip LLM optimization and synthesize the raw file content
      --llm-model <MODEL>      Override the model name of the selected LLM provider
      --translate              Translate the document before speaking (uses VoiceX translate prompt)
      --from <LANG>            Source language for --translate (e.g. auto, zh-CN, en, ja)
      --to <LANG>              Target language for --translate (e.g. zh-CN, en, ja)
      --save-text <FILE>       Write the LLM-optimized text to <FILE> for inspection
      --db <PATH>              Path to voicex.db [default:VOICEX_DB or app data dir]
  -q, --quiet                  Suppress progress messages on stderr
  -h, --help                   Print this help message
";

fn parse_args<I>(iter: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = CliArgs::default();
    let mut it = iter.into_iter();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                args.help = true;
                return Ok(args);
            }
            "-q" | "--quiet" => args.quiet = true,
            "-o" | "--output" => {
                let val = it
                    .next()
                    .ok_or_else(|| format!("{arg} requires a file path"))?;
                args.output = Some(PathBuf::from(val));
            }
            "--db" => {
                let val = it
                    .next()
                    .ok_or_else(|| "--db requires a file path".to_string())?;
                args.db = Some(PathBuf::from(val));
            }
            "--tts" | "--tts-provider" => {
                let val = it
                    .next()
                    .ok_or_else(|| format!("{arg} requires a provider name"))?;
                args.tts_provider = Some(val);
            }
            "--voice" => {
                let val = it
                    .next()
                    .ok_or_else(|| "--voice requires a voice ID".to_string())?;
                args.voice = Some(val);
            }
            "--tts-model" | "--aliyun-model" => {
                let val = it
                    .next()
                    .ok_or_else(|| format!("{arg} requires a model name"))?;
                args.aliyun_model = Some(val);
            }
            "--speed" | "--rate" => {
                let val = it
                    .next()
                    .ok_or_else(|| format!("{arg} requires a speed multiplier (0.5..=2.0)"))?;
                let parsed: f32 = val.parse().map_err(|_| {
                    format!("invalid speed '{val}': expected a number in 0.5..=2.0")
                })?;
                args.speed = Some(parsed);
            }
            "--no-llm" => {
                args.llm_flag = LlmFlag::Off;
                args.llm_provider = None;
            }
            "--llm" | "--llm-provider" => {
                let val = it
                    .next()
                    .ok_or_else(|| format!("{arg} requires a provider name or 'off'"))?;
                let trimmed = val.trim();
                if trimmed.eq_ignore_ascii_case("off") || trimmed.eq_ignore_ascii_case("none") {
                    args.llm_flag = LlmFlag::Off;
                    args.llm_provider = None;
                } else if trimmed.eq_ignore_ascii_case("on") {
                    args.llm_flag = LlmFlag::On;
                } else {
                    args.llm_flag = LlmFlag::On;
                    args.llm_provider = Some(trimmed.to_string());
                }
            }
            "--llm-model" => {
                let val = it
                    .next()
                    .ok_or_else(|| "--llm-model requires a model name".to_string())?;
                args.llm_model = Some(val);
                if args.llm_flag == LlmFlag::Auto {
                    args.llm_flag = LlmFlag::On;
                }
            }
            "--translate" => {
                args.translate = true;
            }
            "--from" | "--source-lang" => {
                let val = it
                    .next()
                    .ok_or_else(|| format!("{arg} requires a language code"))?;
                args.source_language = Some(val);
            }
            "--to" | "--target-lang" => {
                let val = it
                    .next()
                    .ok_or_else(|| format!("{arg} requires a language code"))?;
                args.target_language = Some(val);
                args.translate = true;
            }
            "--save-text" => {
                let val = it
                    .next()
                    .ok_or_else(|| "--save-text requires a file path".to_string())?;
                args.save_text = Some(PathBuf::from(val));
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}' (see --help)"));
            }
            positional => {
                if args.input.is_some() {
                    return Err(format!(
                        "unexpected extra argument '{positional}' (only one input file is accepted)"
                    ));
                }
                args.input = Some(PathBuf::from(positional));
            }
        }
    }

    if args.translate && args.llm_flag == LlmFlag::Off {
        return Err("--translate cannot be combined with --no-llm / --llm off".to_string());
    }

    Ok(args)
}

impl CliArgs {
    fn into_render_options(self) -> RenderOptions {
        let llm = if self.translate {
            LlmMode::Translate {
                provider_key: self.llm_provider,
                model: self.llm_model,
                source_language: self.source_language,
                target_language: self.target_language,
            }
        } else {
            match self.llm_flag {
                LlmFlag::Off => LlmMode::Off,
                LlmFlag::On => LlmMode::Preprocess {
                    provider_key: self.llm_provider,
                    model: self.llm_model,
                },
                LlmFlag::Auto => LlmMode::FromSettings,
            }
        };

        RenderOptions {
            tts_provider: self.tts_provider,
            voice: self.voice,
            aliyun_model: self.aliyun_model,
            speed: self.speed,
            llm,
        }
    }
}

fn load_app_settings(explicit_db: Option<PathBuf>, quiet: bool) -> Result<AppSettings, String> {
    if let Some(path) = explicit_db {
        return load_settings_from_path(&path)
            .map_err(|err| format!("failed to read settings from {}: {err}", path.display()));
    }
    let Some(path) = default_db_path() else {
        return Ok(AppSettings::default());
    };
    if !path.exists() {
        if !quiet {
            eprintln!(
                "note: settings database not found at {}; using built-in defaults",
                path.display()
            );
        }
        return Ok(AppSettings::default());
    }
    load_settings_from_path(&path)
        .map_err(|err| format!("failed to read settings from {}: {err}", path.display()))
}

fn run() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1))?;
    if args.help {
        print!("{USAGE}");
        return Ok(());
    }
    let Some(input_path) = args.input.clone() else {
        return Err(format!("missing <INPUT_FILE>\n\n{USAGE}"));
    };
    let output_path = args
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(&input_path));
    let save_text_path = args.save_text.clone();
    let quiet = args.quiet;
    let db_path = args.db.clone();
    let options = args.into_render_options();

    let raw_text = fs::read_to_string(&input_path)
        .map_err(|err| format!("failed to read {}: {err}", input_path.display()))?;
    if raw_text.trim().is_empty() {
        return Err(format!("input file {} is empty", input_path.display()));
    }

    let settings = load_app_settings(db_path, quiet)?;

    let prepared_text = match resolve_llm_stage(&settings, &options.llm)? {
        Some((llm_config, prompt)) => {
            if !quiet {
                eprintln!(
                    "[llm] provider={:?} model={} input_chars={}",
                    llm_config.provider_type,
                    llm_config.model_name,
                    raw_text.trim().chars().count()
                );
            }
            optimize_text_for_speech(&llm_config, &prompt, &raw_text, |event| {
                if quiet {
                    return;
                }
                match event {
                    RenderEvent::LlmChunk {
                        index,
                        total,
                        chars,
                    } if total > 1 => {
                        eprintln!("[llm] chunk {}/{total} ({chars} chars)", index + 1);
                    }
                    RenderEvent::LlmDone {
                        input_chars,
                        output_chars,
                    } => {
                        eprintln!("[llm] done ({input_chars} -> {output_chars} chars)");
                    }
                    _ => {}
                }
            })
            .map_err(|err| format!("LLM stage failed: {err}"))?
        }
        None => {
            if !quiet {
                eprintln!(
                    "[llm] skipped (raw text: {} chars)",
                    raw_text.trim().chars().count()
                );
            }
            raw_text
        }
    };

    if let Some(path) = &save_text_path {
        fs::write(path, &prepared_text)
            .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
        if !quiet {
            eprintln!("[text] saved to {}", path.display());
        }
    }

    let (effective_settings, request) = build_render_request(&settings, &options, prepared_text)?;
    if !quiet {
        eprintln!(
            "[tts] provider={} voice={} chars={}",
            effective_settings.tts_provider_type,
            request.voice.as_deref().unwrap_or("<default>"),
            request.text.chars().count()
        );
    }

    let total_bytes = write_output(&output_path, |writer| {
        synthesize_to_writer(&effective_settings, &request, writer, |event| {
            if quiet {
                return;
            }
            if let RenderEvent::TtsPiece {
                index,
                total,
                chars,
                bytes,
            } = event
            {
                eprintln!(
                    "[tts] piece {}/{total} ({chars} chars -> {bytes} bytes)",
                    index + 1
                );
            }
        })
    })?;

    if !quiet {
        eprintln!(
            "[done] wrote {} ({total_bytes} bytes)",
            output_path.display()
        );
    }
    Ok(())
}

/// Each export owns an exclusively created temporary file beside its target.
/// Only publish a complete, flushed render; dropping it on any failure leaves
/// existing files (including a possible `<output>.part` input) untouched.
fn write_output(
    output: &Path,
    render: impl FnOnce(&mut BufWriter<&mut File>) -> Result<u64, String>,
) -> Result<u64, String> {
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|err| format!("failed to create directory {}: {err}", parent.display()))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".voicex-tts-")
        .suffix(".part")
        .tempfile_in(parent)
        .map_err(|err| {
            format!(
                "failed to create temporary MP3 in {}: {err}",
                parent.display()
            )
        })?;
    let total_bytes = {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        let bytes = render(&mut writer).map_err(|err| format!("TTS synthesis failed: {err}"))?;
        writer
            .flush()
            .map_err(|err| format!("failed to flush output MP3: {err}"))?;
        bytes
    };
    temporary
        .persist(output)
        .map_err(|err| format!("failed to publish output MP3 {}: {err}", output.display()))?;
    Ok(total_bytes)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_render_preserves_part_input_and_existing_output() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("a.mp3");
        let input = directory.path().join("a.mp3.part");
        fs::write(&input, b"original input").unwrap();
        fs::write(&output, b"previous output").unwrap();

        let result = write_output(&output, |writer| {
            writer.write_all(b"incomplete audio").unwrap();
            Err("provider failed".to_string())
        });

        assert!(result.unwrap_err().contains("provider failed"));
        assert_eq!(fs::read(&input).unwrap(), b"original input");
        assert_eq!(fs::read(&output).unwrap(), b"previous output");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn successful_render_replaces_only_the_output_after_flushing() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("a.mp3");
        let input = directory.path().join("a.mp3.part");
        fs::write(&input, b"original input").unwrap();
        fs::write(&output, b"previous output").unwrap();

        let result = write_output(&output, |writer| {
            writer.write_all(b"complete audio").unwrap();
            assert_eq!(fs::read(&output).unwrap(), b"previous output");
            Ok(14)
        });

        assert_eq!(result.unwrap(), 14);
        assert_eq!(fs::read(&output).unwrap(), b"complete audio");
        assert_eq!(fs::read(&input).unwrap(), b"original input");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn concurrent_renders_publish_complete_files_without_sharing_a_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("a.mp3");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = [b'A', b'B']
            .into_iter()
            .map(|byte| {
                let output = output.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    write_output(&output, |writer| {
                        writer.write_all(&vec![byte; 16_384]).unwrap();
                        barrier.wait();
                        Ok(16_384)
                    })
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(worker.join().unwrap().unwrap(), 16_384);
        }
        let bytes = fs::read(&output).unwrap();
        assert!(bytes == vec![b'A'; 16_384] || bytes == vec![b'B'; 16_384]);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_publish_cleans_up_only_its_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("occupied");
        fs::create_dir(&output).unwrap();
        let sentinel = output.join("keep.txt");
        fs::write(&sentinel, b"keep").unwrap();

        let result = write_output(&output, |writer| {
            writer.write_all(b"audio").unwrap();
            Ok(5)
        });

        assert!(result.unwrap_err().contains("failed to publish"));
        assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    fn parse(words: &[&str]) -> Result<CliArgs, String> {
        parse_args(words.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn minimal_invocation_uses_settings_defaults() {
        let args = parse(&["notes.md"]).unwrap();
        assert_eq!(args.input, Some(PathBuf::from("notes.md")));
        assert_eq!(args.output, None);
        let opts = args.into_render_options();
        assert_eq!(opts.llm, LlmMode::FromSettings);
        assert_eq!(opts.tts_provider, None);
    }

    #[test]
    fn llm_and_tts_flags_map_into_render_options() {
        let args = parse(&[
            "doc.md",
            "-o",
            "out.mp3",
            "--llm",
            "deepseek",
            "--llm-model",
            "deepseek-chat",
            "--tts",
            "aliyun",
            "--tts-model",
            "qwen-audio-3.1-tts-flash",
            "--voice",
            "Cherry",
            "--speed",
            "1.25",
        ])
        .unwrap();
        assert_eq!(args.output, Some(PathBuf::from("out.mp3")));
        let opts = args.into_render_options();
        assert_eq!(opts.tts_provider.as_deref(), Some("aliyun"));
        assert_eq!(
            opts.aliyun_model.as_deref(),
            Some("qwen-audio-3.1-tts-flash")
        );
        assert_eq!(opts.voice.as_deref(), Some("Cherry"));
        assert_eq!(opts.speed, Some(1.25));
        assert_eq!(
            opts.llm,
            LlmMode::Preprocess {
                provider_key: Some("deepseek".to_string()),
                model: Some("deepseek-chat".to_string()),
            }
        );
    }

    #[test]
    fn no_llm_and_llm_off_both_disable_the_llm_stage() {
        for words in [&["a.md", "--no-llm"][..], &["a.md", "--llm", "off"][..]] {
            let opts = parse(words).unwrap().into_render_options();
            assert_eq!(opts.llm, LlmMode::Off);
        }
    }

    #[test]
    fn translate_and_no_llm_conflict_is_rejected() {
        assert!(parse(&["a.md", "--translate", "--no-llm"]).is_err());
    }
}
