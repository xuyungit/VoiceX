//! CLI entrypoint for one-time VoiceX Sync v1 -> v2 migration rehearsal & cutover.
//!
//! Usage:
//!   cargo run --manifest-path src-tauri/Cargo.toml --bin voicex-sync-migrate -- \
//!     --server-v1 .local-notes/sync-v2-audit/server-v1.db \
//!     --out-server-v2 .local-notes/sync-v2-audit/run1/server-v2.db \
//!     --device ".local-notes/sync-v2-audit/mac-v1.db|.local-notes/sync-v2-audit/run1/mac-v2.db|history,edits" \
//!     --verify-replay

use std::path::PathBuf;
use voicex_lib::sync_v2::migrate::{
    run_migration, verify_server_v2_replay, DeviceMigrationInput, MigrationConfig,
};

fn usage() -> ! {
    eprintln!(
        "Usage: voicex-sync-migrate \\
  --server-v1 <path> \\
  --out-server-v2 <path> \\
  [--device \"<in_db>|<out_db>[|<flags>]\"] (repeatable; flags: stats_only,history,edits,import_edits) \\
  [--server-url <url>] \\
  [--label <account_label>] \\
  [--recovery-key <VXK2-...>] \\
  [--verify-replay]"
    );
    std::process::exit(2);
}

fn parse_device_arg(raw: &str) -> Result<DeviceMigrationInput, String> {
    let mut parts = raw.splitn(3, '|');
    let in_path = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("invalid --device arg: {raw}"))?;
    let out_path = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("invalid --device arg (missing out_db): {raw}"))?;
    let flags_str = parts.next().unwrap_or("history");

    let mut scope_history = false;
    let mut scope_edits = false;
    let mut import_existing_edits = false;
    for flag in flags_str.split(',').filter(|s| !s.is_empty()) {
        match flag {
            "stats_only" => {
                scope_history = false;
                scope_edits = false;
                import_existing_edits = false;
            }
            "history" => scope_history = true,
            "edits" => {
                scope_history = true;
                scope_edits = true;
            }
            "import_edits" => {
                scope_history = true;
                scope_edits = true;
                import_existing_edits = true;
            }
            other => return Err(format!("unknown device flag '{other}' in {raw}")),
        }
    }

    Ok(DeviceMigrationInput {
        db_path: PathBuf::from(in_path),
        out_db_path: PathBuf::from(out_path),
        scope_history,
        scope_edits,
        import_existing_edits,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        usage();
    }

    let mut server_v1_db: Option<PathBuf> = None;
    let mut out_server_v2_db: Option<PathBuf> = None;
    let mut device_inputs: Vec<DeviceMigrationInput> = Vec::new();
    let mut server_url = "http://127.0.0.1:8787".to_string();
    let mut account_label = "personal".to_string();
    let mut recovery_key: Option<String> = None;
    let mut verify_replay = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--server-v1" => {
                i += 1;
                server_v1_db = args.get(i).map(PathBuf::from);
            }
            "--out-server-v2" => {
                i += 1;
                out_server_v2_db = args.get(i).map(PathBuf::from);
            }
            "--device" => {
                i += 1;
                let raw = args.get(i).unwrap_or_else(|| usage());
                match parse_device_arg(raw) {
                    Ok(d) => device_inputs.push(d),
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(2);
                    }
                }
            }
            "--server-url" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    server_url = v.clone();
                }
            }
            "--label" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    account_label = v.clone();
                }
            }
            "--recovery-key" => {
                i += 1;
                recovery_key = args.get(i).cloned();
            }
            "--verify-replay" => {
                verify_replay = true;
            }
            other => {
                eprintln!("unknown argument: {other}");
                usage();
            }
        }
        i += 1;
    }

    let cfg = MigrationConfig {
        server_v1_db: server_v1_db.unwrap_or_else(|| usage()),
        out_server_v2_db: out_server_v2_db.unwrap_or_else(|| usage()),
        device_inputs,
        server_url,
        account_label,
        recovery_key,
    };

    let report = match run_migration(&cfg) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("Migration failed: {err}");
            std::process::exit(1);
        }
    };

    let replay = if verify_replay {
        match verify_server_v2_replay(&cfg.out_server_v2_db, &report.recovery_key) {
            Ok(v) => Some(v),
            Err(err) => {
                eprintln!("Replay verification failed: {err}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    let output = serde_json::json!({
        "migration": report,
        "replayVerification": replay,
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}
