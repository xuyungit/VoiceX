//! Rolling quick standings reuse the legacy points/decay engine, with separate, compatible seasons.
use crate::{
    recompute_standings, StandingEntry, StandingLens, StandingPlace, StandingRace, StandingScheme,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufReader;
use std::path::Path;

struct HistoryLock(std::path::PathBuf);
impl Drop for HistoryLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Place {
    provider: String,
    place: usize,
    points: f64,
    passes: usize,
    total_rounds: usize,
    needs_revision: usize,
}
impl StandingPlace for Place {
    fn provider(&self) -> &str {
        &self.provider
    }
    fn place(&self) -> usize {
        self.place
    }
    fn points(&self) -> f64 {
        self.points
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Race {
    id: String,
    at: String,
    timestamp: i64,
    season: String,
    ranking: Vec<Place>,
}
impl StandingRace for Race {
    type Place = Place;
    fn places(&self) -> &[Place] {
        &self.ranking
    }
}
#[derive(Serialize, Deserialize)]
struct History {
    format: String,
    runs: Vec<Race>,
    /// Avoid reparsing unchanged legacy reports on every quick run (some are many MB).
    scanned: BTreeMap<String, (u64, u128)>,
}

fn fingerprint(value: &Value) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.to_string().bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn season(payload: &Value) -> Result<String, String> {
    if payload["format"] != "quick-ranking-v2" {
        return Err("rolling quick standings require quick-ranking-v2 results".into());
    }
    let run = &payload["run"];
    let rows = payload["cases"].as_array().ok_or("missing cases")?;
    let cases: BTreeSet<_> = rows
        .iter()
        .map(|c| {
            json!({"input": c["input"],
        "expected": c["expected"], "focus": c["focus"]})
            .to_string()
        })
        .collect();
    let providers: BTreeSet<_> = run["providers"]
        .as_array()
        .ok_or("missing providers")?
        .iter()
        .map(Value::to_string)
        .collect();
    let judge_models: BTreeSet<_> = rows
        .iter()
        .flat_map(|c| c["rounds"].as_array().into_iter().flatten())
        .filter_map(|r| r["basic"]["reported_model"].as_str())
        .collect();
    Ok(fingerprint(&json!({"cases": cases, "providers": providers,
        "prompt": run["prompt"], "dictionary": run["dictionary"], "rounds": run["rounds"],
        "ranking_policy": run["ranking_policy"], "quality_evaluation": run["quality_evaluation"],
        "judge": {"backend": payload["judge"]["backend"], "model": payload["judge"]["model"],
            "reported_models": judge_models, "rubric": payload["judge"]["rubric"],
            "confidence_gate": payload["judge"]["confidence_threshold"]}})))
}

fn race(payload: &Value) -> Result<Option<Race>, String> {
    let season = season(payload)?;
    let at = payload["run"]["at"]
        .as_str()
        .ok_or("missing run timestamp")?
        .to_owned();
    let timestamp = chrono::DateTime::parse_from_rfc3339(&at)
        .map_err(|e| e.to_string())?
        .timestamp_millis();
    let rows = payload["ranking"]
        .as_array()
        .filter(|r| !r.is_empty())
        .ok_or("missing ranking")?;
    // A judge failure must not turn into last-place points or a win against an unjudged opponent.
    if rows.iter().any(|r| r["place"].is_null()) {
        return Ok(None);
    }
    let mut ranking = Vec::new();
    let mut names = BTreeSet::new();
    for row in rows {
        let number = |key: &str| {
            row[key]
                .as_u64()
                .map(|n| n as usize)
                .ok_or(format!("invalid ranking {key}"))
        };
        let provider = row["provider"]
            .as_str()
            .ok_or("missing provider")?
            .to_owned();
        let place = number("place")?;
        if !names.insert(provider.clone()) || place == 0 || place > rows.len() {
            return Err("duplicate provider or invalid place in quick results".into());
        }
        ranking.push(Place {
            provider,
            place,
            points: 0.0,
            passes: number("passes")?,
            total_rounds: number("total_rounds")?,
            needs_revision: number("needs_revision")?,
        });
    }
    ranking.sort_by(|a, b| a.place.cmp(&b.place).then(a.provider.cmp(&b.provider)));
    Ok(Some(Race {
        id: fingerprint(&json!([at, season])),
        at,
        timestamp,
        season,
        ranking,
    }))
}

fn insert(history: &mut History, candidate: Race) -> Result<(), String> {
    if let Some(old) = history.runs.iter().find(|r| r.id == candidate.id) {
        let mut old = old.clone();
        for p in &mut old.ranking {
            p.points = 0.0;
        }
        if old.ranking != candidate.ranking {
            return Err(format!("conflicting results for run {}", candidate.at));
        }
    } else {
        history.runs.push(candidate);
    }
    Ok(())
}

fn import_saved(history: &mut History, directory: &Path) -> Result<(), String> {
    if !directory.exists() {
        return Ok(());
    }
    #[derive(Deserialize)]
    struct Format {
        format: Option<String>,
    }
    for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        let path = entry
            .map_err(|e| e.to_string())?
            .path()
            .join("results.json");
        if !path.is_file() {
            continue;
        }
        let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        let stamp = (
            metadata.len(),
            metadata
                .modified()
                .map_err(|e| e.to_string())?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos(),
        );
        let key = std::fs::canonicalize(&path)
            .map_err(|e| e.to_string())?
            .display()
            .to_string();
        if history.scanned.get(&key) == Some(&stamp) {
            continue;
        }
        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let format: Format = serde_json::from_reader(BufReader::new(file))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if format.format.as_deref() == Some("quick-ranking-v2") {
            let payload = read(&path)?;
            if let Some(race) = race(&payload)? {
                insert(history, race)?;
            }
        }
        history.scanned.insert(key, stamp);
    }
    Ok(())
}

fn award(race: &mut Race, scheme: StandingScheme) {
    let field = race.ranking.len();
    let mut i = 0;
    while i < field {
        let place = race.ranking[i].place;
        let mut end = i + 1;
        while end < field && race.ranking[end].place == place {
            end += 1;
        }
        let points = (place..place + end - i)
            .map(|p| crate::points_for_place(p, field, scheme))
            .sum::<f64>()
            / (end - i) as f64;
        for p in &mut race.ranking[i..end] {
            p.points = points;
        }
        i = end;
    }
}

fn positions(entries: &[StandingEntry]) -> BTreeMap<&str, usize> {
    entries
        .iter()
        .filter(|r| r.active)
        .enumerate()
        .map(|(i, r)| (r.provider.as_str(), i + 1))
        .collect()
}

pub fn update(
    payload: &Value,
    directory: &Path,
    path: &Path,
    lens: &StandingLens,
) -> Result<Value, String> {
    if !lens.decay.is_finite() || lens.decay <= 0.0 || lens.decay > 1.0 {
        return Err("standing_decay must be in (0, 1]".into());
    }
    let lock_path = path.with_extension("json.lock");
    let _lock = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|e| {
            format!(
                "cannot lock {}: {e}; another standings update may be running",
                path.display()
            )
        })?;
    let _lock_guard = HistoryLock(lock_path);
    let mut history = if path.exists() {
        let history: History =
            serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
                .map_err(|e| {
                    format!(
                        "{}: {e}; quick standings need their own file",
                        path.display()
                    )
                })?;
        if history.format != "quick-standings-v1" {
            return Err("unsupported quick standings format".into());
        }
        history
    } else {
        History {
            format: "quick-standings-v1".into(),
            runs: Vec::new(),
            scanned: BTreeMap::new(),
        }
    };
    import_saved(&mut history, directory)?;
    let current = race(payload)?;
    let current_season = season(payload)?;
    if let Some(race) = &current {
        insert(&mut history, race.clone())?;
    }
    let scheme = StandingScheme::parse(Some(&lens.scheme));
    for race in &mut history.runs {
        award(race, scheme);
    }
    history
        .runs
        .sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.id.cmp(&b.id)));
    let at = payload["run"]["at"].as_str().ok_or("missing timestamp")?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(at)
        .map_err(|e| e.to_string())?
        .timestamp_millis();
    let compatible: Vec<_> = history
        .runs
        .iter()
        .filter(|r| r.season == current_season && r.timestamp <= timestamp)
        .cloned()
        .collect();
    let prior: Vec<_> = compatible
        .iter()
        .filter(|r| Some(&r.id) != current.as_ref().map(|r| &r.id))
        .cloned()
        .collect();
    let previous = recompute_standings(&prior, lens);
    let previous_positions = positions(&previous);
    let standings = recompute_standings(&compatible, lens);
    let mut entries = Vec::new();
    for (i, entry) in standings.iter().filter(|s| s.active).enumerate() {
        let mut row = serde_json::to_value(entry).map_err(|e| e.to_string())?;
        row["place"] = json!(i + 1);
        row["previous_place"] = json!(previous_positions.get(entry.provider.as_str()));
        row["change"] = json!(previous_positions
            .get(entry.provider.as_str())
            .map(|p| *p as i64 - (i + 1) as i64));
        entries.push(row);
    }
    let report = json!({"season": current_season, "at": at, "counted_runs": compatible.len(),
        "other_season_runs": history.runs.iter().filter(|r| r.season != current_season).count(),
        "current_counted": current.is_some(), "lens": lens, "entries": entries, "path": path});
    let temp = path.with_extension("json.tmp");
    std::fs::write(
        &temp,
        serde_json::to_vec_pretty(&history).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
    Ok(report)
}

pub fn print(report: &Value, ranking: &[Value]) {
    println!(
        "\n滚动排行榜 · {} · {} 轮 · 最近 {} 轮 · 衰减 {} · {} 积分",
        report["at"].as_str().unwrap(),
        report["counted_runs"],
        report["lens"]["window"],
        report["lens"]["decay"],
        report["lens"]["scheme"].as_str().unwrap()
    );
    println!("本轮名次用于单次比较；滚动名次按近期加权平均积分排序，变化相对上一轮累计榜。");
    if report["current_counted"] == false {
        println!("本轮有待定判定，未计入积分；下表为此前累计结果。");
    }
    if report["other_season_runs"].as_u64().unwrap_or(0) > 0 {
        println!(
            "另有 {} 轮的 case、规则或参评配置不同，保留在各自赛季。",
            report["other_season_runs"]
        );
    }
    println!("  名次  变化    近期积分  参赛  本轮名次  本轮通过  模型");
    for row in report["entries"].as_array().unwrap() {
        let name = row["provider"].as_str().unwrap();
        let latest = ranking.iter().find(|r| r["provider"] == name);
        let change = match row["change"].as_i64() {
            None => "新".into(),
            Some(0) => "—".into(),
            Some(n) if n > 0 => format!("↑{n}"),
            Some(n) => format!("↓{}", -n),
        };
        let place = latest
            .and_then(|r| r["place"].as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into());
        let basic = latest
            .map(|r| format!("{}/{}", r["passes"], r["total_rounds"]))
            .unwrap_or_else(|| "—".into());
        println!(
            "  {:>4}  {:>4}  {:>10.2}  {:>4}  {:>8}  {:>8}  {}",
            row["place"].as_u64().unwrap(),
            change,
            row["form"].as_f64().unwrap(),
            row["window_runs"].as_u64().unwrap(),
            place,
            basic,
            name
        );
    }
    println!("Standings: {}", report["path"].as_str().unwrap());
}

pub fn read(path: &Path) -> Result<Value, String> {
    serde_json::from_reader(BufReader::new(
        std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?,
    ))
    .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn from_file(
    source: &Path,
    directory: &Path,
    path: &Path,
    lens: &StandingLens,
) -> Result<(), String> {
    let payload = read(source)?;
    season(&payload)?;
    println!(
        "本轮结果：{} · {}",
        source.display(),
        payload["run"]["at"].as_str().unwrap()
    );
    let ranking = payload["ranking"].as_array().ok_or("missing ranking")?;
    crate::quick::print_ranking(
        ranking,
        payload["run"]["quality_evaluation"] == "not_performed",
    );
    let report = update(&payload, directory, path, lens)?;
    print(&report, ranking);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(at: &str) -> Value {
        json!({"format":"quick-ranking-v2", "run":{"at":at,"rounds":1,"providers":[{"name":"a","model":"one"},{"name":"b","model":"two"}],
            "prompt":"correct","dictionary":"","ranking_policy":"basic-speed","quality_evaluation":"basic"},
            "cases":[{"case":"sample","input":"input","expected":"reference","focus":"meaning","rounds":[{"basic":{"reported_model":"judge-v1"}}]}],
            "judge":{"backend":"typesafe","model":"latest","rubric":{"task":"meaning"}},
            "ranking":[{"provider":"a","place":1,"passes":1,"total_rounds":1,"needs_revision":0},
                {"provider":"b","place":2,"passes":1,"total_rounds":1,"needs_revision":0}]})
    }
    #[test]
    fn season_tracks_meaningful_settings_and_ignores_cosmetic_case_names() {
        let source = fixture("2026-10-18T10:00:00+08:00");
        let key = season(&source).unwrap();
        let mut renamed = source.clone();
        renamed["cases"][0]["case"] = json!("renamed");
        renamed["run"]["providers"]
            .as_array_mut()
            .unwrap()
            .reverse();
        assert_eq!(key, season(&renamed).unwrap());
        for pointer in [
            "/cases/0/focus",
            "/judge/rubric/task",
            "/run/prompt",
            "/run/ranking_policy",
            "/run/providers/0/model",
        ] {
            let mut changed = source.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("changed");
            assert_ne!(key, season(&changed).unwrap(), "{pointer}");
        }
    }
    #[test]
    fn decay_window_ties_and_pending_use_shared_standings_rules() {
        let mut first = race(&fixture("2026-10-18T10:00:00+08:00"))
            .unwrap()
            .unwrap();
        award(&mut first, StandingScheme::Borda);
        let mut second = first.clone();
        second.ranking[0].place = 2;
        second.ranking[1].place = 1;
        second.ranking.reverse();
        award(&mut second, StandingScheme::Borda);
        let lens = StandingLens {
            scheme: "borda".into(),
            decay: 0.5,
            window: 2,
            retire_after: 3,
        };
        let table = recompute_standings(&[first.clone(), second.clone()], &lens);
        assert_eq!(table[0].provider, "b");
        assert!((table[0].form - 5.0 / 3.0).abs() < 1e-9);
        assert_eq!(table[0].window_runs, 2);
        let table = recompute_standings(
            &[first, second.clone()],
            &StandingLens { window: 1, ..lens },
        );
        assert_eq!(table[0].form, 2.0);
        second.ranking[1].place = 1;
        award(&mut second, StandingScheme::Borda);
        assert_eq!(second.ranking[0].points, 1.5);
        assert_eq!(second.ranking[1].points, 1.5);
        let mut pending = fixture("2026-10-18T10:00:00+08:00");
        pending["ranking"][0]["place"] = Value::Null;
        assert!(race(&pending).unwrap().is_none());
    }
}
