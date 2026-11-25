//! Reconcile an installation's absolute counters with any snapshot already
//! published for its device ID before allocating its first outbound revision.
use super::payloads::StatsShardPayload;
use super::store::{
    apply_remote_stats_daily_month_tx, apply_remote_stats_totals_tx,
    build_local_daily_month_shard_in, build_local_totals_shard_in,
};
use crate::storage::StorageError;
use rusqlite::{params, Connection};
use std::collections::BTreeMap;

pub(super) fn reconcile(
    conn: &Connection,
    space: &str,
    device: &str,
    incoming: &StatsShardPayload,
) -> Result<(), StorageError> {
    let sql = |e: rusqlite::Error| StorageError::QueryFailed(e.to_string());
    let (key, revision) = match incoming {
        StatsShardPayload::Totals(t) => ("totals".into(), t.revision),
        StatsShardPayload::DailyMonth(d) => (format!("daily:{}", d.month), d.revision),
    };
    // Remote-apply normally deduplicates by revision. During reconciliation
    // we also need to recover rows previously acknowledged by op_id alone.
    conn.execute(
        "UPDATE sync_stats_shards SET remote_revision=0
         WHERE space_id=?1 AND device_id=?2 AND shard_key=?3",
        params![space, device, key],
    )
    .map_err(sql)?;
    let differs = match incoming {
        StatsShardPayload::Totals(remote) => {
            let mut local = build_local_totals_shard_in(conn, device, revision)?;
            let d = &mut local.dictation;
            let r = &remote.dictation;
            d.recording_count = d.recording_count.max(r.recording_count);
            d.total_characters = d.total_characters.max(r.total_characters);
            d.total_duration_ms = d.total_duration_ms.max(r.total_duration_ms);
            d.llm_invoked_count = d.llm_invoked_count.max(r.llm_invoked_count);
            d.ai_correction_count = d.ai_correction_count.max(r.ai_correction_count);
            let t = &mut local.tts;
            let r = &remote.tts;
            t.read_count = t.read_count.max(r.read_count);
            t.translate_count = t.translate_count.max(r.translate_count);
            t.characters = t.characters.max(r.characters);
            t.duration_ms = t.duration_ms.max(r.duration_ms);
            t.llm_count = t.llm_count.max(r.llm_count);
            let differs = local.dictation != remote.dictation || local.tts != remote.tts;
            apply_remote_stats_totals_tx(conn, space, &local)?;
            differs
        }
        StatsShardPayload::DailyMonth(remote) => {
            let mut local =
                build_local_daily_month_shard_in(conn, device, &remote.month, revision)?;
            // A migration may classify the same dictations under "untracked"
            // while the device knows the app. Adding both sets double counts.
            local.app_rows = reconcile_days(
                local.app_rows,
                &remote.app_rows,
                |r| (&r.local_date, &r.feature_kind),
                |r| {
                    [
                        r.usage_count,
                        r.total_characters,
                        r.total_duration_ms,
                        r.llm_count,
                    ]
                },
                |r| {
                    (r.app_key != crate::storage::UNTRACKED_APP_KEY).then_some((
                        &r.platform,
                        &r.app_key,
                        "",
                    ))
                },
            )?;
            local.model_rows = reconcile_days(
                local.model_rows,
                &remote.model_rows,
                |r| (&r.local_date, ""),
                |r| {
                    [
                        r.usage_count,
                        r.total_characters,
                        r.total_duration_ms,
                        r.ai_correction_count,
                    ]
                },
                |r| {
                    (!r.asr_model_name.is_empty()
                        && r.llm_state != voicex_sync_protocol::LlmUsageState::Unknown)
                        .then_some((&r.asr_model_name, r.llm_state.as_str(), &r.llm_model_name))
                },
            )?;
            let differs =
                local.app_rows != remote.app_rows || local.model_rows != remote.model_rows;
            apply_remote_stats_daily_month_tx(conn, space, &local)?;
            differs
        }
    };
    let next = revision
        .checked_add(i64::from(differs))
        .ok_or_else(|| StorageError::InvalidInput("stats revision overflow".into()))?;
    conn.execute(
        "UPDATE sync_stats_shards SET local_revision=MAX(local_revision, ?4), acked_revision=?5
         WHERE space_id=?1 AND device_id=?2 AND shard_key=?3",
        params![space, device, key, next, revision],
    )
    .map_err(sql)?;
    Ok(())
}

fn reconcile_days<R: Clone>(
    local: Vec<R>,
    remote: &[R],
    group: impl Fn(&R) -> (&str, &str),
    counters: impl Fn(&R) -> [i64; 4],
    category: impl Fn(&R) -> Option<(&str, &str, &str)>,
) -> Result<Vec<R>, StorageError> {
    let mut days = BTreeMap::<(String, String), (Vec<R>, Vec<R>)>::new();
    for r in local {
        let (day, feature) = group(&r);
        days.entry((day.into(), feature.into()))
            .or_default()
            .0
            .push(r);
    }
    for r in remote {
        let (day, feature) = group(r);
        days.entry((day.into(), feature.into()))
            .or_default()
            .1
            .push(r.clone());
    }
    let totals = |rows: &[R]| {
        rows.iter().map(&counters).fold([0i128; 4], |sum, n| {
            std::array::from_fn(|i| sum[i] + i128::from(n[i]))
        })
    };
    // A larger day can absorb an unclassified legacy bucket. Preserve known
    // app/model categories: conflicting distributions need source reconciliation.
    let covers = |larger: &[R], smaller: &[R]| {
        smaller.iter().all(|row| match category(row) {
            None => true,
            Some(key) => larger.iter().any(|other| {
                category(other) == Some(key)
                    && (0..4).all(|i| counters(other)[i] >= counters(row)[i])
            }),
        })
    };
    let mut rows = Vec::new();
    for ((day, feature), (local, remote)) in days {
        let a = totals(&local);
        let b = totals(&remote);
        if (0..4).all(|i| a[i] >= b[i]) && covers(&local, &remote) {
            rows.extend(local);
        } else if (0..4).all(|i| b[i] >= a[i]) && covers(&remote, &local) {
            rows.extend(remote);
        } else {
            return Err(StorageError::InvalidInput(format!(
                "inconsistent stats for {day} {feature}: local {a:?}, server {b:?}; reconcile the source databases"
            )));
        }
    }
    Ok(rows)
}
