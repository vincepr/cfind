use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    Symbol,
    config::{Config, SupportedLanguage},
    git::{Repository, discover_repositories, tracked_files},
    language::parse_file,
};

#[derive(Debug, Default)]
pub struct IndexStats {
    pub repositories: usize,
    pub tracked_source_files: usize,
    pub parsed_files: usize,
    pub unchanged_files: usize,
    pub symbols: usize,
    pub elapsed_ms: u128,
}

pub const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

struct ParsedFile {
    path: String,
    blob_id: String,
    language: SupportedLanguage,
    symbols: Vec<Symbol>,
}

pub fn open_database(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create index directory {}", parent.display()))?;
    }
    let connection = Connection::open(path)
        .with_context(|| format!("could not open index at {}", path.display()))?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS repositories (
             id INTEGER PRIMARY KEY,
             root TEXT NOT NULL UNIQUE,
             remote TEXT,
             revision TEXT NOT NULL,
             branch TEXT
         );
         CREATE TABLE IF NOT EXISTS files (
             id INTEGER PRIMARY KEY,
             repository_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
             path TEXT NOT NULL,
             blob_id TEXT NOT NULL,
             language TEXT NOT NULL,
             UNIQUE(repository_id, path)
         );
         CREATE TABLE IF NOT EXISTS symbols (
             id INTEGER PRIMARY KEY,
             file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
             name TEXT NOT NULL,
             normalized_name TEXT NOT NULL,
             kind TEXT NOT NULL,
             namespace TEXT,
             start_line INTEGER NOT NULL,
             start_column INTEGER NOT NULL,
             end_line INTEGER NOT NULL,
             end_column INTEGER NOT NULL,
             parent TEXT
         );
         CREATE INDEX IF NOT EXISTS symbols_normalized_name ON symbols(normalized_name);
         CREATE INDEX IF NOT EXISTS symbols_file_id ON symbols(file_id);
         CREATE TABLE IF NOT EXISTS index_metadata (
             key TEXT PRIMARY KEY,
             value INTEGER NOT NULL
         );",
    )?;
    ensure_repository_branch_column(&connection)?;
    ensure_symbol_namespace_column(&connection)?;
    Ok(connection)
}

fn ensure_repository_branch_column(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(repositories)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    if !columns.contains("branch") {
        connection.execute("ALTER TABLE repositories ADD COLUMN branch TEXT", [])?;
    }
    Ok(())
}

fn ensure_symbol_namespace_column(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(symbols)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    if !columns.contains("namespace") {
        connection.execute("ALTER TABLE symbols ADD COLUMN namespace TEXT", [])?;
    }
    Ok(())
}

pub fn rebuild(config: &Config) -> Result<IndexStats> {
    let started = Instant::now();
    let repositories = discover_repositories(&config.root)?;
    let mut connection = open_database(&config.index_path)?;
    let mut stats = IndexStats {
        repositories: repositories.len(),
        ..IndexStats::default()
    };

    let active_roots = repositories
        .iter()
        .map(|repository| repository.root.to_string_lossy().into_owned())
        .collect::<HashSet<_>>();
    remove_missing_repositories(&connection, &config.root, &active_roots)?;

    for repository in &repositories {
        index_repository(&mut connection, repository, config, &mut stats)?;
    }
    stats.symbols = connection.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
    record_index_completion(&connection)?;
    stats.elapsed_ms = started.elapsed().as_millis();
    Ok(stats)
}

fn record_index_completion(connection: &Connection) -> Result<()> {
    let completed_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    connection.execute(
        "INSERT INTO index_metadata(key, value) VALUES ('completed_at', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [completed_at],
    )?;
    Ok(())
}

pub fn index_is_stale(connection: &Connection) -> Result<bool> {
    let completed_at = connection
        .query_row(
            "SELECT value FROM index_metadata WHERE key = 'completed_at'",
            [],
            |row| row.get::<_, u64>(0),
        )
        .optional()?;
    let Some(completed_at) = completed_at else {
        // Indexes created before freshness metadata was introduced should be
        // refreshed once before they can be considered current.
        return Ok(true);
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(now.saturating_sub(completed_at) > STALE_AFTER.as_secs())
}

fn index_repository(
    connection: &mut Connection,
    repository: &Repository,
    config: &Config,
    stats: &mut IndexStats,
) -> Result<()> {
    let tracked = tracked_files(repository)?
        .into_iter()
        .filter_map(|file| {
            let language = SupportedLanguage::from_path(Path::new(&file.path))?;
            config
                .languages
                .contains(&language)
                .then_some((file, language))
        })
        .collect::<Vec<_>>();
    stats.tracked_source_files += tracked.len();

    let root = repository.root.to_string_lossy().into_owned();
    connection.execute(
        "INSERT INTO repositories(root, remote, revision, branch) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(root) DO UPDATE SET remote = excluded.remote, revision = excluded.revision, branch = excluded.branch",
        params![
            root,
            repository.remote,
            repository.revision,
            repository.branch
        ],
    )?;
    let repository_id: i64 = connection.query_row(
        "SELECT id FROM repositories WHERE root = ?1",
        [&root],
        |row| row.get(0),
    )?;

    let existing = load_existing_files(connection, repository_id)?;
    let changed = tracked
        .iter()
        .filter(|(file, _)| file.dirty || existing.get(&file.path) != Some(&file.blob_id))
        .cloned()
        .collect::<Vec<_>>();
    stats.parsed_files += changed.len();
    stats.unchanged_files += tracked.len() - changed.len();

    let parsed = changed
        .par_iter()
        .map(|(file, language)| {
            let symbols = parse_file(&repository.root.join(&file.path), *language)?;
            Ok(ParsedFile {
                path: file.path.clone(),
                blob_id: file.blob_id.clone(),
                language: *language,
                symbols,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let tracked_paths = tracked
        .iter()
        .map(|(file, _)| file.path.as_str())
        .collect::<HashSet<_>>();
    let transaction = connection.transaction()?;
    for old_path in existing
        .keys()
        .filter(|path| !tracked_paths.contains(path.as_str()))
    {
        transaction.execute(
            "DELETE FROM files WHERE repository_id = ?1 AND path = ?2",
            params![repository_id, old_path],
        )?;
    }
    for file in parsed {
        transaction.execute(
            "INSERT INTO files(repository_id, path, blob_id, language) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repository_id, path) DO UPDATE SET blob_id = excluded.blob_id, language = excluded.language",
            params![repository_id, file.path, file.blob_id, file.language.as_str()],
        )?;
        let file_id: i64 = transaction.query_row(
            "SELECT id FROM files WHERE repository_id = ?1 AND path = ?2",
            params![repository_id, file.path],
            |row| row.get(0),
        )?;
        transaction.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;
        let mut insert = transaction.prepare_cached(
            "INSERT INTO symbols(
                file_id, name, normalized_name, kind, namespace, start_line,
                start_column, end_line, end_column, parent
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for symbol in file.symbols {
            insert.execute(params![
                file_id,
                symbol.name,
                symbol.name.to_ascii_lowercase(),
                symbol.kind,
                symbol.namespace,
                symbol.start_line,
                symbol.start_column,
                symbol.end_line,
                symbol.end_column,
                symbol.parent,
            ])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn load_existing_files(
    connection: &Connection,
    repository_id: i64,
) -> Result<HashMap<String, String>> {
    let mut statement =
        connection.prepare("SELECT path, blob_id FROM files WHERE repository_id = ?1")?;
    let rows = statement.query_map([repository_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(Into::into)
}

fn remove_missing_repositories(
    connection: &Connection,
    search_root: &Path,
    active_roots: &HashSet<String>,
) -> Result<()> {
    let mut statement = connection.prepare("SELECT root FROM repositories")?;
    let roots = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for root in roots {
        if Path::new(&root).starts_with(search_root) && !active_roots.contains(&root) {
            connection.execute("DELETE FROM repositories WHERE root = ?1", [&root])?;
        }
    }
    Ok(())
}

pub fn index_exists(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let connection = Connection::open(path)?;
    let symbols_exist = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'symbols'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !symbols_exist {
        return Ok(false);
    }
    let mut statement = connection.prepare("PRAGMA table_info(repositories)")?;
    let repository_columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    if !repository_columns.contains("branch") {
        return Ok(false);
    }
    let mut statement = connection.prepare("PRAGMA table_info(symbols)")?;
    let symbol_columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    if !symbol_columns.contains("namespace") {
        return Ok(false);
    }
    let metadata_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'index_metadata'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !metadata_exists {
        return Ok(false);
    }
    let completed = connection
        .query_row(
            "SELECT 1 FROM index_metadata WHERE key = 'completed_at'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_without_a_completed_run_is_stale() {
        let temporary = tempfile::TempDir::new().unwrap();
        let connection = open_database(&temporary.path().join("index.sqlite3")).unwrap();

        assert!(index_is_stale(&connection).unwrap());
        assert!(!index_exists(&temporary.path().join("index.sqlite3")).unwrap());
    }

    #[test]
    fn existing_indexes_gain_the_branch_column() {
        let temporary = tempfile::TempDir::new().unwrap();
        let path = temporary.path().join("index.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE repositories (
                    id INTEGER PRIMARY KEY,
                    root TEXT NOT NULL UNIQUE,
                    remote TEXT,
                    revision TEXT NOT NULL
                );
                CREATE TABLE files (
                    id INTEGER PRIMARY KEY,
                    repository_id INTEGER NOT NULL,
                    path TEXT NOT NULL,
                    blob_id TEXT NOT NULL,
                    language TEXT NOT NULL,
                    UNIQUE(repository_id, path)
                );
                CREATE TABLE symbols (
                    id INTEGER PRIMARY KEY,
                    file_id INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    normalized_name TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    start_line INTEGER NOT NULL,
                    start_column INTEGER NOT NULL,
                    end_line INTEGER NOT NULL,
                    end_column INTEGER NOT NULL,
                    parent TEXT
                );",
            )
            .unwrap();
        drop(connection);

        let connection = open_database(&path).unwrap();
        let mut statement = connection
            .prepare("PRAGMA table_info(repositories)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "branch"));
        let mut statement = connection.prepare("PRAGMA table_info(symbols)").unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "namespace"));
    }

    #[test]
    fn completed_index_becomes_stale_after_one_day() {
        let temporary = tempfile::TempDir::new().unwrap();
        let connection = open_database(&temporary.path().join("index.sqlite3")).unwrap();
        record_index_completion(&connection).unwrap();
        assert!(!index_is_stale(&connection).unwrap());
        assert!(index_exists(&temporary.path().join("index.sqlite3")).unwrap());

        let stale_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - STALE_AFTER.as_secs()
            - 1;
        connection
            .execute(
                "UPDATE index_metadata SET value = ?1 WHERE key = 'completed_at'",
                [stale_time],
            )
            .unwrap();

        assert!(index_is_stale(&connection).unwrap());
    }
}
