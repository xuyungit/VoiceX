//! Synthetic data only; never opens the user's database.
//! cargo run --example history_search_bench -- 20000
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};
use voicex_lib::storage::{self, history_search, HistoryRecordType};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rows: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "20000".into())
        .parse()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("history.db");
    storage::init_database(&path)?;
    let mut writer = Connection::open(&path)?;
    let text = "这是用于性能评审的合成历史文本，不含用户数据。".repeat(50);
    let tx = writer.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO history_record
             (id,timestamp,text,original_text,edited_text,mode,duration_ms)
             VALUES (?1,?2,?3,?3,?3,'assistant_raw',1000)",
        )?;
        let now = chrono::Utc::now();
        for i in 0..rows {
            insert.execute(params![
                i.to_string(),
                (now - chrono::Duration::seconds(i as i64)).to_rfc3339(),
                &text
            ])?;
        }
    }
    tx.commit()?;
    // Start measurements with seeding/checkpoint work finished.
    writer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;

    let id = history_search::begin()?;
    let started = Instant::now();
    let count = history_search::count(id, "未出现的关键词".into(), HistoryRecordType::All).await?;
    let full_count_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(count, 0);

    let count = tokio::spawn(history_search::count(
        id,
        "未出现的关键词".into(),
        HistoryRecordType::All,
    ));
    tokio::time::sleep(Duration::from_millis(10)).await;
    let count_running_before_operations = !count.is_finished();
    let started = Instant::now();
    let page = history_search::page(id, 20, 0, "合成".into(), HistoryRecordType::All).await?;
    let page_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    tauri::async_runtime::spawn_blocking(|| {
        let settings = storage::get_settings()?;
        storage::save_settings(&settings)
    })
    .await??;
    let settings_read_write_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    tauri::async_runtime::spawn_blocking(|| {
        let mut record = storage::get_history(1, 0)?.remove(0);
        record.id = uuid::Uuid::new_v4().to_string();
        record.timestamp = chrono::Utc::now().to_rfc3339();
        storage::insert_history_record_with_stats(&record, true)
    })
    .await??;
    let history_save_ms = started.elapsed().as_secs_f64() * 1000.0;
    let count_running_before_cancel = !count.is_finished();
    let started = Instant::now();
    history_search::begin()?;
    let cancelled = count.await?.is_err();
    let cancellation_ms = started.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "rows": rows,
            "characters_per_text_version": text.chars().count(),
            "full_count_ms": full_count_ms,
            "count_running_before_operations": count_running_before_operations,
            "page_rows": page.len(),
            "page_ms": page_ms,
            "settings_read_write_ms": settings_read_write_ms,
            "history_save_ms": history_save_ms,
            "count_running_before_cancel": count_running_before_cancel,
            "cancelled": cancelled,
            "cancellation_ms": cancellation_ms,
        }))?
    );
    Ok(())
}
