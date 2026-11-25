use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{Duration as ChronoDuration, Utc};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;
use voicex_sync_protocol::{
    error_codes, parse_stats_object_id, AccountInfoResponse, DeviceInfo, DeviceUpdateRequest,
    EnvelopeItem, EnvelopeResult, EnvelopeStatus, NotifyEvent, PairRedeemRequest,
    PairRedeemResponse, PullEnvelopesResponse, PushEnvelopesRequest, PushEnvelopesResponse,
    SyncDomain, DEFAULT_PULL_LIMIT, MAX_PULL_LIMIT, MAX_PUSH_BATCH_LEN,
};

pub const DEFAULT_STORAGE_QUOTA_BYTES: i64 = 512 * 1024 * 1024;
const TOKEN_PREFIX: &str = "vxd2_";
const PAIRING_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Error)]
pub enum DbApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("device credential has been revoked")]
    DeviceRevoked,
    #[error("{0}")]
    ScopeForbidden(String),
    #[error("{0}")]
    PairingInvalidOrExpired(String),
    #[error("{0}")]
    DeviceIdConflict(String),
    #[error("account storage quota exceeded")]
    QuotaExceeded,
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

impl DbApiError {
    pub fn error_code(&self) -> &'static str {
        match self {
            DbApiError::BadRequest(_) => error_codes::BAD_REQUEST,
            DbApiError::Unauthorized => error_codes::UNAUTHORIZED,
            DbApiError::DeviceRevoked => error_codes::DEVICE_REVOKED,
            DbApiError::ScopeForbidden(_) => error_codes::SCOPE_FORBIDDEN,
            DbApiError::PairingInvalidOrExpired(_) => error_codes::PAIRING_INVALID_OR_EXPIRED,
            DbApiError::DeviceIdConflict(_) => error_codes::DEVICE_ID_CONFLICT,
            DbApiError::QuotaExceeded => error_codes::QUOTA_EXCEEDED,
            DbApiError::Sqlx(_) => error_codes::INTERNAL_ERROR,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthedDevice {
    pub knowledge_scopes: u8,
    pub account_id: String,
    pub device_id: String,
    pub allow_stats: bool,
    pub allow_history: bool,
    pub allow_edits: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedAccount {
    pub account_id: String,
    pub label: String,
    pub storage_quota_bytes: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedPairingCode {
    pub account_id: String,
    pub pairing_code: String,
    pub expires_at: String,
}

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

fn generate_device_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Generate a human-typable high-entropy one-time pairing code (`VXP2-XXXX-XXXX-XXXX-XXXX`).
fn generate_pairing_code() -> String {
    let mut raw = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let chars: String = raw
        .iter()
        .map(|&b| PAIRING_CODE_ALPHABET[(b as usize) % PAIRING_CODE_ALPHABET.len()] as char)
        .collect();
    format!(
        "VXP2-{}-{}-{}-{}",
        &chars[0..4],
        &chars[4..8],
        &chars[8..12],
        &chars[12..16]
    )
}

fn normalize_pairing_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

pub async fn open_pool(db_path: &str) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    init_db(&pool).await?;
    Ok(pool)
}

pub async fn init_db(db: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("PRAGMA journal_mode = WAL;").execute(db).await?;
    sqlx::query("PRAGMA busy_timeout = 5000;").execute(db).await?;
    sqlx::query("PRAGMA foreign_keys = ON;").execute(db).await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS server_info (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            server_id TEXT NOT NULL
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS accounts (
            account_id TEXT PRIMARY KEY,
            label TEXT NOT NULL DEFAULT '',
            next_seq INTEGER NOT NULL DEFAULT 0,
            key_check_tag TEXT,
            storage_quota_bytes INTEGER NOT NULL DEFAULT 536870912,
            used_bytes INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            disabled_at TEXT
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pairing_codes (
            code_hash TEXT PRIMARY KEY,
            account_id TEXT NOT NULL REFERENCES accounts(account_id),
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            redeemed_at TEXT,
            redeemed_device_id TEXT
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS devices (
            account_id TEXT NOT NULL REFERENCES accounts(account_id),
            device_id TEXT NOT NULL,
            device_name TEXT NOT NULL,
            platform TEXT NOT NULL DEFAULT '',
            app_version TEXT NOT NULL DEFAULT '',
            token_hash TEXT NOT NULL UNIQUE,
            allow_stats INTEGER NOT NULL DEFAULT 1,
            allow_history INTEGER NOT NULL DEFAULT 1,
            allow_edits INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            revoked_at TEXT,
            PRIMARY KEY (account_id, device_id)
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS device_pair_bindings (
           account_id TEXT NOT NULL, device_id TEXT NOT NULL, proof_hash TEXT NOT NULL,
           last_code_hash TEXT NOT NULL, PRIMARY KEY(account_id, device_id))",
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS device_knowledge_scopes (
            account_id TEXT NOT NULL, device_id TEXT NOT NULL, mask INTEGER NOT NULL,
            PRIMARY KEY(account_id, device_id),
            FOREIGN KEY(account_id, device_id) REFERENCES devices(account_id, device_id)
        )"#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS envelopes (
            account_id TEXT NOT NULL REFERENCES accounts(account_id),
            seq INTEGER NOT NULL,
            op_id TEXT NOT NULL,
            domain TEXT NOT NULL,
            device_id TEXT NOT NULL,
            object_id TEXT NOT NULL,
            revision INTEGER NOT NULL DEFAULT 0,
            nonce TEXT NOT NULL,
            ciphertext TEXT NOT NULL,
            byte_size INTEGER NOT NULL,
            superseded INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            PRIMARY KEY (account_id, seq),
            UNIQUE (account_id, op_id)
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS envelopes_pull_idx
         ON envelopes(account_id, domain, superseded, seq)",
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS stats_shards (
            account_id TEXT NOT NULL REFERENCES accounts(account_id),
            device_id TEXT NOT NULL,
            shard_key TEXT NOT NULL,
            revision INTEGER NOT NULL,
            op_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            nonce TEXT NOT NULL,
            ciphertext TEXT NOT NULL,
            byte_size INTEGER NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (account_id, device_id, shard_key)
        )
        "#,
    )
    .execute(db)
    .await?;

    Ok(())
}

pub async fn ensure_server_id(db: &SqlitePool) -> Result<String, sqlx::Error> {
    if let Some(row) = sqlx::query("SELECT server_id FROM server_info WHERE id = 1")
        .fetch_optional(db)
        .await?
    {
        return Ok(row.get(0));
    }
    let server_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO server_info (id, server_id) VALUES (1, ?1)")
        .bind(&server_id)
        .execute(db)
        .await?;
    Ok(server_id)
}

pub async fn create_account(
    db: &SqlitePool,
    label: &str,
    storage_quota_bytes: i64,
) -> Result<CreatedAccount, DbApiError> {
    let account_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let quota = storage_quota_bytes.max(1024);
    sqlx::query(
        "INSERT INTO accounts (account_id, label, next_seq, storage_quota_bytes, used_bytes, created_at)
         VALUES (?1, ?2, 0, ?3, 0, ?4)",
    )
    .bind(&account_id)
    .bind(label.trim())
    .bind(quota)
    .bind(&created_at)
    .execute(db)
    .await?;
    Ok(CreatedAccount {
        account_id,
        label: label.trim().to_string(),
        storage_quota_bytes: quota,
        created_at,
    })
}

pub async fn create_pairing_code(
    db: &SqlitePool,
    account_id: &str,
    ttl_minutes: i64,
) -> Result<CreatedPairingCode, DbApiError> {
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM accounts WHERE account_id = ?1 AND disabled_at IS NULL",
    )
    .bind(account_id)
    .fetch_optional(db)
    .await?;
    if exists.is_none() {
        return Err(DbApiError::BadRequest(format!(
            "account not found or disabled: {account_id}"
        )));
    }

    let pairing_code = generate_pairing_code();
    let code_hash = sha256_hex(&normalize_pairing_code(&pairing_code));
    let now = Utc::now();
    let expires = now + ChronoDuration::minutes(ttl_minutes);
    sqlx::query(
        "INSERT INTO pairing_codes (code_hash, account_id, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(&code_hash)
    .bind(account_id)
    .bind(now.to_rfc3339())
    .bind(expires.to_rfc3339())
    .execute(db)
    .await?;

    Ok(CreatedPairingCode {
        account_id: account_id.to_string(),
        pairing_code,
        expires_at: expires.to_rfc3339(),
    })
}

pub async fn list_devices(db: &SqlitePool, account_id: &str) -> Result<Vec<DeviceInfo>, DbApiError> {
    let rows = sqlx::query(
        "SELECT device_id, device_name, platform, app_version,
                allow_stats, allow_history, allow_edits, last_seen_at, revoked_at,
                COALESCE((SELECT mask FROM device_knowledge_scopes k WHERE k.account_id=d.account_id AND k.device_id=d.device_id),0)
         FROM devices d
         WHERE account_id = ?1
         ORDER BY last_seen_at DESC, device_name ASC",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| DeviceInfo {
            knowledge_scopes: r.get::<i64, _>(9) as u8,
            device_id: r.get(0),
            device_name: r.get(1),
            platform: r.get(2),
            app_version: r.get(3),
            allow_stats: r.get::<i64, _>(4) != 0,
            allow_history: r.get::<i64, _>(5) != 0,
            allow_edits: r.get::<i64, _>(6) != 0,
            last_seen_at: r.get(7),
            revoked_at: r.get(8),
        })
        .collect())
}

pub async fn revoke_device(
    db: &SqlitePool,
    account_id: &str,
    device_id: &str,
) -> Result<bool, DbApiError> {
    let now = Utc::now().to_rfc3339();
    let res = sqlx::query(
        "UPDATE devices
         SET revoked_at = COALESCE(revoked_at, ?3)
         WHERE account_id = ?1 AND device_id = ?2",
    )
    .bind(account_id)
    .bind(device_id)
    .bind(&now)
    .execute(db)
    .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn redeem_pairing_code(
    db: &SqlitePool,
    server_id: &str,
    req: &PairRedeemRequest,
) -> Result<PairRedeemResponse, DbApiError> {
    let device_id = req.device_id.trim();
    let device_name = req.device_name.trim();
    if device_id.is_empty() || device_name.is_empty() {
        return Err(DbApiError::BadRequest(
            "deviceId and deviceName are required".into(),
        ));
    }
    let normalized = normalize_pairing_code(&req.pairing_code);
    if normalized.is_empty() {
        return Err(DbApiError::PairingInvalidOrExpired(
            "pairing code is required".into(),
        ));
    }
    let code_hash = sha256_hex(&normalized);
    let now = Utc::now();
    let now_str = now.to_rfc3339();

    let mut tx = db.begin().await?;
    let row = sqlx::query(
        "SELECT p.account_id, p.expires_at, p.redeemed_at, a.key_check_tag, a.disabled_at
         FROM pairing_codes p
         JOIN accounts a ON a.account_id = p.account_id
         WHERE p.code_hash = ?1",
    )
    .bind(&code_hash)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(row) = row else {
        return Err(DbApiError::PairingInvalidOrExpired(
            "pairing code not found".into(),
        ));
    };

    let account_id: String = row.get(0);
    let expires_at_str: String = row.get(1);
    let redeemed_at: Option<String> = row.get(2);
    let key_check_tag: Option<String> = row.get(3);
    let disabled_at: Option<String> = row.get(4);

    if disabled_at.is_some() {
        return Err(DbApiError::PairingInvalidOrExpired(
            "account is disabled".into(),
        ));
    }
    let expired = chrono::DateTime::parse_from_rfc3339(&expires_at_str)
        .map(|dt| dt.with_timezone(&Utc) <= now)
        .unwrap_or(true);
    if expired {
        return Err(DbApiError::PairingInvalidOrExpired(
            "pairing code has expired".into(),
        ));
    }

    let proof = req.installation_proof.as_deref();
    if proof.is_some_and(|p| p.len() != 64 || !p.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(DbApiError::BadRequest("invalid installation proof".into()));
    }
    let proof_hash = proof.map(sha256_hex);
    let device = sqlx::query(
        "SELECT d.token_hash, d.revoked_at, b.proof_hash, b.last_code_hash
         FROM devices d LEFT JOIN device_pair_bindings b USING(account_id, device_id)
         WHERE d.account_id = ?1 AND d.device_id = ?2",
    )
    .bind(&account_id)
    .bind(device_id)
    .fetch_optional(&mut *tx)
    .await?;
    let mut resume = false;
    if let Some(row) = device {
        let hash: String = row.get(0);
        let revoked: Option<String> = row.get(1);
        let bound: Option<String> = row.get(2);
        let last_code: Option<String> = row.get(3);
        let active = revoked.is_none() && !hash.starts_with("migrated-unpaired:");
        let owns_binding = proof_hash.is_some() && bound == proof_hash;
        let owns_token = req
            .previous_device_token
            .as_deref()
            .is_some_and(|t| sha256_hex(t) == hash);
        resume = active && owns_binding && last_code.as_deref() == Some(code_hash.as_str());
        if redeemed_at.is_none() && active && !owns_binding && !owns_token {
            return Err(DbApiError::DeviceIdConflict(
                "active device requires its original installation or credential".into(),
            ));
        }
    }
    if redeemed_at.is_some() && !resume {
        return Err(DbApiError::PairingInvalidOrExpired(
            "pairing code has already been redeemed".into(),
        ));
    }

    // The same installation can recover the original result if the response was lost.
    // Each newly issued code produces a distinct token, including after revocation.
    let device_token = match proof {
        Some(secret) => {
            use hmac::{Hmac, Mac};
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                .expect("HMAC accepts any key length");
            mac.update(
                format!("voicex-pair-v2:{server_id}:{account_id}:{device_id}:{code_hash}")
                    .as_bytes(),
            );
            format!(
                "{TOKEN_PREFIX}{}",
                URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
            )
        }
        None => generate_device_token(),
    };
    if resume {
        return Ok(PairRedeemResponse {
            account_id,
            server_id: server_id.into(),
            device_id: device_id.into(),
            device_token,
            key_check_tag,
        });
    }
    let updated = sqlx::query(
        "UPDATE pairing_codes SET redeemed_at = ?2, redeemed_device_id = ?3
         WHERE code_hash = ?1 AND redeemed_at IS NULL",
    )
    .bind(&code_hash)
    .bind(&now_str)
    .bind(device_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(DbApiError::PairingInvalidOrExpired(
            "pairing code was already redeemed".into(),
        ));
    }
    sqlx::query("DELETE FROM device_pair_bindings WHERE account_id=?1 AND device_id=?2")
        .bind(&account_id)
        .bind(device_id)
        .execute(&mut *tx)
        .await?;
    if let Some(hash) = proof_hash {
        sqlx::query("INSERT INTO device_pair_bindings VALUES (?1, ?2, ?3, ?4)")
            .bind(&account_id)
            .bind(device_id)
            .bind(hash)
            .bind(&code_hash)
            .execute(&mut *tx)
            .await?;
    }
    let token_hash = sha256_hex(&device_token);

    sqlx::query(
        "INSERT INTO devices (
            account_id, device_id, device_name, platform, app_version,
            token_hash, allow_stats, allow_history, allow_edits,
            created_at, last_seen_at, revoked_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 1, 1, ?7, ?7, NULL)
         ON CONFLICT(account_id, device_id) DO UPDATE SET
            device_name = excluded.device_name,
            platform = excluded.platform,
            app_version = excluded.app_version,
            token_hash = excluded.token_hash,
            last_seen_at = excluded.last_seen_at,
            revoked_at = NULL",
    )
    .bind(&account_id)
    .bind(device_id)
    .bind(device_name)
    .bind(req.platform.trim())
    .bind(req.app_version.trim())
    .bind(&token_hash)
    .bind(&now_str)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(PairRedeemResponse {
        account_id,
        server_id: server_id.to_string(),
        device_id: device_id.to_string(),
        device_token,
        key_check_tag,
    })
}

pub async fn authenticate_token(
    db: &SqlitePool,
    bearer_token: &str,
) -> Result<AuthedDevice, DbApiError> {
    let trimmed = bearer_token.trim();
    if trimmed.is_empty() {
        return Err(DbApiError::Unauthorized);
    }
    let token_hash = sha256_hex(trimmed);
    let row = sqlx::query(
        "SELECT d.account_id, d.device_id, d.allow_stats, d.allow_history, d.allow_edits,
                d.revoked_at, a.disabled_at,
                COALESCE((SELECT mask FROM device_knowledge_scopes k WHERE k.account_id=d.account_id AND k.device_id=d.device_id),0)
         FROM devices d
         JOIN accounts a ON a.account_id = d.account_id
         WHERE d.token_hash = ?1",
    )
    .bind(&token_hash)
    .fetch_optional(db)
    .await?;

    let Some(row) = row else {
        return Err(DbApiError::Unauthorized);
    };
    let revoked_at: Option<String> = row.get(5);
    let disabled_at: Option<String> = row.get(6);
    if revoked_at.is_some() || disabled_at.is_some() {
        return Err(DbApiError::DeviceRevoked);
    }

    Ok(AuthedDevice {
        knowledge_scopes: row.get::<i64, _>(7) as u8,
        account_id: row.get(0),
        device_id: row.get(1),
        allow_stats: row.get::<i64, _>(2) != 0,
        allow_history: row.get::<i64, _>(3) != 0,
        allow_edits: row.get::<i64, _>(4) != 0,
    })
}

pub async fn get_account_info(
    db: &SqlitePool,
    server_id: &str,
    account_id: &str,
) -> Result<AccountInfoResponse, DbApiError> {
    let row = sqlx::query(
        "SELECT next_seq, key_check_tag FROM accounts WHERE account_id = ?1",
    )
    .bind(account_id)
    .fetch_optional(db)
    .await?
    .ok_or(DbApiError::Unauthorized)?;

    let latest_seq: i64 = row.get(0);
    let key_check_tag: Option<String> = row.get(1);
    let devices = list_devices(db, account_id).await?;

    Ok(AccountInfoResponse {
        account_id: account_id.to_string(),
        server_id: server_id.to_string(),
        latest_seq,
        key_check_tag,
        devices,
    })
}

pub async fn update_device(
    db: &SqlitePool,
    auth: &AuthedDevice,
    path_device_id: &str,
    req: &DeviceUpdateRequest,
) -> Result<DeviceInfo, DbApiError> {
    if path_device_id != auth.device_id {
        return Err(DbApiError::ScopeForbidden(
            "device credential can only update its own device record".into(),
        ));
    }
    let name = req.device_name.trim();
    if name.is_empty() {
        return Err(DbApiError::BadRequest("deviceName must not be empty".into()));
    }
    let now = Utc::now().to_rfc3339();
    let allow_edits = req.allow_history && req.allow_edits;

    let mut tx = db.begin().await?;
    if let Some(tag) = req.key_check_tag.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let existing_tag: Option<String> = sqlx::query_scalar(
            "SELECT key_check_tag FROM accounts WHERE account_id = ?1",
        )
        .bind(&auth.account_id)
        .fetch_one(&mut *tx)
        .await?;
        match existing_tag {
            None => {
                sqlx::query("UPDATE accounts SET key_check_tag = ?2 WHERE account_id = ?1")
                    .bind(&auth.account_id)
                    .bind(tag)
                    .execute(&mut *tx)
                    .await?;
            }
            Some(ref existing) if existing != tag => {
                return Err(DbApiError::BadRequest(
                    "keyCheckTag does not match existing sync space recovery key".into(),
                ));
            }
            Some(_) => {}
        }
    }

    sqlx::query(
        "UPDATE devices
         SET device_name = ?3, platform = ?4, app_version = ?5,
             allow_stats = ?6, allow_history = ?7, allow_edits = ?8, last_seen_at = ?9
         WHERE account_id = ?1 AND device_id = ?2",
    )
    .bind(&auth.account_id)
    .bind(&auth.device_id)
    .bind(name)
    .bind(req.platform.trim())
    .bind(req.app_version.trim())
    .bind(i64::from(req.allow_stats))
    .bind(i64::from(req.allow_history))
    .bind(i64::from(allow_edits))
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    if let Some(mask) = req.knowledge_scopes {
        if mask & !7 != 0 {
            return Err(DbApiError::BadRequest("unknown knowledge scope".into()));
        }
        sqlx::query(
            "INSERT INTO device_knowledge_scopes(account_id,device_id,mask) VALUES(?1,?2,?3)
            ON CONFLICT(account_id,device_id) DO UPDATE SET mask=excluded.mask",
        )
        .bind(&auth.account_id)
        .bind(&auth.device_id)
        .bind(i64::from(mask))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(DeviceInfo {
        knowledge_scopes: req.knowledge_scopes.unwrap_or(auth.knowledge_scopes),
        device_id: auth.device_id.clone(),
        device_name: name.to_string(),
        platform: req.platform.trim().to_string(),
        app_version: req.app_version.trim().to_string(),
        allow_stats: req.allow_stats,
        allow_history: req.allow_history,
        allow_edits,
        last_seen_at: now,
        revoked_at: None,
    })
}

pub async fn push_envelopes(
    db: &SqlitePool,
    auth: &AuthedDevice,
    req: &PushEnvelopesRequest,
) -> Result<(PushEnvelopesResponse, Vec<NotifyEvent>), DbApiError> {
    if req.envelopes.len() > MAX_PUSH_BATCH_LEN {
        return Err(DbApiError::BadRequest(format!(
            "batch exceeds maximum size of {MAX_PUSH_BATCH_LEN}"
        )));
    }

    // Pre-validate every item before taking the SQLite write lock.
    for sub in &req.envelopes {
        if sub.op_id.trim().is_empty()
            || sub.object_id.trim().is_empty()
            || sub.nonce.trim().is_empty()
            || sub.ciphertext.trim().is_empty()
        {
            return Err(DbApiError::BadRequest(
                "opId, objectId, nonce, and ciphertext must be non-empty".into(),
            ));
        }
        match sub.domain {
            SyncDomain::Dictionary | SyncDomain::Rules | SyncDomain::Prompts => {
                if auth.knowledge_scopes & sub.domain.knowledge_bit() == 0 {
                    return Err(DbApiError::ScopeForbidden(format!(
                        "{} sync is disabled",
                        sub.domain
                    )));
                }
            }
            SyncDomain::Stats => {
                if !auth.allow_stats {
                    return Err(DbApiError::ScopeForbidden(
                        "device is not permitted to push stats".into(),
                    ));
                }
                let Some((owner_dev, _shard_key)) = parse_stats_object_id(&sub.object_id) else {
                    return Err(DbApiError::BadRequest(format!(
                        "invalid stats objectId: {}",
                        sub.object_id
                    )));
                };
                if owner_dev != auth.device_id {
                    return Err(DbApiError::ScopeForbidden(
                        "device can only push stats shards for its own deviceId".into(),
                    ));
                }
                if sub.revision <= 0 {
                    return Err(DbApiError::BadRequest(
                        "stats shard revision must be > 0".into(),
                    ));
                }
            }
            SyncDomain::History => {
                if !auth.allow_history && !sub.op_id.starts_with("delete:") {
                    return Err(DbApiError::ScopeForbidden(
                        "device is not permitted to push history".into(),
                    ));
                }
            }
            SyncDomain::Edits => {
                if !auth.allow_edits {
                    return Err(DbApiError::ScopeForbidden(
                        "device is not permitted to push edits".into(),
                    ));
                }
            }
        }
    }

    let now = Utc::now().to_rfc3339();
    let mut tx = db.begin().await?;

    let acc_row = sqlx::query(
        "SELECT next_seq, storage_quota_bytes, used_bytes
         FROM accounts WHERE account_id = ?1",
    )
    .bind(&auth.account_id)
    .fetch_one(&mut *tx)
    .await?;

    let mut next_seq: i64 = acc_row.get(0);
    let quota_bytes: i64 = acc_row.get(1);
    let mut used_bytes: i64 = acc_row.get(2);

    let mut results = Vec::with_capacity(req.envelopes.len());
    let mut notifications = Vec::new();

    for sub in &req.envelopes {
        // 1. Idempotent dedup by (account_id, op_id)
        if let Some(existing_seq) = sqlx::query_scalar::<_, i64>(
            "SELECT seq FROM envelopes WHERE account_id = ?1 AND op_id = ?2",
        )
        .bind(&auth.account_id)
        .bind(&sub.op_id)
        .fetch_optional(&mut *tx)
        .await?
        {
            results.push(EnvelopeResult {
                op_id: sub.op_id.clone(),
                status: EnvelopeStatus::Duplicate,
                seq: existing_seq,
            });
            continue;
        }

        let new_bytes = (sub.nonce.len() + sub.ciphertext.len()) as i64;

        if sub.domain == SyncDomain::Stats {
            let (_, shard_key) =
                parse_stats_object_id(&sub.object_id).expect("validated stats objectId");

            let current_shard = sqlx::query(
                "SELECT revision, seq, byte_size FROM stats_shards
                 WHERE account_id = ?1 AND device_id = ?2 AND shard_key = ?3",
            )
            .bind(&auth.account_id)
            .bind(&auth.device_id)
            .bind(shard_key)
            .fetch_optional(&mut *tx)
            .await?;

            let mut reclaimed_bytes: i64 = 0;
            let mut prev_seq: Option<i64> = None;
            if let Some(row) = current_shard {
                let current_rev: i64 = row.get(0);
                let current_seq: i64 = row.get(1);
                let old_bytes: i64 = row.get(2);
                if sub.revision <= current_rev {
                    results.push(EnvelopeResult {
                        op_id: sub.op_id.clone(),
                        status: EnvelopeStatus::StaleIgnored,
                        seq: current_seq,
                    });
                    continue;
                }
                reclaimed_bytes = old_bytes;
                prev_seq = Some(current_seq);
            }

            let projected_used = (used_bytes - reclaimed_bytes).max(0) + new_bytes;
            if projected_used > quota_bytes {
                return Err(DbApiError::QuotaExceeded);
            }

            if let Some(old_seq) = prev_seq {
                // C5: Clear superseded stats ciphertext/nonce in the same transaction so
                // SQLite reclaims storage and `SUM(byte_size)` matches `accounts.used_bytes`,
                // while keeping `op_id` + `seq` intact for idempotent duplicate checks.
                sqlx::query(
                    "UPDATE envelopes
                     SET superseded = 1, nonce = '', ciphertext = '', byte_size = 0
                     WHERE account_id = ?1 AND seq = ?2",
                )
                .bind(&auth.account_id)
                .bind(old_seq)
                .execute(&mut *tx)
                .await?;
            }

            next_seq += 1;
            used_bytes = projected_used;

            sqlx::query(
                "INSERT INTO envelopes (
                    account_id, seq, op_id, domain, device_id, object_id, revision,
                    nonce, ciphertext, byte_size, superseded, created_at
                 ) VALUES (?1, ?2, ?3, 'stats', ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)",
            )
            .bind(&auth.account_id)
            .bind(next_seq)
            .bind(&sub.op_id)
            .bind(&auth.device_id)
            .bind(&sub.object_id)
            .bind(sub.revision)
            .bind(&sub.nonce)
            .bind(&sub.ciphertext)
            .bind(new_bytes)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO stats_shards (
                    account_id, device_id, shard_key, revision, op_id, seq,
                    nonce, ciphertext, byte_size, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(account_id, device_id, shard_key) DO UPDATE SET
                    revision = excluded.revision,
                    op_id = excluded.op_id,
                    seq = excluded.seq,
                    nonce = excluded.nonce,
                    ciphertext = excluded.ciphertext,
                    byte_size = excluded.byte_size,
                    updated_at = excluded.updated_at",
            )
            .bind(&auth.account_id)
            .bind(&auth.device_id)
            .bind(shard_key)
            .bind(sub.revision)
            .bind(&sub.op_id)
            .bind(next_seq)
            .bind(&sub.nonce)
            .bind(&sub.ciphertext)
            .bind(new_bytes)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            results.push(EnvelopeResult {
                op_id: sub.op_id.clone(),
                status: EnvelopeStatus::Accepted,
                seq: next_seq,
            });
            notifications.push(NotifyEvent {
                domain: SyncDomain::Stats,
                seq: next_seq,
            });
            continue;
        }

        // History / Edits domains
        if used_bytes + new_bytes > quota_bytes {
            return Err(DbApiError::QuotaExceeded);
        }

        next_seq += 1;
        used_bytes += new_bytes;

        sqlx::query(
            "INSERT INTO envelopes (
                account_id, seq, op_id, domain, device_id, object_id, revision,
                nonce, ciphertext, byte_size, superseded, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, 0, ?10)",
        )
        .bind(&auth.account_id)
        .bind(next_seq)
        .bind(&sub.op_id)
        .bind(sub.domain.as_str())
        .bind(&auth.device_id)
        .bind(&sub.object_id)
        .bind(&sub.nonce)
        .bind(&sub.ciphertext)
        .bind(new_bytes)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        results.push(EnvelopeResult {
            op_id: sub.op_id.clone(),
            status: EnvelopeStatus::Accepted,
            seq: next_seq,
        });
        notifications.push(NotifyEvent {
            domain: sub.domain,
            seq: next_seq,
        });
    }

    sqlx::query("UPDATE accounts SET next_seq = ?2, used_bytes = ?3 WHERE account_id = ?1")
        .bind(&auth.account_id)
        .bind(next_seq)
        .bind(used_bytes)
        .execute(&mut *tx)
        .await?;

    sqlx::query("UPDATE devices SET last_seen_at = ?3 WHERE account_id = ?1 AND device_id = ?2")
        .bind(&auth.account_id)
        .bind(&auth.device_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok((
        PushEnvelopesResponse {
            results,
            latest_seq: next_seq,
        },
        notifications,
    ))
}

pub async fn pull_envelopes(
    db: &SqlitePool,
    account_id: &str,
    domain: SyncDomain,
    since: i64,
    limit: Option<usize>,
) -> Result<PullEnvelopesResponse, DbApiError> {
    let since = since.max(0);
    let page_limit = limit
        .unwrap_or(DEFAULT_PULL_LIMIT)
        .clamp(1, MAX_PULL_LIMIT);
    let fetch_limit = (page_limit + 1) as i64;

    let mut tx = db.begin().await?;
    let latest_seq: i64 = sqlx::query_scalar("SELECT next_seq FROM accounts WHERE account_id = ?1")
        .bind(account_id)
        .fetch_one(&mut *tx)
        .await?;

    let rows = sqlx::query(
        "SELECT seq, op_id, domain, device_id, object_id, revision, nonce, ciphertext, created_at
         FROM envelopes
         WHERE account_id = ?1 AND domain = ?2 AND superseded = 0 AND seq > ?3
         ORDER BY seq ASC
         LIMIT ?4",
    )
    .bind(account_id)
    .bind(domain.as_str())
    .bind(since)
    .bind(fetch_limit)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let has_more = rows.len() > page_limit;
    let mut items = Vec::with_capacity(rows.len().min(page_limit));
    for r in rows.into_iter().take(page_limit) {
        let domain_str: String = r.get(2);
        let parsed_domain = SyncDomain::from_str(&domain_str).unwrap_or(domain);
        items.push(EnvelopeItem {
            seq: r.get(0),
            op_id: r.get(1),
            domain: parsed_domain,
            device_id: r.get(3),
            object_id: r.get(4),
            revision: r.get(5),
            nonce: r.get(6),
            ciphertext: r.get(7),
            created_at: r.get(8),
        });
    }

    let next_cursor = if has_more {
        items.last().map(|i| i.seq).unwrap_or(since)
    } else {
        items
            .last()
            .map(|i| i.seq)
            .unwrap_or(since)
            .max(latest_seq)
    };

    Ok(PullEnvelopesResponse {
        items,
        next_cursor,
        has_more,
    })
}
