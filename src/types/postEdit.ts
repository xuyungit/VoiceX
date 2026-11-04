export type PostEditTestStatus =
  | 'disabled' | 'ready' | 'verifying' | 'observing' | 'observed_unchanged'
  | 'observed_edited' | 'partial' | 'ambiguous' | 'baseline_unavailable'
  | 'skipped' | 'read_failed'

export type PostEditTestReason =
  | 'none' | 'mode_excluded' | 'remote_target_unsupported' | 'target_unavailable'
  | 'secure_input' | 'security_unknown' | 'unsupported_control' | 'permission_denied'
  | 'target_changed' | 'no_value' | 'too_large' | 'read_timeout' | 'native_error'
  | 'pre_read_unavailable' | 'baseline_transformed' | 'dispatch_failed' | 'interrupted'
  | 'field_cleared' | 'range_ambiguous' | 'budget_exceeded' | 'timeout'
  | 'focus_left' | 'next_recording' | 'next_injection' | 'element_destroyed'
  | 'session_cancelled' | 'composition_active'

export interface PostEditTestSnapshot {
  enabled: boolean
  status: PostEditTestStatus
  reason: PostEditTestReason
  nativeCode: number | null
  insertions: number | null
  deletions: number | null
  substitutions: number | null
  strategy: 'event_with_checkpoints' | 'checkpoint_only' | null
  rangeEvidence: 'before_after_verified' | 'post_anchored' | null
  lastObservedMs: number | null
  hadIntermediateChanges: boolean
}

export type PostEditMode = 'off' | 'metrics_only' | 'records'

export interface PostEditSettings {
  mode: PostEditMode
}

export interface PostEditRecord {
  id: string
  recordId: string
  sessionId: string
  timestamp: string
  lastObservedAt: string
  appName: string | null
  originalText: string
  editedText: string
  complete: boolean
  endReason: PostEditTestReason
  strategy: PostEditTestSnapshot['strategy']
  rangeEvidence: PostEditTestSnapshot['rangeEvidence']
  insertions: number | null
  deletions: number | null
  substitutions: number | null
  hadIntermediateChanges: boolean
  stable: boolean
  composition: 'unknown' | 'inactive' | 'active'
}

export interface PostEditRecordPage {
  records: PostEditRecord[]
  saveError: string | null
}
