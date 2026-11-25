//! Real-TCP end-to-end integration test: real `voicex-sync-server` binary +
//! admin CLI + real `SelfHostTransport` HTTP/SSE + real `CredentialStore` +
//! real SQLite client DBs (Mac + Windows).

use chrono::Utc;
use futures_util::StreamExt;
use rusqlite::{params, Connection};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tempfile::TempDir;
use voicex_lib::storage::{HistoryRecord, StorageError};
use voicex_lib::sync_v2::credentials::CredentialStore;
use voicex_lib::sync_v2::engine::{
    pair_space, resolve_edit_conflict_in, sync_once, SyncEngineError,
};
use voicex_lib::sync_v2::store::{
    get_overview_usage_stats_in, init_schema, list_edit_conflict_alternatives_in,
    on_local_edit_applied_tx, on_local_history_deleted_tx, on_local_history_inserted_tx,
    OverviewScope,
};
use voicex_lib::sync_v2::transport::{SelfHostTransport, TransportError};
use voicex_sync_protocol::EditKind;

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn server_bin_path() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exe = if cfg!(windows) {
        "voicex-sync-server.exe"
    } else {
        "voicex-sync-server"
    };
    manifest_dir
        .join("..")
        .join("sync-server")
        .join("target")
        .join("debug")
        .join(exe)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn run_admin(bin: &Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(bin)
        .args(args)
        .output()
        .expect("spawn voicex-sync-server admin");
    assert!(
        out.status.success(),
        "admin {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("admin JSON stdout")
}

struct RealClient {
    device_id: String,
    conn: Mutex<Connection>,
    creds: CredentialStore,
    _dir: TempDir,
}

impl RealClient {
    fn new(device_id: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("voicex.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE history_record (
                id TEXT PRIMARY KEY, timestamp TEXT NOT NULL, text TEXT NOT NULL,
                original_text TEXT, ai_correction_applied INTEGER DEFAULT 0, llm_invoked INTEGER DEFAULT 0,
                mode TEXT NOT NULL, duration_ms INTEGER NOT NULL, audio_path TEXT,
                is_final INTEGER DEFAULT 1, error_code INTEGER DEFAULT 0, error_message TEXT,
                source_device_id TEXT, asr_model_name TEXT, llm_model_name TEXT,
                edited_text TEXT, edit_source TEXT, edited_at TEXT,
                target_app_platform TEXT, target_app_key TEXT, target_app_name TEXT
            );
            CREATE TABLE usage_stats (
                id INTEGER PRIMARY KEY CHECK (id = 1), total_duration_ms INTEGER DEFAULT 0,
                total_characters INTEGER DEFAULT 0, llm_correction_count INTEGER DEFAULT 0,
                last_updated TEXT, total_recording_count INTEGER DEFAULT 0,
                tts_read_count INTEGER DEFAULT 0, tts_translate_count INTEGER DEFAULT 0,
                tts_characters INTEGER DEFAULT 0, tts_duration_ms INTEGER DEFAULT 0, tts_llm_count INTEGER DEFAULT 0
            );
            INSERT INTO usage_stats (id) VALUES (1);
            CREATE TABLE device_usage_stats (
                device_id TEXT PRIMARY KEY, total_duration_ms INTEGER DEFAULT 0,
                total_characters INTEGER DEFAULT 0, llm_correction_count INTEGER DEFAULT 0,
                last_updated TEXT, total_recording_count INTEGER DEFAULT 0,
                tts_read_count INTEGER DEFAULT 0, tts_translate_count INTEGER DEFAULT 0,
                tts_characters INTEGER DEFAULT 0, tts_duration_ms INTEGER DEFAULT 0, tts_llm_count INTEGER DEFAULT 0
            );
            CREATE TABLE daily_usage_stats (
                device_id TEXT NOT NULL, local_date TEXT NOT NULL, feature_kind TEXT NOT NULL,
                platform TEXT NOT NULL, app_key TEXT NOT NULL, app_name TEXT NOT NULL,
                usage_count INTEGER NOT NULL DEFAULT 0, total_characters INTEGER NOT NULL DEFAULT 0,
                total_duration_ms INTEGER NOT NULL DEFAULT 0, llm_count INTEGER NOT NULL DEFAULT 0,
                last_used_at TEXT NOT NULL,
                PRIMARY KEY (device_id, local_date, feature_kind, platform, app_key)
            );
            CREATE TABLE user_config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE device_registry (device_id TEXT PRIMARY KEY, device_name TEXT NOT NULL, updated_at TEXT NOT NULL);
            "#,
        )
        .unwrap();
        voicex_lib::post_edit::records::create_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO user_config (key, value) VALUES ('sync_device_id', ?1)",
            params![device_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO device_usage_stats (device_id) VALUES (?1)",
            params![device_id],
        )
        .unwrap();
        init_schema(&conn).unwrap();
        let creds = CredentialStore::in_data_dir(dir.path());
        Self {
            device_id: device_id.to_string(),
            conn: Mutex::new(conn),
            creds,
            _dir: dir,
        }
    }

    fn with_db(
        &self,
        f: &dyn Fn(&Connection) -> Result<(), StorageError>,
    ) -> Result<(), StorageError> {
        let g = self.conn.lock().unwrap();
        f(&g)
    }

    fn dictate(&self, id: &str, text: &str, platform: &str, app_key: &str, app_name: &str) {
        let now_ts = Utc::now().to_rfc3339();
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let rec = HistoryRecord {
            id: id.to_string(),
            timestamp: now_ts.clone(),
            text: text.to_string(),
            original_text: None,
            ai_correction_applied: true,
            llm_invoked: true,
            mode: "assistant_hands_free".to_string(),
            duration_ms: 2100,
            audio_path: None,
            is_final: true,
            error_code: 0,
            error_message: None,
            source_device_id: Some(self.device_id.clone()),
            source_device_name: None,
            asr_model_name: Some("Qwen / qwen-audio-3.1-asr-flash-streaming".to_string()),
            llm_model_name: Some("DeepSeek / deepseek-flash".to_string()),
            edited_text: None,
            target_app_platform: Some(platform.to_string()),
            target_app_key: Some(app_key.to_string()),
            target_app_name: Some(app_name.to_string()),
            has_edit_conflict: false,
        };
        let g = self.conn.lock().unwrap();
        let tx = g.unchecked_transaction().unwrap();
        tx.execute(
            "INSERT INTO history_record (id, timestamp, text, ai_correction_applied, llm_invoked, mode, duration_ms, source_device_id, asr_model_name, llm_model_name, target_app_platform, target_app_key, target_app_name)
             VALUES (?1, ?2, ?3, 1, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                rec.id, rec.timestamp, rec.text, rec.mode, rec.duration_ms, rec.source_device_id,
                rec.asr_model_name, rec.llm_model_name, rec.target_app_platform, rec.target_app_key, rec.target_app_name
            ],
        )
        .unwrap();
        tx.execute(
            "UPDATE device_usage_stats SET total_recording_count = total_recording_count + 1, total_characters = total_characters + ?2, total_duration_ms = total_duration_ms + ?3, llm_correction_count = llm_correction_count + 1 WHERE device_id = ?1",
            params![self.device_id, text.chars().count() as i64, rec.duration_ms],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO daily_usage_stats (device_id, local_date, feature_kind, platform, app_key, app_name, usage_count, total_characters, total_duration_ms, llm_count, last_used_at)
             VALUES (?1, ?2, 'dictation', ?3, ?4, ?5, 1, ?6, ?7, 1, ?8)
             ON CONFLICT(device_id, local_date, feature_kind, platform, app_key) DO UPDATE SET
               usage_count = usage_count + 1, total_characters = total_characters + excluded.total_characters",
            params![self.device_id, today, platform, app_key, app_name, text.chars().count() as i64, rec.duration_ms, now_ts],
        )
        .unwrap();
        on_local_history_inserted_tx(&tx, &rec, &today).unwrap();
        tx.commit().unwrap();
    }
}

#[tokio::test]
async fn real_tcp_two_clients_sse_e2ee_conflict_delete_and_cli_revoke() {
    let bin = server_bin_path();
    assert!(
        bin.exists(),
        "build sync-server first: `cargo build --manifest-path sync-server/Cargo.toml` (missing {:?})",
        bin
    );

    let srv_dir = TempDir::new().unwrap();
    let srv_db = srv_dir.path().join("srv.db");
    let srv_logs = srv_dir.path().join("logs");
    let db_str = srv_db.to_str().unwrap();

    // 1. Admin CLI: create account + two one-time pairing codes
    let acc = run_admin(&bin, &["admin", "create-account", "--db", db_str, "--label", "e2e"]);
    let account_id = acc["accountId"].as_str().unwrap().to_string();
    let code_mac = run_admin(&bin, &["admin", "create-pairing-code", "--db", db_str, "--account", &account_id])["pairingCode"]
        .as_str()
        .unwrap()
        .to_string();
    let code_win = run_admin(&bin, &["admin", "create-pairing-code", "--db", db_str, "--account", &account_id])["pairingCode"]
        .as_str()
        .unwrap()
        .to_string();

    // 2. Spawn real voicex-sync-server HTTP/SSE daemon on ephemeral localhost port
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let server_url = format!("http://{addr}");
    let _guard = ChildGuard(
        Command::new(&bin)
            .args(["serve", "--addr", &addr, "--db", db_str, "--log-dir", srv_logs.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn voicex-sync-server"),
    );

    // Wait for /healthz
    let health_client = reqwest::Client::new();
    let mut ready = false;
    for _ in 0..40 {
        if let Ok(r) = health_client.get(format!("{server_url}/healthz")).send().await {
            if r.status().is_success() {
                ready = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ready, "voicex-sync-server failed to become ready on {server_url}");

    // 3. Pair Mac + Windows over real HTTP with E2EE recovery key
    let transport = SelfHostTransport::new(&server_url).unwrap();
    let mac = RealClient::new("mac-real-1");
    let win = RealClient::new("win-real-1");

    let mac_pair = pair_space(
        |f| mac.with_db(f),
        &mac.creds,
        &transport,
        &server_url,
        &code_mac,
        "mac-real-1",
        "MBP_M5",
        None,
        true,
        true,
    )
    .await
    .unwrap();
    assert!(mac_pair.generated_new_recovery_key);

    let win_pair = pair_space(
        |f| win.with_db(f),
        &win.creds,
        &transport,
        &server_url,
        &code_win,
        "win-real-1",
        "xymi01",
        Some(&mac_pair.recovery_key),
        true,
        true,
    )
    .await
    .unwrap();
    assert!(!win_pair.generated_new_recovery_key);

    // 4. Open real SSE stream on Windows BEFORE Mac dictates & pushes
    let win_token = win.creds.get(&win_pair.space_id).unwrap().unwrap().device_token;
    let sse_resp = transport.subscribe_sse(&win_token).await.unwrap();
    let mut sse_stream = sse_resp.bytes_stream();

    mac.dictate("real-rec-1", "Hello from Mac", "macos", "com.apple.Notes", "Notes");
    sync_once(|f| mac.with_db(f), &mac.creds, &transport, 0).await.unwrap();

    // Verify Windows receives real SSE notify over TCP
    let chunk = tokio::time::timeout(Duration::from_secs(3), sse_stream.next())
        .await
        .expect("SSE timeout")
        .expect("SSE stream item")
        .expect("SSE bytes");
    let sse_text = String::from_utf8_lossy(&chunk);
    assert!(sse_text.contains("notify"), "unexpected SSE chunk: {sse_text}");

    // Windows pulls & decrypts real-rec-1 + Mac stats, then dictates its own Windows row
    sync_once(|f| win.with_db(f), &win.creds, &transport, 0).await.unwrap();
    win.dictate("real-rec-2", "Hello from Win", "windows", "notepad.exe", "Notepad");
    sync_once(|f| win.with_db(f), &win.creds, &transport, 0).await.unwrap();
    sync_once(|f| mac.with_db(f), &mac.creds, &transport, 0).await.unwrap();

    // Verify both clients converged on 2 dictations & separate macOS/Windows platform apps!
    for client in [&mac, &win] {
        let g = client.conn.lock().unwrap();
        let all = get_overview_usage_stats_in(&g, &client.device_id, OverviewScope::All).unwrap();
        assert_eq!(all.total_recording_count, 2);
        let rec_cnt: i64 = g.query_row("SELECT COUNT(*) FROM history_record", [], |r| r.get(0)).unwrap();
        assert_eq!(rec_cnt, 2);
    }

    // 5. Concurrent offline manual edits on real-rec-1 -> both detect conflict -> resolve on Mac -> Win converges!
    let edit_ts = Utc::now().to_rfc3339();
    {
        let mg = mac.conn.lock().unwrap();
        on_local_edit_applied_tx(&mg, "real-rec-1", "e-mac", EditKind::Manual, Some("Mac edit"), &edit_ts, None).unwrap();
    }
    {
        let wg = win.conn.lock().unwrap();
        on_local_edit_applied_tx(&wg, "real-rec-1", "e-win", EditKind::Manual, Some("Win edit"), &edit_ts, None).unwrap();
    }
    sync_once(|f| mac.with_db(f), &mac.creds, &transport, 0).await.unwrap();
    sync_once(|f| win.with_db(f), &win.creds, &transport, 0).await.unwrap();
    sync_once(|f| mac.with_db(f), &mac.creds, &transport, 0).await.unwrap();

    {
        let mg = mac.conn.lock().unwrap();
        let wg = win.conn.lock().unwrap();
        assert_eq!(list_edit_conflict_alternatives_in(&mg, "real-rec-1").unwrap().len(), 1);
        assert_eq!(list_edit_conflict_alternatives_in(&wg, "real-rec-1").unwrap().len(), 1);
        resolve_edit_conflict_in(&mg, "real-rec-1", Some("Mac edit")).unwrap();
    }
    sync_once(|f| mac.with_db(f), &mac.creds, &transport, 0).await.unwrap();
    sync_once(|f| win.with_db(f), &win.creds, &transport, 0).await.unwrap();
    {
        let wg = win.conn.lock().unwrap();
        assert!(list_edit_conflict_alternatives_in(&wg, "real-rec-1").unwrap().is_empty());
        let txt: Option<String> = wg
            .query_row("SELECT edited_text FROM history_record WHERE id = 'real-rec-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(txt.as_deref(), Some("Mac edit"));
    }

    // 6. Delete on Mac -> propagates to Win while totals remain 2
    {
        let mg = mac.conn.lock().unwrap();
        let tx = mg.unchecked_transaction().unwrap();
        on_local_history_deleted_tx(&tx, "real-rec-1", true).unwrap();
        tx.execute("DELETE FROM history_record WHERE id = 'real-rec-1'", []).unwrap();
        tx.commit().unwrap();
    }
    sync_once(|f| mac.with_db(f), &mac.creds, &transport, 0).await.unwrap();
    sync_once(|f| win.with_db(f), &win.creds, &transport, 0).await.unwrap();
    {
        let wg = win.conn.lock().unwrap();
        let cnt: i64 = wg.query_row("SELECT COUNT(*) FROM history_record", [], |r| r.get(0)).unwrap();
        assert_eq!(cnt, 1);
    }

    // 7. Revoke Windows via real admin CLI -> Win gets DeviceRevoked, Mac stays healthy
    let rev = run_admin(&bin, &["admin", "revoke-device", "--db", db_str, "--account", &account_id, "--device", "win-real-1"]);
    assert_eq!(rev["revoked"], true);

    let win_res = sync_once(|f| win.with_db(f), &win.creds, &transport, 0).await;
    assert!(matches!(
        win_res,
        Err(SyncEngineError::Transport(TransportError::DeviceRevoked(_)))
    ));
    assert!(sync_once(|f| mac.with_db(f), &mac.creds, &transport, 0).await.is_ok());
}
