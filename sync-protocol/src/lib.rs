//! Shared wire DTOs, domain types, error codes, and canonical AAD construction
//! for VoiceX Sync v2 (shared by `src-tauri` and `sync-server`).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Protocol version tag bound into every AEAD Associated Data (AAD).
pub const PROTOCOL_VERSION: &str = "vx2";

/// Maximum envelopes accepted in a single `POST /v2/envelopes` batch.
pub const MAX_PUSH_BATCH_LEN: usize = 100;

/// Maximum serialized HTTP request body size (2 MiB) for `POST /v2/envelopes`.
pub const MAX_PUSH_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Default and maximum page sizes for `GET /v2/envelopes`.
pub const DEFAULT_PULL_LIMIT: usize = 100;
pub const MAX_PULL_LIMIT: usize = 200;

/// Sync data domains with independent pull cursors and device scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDomain {
    Stats,
    History,
    Edits,
    Dictionary,
    Rules,
    Prompts,
}

impl SyncDomain {
    pub const KNOWLEDGE: [Self; 3] = [Self::Dictionary, Self::Rules, Self::Prompts];
    pub const ALL: [Self; 6] = [
        Self::Stats,
        Self::History,
        Self::Edits,
        Self::Dictionary,
        Self::Rules,
        Self::Prompts,
    ];

    pub const fn knowledge_bit(self) -> u8 {
        match self {
            Self::Dictionary => 1,
            Self::Rules => 2,
            Self::Prompts => 4,
            _ => 0,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            SyncDomain::Stats => "stats",
            SyncDomain::History => "history",
            SyncDomain::Edits => "edits",
            Self::Dictionary => "dictionary",
            Self::Rules => "rules",
            Self::Prompts => "prompts",
        }
    }
}

impl fmt::Display for SyncDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SyncDomain {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stats" => Ok(SyncDomain::Stats),
            "history" => Ok(SyncDomain::History),
            "edits" => Ok(SyncDomain::Edits),
            "dictionary" => Ok(Self::Dictionary),
            "rules" => Ok(Self::Rules),
            "prompts" => Ok(Self::Prompts),
            other => Err(format!("unknown sync domain: {other}")),
        }
    }
}

/// Distinguishes uninvoked LLM, known model name, and legacy invoked-without-name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmUsageState {
    None,
    Known,
    Unknown,
}

impl LlmUsageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            LlmUsageState::None => "none",
            LlmUsageState::Known => "known",
            LlmUsageState::Unknown => "unknown",
        }
    }

    pub fn from_record_fields(llm_invoked: bool, llm_model_name: Option<&str>) -> (Self, String) {
        if !llm_invoked {
            return (LlmUsageState::None, String::new());
        }
        match llm_model_name.map(str::trim).filter(|s| !s.is_empty()) {
            Some(model) => (LlmUsageState::Known, model.to_string()),
            None => (LlmUsageState::Unknown, String::new()),
        }
    }
}

impl fmt::Display for LlmUsageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LlmUsageState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(LlmUsageState::None),
            "known" => Ok(LlmUsageState::Known),
            "unknown" => Ok(LlmUsageState::Unknown),
            other => Err(format!("unknown llm usage state: {other}")),
        }
    }
}

/// Authoritative edit version kind for `history.edit` operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
    Observed,
    Manual,
    ManualRestore,
}

impl EditKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            EditKind::Observed => "observed",
            EditKind::Manual => "manual",
            EditKind::ManualRestore => "manual_restore",
        }
    }

    /// Manual edits and explicit manual restores outrank automatic observations.
    pub const fn is_manual(self) -> bool {
        matches!(self, EditKind::Manual | EditKind::ManualRestore)
    }
}

impl fmt::Display for EditKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EditKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "observed" => Ok(EditKind::Observed),
            "manual" => Ok(EditKind::Manual),
            "manual_restore" => Ok(EditKind::ManualRestore),
            other => Err(format!("unknown edit kind: {other}")),
        }
    }
}

/// Per-envelope result returned by `POST /v2/envelopes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeStatus {
    Accepted,
    Duplicate,
    StaleIgnored,
}

/// Stable machine-readable API error codes.
pub mod error_codes {
    pub const BAD_REQUEST: &str = "bad_request";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const DEVICE_REVOKED: &str = "device_revoked";
    pub const SCOPE_FORBIDDEN: &str = "scope_forbidden";
    pub const PAIRING_INVALID_OR_EXPIRED: &str = "pairing_invalid_or_expired";
    pub const DEVICE_ID_CONFLICT: &str = "device_id_conflict";
    pub const PAYLOAD_TOO_LARGE: &str = "payload_too_large";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const QUOTA_EXCEEDED: &str = "quota_exceeded";
    pub const INTERNAL_ERROR: &str = "internal_error";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRedeemRequest {
    pub pairing_code: String,
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
    pub app_version: String,
    /// Installation secret scoped to this server and device, persisted before redeem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_proof: Option<String>,
    /// Authorizes enrollment of an existing device created before installation proofs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_device_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRedeemResponse {
    pub account_id: String,
    pub server_id: String,
    pub device_id: String,
    pub device_token: String,
    pub key_check_tag: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePairingCodeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_minutes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePairingCodeResponse {
    pub pairing_code: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    #[serde(default)]
    pub knowledge_scopes: u8,
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
    pub app_version: String,
    pub allow_stats: bool,
    pub allow_history: bool,
    pub allow_edits: bool,
    pub last_seen_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfoResponse {
    pub account_id: String,
    pub server_id: String,
    pub latest_seq: i64,
    pub key_check_tag: Option<String>,
    pub devices: Vec<DeviceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceUpdateRequest {
    /// Omitted by older clients; preserve the current device grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_scopes: Option<u8>,
    pub device_name: String,
    pub platform: String,
    pub app_version: String,
    pub allow_stats: bool,
    pub allow_history: bool,
    pub allow_edits: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_check_tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvelopeSubmission {
    pub op_id: String,
    pub domain: SyncDomain,
    pub object_id: String,
    /// Monotonic revision (> 0) for `SyncDomain::Stats`; 0 for history/edits.
    #[serde(default)]
    pub revision: i64,
    /// Base64-encoded 24-byte random XChaCha20-Poly1305 nonce.
    pub nonce: String,
    /// Base64-encoded AEAD ciphertext (including 16-byte tag).
    pub ciphertext: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushEnvelopesRequest {
    pub envelopes: Vec<EnvelopeSubmission>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvelopeResult {
    pub op_id: String,
    pub status: EnvelopeStatus,
    pub seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushEnvelopesResponse {
    pub results: Vec<EnvelopeResult>,
    pub latest_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvelopeItem {
    pub seq: i64,
    pub op_id: String,
    pub domain: SyncDomain,
    pub device_id: String,
    pub object_id: String,
    pub revision: i64,
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullEnvelopesResponse {
    pub items: Vec<EnvelopeItem>,
    pub next_cursor: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyEvent {
    pub domain: SyncDomain,
    pub seq: i64,
}

/// Format a sync space identifier for a self-hosted v2 server.
pub fn selfhost_space_id(server_id: &str, account_id: &str) -> String {
    format!("selfhost:{server_id}:{account_id}")
}

/// Format `object_id` for a stats shard belonging to `device_id`.
pub fn stats_object_id(device_id: &str, shard_key: &str) -> String {
    format!("{device_id}:{shard_key}")
}

/// Parse a stats `object_id` (`"{device_id}:{shard_key}"`) and validate the shard key.
pub fn parse_stats_object_id(object_id: &str) -> Option<(&str, &str)> {
    let (device_id, shard_key) = object_id.split_once(':')?;
    if device_id.is_empty() || !is_valid_shard_key(shard_key) {
        return None;
    }
    Some((device_id, shard_key))
}

/// Validate a stats shard key: either `"totals"` or `"daily:YYYY-MM"`.
pub fn is_valid_shard_key(shard_key: &str) -> bool {
    if shard_key == "totals" {
        return true;
    }
    let Some(ym) = shard_key.strip_prefix("daily:") else {
        return false;
    };
    let bytes = ym.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return false;
    }
    let year_ok = bytes[0..4].iter().all(|b| b.is_ascii_digit());
    let month_ok = bytes[5..7].iter().all(|b| b.is_ascii_digit());
    if !year_ok || !month_ok {
        return false;
    }
    let month: u32 = ym[5..7].parse().unwrap_or(0);
    (1..=12).contains(&month)
}

/// Build canonical length-prefixed AAD binding every routing and version field.
///
/// Format: repeated `u32_be(field.len()) || field.as_bytes()` for:
/// `[PROTOCOL_VERSION, account_id, space_id, domain.as_str(), object_id, rev_or_op_id]`.
pub fn build_envelope_aad(
    account_id: &str,
    space_id: &str,
    domain: SyncDomain,
    object_id: &str,
    rev_or_op_id: &str,
) -> Vec<u8> {
    let fields = [
        PROTOCOL_VERSION,
        account_id,
        space_id,
        domain.as_str(),
        object_id,
        rev_or_op_id,
    ];
    let total_len: usize = fields.iter().map(|f| 4 + f.len()).sum();
    let mut out = Vec::with_capacity(total_len);
    for field in fields {
        let len = u32::try_from(field.len()).expect("AAD field fits in u32");
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(field.as_bytes());
    }
    out
}

/// Canonical `rev_or_op_id` binding value used in AAD.
pub fn aad_rev_or_op_id(domain: SyncDomain, revision: i64, op_id: &str) -> String {
    match domain {
        SyncDomain::Stats => revision.to_string(),
        _ => op_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_domain_round_trip() {
        for domain in SyncDomain::ALL {
            let s = domain.as_str();
            assert_eq!(SyncDomain::from_str(s).unwrap(), domain);
            let json = serde_json::to_string(&domain).unwrap();
            let parsed: SyncDomain = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, domain);
        }
        assert!(SyncDomain::from_str("unknown").is_err());
    }

    #[test]
    fn llm_usage_state_from_record_fields_distinguishes_all_three() {
        assert_eq!(
            LlmUsageState::from_record_fields(false, Some("DeepSeek")),
            (LlmUsageState::None, String::new())
        );
        assert_eq!(
            LlmUsageState::from_record_fields(true, Some("  DeepSeek / flash  ")),
            (LlmUsageState::Known, "DeepSeek / flash".to_string())
        );
        assert_eq!(
            LlmUsageState::from_record_fields(true, None),
            (LlmUsageState::Unknown, String::new())
        );
        assert_eq!(
            LlmUsageState::from_record_fields(true, Some("   ")),
            (LlmUsageState::Unknown, String::new())
        );
    }

    #[test]
    fn shard_key_validation() {
        assert!(is_valid_shard_key("totals"));
        assert!(is_valid_shard_key("daily:2026-01"));
        assert!(is_valid_shard_key("daily:2026-12"));
        assert!(!is_valid_shard_key("daily:2026-00"));
        assert!(!is_valid_shard_key("daily:2026-13"));
        assert!(!is_valid_shard_key("daily:2026-1"));
        assert!(!is_valid_shard_key("daily:2026-11-24"));
        assert!(!is_valid_shard_key("other"));

        assert_eq!(
            parse_stats_object_id("dev-1:totals"),
            Some(("dev-1", "totals"))
        );
        assert_eq!(
            parse_stats_object_id("dev-1:daily:2026-11"),
            Some(("dev-1", "daily:2026-11"))
        );
        assert_eq!(parse_stats_object_id(":totals"), None);
        assert_eq!(parse_stats_object_id("dev-1:daily:2026-99"), None);
    }

    #[test]
    fn aad_has_no_delimiter_ambiguity() {
        let a = build_envelope_aad("acc:1", "space", SyncDomain::History, "obj", "op");
        let b = build_envelope_aad("acc", "1:space", SyncDomain::History, "obj", "op");
        assert_ne!(a, b);

        let c = build_envelope_aad("acc", "space", SyncDomain::Stats, "dev:totals", "10");
        let d = build_envelope_aad("acc", "space", SyncDomain::Stats, "dev:totals", "11");
        assert_ne!(c, d);
    }

    #[test]
    fn envelope_dto_json_camel_case() {
        let sub = EnvelopeSubmission {
            op_id: "op-1".into(),
            domain: SyncDomain::Stats,
            object_id: "dev-1:totals".into(),
            revision: 7,
            nonce: "AAAA".into(),
            ciphertext: "BBBB".into(),
        };
        let v = serde_json::to_value(&sub).unwrap();
        assert_eq!(v["opId"], "op-1");
        assert_eq!(v["domain"], "stats");
        assert_eq!(v["objectId"], "dev-1:totals");
        assert_eq!(v["revision"], 7);
    }
}
