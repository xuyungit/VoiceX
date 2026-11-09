//! Cancellable history reads, isolated from the connection used by dictation.
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::Mutex;

use super::database::{count_history_in, search_history_in};
use super::{HistoryRecord, HistoryRecordType, StorageError};

const SUPERSEDED: &str = "History search superseded";
static READERS: OnceLock<SearchReaders> = OnceLock::new();

struct SearchReaders {
    // A full count must not delay a page, including a later scroll page.
    pages: Arc<Mutex<Connection>>,
    counts: Arc<Mutex<Connection>>,
    generation: Arc<AtomicU64>,
}

impl SearchReaders {
    fn open(path: &Path) -> Result<Self, StorageError> {
        let open = || {
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map(|conn| Arc::new(Mutex::new(conn)))
                .map_err(|e| StorageError::ConnectionFailed(e.to_string()))
        };
        Ok(Self {
            pages: open()?,
            counts: open()?,
            generation: Arc::new(AtomicU64::new(0)),
        })
    }

    fn begin(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn is_current(&self, id: u64) -> bool {
        id == self.generation.load(Ordering::SeqCst)
    }

    async fn read<T: Send + 'static>(
        &self,
        reader: Arc<Mutex<Connection>>,
        id: u64,
        query: impl FnOnce(&Connection) -> Result<T, StorageError> + Send + 'static,
    ) -> Result<T, String> {
        if !self.is_current(id) {
            return Err(SUPERSEDED.into());
        }
        // Waiting requests do not occupy blocking-pool or async-runtime threads.
        // Superseded requests are discarded before executing any SQL.
        let conn = reader.lock_owned().await;
        if !self.is_current(id) {
            return Err(SUPERSEDED.into());
        }
        let generation = self.generation.clone();
        tauri::async_runtime::spawn_blocking(move || run_query(&conn, generation, id, query))
            .await
            .map_err(|e| e.to_string())?
    }
}

fn run_query<T>(
    conn: &Connection,
    generation: Arc<AtomicU64>,
    id: u64,
    query: impl FnOnce(&Connection) -> Result<T, StorageError>,
) -> Result<T, String> {
    if generation.load(Ordering::SeqCst) != id {
        return Err(SUPERSEDED.into());
    }
    let current = generation.clone();
    // SQLite checks this while scanning, so an old LIKE/COUNT really stops.
    // No interrupt is ever sent to the recording/settings connection.
    conn.progress_handler(1000, Some(move || current.load(Ordering::SeqCst) != id));
    let result = query(conn);
    conn.progress_handler(0, None::<fn() -> bool>);
    if generation.load(Ordering::SeqCst) != id {
        return Err(SUPERSEDED.into());
    }
    result.map_err(|e| e.to_string())
}

pub(super) fn init(path: &Path) -> Result<(), StorageError> {
    let readers = SearchReaders::open(path)?;
    let _ = READERS.set(readers);
    Ok(())
}

fn readers() -> Result<&'static SearchReaders, String> {
    READERS
        .get()
        .ok_or_else(|| StorageError::NotInitialized.to_string())
}

/// Called on the main IPC path before submitting any SQL for a new search.
pub fn begin() -> Result<u64, String> {
    Ok(readers()?.begin())
}

pub async fn page(
    id: u64,
    limit: u32,
    offset: u32,
    query: String,
    kind: HistoryRecordType,
) -> Result<Vec<HistoryRecord>, String> {
    let readers = readers()?;
    readers
        .read(readers.pages.clone(), id, move |conn| {
            search_history_in(conn, limit, offset, &query, kind)
        })
        .await
}

pub async fn count(id: u64, query: String, kind: HistoryRecordType) -> Result<u32, String> {
    let readers = readers()?;
    readers
        .read(readers.counts.clone(), id, move |conn| {
            count_history_in(conn, &query, kind)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn fixture() -> (tempfile::TempDir, Connection, Arc<SearchReaders>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE entries(value INTEGER);
             INSERT INTO entries VALUES (1);",
            )
            .unwrap();
        writer.busy_timeout(Duration::ZERO).unwrap();
        let readers = Arc::new(SearchReaders::open(&path).unwrap());
        (dir, writer, readers)
    }

    fn value(conn: &Connection) -> Result<i64, StorageError> {
        conn.query_row("SELECT value FROM entries", [], |row| row.get(0))
            .map_err(|e| StorageError::QueryFailed(e.to_string()))
    }

    #[tokio::test]
    async fn count_snapshot_does_not_block_writes_or_pages() {
        let (_dir, writer, readers) = fixture();
        let id = readers.begin();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let counter = readers.clone();
        let count = tokio::spawn(async move {
            counter
                .read(counter.counts.clone(), id, move |conn| {
                    conn.execute_batch("BEGIN").unwrap();
                    let snapshot = value(conn)?;
                    started.send(()).unwrap();
                    resume.recv_timeout(Duration::from_secs(5)).unwrap();
                    conn.execute_batch("COMMIT").unwrap();
                    Ok(snapshot)
                })
                .await
        });
        ready.await.unwrap();
        // busy_timeout=0: this would fail immediately with a rollback journal.
        writer.execute("UPDATE entries SET value=2", []).unwrap();
        let page = tokio::time::timeout(
            Duration::from_secs(2),
            readers.read(readers.pages.clone(), id, value),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(page, 2);
        release.send(()).unwrap();
        assert_eq!(count.await.unwrap().unwrap(), 1);
        assert_eq!(
            readers
                .read(readers.counts.clone(), id, value)
                .await
                .unwrap(),
            2
        );
        let write = readers
            .read(readers.pages.clone(), id, |conn| {
                conn.execute("UPDATE entries SET value=3", [])
                    .map_err(|e| StorageError::QueryFailed(e.to_string()))
            })
            .await;
        assert!(write.unwrap_err().contains("readonly"));
    }

    #[tokio::test]
    async fn superseded_queued_reads_never_execute_sql() {
        let (_dir, _writer, readers) = fixture();
        let held = readers.pages.lock().await;
        let executed = Arc::new(AtomicUsize::new(0));
        let mut waiting = Vec::new();
        for _ in 0..3 {
            let id = readers.begin();
            let reader = readers.clone();
            let executed = executed.clone();
            waiting.push(tokio::spawn(async move {
                reader
                    .read(reader.pages.clone(), id, move |conn| {
                        executed.fetch_add(1, Ordering::SeqCst);
                        value(conn)
                    })
                    .await
            }));
            tokio::task::yield_now().await;
        }
        drop(held);
        let latest = waiting.pop().unwrap();
        for old in waiting {
            assert_eq!(old.await.unwrap().unwrap_err(), SUPERSEDED);
        }
        assert_eq!(latest.await.unwrap().unwrap(), 1);
        assert_eq!(executed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn changing_generation_interrupts_sql_and_connection_can_be_reused() {
        let conn = Connection::open_in_memory().unwrap();
        let generation = Arc::new(AtomicU64::new(1));
        let current = generation.clone();
        let interrupt = conn.get_interrupt_handle();
        let (started, ready) = std::sync::mpsc::channel();
        let (done, finished) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = run_query(&conn, current.clone(), 1, |conn| {
                started.send(()).unwrap();
                conn.query_row(
                    "WITH RECURSIVE numbers(n) AS (VALUES(0) UNION ALL
                     SELECT n+1 FROM numbers WHERE n<1000000000)
                     SELECT sum(n) FROM numbers",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| StorageError::QueryFailed(e.to_string()))
            });
            done.send(result).unwrap();
            run_query(&conn, current, 2, |conn| {
                conn.query_row("SELECT 42", [], |row| row.get::<_, i64>(0))
                    .map_err(|e| StorageError::QueryFailed(e.to_string()))
            })
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        generation.store(2, Ordering::SeqCst);
        let result = finished.recv_timeout(Duration::from_secs(2));
        // Keep a broken cancellation test from leaving a billion-row scan alive.
        if result.is_err() {
            interrupt.interrupt();
        }
        assert_eq!(result.unwrap().unwrap_err(), SUPERSEDED);
        assert_eq!(worker.join().unwrap().unwrap(), 42);
    }
}
