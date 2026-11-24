//! Single authoritative sync scope policy (design doc Section 2.1).
//!
//! Real-time dictation persistence, initial import, post-edit updates, and
//! pre-send outbox filtering MUST all consult this module so private content
//! can never leak across scope boundaries.

use crate::services::history_service::{HISTORY_ERROR_NONE, HISTORY_MODE_TRANSLATE_READ};
use voicex_sync_protocol::SyncDomain;

/// Per-device sync scope settings for the active sync space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncScopeConfig {
    pub enabled: bool,
    /// When false, this device operates in "stats-only" mode: never pushes or
    /// pulls dictation history text or edits.
    pub sync_history: bool,
    /// Independent toggle for sharing post-edits (manual, observed, restore).
    /// Effective only when `sync_history` is also enabled.
    pub sync_edits: bool,
}

impl SyncScopeConfig {
    pub const fn stats_only() -> Self {
        Self {
            enabled: true,
            sync_history: false,
            sync_edits: false,
        }
    }

    pub const fn history_without_edits() -> Self {
        Self {
            enabled: true,
            sync_history: true,
            sync_edits: false,
        }
    }

    pub const fn history_and_edits() -> Self {
        Self {
            enabled: true,
            sync_history: true,
            sync_edits: true,
        }
    }

    pub const fn effective_history(&self) -> bool {
        self.enabled && self.sync_history
    }

    pub const fn effective_edits(&self) -> bool {
        self.enabled && self.sync_history && self.sync_edits
    }

    /// Domains this device is allowed to pull from the sync backend.
    pub fn pull_domains(&self) -> Vec<SyncDomain> {
        if !self.enabled {
            return Vec::new();
        }
        let mut domains = vec![SyncDomain::Stats];
        if self.effective_history() {
            domains.push(SyncDomain::History);
        }
        if self.effective_edits() {
            domains.push(SyncDomain::Edits);
        }
        domains
    }

    /// Domains whose pending outbox entries must be cancelled when narrowing
    /// from `previous` to `self`.
    pub fn domains_to_cancel_on_narrow(&self, previous: &Self) -> Vec<SyncDomain> {
        let mut cancel = Vec::new();
        if previous.effective_history() && !self.effective_history() {
            cancel.push(SyncDomain::History);
        }
        if previous.effective_edits() && !self.effective_edits() {
            cancel.push(SyncDomain::Edits);
        }
        cancel
    }
}

/// Intrinsic record attributes evaluated by the scope policy.
#[derive(Debug, Clone, Copy)]
pub struct RecordEligibility<'a> {
    pub mode: &'a str,
    pub error_code: i32,
    pub is_final: bool,
}

/// Whether a dictation row is ever eligible to leave this machine (independent
/// of user toggles). Failed records, non-final records, and translate-and-read
/// records are strictly local-only.
pub fn is_record_inherently_shareable(record: RecordEligibility<'_>) -> bool {
    record.is_final
        && record.error_code == HISTORY_ERROR_NONE
        && record.mode != HISTORY_MODE_TRANSLATE_READ
}

/// Whether a `history.create` operation may be enqueued or sent under `scope`.
pub fn can_share_history_create(scope: &SyncScopeConfig, record: RecordEligibility<'_>) -> bool {
    scope.effective_history() && is_record_inherently_shareable(record)
}

/// Whether a `history.edit` operation may be enqueued or sent under `scope`.
///
/// Requires both `sync_edits` enabled AND the underlying record already being
/// shared (`record_is_shared`) so edits to private/unshared rows never leak.
pub fn can_share_history_edit(
    scope: &SyncScopeConfig,
    record: RecordEligibility<'_>,
    record_is_shared: bool,
) -> bool {
    scope.effective_edits() && record_is_shared && is_record_inherently_shareable(record)
}

/// Whether a `history.delete` tombstone may be sent ("Delete from all devices").
///
/// Allowed whenever sync is enabled and the record was previously shared — even
/// if the user later narrowed this device back to stats-only (Section 2.1).
pub fn can_send_history_delete(scope: &SyncScopeConfig, record_was_shared: bool) -> bool {
    scope.enabled && record_was_shared
}

/// Pre-send gate executed immediately before encrypting/uploading an outbox op.
pub fn can_send_outbox_op(
    scope: &SyncScopeConfig,
    op_type: &str,
    record_was_shared: bool,
) -> bool {
    if !scope.enabled {
        return false;
    }
    match op_type {
        "history.create" => scope.effective_history(),
        "history.edit" => scope.effective_edits() && record_was_shared,
        "history.delete" => record_was_shared,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_modes_and_failures_never_leave_machine() {
        let full = SyncScopeConfig::history_and_edits();

        assert!(can_share_history_create(
            &full,
            RecordEligibility {
                mode: "assistant_hands_free",
                error_code: HISTORY_ERROR_NONE,
                is_final: true,
            }
        ));
        assert!(can_share_history_create(
            &full,
            RecordEligibility {
                mode: "translate_en_hands_free",
                error_code: HISTORY_ERROR_NONE,
                is_final: true,
            }
        ));
        assert!(!can_share_history_create(
            &full,
            RecordEligibility {
                mode: HISTORY_MODE_TRANSLATE_READ,
                error_code: HISTORY_ERROR_NONE,
                is_final: true,
            }
        ));
        assert!(!can_share_history_create(
            &full,
            RecordEligibility {
                mode: "assistant_hands_free",
                error_code: 1,
                is_final: true,
            }
        ));
        assert!(!can_share_history_create(
            &full,
            RecordEligibility {
                mode: "assistant_hands_free",
                error_code: HISTORY_ERROR_NONE,
                is_final: false,
            }
        ));
    }

    #[test]
    fn stats_only_blocks_history_and_edits_but_allows_deleting_previously_shared() {
        let stats_only = SyncScopeConfig::stats_only();
        let rec = RecordEligibility {
            mode: "assistant_hands_free",
            error_code: HISTORY_ERROR_NONE,
            is_final: true,
        };

        assert_eq!(stats_only.pull_domains(), vec![SyncDomain::Stats]);
        assert!(!can_share_history_create(&stats_only, rec));
        assert!(!can_share_history_edit(&stats_only, rec, true));
        assert!(!can_send_outbox_op(&stats_only, "history.create", true));
        assert!(!can_send_outbox_op(&stats_only, "history.edit", true));
        assert!(can_send_history_delete(&stats_only, true));
        assert!(!can_send_history_delete(&stats_only, false));
        assert!(can_send_outbox_op(&stats_only, "history.delete", true));
    }

    #[test]
    fn narrowing_scope_identifies_exact_domains_to_cancel() {
        let full = SyncScopeConfig::history_and_edits();
        let history_only = SyncScopeConfig::history_without_edits();
        let stats_only = SyncScopeConfig::stats_only();

        assert_eq!(
            history_only.domains_to_cancel_on_narrow(&full),
            vec![SyncDomain::Edits]
        );
        assert_eq!(
            stats_only.domains_to_cancel_on_narrow(&full),
            vec![SyncDomain::History, SyncDomain::Edits]
        );
        assert!(full.domains_to_cancel_on_narrow(&stats_only).is_empty());
    }

    #[test]
    fn edits_require_both_toggle_and_shared_base_record() {
        let full = SyncScopeConfig::history_and_edits();
        let no_edits = SyncScopeConfig::history_without_edits();
        let rec = RecordEligibility {
            mode: "assistant_hands_free",
            error_code: HISTORY_ERROR_NONE,
            is_final: true,
        };

        assert!(can_share_history_edit(&full, rec, true));
        assert!(!can_share_history_edit(&full, rec, false));
        assert!(!can_share_history_edit(&no_edits, rec, true));
    }
}
