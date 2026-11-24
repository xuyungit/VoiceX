//! Deterministic, order-independent merge rules for Sync v2:
//! - History edit DAG resolution (manual overrides observed, manual_restore blocks
//!   late observations, concurrent manual edits produce recoverable alternatives)
//! - Stats shard revision monotonicity checks

use std::collections::{HashMap, HashSet};
use voicex_sync_protocol::EditKind;

/// Minimal edit version node required to deterministically compute display text
/// and concurrent conflict branches for a single `record_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditVersionNode {
    pub edit_id: String,
    pub source_device_id: String,
    pub edit_kind: EditKind,
    pub parent_edit_ids: Vec<String>,
    /// Fixed-width HLC wire string (`"{wall_ms:016}-{counter:06}-{device_id}"`).
    pub hlc: String,
    pub edited_at: String,
    /// `None` for `EditKind::ManualRestore` (restored to VoiceX output).
    pub final_text: Option<String>,
}

/// Deterministic merge result computed from all known `EditVersionNode`s of a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEditState {
    /// Winning version ID (present whenever at least one edit version exists,
    /// including `ManualRestore` which sets `displayed_edited_text` to `None`).
    pub active_edit_id: Option<String>,
    /// Winning `EditKind` (used for `history_record.edit_source` projection).
    pub active_edit_kind: Option<EditKind>,
    /// Final displayed edited text; `None` means show unedited VoiceX output.
    pub displayed_edited_text: Option<String>,
    pub displayed_edited_at: Option<String>,
    /// Active head edit IDs that a subsequent manual edit should cite as
    /// `parent_edit_ids` (all manual heads when manual versions exist, else
    /// the active observed edit if any).
    pub next_parent_ids: Vec<String>,
    /// Recoverable concurrent manual branches whose final text differs from
    /// the displayed winner, ordered by descending HLC.
    pub conflict_alternatives: Vec<EditVersionNode>,
}

impl ResolvedEditState {
    pub fn has_conflict(&self) -> bool {
        !self.conflict_alternatives.is_empty()
    }
}

/// Deterministically resolve all edit versions for a single dictation record.
///
/// Order-independent: shuffling `versions` always yields the exact same result.
pub fn resolve_edits(versions: &[EditVersionNode]) -> ResolvedEditState {
    if versions.is_empty() {
        return ResolvedEditState {
            active_edit_id: None,
            active_edit_kind: None,
            displayed_edited_text: None,
            displayed_edited_at: None,
            next_parent_ids: Vec::new(),
            conflict_alternatives: Vec::new(),
        };
    }

    // Deduplicate by `edit_id` so replays never double-count.
    let mut by_id: HashMap<&str, &EditVersionNode> = HashMap::with_capacity(versions.len());
    for v in versions {
        by_id.entry(v.edit_id.as_str()).or_insert(v);
    }

    let manual_nodes: Vec<&EditVersionNode> = by_id
        .values()
        .copied()
        .filter(|v| v.edit_kind.is_manual())
        .collect();

    if manual_nodes.is_empty() {
        // Observed-only: highest HLC (tie-broken by edit_id) wins; no conflict UI.
        let winner = by_id
            .values()
            .copied()
            .max_by(|a, b| compare_nodes(a, b))
            .expect("non-empty versions");
        return ResolvedEditState {
            active_edit_id: Some(winner.edit_id.clone()),
            active_edit_kind: Some(winner.edit_kind),
            displayed_edited_text: effective_final_text(winner),
            displayed_edited_at: Some(winner.edited_at.clone()),
            next_parent_ids: vec![winner.edit_id.clone()],
            conflict_alternatives: Vec::new(),
        };
    }

    // Collect every edit_id referenced as a parent by another manual version.
    // Any manual node referenced as a parent has a successor and is no longer a head.
    let mut superseded: HashSet<&str> = HashSet::new();
    for node in &manual_nodes {
        for parent_id in &node.parent_edit_ids {
            superseded.insert(parent_id.as_str());
        }
    }

    let mut heads: Vec<&EditVersionNode> = manual_nodes
        .iter()
        .copied()
        .filter(|node| !superseded.contains(node.edit_id.as_str()))
        .collect();

    // Defensive guard against malformed cyclic parent graphs: fall back to all manual nodes.
    if heads.is_empty() {
        heads = manual_nodes;
    }

    // Sort heads descending by HLC (deterministic tie-break on edit_id).
    heads.sort_by(|a, b| compare_nodes(b, a));

    let winner = heads[0];
    let winner_text = effective_final_text(winner);

    // Concurrent heads with identical final text do not need conflict recovery.
    // Distinct texts among remaining heads are exposed as recoverable alternatives.
    let mut seen_texts: HashSet<Option<String>> = HashSet::new();
    seen_texts.insert(winner_text.clone());

    let mut conflict_alternatives = Vec::new();
    for &head in &heads[1..] {
        let candidate_text = effective_final_text(head);
        if seen_texts.insert(candidate_text) {
            conflict_alternatives.push(head.clone());
        }
    }

    let next_parent_ids = heads.iter().map(|h| h.edit_id.clone()).collect();

    ResolvedEditState {
        active_edit_id: Some(winner.edit_id.clone()),
        active_edit_kind: Some(winner.edit_kind),
        displayed_edited_text: winner_text,
        displayed_edited_at: Some(winner.edited_at.clone()),
        next_parent_ids,
        conflict_alternatives,
    }
}

fn effective_final_text(node: &EditVersionNode) -> Option<String> {
    match node.edit_kind {
        EditKind::ManualRestore => None,
        EditKind::Manual | EditKind::Observed => node.final_text.clone(),
    }
}

fn compare_nodes(a: &EditVersionNode, b: &EditVersionNode) -> std::cmp::Ordering {
    a.hlc
        .cmp(&b.hlc)
        .then_with(|| a.edit_id.cmp(&b.edit_id))
}

/// Monotonic stats shard acceptance rule: only strictly newer revisions replace
/// a shard's stored value. Ensures revision 100 arriving after 101 is ignored.
pub fn should_accept_stats_revision(current_revision: i64, incoming_revision: i64) -> bool {
    incoming_revision > 0 && incoming_revision > current_revision
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_v2::hlc::Hlc;

    fn hlc(wall_ms: i64, counter: u32, dev: &str) -> String {
        Hlc::new(wall_ms, counter, dev).unwrap().to_wire_string()
    }

    fn node(
        edit_id: &str,
        dev: &str,
        kind: EditKind,
        parents: &[&str],
        hlc_str: String,
        text: Option<&str>,
    ) -> EditVersionNode {
        EditVersionNode {
            edit_id: edit_id.to_string(),
            source_device_id: dev.to_string(),
            edit_kind: kind,
            parent_edit_ids: parents.iter().map(|s| s.to_string()).collect(),
            hlc: hlc_str,
            edited_at: "2026-11-24T12:00:00Z".to_string(),
            final_text: text.map(str::to_string),
        }
    }

    #[test]
    fn mac_create_win_edit_twice_mac_restore_blocks_late_observer_and_allows_reedit() {
        // Windows edits twice sequentially: e1 -> e2
        let e1 = node(
            "e1",
            "win",
            EditKind::Manual,
            &[],
            hlc(1000, 0, "win"),
            Some("win edit 1"),
        );
        let e2 = node(
            "e2",
            "win",
            EditKind::Manual,
            &["e1"],
            hlc(2000, 0, "win"),
            Some("win edit 2"),
        );
        // Mac restores original on top of e2
        let r1 = node(
            "r1",
            "mac",
            EditKind::ManualRestore,
            &["e2"],
            hlc(3000, 0, "mac"),
            None,
        );
        // A late automatic observer arrives with an even HIGHER HLC
        let late_obs = node(
            "obs-late",
            "win",
            EditKind::Observed,
            &[],
            hlc(9000, 0, "win"),
            Some("stale observed text"),
        );

        let state = resolve_edits(&[e1.clone(), e2.clone(), r1.clone(), late_obs.clone()]);
        assert_eq!(state.active_edit_id.as_deref(), Some("r1"));
        assert_eq!(state.active_edit_kind, Some(EditKind::ManualRestore));
        assert_eq!(state.displayed_edited_text, None);
        assert!(!state.has_conflict());

        // Later explicit manual edit on top of r1 succeeds cleanly
        let e3 = node(
            "e3",
            "mac",
            EditKind::Manual,
            &["r1"],
            hlc(10000, 0, "mac"),
            Some("mac manual after restore"),
        );
        let state2 = resolve_edits(&[e1, e2, r1, late_obs, e3]);
        assert_eq!(state2.active_edit_id.as_deref(), Some("e3"));
        assert_eq!(
            state2.displayed_edited_text.as_deref(),
            Some("mac manual after restore")
        );
        assert!(!state2.has_conflict());
    }

    #[test]
    fn sequential_edits_arriving_backwards_do_not_create_false_conflict() {
        let e1 = node(
            "e1",
            "mac",
            EditKind::Manual,
            &[],
            hlc(1000, 0, "mac"),
            Some("v1"),
        );
        let e2 = node(
            "e2",
            "win",
            EditKind::Manual,
            &["e1"],
            hlc(2000, 0, "win"),
            Some("v2"),
        );

        // Child arrives first, then ancestor arrives late
        let child_only = resolve_edits(std::slice::from_ref(&e2));
        assert_eq!(child_only.displayed_edited_text.as_deref(), Some("v2"));
        assert!(!child_only.has_conflict());

        let both_backwards = resolve_edits(&[e2, e1]);
        assert_eq!(both_backwards.displayed_edited_text.as_deref(), Some("v2"));
        assert!(!both_backwards.has_conflict());
    }

    #[test]
    fn concurrent_offline_manual_edits_converge_regardless_of_delivery_order_and_resolve_cleanly() {
        let base = node(
            "e0",
            "mac",
            EditKind::Manual,
            &[],
            hlc(1000, 0, "mac"),
            Some("base"),
        );
        let mac_branch = node(
            "e-mac",
            "mac",
            EditKind::Manual,
            &["e0"],
            hlc(2000, 0, "mac"),
            Some("mac version"),
        );
        let win_branch = node(
            "e-win",
            "win",
            EditKind::Manual,
            &["e0"],
            hlc(2000, 0, "win"), // "win" > "mac" lexicographically
            Some("win version"),
        );

        let order_a = resolve_edits(&[base.clone(), mac_branch.clone(), win_branch.clone()]);
        let order_b = resolve_edits(&[win_branch.clone(), mac_branch.clone(), base.clone()]);
        assert_eq!(order_a, order_b);

        assert_eq!(order_a.active_edit_id.as_deref(), Some("e-win"));
        assert_eq!(order_a.displayed_edited_text.as_deref(), Some("win version"));
        assert!(order_a.has_conflict());
        assert_eq!(order_a.conflict_alternatives.len(), 1);
        assert_eq!(order_a.conflict_alternatives[0].edit_id, "e-mac");
        assert_eq!(
            order_a.conflict_alternatives[0].final_text.as_deref(),
            Some("mac version")
        );

        // User chooses Mac's version -> writes a new manual version citing BOTH heads.
        let resolved = node(
            "e-resolved",
            "mac",
            EditKind::Manual,
            &order_a
                .next_parent_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            hlc(3000, 0, "mac"),
            Some("mac version"),
        );
        let after = resolve_edits(&[base, mac_branch, win_branch, resolved]);
        assert_eq!(after.active_edit_id.as_deref(), Some("e-resolved"));
        assert_eq!(after.displayed_edited_text.as_deref(), Some("mac version"));
        assert!(!after.has_conflict());
    }

    #[test]
    fn concurrent_manual_edits_with_identical_text_produce_no_conflict() {
        let a = node(
            "e-a",
            "mac",
            EditKind::Manual,
            &[],
            hlc(2000, 0, "mac"),
            Some("same text"),
        );
        let b = node(
            "e-b",
            "win",
            EditKind::Manual,
            &[],
            hlc(2001, 0, "win"),
            Some("same text"),
        );
        let res = resolve_edits(&[a, b]);
        assert_eq!(res.displayed_edited_text.as_deref(), Some("same text"));
        assert!(!res.has_conflict());
    }

    #[test]
    fn stats_revision_monotonicity_rejects_stale_and_duplicate() {
        assert!(should_accept_stats_revision(0, 101));
        assert!(!should_accept_stats_revision(101, 100));
        assert!(!should_accept_stats_revision(101, 101));
        assert!(!should_accept_stats_revision(0, 0));
        assert!(should_accept_stats_revision(101, 102));
    }
}
