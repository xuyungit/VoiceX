//! Encrypted per-item edits. Parents identify exactly the values a writer saw.
//! Keep the parent edges even when a child arrives before its parent.
use crate::commands::settings::{AppSettings, ReplacementRule};
use crate::storage::{self, StorageError};
use crate::sync_v2::store::active_space_in;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
use voicex_sync_protocol::SyncDomain;

type Result<T> = std::result::Result<T, StorageError>;
type Entries = BTreeMap<String, Item>;
fn sql(e: rusqlite::Error) -> StorageError {
    StorageError::QueryFailed(e.to_string())
}
fn json(e: serde_json::Error) -> StorageError {
    StorageError::SerializeFailed(e.to_string())
}

const PROMPT_KEYS: [&str; 4] = ["assistant", "translation", "ttsTranslate", "ttsPreprocess"];
static DEFAULTS: LazyLock<BTreeMap<String, BTreeMap<String, String>>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../src/shared/defaultPrompts.json"))
        .expect("bundled prompts")
});

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeFields {
    pub dictionary_text: String,
    pub replacement_rules: Vec<ReplacementRule>,
    pub llm_prompt_template: String,
    pub translation_prompt_template: String,
    pub tts_translate_prompt_template: String,
    pub tts_preprocess_prompt_template: String,
}

impl From<&AppSettings> for KnowledgeFields {
    fn from(s: &AppSettings) -> Self {
        Self {
            dictionary_text: s.dictionary_text.clone(),
            replacement_rules: s.replacement_rules.clone(),
            llm_prompt_template: s.llm_prompt_template.clone(),
            translation_prompt_template: s.translation_prompt_template.clone(),
            tts_translate_prompt_template: s.tts_translate_prompt_template.clone(),
            tts_preprocess_prompt_template: s.tts_preprocess_prompt_template.clone(),
        }
    }
}
impl KnowledgeFields {
    fn prompt(&self, key: &str) -> &str {
        match key {
            "assistant" => &self.llm_prompt_template,
            "translation" => &self.translation_prompt_template,
            "ttsTranslate" => &self.tts_translate_prompt_template,
            "ttsPreprocess" => &self.tts_preprocess_prompt_template,
            _ => unreachable!(),
        }
    }
    fn entries(&self, domain: SyncDomain) -> Entries {
        let mut entries = Entries::new();
        match domain {
            SyncDomain::Dictionary => {
                for word in self
                    .dictionary_text
                    .lines()
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
                {
                    entries.insert(word.into(), Item::Word);
                }
            }
            SyncDomain::Rules => {
                for (index, rule) in self.replacement_rules.iter().enumerate() {
                    let key = rule_key(rule);
                    if let Item::Rules(group) = entries
                        .entry(key)
                        .or_insert_with(|| Item::Rules(Vec::new()))
                    {
                        group.push(RankedRule {
                            position: index,
                            rule: rule.clone(),
                        });
                    }
                }
            }
            SyncDomain::Prompts => {
                for key in PROMPT_KEYS {
                    let value = self.prompt(key);
                    if !is_default(key, value) {
                        entries.insert(key.into(), Item::Prompt(value.into()));
                    }
                }
            }
            _ => unreachable!(),
        }
        entries
    }
}
fn is_default(key: &str, value: &str) -> bool {
    value.trim().is_empty()
        || DEFAULTS
            .get(key)
            .is_some_and(|langs| langs.values().any(|v| v.trim() == value.trim()))
}
fn rule_key(rule: &ReplacementRule) -> String {
    rule.keyword.clone()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankedRule {
    pub position: usize,
    pub rule: ReplacementRule,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Item {
    Word,
    Rules(Vec<RankedRule>),
    Prompt(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub key: String,
    pub value: Option<Item>,
    pub parents: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Version {
    pub id: String,
    pub value: Option<Item>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Conflict {
    pub domain: SyncDomain,
    pub key: String,
    pub versions: Vec<Version>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub space_id: Option<String>,
    pub scopes: u8,
    pub conflicts: Vec<Conflict>,
}

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS knowledge_scope(
        space TEXT NOT NULL,domain TEXT NOT NULL,enabled INTEGER NOT NULL,cursor INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(space,domain));
        CREATE TABLE IF NOT EXISTS knowledge_versions(
        space TEXT NOT NULL,domain TEXT NOT NULL,item TEXT NOT NULL,id TEXT NOT NULL,value TEXT NOT NULL,
        PRIMARY KEY(space,domain,item,id));
        CREATE TABLE IF NOT EXISTS knowledge_parents(
        space TEXT NOT NULL,domain TEXT NOT NULL,item TEXT NOT NULL,id TEXT NOT NULL,
        PRIMARY KEY(space,domain,item,id));").map_err(sql)
}
pub fn mask(c: &Connection, space: &str) -> Result<u8> {
    let mut stmt = c
        .prepare("SELECT domain FROM knowledge_scope WHERE space=? AND enabled=1")
        .map_err(sql)?;
    let mut mask = 0;
    for row in stmt
        .query_map([space], |r| r.get::<_, String>(0))
        .map_err(sql)?
    {
        let d: SyncDomain = row
            .map_err(sql)?
            .parse()
            .map_err(StorageError::InvalidInput)?;
        mask |= d.knowledge_bit();
    }
    Ok(mask)
}
fn heads(c: &Connection, space: &str, d: SyncDomain) -> Result<BTreeMap<String, Vec<Version>>> {
    let mut stmt=c.prepare("SELECT item,id,value FROM knowledge_versions v WHERE space=? AND domain=?
        AND NOT EXISTS(SELECT 1 FROM knowledge_parents p WHERE p.space=v.space AND p.domain=v.domain AND p.item=v.item AND p.id=v.id)
        ORDER BY item,id").map_err(sql)?;
    let mut map = BTreeMap::<String, Vec<Version>>::new();
    let mut rows = stmt.query(params![space, d.as_str()]).map_err(sql)?;
    while let Some(row) = rows.next().map_err(sql)? {
        map.entry(row.get(0).map_err(sql)?)
            .or_default()
            .push(Version {
                id: row.get(1).map_err(sql)?,
                value: serde_json::from_str(&row.get::<_, String>(2).map_err(sql)?)
                    .map_err(json)?,
            });
    }
    Ok(map)
}
pub fn apply(c: &Connection, space: &str, d: SyncDomain, id: &str, op: &Change) -> Result<()> {
    let valid = match (d, &op.value) {
        (SyncDomain::Dictionary, None | Some(Item::Word)) => {
            !op.key.is_empty() && op.key.trim() == op.key && !op.key.contains('\n')
        }
        (SyncDomain::Prompts, None) => PROMPT_KEYS.contains(&op.key.as_str()),
        (SyncDomain::Prompts, Some(Item::Prompt(s))) => {
            PROMPT_KEYS.contains(&op.key.as_str()) && !is_default(&op.key, s)
        }
        (SyncDomain::Rules, None) => true,
        (SyncDomain::Rules, Some(Item::Rules(rows))) => {
            !rows.is_empty() && rows.iter().all(|r| rule_key(&r.rule) == op.key)
        }
        _ => false,
    };
    if !valid || op.parents.iter().any(|p| p == id) {
        return Err(StorageError::InvalidInput(
            "Invalid knowledge change".into(),
        ));
    }
    let args = params![
        space,
        d.as_str(),
        op.key,
        id,
        serde_json::to_string(&op.value).map_err(json)?
    ];
    c.execute(
        "INSERT OR IGNORE INTO knowledge_versions VALUES(?1,?2,?3,?4,?5)",
        args,
    )
    .map_err(sql)?;
    for parent in &op.parents {
        c.execute(
            "INSERT OR IGNORE INTO knowledge_parents VALUES(?,?,?,?)",
            params![space, d.as_str(), op.key, parent],
        )
        .map_err(sql)?;
    }
    Ok(())
}
fn enqueue(c: &Connection, space: &str, d: SyncDomain, op: Change) -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::to_string(&op).map_err(json)?;
    apply(c, space, d, &id, &op)?;
    // The relay sees random IDs only, including for dictionary words and rule patterns.
    c.execute("INSERT INTO sync_outbox_ops(op_id,space_id,domain,op_type,record_id,payload_json,created_at)
        VALUES(?1,?2,?3,'knowledge.change',?1,?4,?5)",params![id,space,d.as_str(),payload,chrono::Utc::now().to_rfc3339()]).map_err(sql)?;
    Ok(())
}
fn equivalent(a: &Option<Item>, b: &Option<Item>) -> bool {
    match (a, b) {
        (Some(Item::Rules(a)), Some(Item::Rules(b))) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| {
                    x.rule.match_mode == y.rule.match_mode
                        && x.rule.replacement == y.rule.replacement
                        && x.rule.enabled == y.rule.enabled
                })
        }
        _ => a == b,
    }
}
fn conflicted(versions: &[Version]) -> bool {
    versions
        .iter()
        .skip(1)
        .any(|v| !equivalent(&v.value, &versions[0].value))
}
fn diff(
    c: &Connection,
    space: &str,
    d: SyncDomain,
    before: &Entries,
    after: &Entries,
) -> Result<()> {
    if before == after {
        return Ok(());
    }
    let heads = heads(c, space, d)?;
    for key in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
        let old = before.get(key);
        let new = after.get(key);
        if old == new {
            continue;
        }
        let versions = heads.get(key).into_iter().flatten();
        let parents = versions
            .filter(|v| equivalent(&v.value, &old.cloned()))
            .map(|v| v.id.clone())
            .collect();
        enqueue(
            c,
            space,
            d,
            Change {
                key: key.clone(),
                value: new.cloned(),
                parents,
            },
        )?;
    }
    Ok(())
}

pub fn project(c: &Connection, space: &str, d: SyncDomain, s: &mut AppSettings) -> Result<()> {
    let heads = heads(c, space, d)?;
    match d {
        SyncDomain::Dictionary => {
            let mut words: BTreeSet<_> = heads
                .into_iter()
                .filter(|(_, vs)| vs.iter().any(|v| v.value.is_some()))
                .map(|(k, _)| k)
                .collect();
            let mut ordered: Vec<String> = s
                .dictionary_text
                .lines()
                .map(str::trim)
                .filter(|w| words.remove(*w))
                .map(str::to_owned)
                .collect();
            ordered.extend(words);
            let text = ordered.join("\n");
            if s.dictionary_text != text {
                s.dictionary_text = text;
                s.local_hotword_updated_at = chrono::Utc::now().to_rfc3339();
            }
        }
        SyncDomain::Rules => {
            let mut rules = Vec::new();
            for (_, versions) in heads {
                if conflicted(&versions) {
                    continue;
                }
                if let Some(Item::Rules(group)) = &versions.last().unwrap().value {
                    rules.extend(group.clone());
                }
            }
            rules.sort_by(|a, b| {
                a.position
                    .cmp(&b.position)
                    .then_with(|| a.rule.id.cmp(&b.rule.id))
            });
            // Concurrent renames may carry the same local editor ID to different keys.
            let mut ids = BTreeSet::new();
            for row in &mut rules {
                while !ids.insert(row.rule.id.clone()) {
                    use sha2::{Digest, Sha256};
                    let identity =
                        serde_json::to_vec(&(&row.rule.keyword, &row.rule.id)).map_err(json)?;
                    row.rule.id = hex::encode(Sha256::digest(identity));
                }
            }
            s.replacement_rules = rules.into_iter().map(|r| r.rule).collect();
        }
        SyncDomain::Prompts => {
            for (key, versions) in heads {
                let field = match key.as_str() {
                    "assistant" => &mut s.llm_prompt_template,
                    "translation" => &mut s.translation_prompt_template,
                    "ttsPreprocess" => &mut s.tts_preprocess_prompt_template,
                    "ttsTranslate" => &mut s.tts_translate_prompt_template,
                    _ => return Err(StorageError::InvalidInput("unknown prompt".into())),
                };
                match &versions.last().unwrap().value {
                    Some(Item::Prompt(value)) => *field = value.clone(),
                    None if !is_default(&key, field) => {
                        *field =
                            DEFAULTS[&key][if crate::ui_locale::resolve_ui_locale(&s.ui_language)
                                .starts_with("en")
                            {
                                "en"
                            } else {
                                "zh"
                            }]
                            .clone()
                    }
                    _ => {}
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}
pub fn capture(c: &Connection, before: &KnowledgeFields, desired: &mut AppSettings) -> Result<()> {
    let Some(space) = active_space_in(c)? else {
        return Ok(());
    };
    if !space.scope_config().enabled {
        return Ok(());
    }
    let scopes = mask(c, &space.space_id)?;
    for d in SyncDomain::KNOWLEDGE {
        if scopes & d.knowledge_bit() == 0 {
            continue;
        }
        let after = KnowledgeFields::from(&*desired).entries(d);
        diff(c, &space.space_id, d, &before.entries(d), &after)?;
        project(c, &space.space_id, d, desired)?;
    }
    Ok(())
}
pub fn status(c: &Connection) -> Result<Status> {
    let mut status = Status {
        space_id: None,
        scopes: 0,
        conflicts: vec![],
    };
    let Some(space) = active_space_in(c)? else {
        return Ok(status);
    };
    if !space.scope_config().enabled {
        return Ok(status);
    }
    status.scopes = mask(c, &space.space_id)?;
    for d in [SyncDomain::Rules, SyncDomain::Prompts] {
        if status.scopes & d.knowledge_bit() == 0 {
            continue;
        }
        for (key, versions) in heads(c, &space.space_id, d)? {
            if conflicted(&versions) {
                status.conflicts.push(Conflict {
                    domain: d,
                    key,
                    versions,
                })
            }
        }
    }
    status.space_id = Some(space.space_id);
    Ok(status)
}
pub fn set_enabled(c: &Connection, space: &str, d: SyncDomain, enabled: bool) -> Result<()> {
    if d.knowledge_bit() == 0 {
        return Err(StorageError::InvalidInput("Invalid knowledge scope".into()));
    }
    let old = c
        .query_row(
            "SELECT enabled FROM knowledge_scope WHERE space=? AND domain=?",
            params![space, d.as_str()],
            |r| r.get::<_, bool>(0),
        )
        .optional()
        .map_err(sql)?;
    if old.unwrap_or(false) == enabled {
        return Ok(());
    }
    let s = storage::read_settings_for_update(c)?;
    if enabled {
        let mut before = Entries::new();
        if old.is_some() {
            let mut previous = AppSettings::default();
            project(c, space, d, &mut previous)?;
            before = KnowledgeFields::from(&previous).entries(d);
        }
        diff(c, space, d, &before, &KnowledgeFields::from(&s).entries(d))?;
    }
    c.execute("INSERT INTO knowledge_scope(space,domain,enabled) VALUES(?,?,?) ON CONFLICT(space,domain) DO UPDATE SET enabled=excluded.enabled",params![space,d.as_str(),enabled]).map_err(sql)?;
    c.execute(
        "UPDATE sync_spaces SET scope_version=scope_version+1 WHERE space_id=?",
        [space],
    )
    .map_err(sql)?;
    Ok(())
}
pub fn resolve(
    c: &Connection,
    space: &str,
    d: SyncDomain,
    key: String,
    selected: &str,
    expected: Vec<String>,
) -> Result<()> {
    if d.knowledge_bit() == 0 || mask(c, space)? & d.knowledge_bit() == 0 {
        return Err(StorageError::InvalidInput("Sync scope is disabled".into()));
    }
    let versions = heads(c, space, d)?.remove(&key).unwrap_or_default();
    if versions.iter().map(|v| &v.id).collect::<BTreeSet<_>>() != expected.iter().collect() {
        return Err(StorageError::InvalidInput(
            "Conflict changed; reload the versions".into(),
        ));
    }
    let value = versions
        .iter()
        .find(|v| v.id == selected)
        .ok_or_else(|| StorageError::InvalidInput("Unknown version".into()))?
        .value
        .clone();
    enqueue(
        c,
        space,
        d,
        Change {
            key,
            value,
            parents: expected,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaying_a_deleted_word_before_its_parent_keeps_it_deleted() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let word = Change {
            key: "term".into(),
            value: Some(Item::Word),
            parents: vec![],
        };
        let removed = Change {
            key: word.key.clone(),
            value: None,
            parents: vec!["first".into()],
        };
        for (id, change) in [("later", &removed), ("first", &word), ("later", &removed)] {
            apply(&db, "space", SyncDomain::Dictionary, id, change).unwrap();
        }
        let mut settings = AppSettings::default();
        project(&db, "space", SyncDomain::Dictionary, &mut settings).unwrap();
        assert!(settings.dictionary_text.is_empty());
        let versions = heads(&db, "space", SyncDomain::Dictionary).unwrap();
        assert_eq!(versions["term"].len(), 1);
        assert_eq!(versions["term"][0].id, "later");
    }

    #[test]
    fn bundled_prompts_stay_local_in_both_languages() {
        for key in PROMPT_KEYS {
            for language in ["en", "zh"] {
                assert!(is_default(key, &DEFAULTS[key][language]));
            }
            assert!(!is_default(key, "user's own prompt"));
        }
    }
}
