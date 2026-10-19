//! Rules for what a log line may carry.
//!
//! VoiceX handles text the user said, selected or is about to type into another
//! application, so ordinary (info-level) lines must stay readable without
//! becoming a transcript: they may say *which* sentence a line is about, never
//! quote it whole. Text that a report really needs in full belongs on a
//! `log::debug!` line, which is only written when someone asked for it with
//! `RUST_LOG`.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Keep diagnostics for GUI launches too, while retaining terminal output.
pub fn init(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(directory)?;
    let writer = DiagnosticWriter::new(directory.join("voicex.log"), MAX_LOG_BYTES)?;
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .write_style(env_logger::WriteStyle::Never)
        .target(env_logger::Target::Pipe(Box::new(writer)))
        .try_init()?;
    log::info!("Diagnostic logs: {}", directory.display());
    Ok(())
}

struct DiagnosticWriter {
    file: File,
    path: PathBuf,
    bytes: u64,
    limit: u64,
}

impl DiagnosticWriter {
    fn new(path: PathBuf, limit: u64) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            file,
            path,
            bytes,
            limit,
        })
    }

    fn write_file(&mut self, record: &[u8]) -> io::Result<()> {
        if self.bytes > 0 && self.bytes.saturating_add(record.len() as u64) > self.limit {
            // Copy then truncate works with an open handle on Windows as well.
            // Keep one backup, and never split/truncate an individual diagnostic.
            self.file.flush()?;
            std::fs::copy(&self.path, self.path.with_extension("log.1"))?;
            self.file.set_len(0)?;
            self.bytes = 0;
        }
        self.file.write_all(record)?;
        self.bytes += record.len() as u64;
        self.file.flush()
    }
}

impl Write for DiagnosticWriter {
    fn write(&mut self, record: &[u8]) -> io::Result<usize> {
        // A GUI process can lack stderr; this must not prevent file logging.
        let _ = io::stderr().write_all(record);
        if let Err(error) = self.write_file(record) {
            eprintln!(
                "VoiceX could not write diagnostic log {}: {error}",
                self.path.display()
            );
            return Err(error);
        }
        Ok(record.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Longest run of text an ordinary log line may quote, in characters.
pub const PREVIEW_CHARS: usize = 120;

/// The first [`PREVIEW_CHARS`] characters of `text`, marked with `…` when it
/// was cut. Callers pass user or service text through this before logging it at
/// info level.
pub fn preview(text: &str) -> String {
    let mut s: String = text.chars().take(PREVIEW_CHARS).collect();
    if text.chars().count() > PREVIEW_CHARS {
        s.push('…');
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_logs_append_across_launches_and_rotate_without_cutting_errors() {
        let directory = std::env::temp_dir().join(format!("voicex-logs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("voicex.log");
        let original = "HTTP 503: {\"request_id\":\"abc413\"}\noriginal detail\n";
        {
            let mut writer =
                DiagnosticWriter::new(path.clone(), original.len() as u64 + 8).unwrap();
            writer.write_file(b"before\n").unwrap();
        }
        {
            let mut writer =
                DiagnosticWriter::new(path.clone(), original.len() as u64 + 8).unwrap();
            writer.write_file(original.as_bytes()).unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                format!("before\n{original}")
            );
            writer.write_file(b"next error\n").unwrap();
            assert_eq!(
                std::fs::read_to_string(path.with_extension("log.1")).unwrap(),
                format!("before\n{original}")
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "next error\n");
            // A long diagnostic remains complete even when it exceeds the limit.
            let long = "x".repeat(200);
            writer.write_file(long.as_bytes()).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), long);
            assert_eq!(
                std::fs::read_to_string(path.with_extension("log.1")).unwrap(),
                "next error\n"
            );
        }
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn short_text_is_untouched_and_long_text_is_cut_and_marked() {
        assert_eq!(preview("hello"), "hello");
        assert_eq!(preview(""), "");

        let short: String = "a".repeat(PREVIEW_CHARS);
        assert_eq!(preview(&short), short, "a text of exactly the limit fits");

        let long: String = "a".repeat(PREVIEW_CHARS + 1);
        let cut = preview(&long);
        assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1, "… marks the cut");
        assert!(cut.ends_with('…'));
        assert_eq!(cut.trim_end_matches('…'), short);
    }

    #[test]
    fn the_cut_counts_characters_not_bytes() {
        let text = "中文".repeat(PREVIEW_CHARS);
        let cut = preview(&text);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1);
    }
}
