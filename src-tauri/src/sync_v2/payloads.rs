//! Plaintext payload schemas encrypted inside Sync v2 envelopes.
//!
//! These structs are serialized to JSON locally and encrypted before leaving the
//! device; the sync server never sees their fields.

use serde::{Deserialize, Serialize};
use voicex_sync_protocol::{EditKind, LlmUsageState};

pub const PAYLOAD_SCHEMA_V1: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "opType")]
pub enum HistoryOpPayload {
    #[serde(rename = "history.create")]
    Create(HistoryCreatePayload),
    #[serde(rename = "history.edit")]
    Edit(HistoryEditPayload),
    #[serde(rename = "history.delete")]
    Delete(HistoryDeletePayload),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCreatePayload {
    pub schema: u32,
    pub record_id: String,
    pub source_device_id: String,
    pub timestamp: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_text: Option<String>,
    pub ai_correction_applied: bool,
    pub llm_invoked: bool,
    pub mode: String,
    pub duration_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asr_model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_app_platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_app_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_app_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditMetadataPayload {
    #[serde(default)]
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insertions: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub substitutions: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEditPayload {
    pub schema: u32,
    pub edit_id: String,
    pub record_id: String,
    pub source_device_id: String,
    pub edit_kind: EditKind,
    #[serde(default)]
    pub parent_edit_ids: Vec<String>,
    pub hlc: String,
    pub edited_at: String,
    #[serde(default)]
    pub final_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<EditMetadataPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDeletePayload {
    pub schema: u32,
    pub record_id: String,
    pub source_device_id: String,
    pub hlc: String,
    pub deleted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shardKind")]
pub enum StatsShardPayload {
    #[serde(rename = "totals")]
    Totals(StatsTotalsPayload),
    #[serde(rename = "daily_month")]
    DailyMonth(StatsDailyMonthPayload),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationTotalsPayload {
    pub recording_count: i64,
    pub total_characters: i64,
    pub total_duration_ms: i64,
    pub llm_invoked_count: i64,
    pub ai_correction_count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TtsTotalsPayload {
    pub read_count: i64,
    pub translate_count: i64,
    pub characters: i64,
    pub duration_ms: i64,
    pub llm_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsTotalsPayload {
    pub schema: u32,
    pub device_id: String,
    pub revision: i64,
    pub updated_at: String,
    pub dictation: DictationTotalsPayload,
    pub tts: TtsTotalsPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyAppStatPayload {
    pub local_date: String,
    pub feature_kind: String,
    pub platform: String,
    pub app_key: String,
    pub app_name: String,
    pub usage_count: i64,
    pub total_characters: i64,
    pub total_duration_ms: i64,
    pub llm_count: i64,
    pub last_used_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyModelStatPayload {
    pub local_date: String,
    pub asr_model_name: String,
    pub llm_state: LlmUsageState,
    pub llm_model_name: String,
    pub usage_count: i64,
    pub total_characters: i64,
    pub total_duration_ms: i64,
    pub ai_correction_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsDailyMonthPayload {
    pub schema: u32,
    pub device_id: String,
    pub month: String,
    pub revision: i64,
    pub updated_at: String,
    pub app_rows: Vec<DailyAppStatPayload>,
    pub model_rows: Vec<DailyModelStatPayload>,
}
