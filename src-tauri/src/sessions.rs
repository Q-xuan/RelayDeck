//! Codex session management.
//!
//! Codex keeps every conversation as a `rollout-<timestamp>-<uuid>.jsonl` file under
//! `~/.codex/sessions/YYYY/MM/DD/`, and lists the resumable ones in `~/.codex/session_index.jsonl`.
//! A rollout without an index entry still exists on disk but cannot be resumed, and an index entry
//! without a rollout is a dead row in the picker. This module reconciles the two.
//!
//! Codex Desktop itself lists threads from `state_5.sqlite`, filtered by the `model_provider`
//! configured in `config.toml` — so switching to the relaydeck provider makes every historical
//! conversation vanish from its picker. Aligning `threads.model_provider` with the active provider
//! (the same strategy cockpit-tools uses) keeps them visible; the database is backed up through the
//! SQLite backup API first, which is safe against a live WAL writer. `logs_2.sqlite` is never touched.
//!
//! Other writes are limited to the index file (always backed up first) and to moving rollout files
//! in and out of RelayDeck's own archive folder.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags};

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    models::{CodexSession, SessionOverview, SessionRepairResult},
    RelayError,
};

const SESSIONS_DIR: &str = "sessions";
const ARCHIVE_DIR: &str = "sessions-archive";
const INDEX_FILE: &str = "session_index.jsonl";
const INDEX_BACKUP_PREFIX: &str = "session_index.jsonl.relaydeck-backup-";
const BACKUPS_TO_KEEP: usize = 5;
/// Rollout files reach hundreds of megabytes; every field we need lives in the first lines.
const HEAD_SCAN_BYTES: u64 = 512 * 1024;
const TITLE_LIMIT: usize = 80;
const MAX_DEPTH: usize = 6;
/// Codex Desktop's thread list lives here; `sqlite/state_5.sqlite` is a legacy copy some builds still read.
const STATE_DBS: [&str; 2] = ["state_5.sqlite", "sqlite/state_5.sqlite"];
const DB_BACKUPS_TO_KEEP: usize = 3;
const DB_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// One line of `session_index.jsonl`. Unknown keys are preserved so rewrites stay lossless.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexEntry {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thread_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

pub fn overview(home: &Path) -> Result<SessionOverview, RelayError> {
    let sessions_dir = home.join(SESSIONS_DIR);
    let archive_dir = home.join(ARCHIVE_DIR);
    let index = read_index(home);
    let known: HashMap<&str, &IndexEntry> = index.iter().map(|entry| (entry.id.as_str(), entry)).collect();

    let mut sessions = Vec::new();
    for (path, archived) in collect_rollouts(&sessions_dir, false).into_iter().chain(collect_rollouts(&archive_dir, true)) {
        if let Some(session) = read_session(&path, archived, &known) {
            sessions.push(session);
        }
    }

    let present: HashSet<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
    let ghost_count = index.iter().filter(|entry| !present.contains(entry.id.as_str())).count();
    let total_bytes = sessions.iter().map(|session| session.size_bytes).sum();
    let archived_bytes = sessions.iter().filter(|session| session.archived).map(|session| session.size_bytes).sum();
    let indexed_count = sessions.iter().filter(|session| session.indexed && !session.archived).count();
    let archived_count = sessions.iter().filter(|session| session.archived).count();
    let orphan_count = sessions.iter().filter(|session| is_orphan(session)).count();

    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let active_provider = active_provider(home);
    let provider_mismatch_count = active_provider.as_deref().map(|provider| provider_mismatch_count(home, provider)).unwrap_or(0);

    Ok(SessionOverview {
        sessions,
        sessions_dir: sessions_dir.display().to_string(),
        archive_dir: archive_dir.display().to_string(),
        total_bytes,
        archived_bytes,
        indexed_count,
        orphan_count,
        archived_count,
        ghost_count,
        active_provider,
        provider_mismatch_count,
        scanned_at: Utc::now(),
    })
}

/// A live rollout Codex cannot resume. Subagent threads are never indexed by design, so they do not count.
fn is_orphan(session: &CodexSession) -> bool {
    !session.archived && !session.indexed && !session.subagent
}

/// Re-adds every orphan to the index, drops rows whose rollout file is gone, and re-points
/// historical threads at the provider Codex is currently configured to use.
pub fn repair_visibility(home: &Path) -> Result<SessionRepairResult, RelayError> {
    let snapshot = overview(home)?;
    let mut index = read_index(home);
    let present: HashSet<&str> = snapshot.sessions.iter().filter(|session| !session.archived).map(|session| session.id.as_str()).collect();

    let removed_ghosts = index.len();
    index.retain(|entry| present.contains(entry.id.as_str()));
    let removed_ghosts = removed_ghosts - index.len();

    let restored = snapshot.sessions.iter().filter(|session| is_orphan(session)).map(|session| entry_for(session)).collect::<Vec<_>>();
    let restored_count = restored.len();
    index.extend(restored);

    let (aligned_threads, db_backup_path) = match snapshot.active_provider.as_deref() {
        Some(provider) => align_thread_providers(home, provider)?,
        None => (0, None),
    };

    if restored_count == 0 && removed_ghosts == 0 && aligned_threads == 0 {
        return Ok(SessionRepairResult::default());
    }
    let backup_path = if restored_count > 0 || removed_ghosts > 0 { write_index(home, index)? } else { None };
    Ok(SessionRepairResult { restored: restored_count, removed_ghosts, backup_path, aligned_threads, db_backup_path })
}

/// Moves a rollout out of Codex's reach without deleting anything.
pub fn archive(home: &Path, id: &str) -> Result<CodexSession, RelayError> {
    let snapshot = overview(home)?;
    let session = find(&snapshot, id)?;
    if session.archived {
        return Err(RelayError::InvalidInput("会话已经归档".into()));
    }
    let source = PathBuf::from(&session.path);
    let relative = source.strip_prefix(home.join(SESSIONS_DIR)).map(Path::to_path_buf).unwrap_or_else(|_| PathBuf::from(file_name(&source)));
    let target = home.join(ARCHIVE_DIR).join(&relative);
    move_file(&source, &target)?;

    let mut index = read_index(home);
    let before = index.len();
    index.retain(|entry| entry.id != session.id);
    if index.len() != before {
        write_index(home, index)?;
    }
    Ok(CodexSession { archived: true, indexed: false, path: target.display().to_string(), ..session })
}

/// Puts an archived rollout back and makes it resumable again.
pub fn restore(home: &Path, id: &str) -> Result<CodexSession, RelayError> {
    let snapshot = overview(home)?;
    let session = find(&snapshot, id)?;
    if !session.archived {
        return Err(RelayError::InvalidInput("会话没有归档，无需恢复".into()));
    }
    let source = PathBuf::from(&session.path);
    let relative = source.strip_prefix(home.join(ARCHIVE_DIR)).map(Path::to_path_buf).unwrap_or_else(|_| PathBuf::from(file_name(&source)));
    let target = home.join(SESSIONS_DIR).join(&relative);
    move_file(&source, &target)?;

    let restored = CodexSession { archived: false, indexed: true, path: target.display().to_string(), ..session };
    let mut index = read_index(home);
    if !index.iter().any(|entry| entry.id == restored.id) {
        index.push(entry_for(&restored));
        write_index(home, index)?;
    }
    Ok(restored)
}

/// Permanent delete, only reachable once a session has been archived.
pub fn delete(home: &Path, id: &str) -> Result<u64, RelayError> {
    let snapshot = overview(home)?;
    let session = find(&snapshot, id)?;
    if !session.archived {
        return Err(RelayError::InvalidInput("只能删除已归档的会话，请先归档".into()));
    }
    fs::remove_file(&session.path)?;
    let mut index = read_index(home);
    let before = index.len();
    index.retain(|entry| entry.id != session.id);
    if index.len() != before {
        write_index(home, index)?;
    }
    Ok(session.size_bytes)
}

pub fn session_path(home: &Path, id: &str) -> Result<String, RelayError> {
    Ok(find(&overview(home)?, id)?.path)
}

fn find(snapshot: &SessionOverview, id: &str) -> Result<CodexSession, RelayError> {
    snapshot.sessions.iter().find(|session| session.id == id).cloned().ok_or_else(|| RelayError::InvalidInput("找不到这个会话".into()))
}

fn entry_for(session: &CodexSession) -> IndexEntry {
    IndexEntry {
        id: session.id.clone(),
        thread_name: Some(session.title.clone()),
        updated_at: session.updated_at.map(|at| at.to_rfc3339_opts(SecondsFormat::Micros, true)),
        extra: Map::new(),
    }
}

/* ------------------------------------------------------------------ scanning */

fn collect_rollouts(root: &Path, archived: bool) -> Vec<(PathBuf, bool)> {
    let mut found = Vec::new();
    walk(root, 0, &mut found);
    found.into_iter().map(|path| (path, archived)).collect()
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, depth + 1, found);
        } else if is_rollout(&path) {
            found.push(path);
        }
    }
}

fn is_rollout(path: &Path) -> bool {
    let name = file_name(path);
    name.starts_with("rollout-") && name.ends_with(".jsonl")
}

fn file_name(path: &Path) -> String {
    path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_owned()
}

fn read_session(path: &Path, archived: bool, index: &HashMap<&str, &IndexEntry>) -> Option<CodexSession> {
    let metadata = fs::metadata(path).ok()?;
    let head = read_head(path, HEAD_SCAN_BYTES);
    let meta = first_meta(&head);
    let id = meta
        .as_ref()
        .and_then(|payload| payload.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .or_else(|| id_from_filename(&file_name(path)))?;

    let entry = index.get(id.as_str()).copied();
    let string = |key: &str| meta.as_ref().and_then(|payload| payload.get(key)).and_then(Value::as_str).map(str::to_owned);
    let thread_source = string("thread_source");
    let updated_at = entry
        .and_then(|entry| entry.updated_at.as_deref())
        .and_then(parse_time)
        .or_else(|| metadata.modified().ok().map(DateTime::<Utc>::from));

    let title = entry
        .and_then(|entry| entry.thread_name.clone())
        .filter(|name| !name.trim().is_empty())
        .or_else(|| derive_title(&head))
        .or_else(|| string("agent_nickname").map(|nickname| format!("子代理 · {nickname}")))
        .or_else(|| string("cwd").and_then(|cwd| cwd.rsplit(['\\', '/']).next().map(str::to_owned)))
        .unwrap_or_else(|| "未命名会话".into());

    Some(CodexSession {
        title,
        path: path.display().to_string(),
        cwd: string("cwd"),
        originator: string("originator"),
        cli_version: string("cli_version"),
        model_provider: string("model_provider"),
        agent_nickname: string("agent_nickname"),
        forked_from_id: string("forked_from_id"),
        created_at: string("timestamp").as_deref().and_then(parse_time),
        updated_at,
        size_bytes: metadata.len(),
        indexed: entry.is_some(),
        archived,
        via_relaydeck: string("model_provider").as_deref() == Some("relaydeck"),
        subagent: thread_source.as_deref() == Some("subagent"),
        thread_source,
        id,
    })
}

fn read_head(path: &Path, limit: u64) -> String {
    let Ok(file) = File::open(path) else { return String::new() };
    let mut buffer = Vec::new();
    let _ = file.take(limit).read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).into_owned()
}

fn first_meta(head: &str) -> Option<Value> {
    head.lines()
        .take(64)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|line| line.get("type").and_then(Value::as_str) == Some("session_meta"))
        .and_then(|line| line.get("payload").cloned())
}

/// `rollout-2026-07-23T12-15-47-019f8d2f-b8fb-7021-8f30-4c5006c1150b.jsonl` -> the trailing UUID.
fn id_from_filename(name: &str) -> Option<String> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let parts = stem.split('-').collect::<Vec<_>>();
    if parts.len() < 5 {
        return None;
    }
    Some(parts[parts.len() - 5..].join("-"))
}

/// First real user turn, skipping the injected environment/context blocks Codex prepends.
fn derive_title(head: &str) -> Option<String> {
    for line in head.lines().take(600) {
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        let payload = value.get("payload")?;
        let text = match value.get("type").and_then(Value::as_str) {
            Some("event_msg") if payload.get("type").and_then(Value::as_str) == Some("user_message") => {
                payload.get("message").and_then(Value::as_str).map(str::to_owned)
            }
            Some("response_item")
                if payload.get("type").and_then(Value::as_str) == Some("message")
                    && payload.get("role").and_then(Value::as_str) == Some("user") =>
            {
                payload
                    .get("content")
                    .and_then(Value::as_array)
                    .and_then(|items| items.iter().filter_map(|item| item.get("text").and_then(Value::as_str)).find(|text| is_user_text(text)))
                    .map(str::to_owned)
            }
            _ => None,
        };
        if let Some(text) = text.filter(|text| is_user_text(text)) {
            return Some(shorten(&text));
        }
    }
    None
}

/// Blocks Codex injects as a "user" turn: the file context header, and the harness prompt every
/// spawned thread starts with. Neither says anything about what the conversation is.
const INJECTED_PREFIXES: [&str; 2] = ["# Files mentioned", "The following is the Codex agent history"];

fn is_user_text(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && !trimmed.starts_with('<') && !INJECTED_PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix))
}

fn shorten(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= TITLE_LIMIT {
        return flat;
    }
    format!("{}…", flat.chars().take(TITLE_LIMIT).collect::<String>())
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).ok().map(|at| at.with_timezone(&Utc))
}

/* ----------------------------------------------------------- provider align */

/// The `model_provider` from `~/.codex/config.toml` — the filter Codex Desktop applies to its list.
fn active_provider(home: &Path) -> Option<String> {
    let text = fs::read_to_string(home.join("config.toml")).ok()?;
    let document = text.parse::<toml_edit::DocumentMut>().ok()?;
    document.get("model_provider").and_then(|item| item.as_str()).map(str::to_owned)
}

/// A provider that doesn't match hides the thread, and so does a NULL one.
const MISMATCH_WHERE: &str = "thread_source = 'user' AND (model_provider IS NULL OR model_provider <> ?1)";

/// User threads the configured provider can no longer see. Read-only; safe beside a live writer.
fn provider_mismatch_count(home: &Path, provider: &str) -> usize {
    let path = home.join(STATE_DBS[0]);
    let Ok(connection) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) else { return 0 };
    let _ = connection.busy_timeout(DB_BUSY_TIMEOUT);
    connection
        .query_row(&format!("SELECT COUNT(*) FROM threads WHERE {MISMATCH_WHERE}"), [provider], |row| row.get::<_, i64>(0))
        .map(|count| count as usize)
        .unwrap_or(0)
}

/// Points every user thread at `provider` so history stays visible after a switch — the same
/// strategy cockpit-tools applies with its `targetProvider`. Each database is backed up through
/// the SQLite backup API before the update. Returns (rows updated, main backup path).
pub fn align_thread_providers(home: &Path, provider: &str) -> Result<(usize, Option<String>), RelayError> {
    let mut aligned = 0;
    let mut main_backup = None;
    for (position, relative) in STATE_DBS.iter().enumerate() {
        let path = home.join(relative);
        if !path.exists() {
            continue;
        }
        let connection = Connection::open(&path)?;
        connection.busy_timeout(DB_BUSY_TIMEOUT)?;
        let has_threads: i64 =
            connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'threads'", [], |row| row.get(0))?;
        if has_threads == 0 {
            continue;
        }
        let pending: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM threads WHERE {MISMATCH_WHERE}"), [provider], |row| row.get(0))?;
        if pending == 0 {
            continue;
        }
        let backup = backup_db(&connection, &path)?;
        if position == 0 {
            main_backup = Some(backup);
        }
        aligned += connection.execute(&format!("UPDATE threads SET model_provider = ?1 WHERE {MISMATCH_WHERE}"), [provider])?;
    }
    Ok((aligned, main_backup))
}

/// Snapshot copy via the backup API: consistent even while Codex holds the WAL.
fn backup_db(connection: &Connection, path: &Path) -> Result<String, RelayError> {
    let target = path.with_file_name(format!("{}.relaydeck-backup-{}", file_name(path), Local::now().format("%Y%m%d-%H%M%S")));
    let mut destination = Connection::open(&target)?;
    let backup = rusqlite::backup::Backup::new(connection, &mut destination)?;
    backup.run_to_completion(256, Duration::from_millis(20), None)?;
    drop(backup);
    destination.close().map_err(|(_, error)| error)?;
    prune_db_backups(path);
    Ok(target.display().to_string())
}

fn prune_db_backups(db_path: &Path) {
    let prefix = format!("{}.relaydeck-backup-", file_name(db_path));
    let Some(parent) = db_path.parent() else { return };
    let Ok(entries) = fs::read_dir(parent) else { return };
    let mut backups = entries.flatten().map(|entry| entry.path()).filter(|path| file_name(path).starts_with(&prefix)).collect::<Vec<_>>();
    if backups.len() <= DB_BACKUPS_TO_KEEP {
        return;
    }
    backups.sort();
    for stale in backups.iter().take(backups.len() - DB_BACKUPS_TO_KEEP) {
        let _ = fs::remove_file(stale);
    }
}

/* --------------------------------------------------------------------- index */

fn read_index(home: &Path) -> Vec<IndexEntry> {
    let Ok(content) = fs::read_to_string(home.join(INDEX_FILE)) else { return Vec::new() };
    content.lines().filter(|line| !line.trim().is_empty()).filter_map(|line| serde_json::from_str(line).ok()).collect()
}

/// Backs the index up, then replaces it atomically. Returns the backup path.
fn write_index(home: &Path, mut entries: Vec<IndexEntry>) -> Result<Option<String>, RelayError> {
    entries.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
    let path = home.join(INDEX_FILE);
    let backup = if path.exists() {
        let backup = path.with_file_name(format!("{INDEX_BACKUP_PREFIX}{}", Local::now().format("%Y%m%d-%H%M%S")));
        fs::copy(&path, &backup)?;
        prune_backups(home, BACKUPS_TO_KEEP);
        Some(backup.display().to_string())
    } else {
        None
    };

    let mut body = String::new();
    for entry in &entries {
        body.push_str(&serde_json::to_string(entry)?);
        body.push('\n');
    }
    let tmp = path.with_extension("jsonl.relaydeck-tmp");
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(backup)
}

fn prune_backups(home: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(home) else { return };
    let mut backups = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| file_name(path).starts_with(INDEX_BACKUP_PREFIX))
        .collect::<Vec<_>>();
    if backups.len() <= keep {
        return;
    }
    backups.sort();
    for stale in backups.iter().take(backups.len() - keep) {
        let _ = fs::remove_file(stale);
    }
}

/// Rename when possible, copy + delete when the archive lands on another volume.
fn move_file(source: &Path, target: &Path) -> Result<(), RelayError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if target.exists() {
        return Err(RelayError::InvalidInput(format!("目标位置已存在同名文件: {}", target.display())));
    }
    if fs::rename(source, target).is_ok() {
        return Ok(());
    }
    fs::copy(source, target)?;
    fs::remove_file(source)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        let root = std::env::temp_dir().join(format!("relaydeck-sessions-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("sessions/2026/07/23")).unwrap();
        root
    }

    fn rollout(home: &Path, id: &str, extra_meta: &str, first_user: &str) -> PathBuf {
        let path = home.join("sessions/2026/07/23").join(format!("rollout-2026-07-23T12-15-47-{id}.jsonl"));
        let meta = format!(
            r#"{{"timestamp":"2026-07-23T02:48:10.175Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-07-23T02:48:04.915Z","cwd":"E:\\code\\RelayDeck","originator":"Codex Desktop","cli_version":"0.145.0"{extra_meta}}}}}"#
        );
        let user = format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"<environment_context>ignored</environment_context>"}},{{"type":"input_text","text":"{first_user}"}}]}}}}"#
        );
        fs::write(&path, format!("{meta}\n{user}\n")).unwrap();
        path
    }

    #[test]
    fn reads_metadata_and_falls_back_to_the_first_user_turn_for_a_title() {
        let home = temp_home();
        rollout(&home, "019f8d2f-b8fb-7021-8f30-4c5006c1150b", r#","model_provider":"relaydeck","thread_source":"user""#, "帮我优化保活策略");
        let snapshot = overview(&home).unwrap();
        assert_eq!(snapshot.sessions.len(), 1);
        let session = &snapshot.sessions[0];
        assert_eq!(session.title, "帮我优化保活策略");
        assert_eq!(session.cwd.as_deref(), Some("E:\\code\\RelayDeck"));
        assert!(session.via_relaydeck);
        assert!(!session.indexed);
        assert_eq!(snapshot.orphan_count, 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn index_titles_win_and_subagents_are_not_orphans() {
        let home = temp_home();
        rollout(&home, "019f8d2f-b8fb-7021-8f30-4c5006c1150b", r#","thread_source":"user""#, "第一个会话");
        rollout(&home, "019f8d2f-b8fb-7021-8f30-4c5006c1150c", r#","thread_source":"subagent""#, "子代理会话");
        fs::write(
            home.join(INDEX_FILE),
            "{\"id\":\"019f8d2f-b8fb-7021-8f30-4c5006c1150b\",\"thread_name\":\"来自索引的标题\",\"updated_at\":\"2026-07-23T13:59:02.830446Z\"}\n",
        )
        .unwrap();
        let snapshot = overview(&home).unwrap();
        let indexed = snapshot.sessions.iter().find(|session| session.indexed).unwrap();
        assert_eq!(indexed.title, "来自索引的标题");
        assert_eq!(snapshot.indexed_count, 1);
        assert_eq!(snapshot.orphan_count, 0, "subagent threads are never indexed by Codex");
        assert_eq!(snapshot.ghost_count, 0);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn repair_indexes_orphans_and_drops_dead_rows() {
        let home = temp_home();
        rollout(&home, "019f8d2f-b8fb-7021-8f30-4c5006c1150b", r#","thread_source":"user""#, "需要修复可见性");
        fs::write(home.join(INDEX_FILE), "{\"id\":\"gone-forever\",\"thread_name\":\"已删除\",\"updated_at\":\"2026-01-01T00:00:00Z\"}\n").unwrap();
        assert_eq!(overview(&home).unwrap().ghost_count, 1);

        let result = repair_visibility(&home).unwrap();
        assert_eq!(result.restored, 1);
        assert_eq!(result.removed_ghosts, 1);
        assert!(result.backup_path.is_some(), "the original index is always backed up");

        let snapshot = overview(&home).unwrap();
        assert_eq!(snapshot.orphan_count, 0);
        assert_eq!(snapshot.ghost_count, 0);
        assert_eq!(snapshot.sessions[0].title, "需要修复可见性");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn archive_hides_a_session_and_restore_brings_it_back() {
        let home = temp_home();
        let id = "019f8d2f-b8fb-7021-8f30-4c5006c1150b";
        let source = rollout(&home, id, r#","thread_source":"user""#, "归档我");
        repair_visibility(&home).unwrap();

        let archived = archive(&home, id).unwrap();
        assert!(archived.archived);
        assert!(!source.exists(), "the rollout moved out of the sessions folder");
        assert!(PathBuf::from(&archived.path).exists());
        let snapshot = overview(&home).unwrap();
        assert_eq!(snapshot.archived_count, 1);
        assert_eq!(snapshot.indexed_count, 0);
        assert_eq!(snapshot.orphan_count, 0, "archived sessions are not orphans");
        assert_eq!(snapshot.ghost_count, 0, "archiving also drops the index row");

        let restored = restore(&home, id).unwrap();
        assert!(!restored.archived);
        assert!(source.exists());
        assert_eq!(overview(&home).unwrap().indexed_count, 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn delete_requires_archiving_first() {
        let home = temp_home();
        let id = "019f8d2f-b8fb-7021-8f30-4c5006c1150b";
        rollout(&home, id, r#","thread_source":"user""#, "危险操作");
        assert!(delete(&home, id).is_err(), "a live session cannot be deleted");
        archive(&home, id).unwrap();
        assert!(delete(&home, id).unwrap() > 0);
        assert!(overview(&home).unwrap().sessions.is_empty());
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn aligns_user_threads_to_the_configured_provider_with_a_backup() {
        let home = temp_home();
        fs::write(home.join("config.toml"), "model_provider = \"relaydeck\"\n").unwrap();
        let connection = Connection::open(home.join(STATE_DBS[0])).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, thread_source TEXT, model_provider TEXT);
                 INSERT INTO threads VALUES ('a', 'user', 'codex_local_access');
                 INSERT INTO threads VALUES ('b', 'user', 'relaydeck');
                 INSERT INTO threads VALUES ('c', 'user', NULL);
                 INSERT INTO threads VALUES ('d', 'subagent', 'codex_local_access');",
            )
            .unwrap();
        drop(connection);

        let snapshot = overview(&home).unwrap();
        assert_eq!(snapshot.active_provider.as_deref(), Some("relaydeck"));
        assert_eq!(snapshot.provider_mismatch_count, 2, "the codex_local_access and NULL user threads are hidden");

        let result = repair_visibility(&home).unwrap();
        assert_eq!(result.aligned_threads, 2);
        assert!(result.db_backup_path.is_some());
        assert!(PathBuf::from(result.db_backup_path.unwrap()).exists());

        assert_eq!(overview(&home).unwrap().provider_mismatch_count, 0);
        let connection = Connection::open(home.join(STATE_DBS[0])).unwrap();
        let untouched: String =
            connection.query_row("SELECT model_provider FROM threads WHERE id = 'd'", [], |row| row.get(0)).unwrap();
        assert_eq!(untouched, "codex_local_access", "subagent threads are left alone");

        // A second repair finds nothing to do and does not create another backup.
        assert_eq!(repair_visibility(&home).unwrap().aligned_threads, 0);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn recovers_the_id_from_the_filename() {
        assert_eq!(
            id_from_filename("rollout-2026-07-23T12-15-47-019f8d2f-b8fb-7021-8f30-4c5006c1150b.jsonl").as_deref(),
            Some("019f8d2f-b8fb-7021-8f30-4c5006c1150b")
        );
        assert_eq!(id_from_filename("not-a-rollout.txt"), None);
    }
}
