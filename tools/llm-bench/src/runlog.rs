//! The run log: every run writes a copy of its console output to `<log dir>/<time>/report.log`, so a score can
//! be looked at again later without copying it out of the terminal.
//!
//! The crate's `println!`, `print!` and `eprintln!` are the macros below, not std's: they write to the terminal
//! as usual and, while a run log is open, the same text (without the colour escapes) to the file.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static LOG: Mutex<Option<File>> = Mutex::new(None);

/// The console copy of the run: `report.log` inside the run directory.
pub const REPORT_FILE: &str = "report.log";
/// The detailed results of the run: `results.json` inside the run directory.
pub const RESULTS_FILE: &str = "results.json";

macro_rules! println {
    () => {{
        ::std::println!();
        $crate::runlog::record("\n");
    }};
    ($($arg:tt)*) => {{
        let text = ::std::format!($($arg)*);
        ::std::println!("{}", text);
        $crate::runlog::record(&text);
        $crate::runlog::record("\n");
    }};
}

macro_rules! print {
    ($($arg:tt)*) => {{
        let text = ::std::format!($($arg)*);
        ::std::print!("{}", text);
        $crate::runlog::record(&text);
    }};
}

macro_rules! eprintln {
    () => {{
        ::std::eprintln!();
        $crate::runlog::record("\n");
    }};
    ($($arg:tt)*) => {{
        let text = ::std::format!($($arg)*);
        ::std::eprintln!("{}", text);
        $crate::runlog::record(&text);
        $crate::runlog::record("\n");
    }};
}

/// Creates the run directory under `log_dir`, named after the local time, and opens its report with `header`
/// (the lines about the run that the console does not print) on top. Returns the run directory.
pub fn open(log_dir: &Path, header: &str) -> std::io::Result<PathBuf> {
    let dir = unique_dir(log_dir, &stamp(chrono::Local::now()))?;
    let mut file = File::create(dir.join(REPORT_FILE))?;
    file.write_all(header.as_bytes())?;
    file.write_all(b"\n")?;
    *LOG.lock().unwrap() = Some(file);
    Ok(dir)
}

/// Appends `text` to the open report, colour escapes removed; nothing happens when no run log is open. The
/// first write that fails closes the log and says so once, rather than failing every line after it.
pub fn record(text: &str) {
    let mut log = LOG.lock().unwrap();
    if let Some(file) = log.as_mut() {
        if let Err(e) = file.write_all(strip_ansi(text).as_bytes()) {
            ::std::eprintln!("Run log stopped: {}", e);
            *log = None;
        }
    }
}

/// The lines about the run that only the report carries: when, how it was invoked, and which code ran it.
pub fn header(command: &[String], config_path: &str, cases_path: &str) -> String {
    format!(
        "llm-bench run  {}\ncommand:  {}\nconfig:   {}\ncases:    {}\ngit:      {}\n",
        chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        command.join(" "),
        config_path,
        cases_path,
        git_state()
    )
}

/// The commit the bench was built from and whether this crate's tracked files differ from it, or why that
/// could not be told.
pub fn git_state() -> String {
    let crate_dir = env!("CARGO_MANIFEST_DIR");
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").args(args).current_dir(crate_dir).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    match git(&["rev-parse", "--short", "HEAD"]) {
        Some(sha) => match git(&["status", "--porcelain", "--untracked-files=no", "--", "."]) {
            Some(status) if status.is_empty() => format!("{} (tools/llm-bench clean)", sha),
            Some(_) => format!("{} (tools/llm-bench modified)", sha),
            None => sha,
        },
        None => "unavailable".into(),
    }
}

/// `2026-09-22_18-03-45`: sorts by time and needs no quoting in a shell.
pub fn stamp(now: chrono::DateTime<chrono::Local>) -> String {
    now.format("%Y-%m-%d_%H-%M-%S").to_string()
}

/// Creates `<log_dir>/<name>`, or `<name>-2`, `<name>-3`, ... when a run of the same second already has it.
fn unique_dir(log_dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(log_dir)?;
    for attempt in 1.. {
        let dir = match attempt {
            1 => log_dir.join(name),
            n => log_dir.join(format!("{}-{}", name, n)),
        };
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// `text` without its ANSI escape sequences (`ESC [ ... <final byte>`), which mean nothing in a file.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            // parameter and intermediate bytes run until the final byte, which is in `@`..=`~`
            for c in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_colour_and_keeps_the_text() {
        assert_eq!(strip_ansi("\x1b[1;36m━━━ Bearing ━━━\x1b[0m"), "━━━ Bearing ━━━");
        assert_eq!(strip_ansi("  \x1b[2m• cleanup     嗯， → \x1b[0m"), "  • cleanup     嗯， → ");
        assert_eq!(strip_ansi("plain\n"), "plain\n");
        assert_eq!(strip_ansi("\x1b[38;5;208mx\x1b[0m"), "x");
        // an escape cut off at the end of the text takes nothing after it with it
        assert_eq!(strip_ansi("a\x1b["), "a");
    }

    #[test]
    fn stamp_is_sortable_and_shell_safe() {
        use chrono::TimeZone;
        let at = chrono::Local.with_ymd_and_hms(2026, 9, 22, 18, 3, 45).unwrap();
        assert_eq!(stamp(at), "2026-09-22_18-03-45");
    }

    #[test]
    fn a_second_run_in_the_same_second_gets_its_own_directory() {
        let base = std::env::temp_dir().join(format!("llm-bench-runlog-{}", std::process::id()));
        let first = unique_dir(&base, "2026-09-22_18-03-45").unwrap();
        let second = unique_dir(&base, "2026-09-22_18-03-45").unwrap();
        assert_eq!(first, base.join("2026-09-22_18-03-45"));
        assert_eq!(second, base.join("2026-09-22_18-03-45-2"));
        std::fs::remove_dir_all(&base).unwrap();
    }
}
