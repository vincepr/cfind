use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
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
    pub symbols: usize,
    pub elapsed_ms: u128,
}

const INDEX_VERSION: u64 = 9;

struct ParsedFile {
    path: String,
    language: SupportedLanguage,
    symbols: Vec<Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    Missing,
    ConfigurationMismatch,
    Fresh,
    Warn,
    Rebuild,
}

pub fn open_database(path: &Path) -> Result<Connection> {
    Connection::open(path).with_context(|| format!("could not open index at {}", path.display()))
}

fn create_database(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create index directory {}", parent.display()))?;
    }
    let connection = Connection::open(path)
        .with_context(|| format!("could not open index at {}", path.display()))?;
    connection.execute_batch(
        "PRAGMA journal_mode = DELETE;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS repositories (
             id INTEGER PRIMARY KEY,
             root TEXT NOT NULL UNIQUE,
             remote TEXT,
             revision TEXT NOT NULL,
             branch TEXT,
             origin_branch TEXT,
             current_branch TEXT,
             last_fetch_at INTEGER
         );
         CREATE TABLE IF NOT EXISTS files (
             id INTEGER PRIMARY KEY,
             repository_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
             path TEXT NOT NULL,
             language TEXT NOT NULL,
             UNIQUE(repository_id, path)
         );
         CREATE TABLE IF NOT EXISTS symbols (
             id INTEGER PRIMARY KEY,
             file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
             name TEXT NOT NULL,
             normalized_name TEXT NOT NULL,
             qualified_name TEXT NOT NULL,
             normalized_qualified_name TEXT NOT NULL,
             kind TEXT NOT NULL,
             namespace TEXT,
             start_line INTEGER NOT NULL,
             start_column INTEGER NOT NULL,
             end_line INTEGER NOT NULL,
             end_column INTEGER NOT NULL,
             parent TEXT
         );
         CREATE INDEX IF NOT EXISTS symbols_normalized_name ON symbols(normalized_name);
         CREATE INDEX IF NOT EXISTS symbols_normalized_qualified_name
             ON symbols(normalized_qualified_name);
         CREATE INDEX IF NOT EXISTS symbols_file_id ON symbols(file_id);
         CREATE TABLE IF NOT EXISTS index_metadata (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );",
    )?;
    Ok(connection)
}

pub fn rebuild(config: &Config) -> Result<IndexStats> {
    let started = Instant::now();
    let repositories = discover_repositories(&config.root)?;
    let temporary_path = reserve_temporary_index_path(&config.index_path)?;
    let mut connection = match create_database(&temporary_path) {
        Ok(connection) => connection,
        Err(error) => {
            remove_temporary_index(&temporary_path);
            return Err(error);
        }
    };
    let mut stats = IndexStats {
        repositories: repositories.len(),
        ..IndexStats::default()
    };

    let build_result = (|| {
        for repository in &repositories {
            index_repository(&mut connection, repository, config, &mut stats).with_context(
                || format!("could not index repository {}", repository.root.display()),
            )?;
        }
        stats.symbols =
            connection.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        record_index_configuration(&connection, config)?;
        connection.execute_batch("PRAGMA optimize;")?;
        Ok::<_, anyhow::Error>(())
    })();
    drop(connection);
    if let Err(error) = build_result {
        remove_temporary_index(&temporary_path);
        return Err(error);
    }
    if let Err(error) = replace_index(&temporary_path, &config.index_path) {
        remove_temporary_index(&temporary_path);
        return Err(error);
    }
    stats.elapsed_ms = started.elapsed().as_millis();
    Ok(stats)
}

fn reserve_temporary_index_path(index_path: &Path) -> Result<PathBuf> {
    if let Some(parent) = index_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create index directory {}", parent.display()))?;
    }
    let file_name = index_path
        .file_name()
        .context("index path must include a file name")?
        .to_string_lossy();
    for sequence in 0..1000 {
        let candidate = index_path.with_file_name(format!(
            ".{file_name}.tmp-{}-{sequence}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "could not create temporary index at {}",
                        candidate.display()
                    )
                });
            }
        }
    }
    anyhow::bail!(
        "could not reserve a temporary index beside {}",
        index_path.display()
    )
}

fn remove_temporary_index(path: &Path) {
    let _ = fs::remove_file(path);
    remove_index_sidecars(path);
}

fn remove_index_sidecars(path: &Path) {
    let path = path.to_string_lossy();
    let _ = fs::remove_file(format!("{path}-wal"));
    let _ = fs::remove_file(format!("{path}-shm"));
}

#[cfg(not(target_os = "windows"))]
fn replace_index(temporary_path: &Path, index_path: &Path) -> Result<()> {
    fs::rename(temporary_path, index_path).with_context(|| {
        format!(
            "could not replace index at {} with completed index {}",
            index_path.display(),
            temporary_path.display()
        )
    })?;
    remove_index_sidecars(index_path);
    Ok(())
}

#[cfg(target_os = "windows")]
fn replace_index(temporary_path: &Path, index_path: &Path) -> Result<()> {
    let backup_path = index_path.with_extension("sqlite.cfind-backup");
    if index_path.exists() {
        fs::rename(index_path, &backup_path).with_context(|| {
            format!("could not move existing index at {}", index_path.display())
        })?;
    }
    if let Err(error) = fs::rename(temporary_path, index_path) {
        if backup_path.exists() {
            let _ = fs::rename(&backup_path, index_path);
        }
        return Err(error).with_context(|| {
            format!(
                "could not install completed index at {}",
                index_path.display()
            )
        });
    }
    remove_index_sidecars(index_path);
    if backup_path.exists() {
        let _ = fs::remove_file(&backup_path);
    }
    Ok(())
}

fn record_index_configuration(connection: &Connection, config: &Config) -> Result<()> {
    let created_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let values = [
        ("version", INDEX_VERSION.to_string()),
        ("root", config.root.to_string_lossy().into_owned()),
        ("languages", configured_languages(config)),
        ("created_at", created_at.to_string()),
    ];
    for (key, value) in values {
        connection.execute(
            "INSERT INTO index_metadata(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(())
}

fn configured_languages(config: &Config) -> String {
    let mut languages = config
        .languages
        .iter()
        .map(|language| language.as_str())
        .collect::<Vec<_>>();
    languages.sort_unstable();
    languages.join(",")
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
    stats.parsed_files += tracked.len();

    let root = repository.root.to_string_lossy().into_owned();
    connection.execute(
        "INSERT INTO repositories(
             root, remote, revision, branch, origin_branch, current_branch,
             last_fetch_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ",
        params![
            root,
            repository.remote,
            repository.revision,
            repository.branch,
            repository.origin_branch,
            repository.current_branch,
            repository.last_fetch_at,
        ],
    )?;
    let repository_id: i64 = connection.query_row(
        "SELECT id FROM repositories WHERE root = ?1",
        [&root],
        |row| row.get(0),
    )?;

    let parsed = tracked
        .par_iter()
        .map(|(file, language)| {
            let symbols = parse_file(&repository.root.join(&file.path), *language)?;
            Ok(ParsedFile {
                path: file.path.clone(),
                language: *language,
                symbols,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let transaction = connection.transaction()?;
    for file in parsed {
        transaction.execute(
            "INSERT INTO files(repository_id, path, language) VALUES (?1, ?2, ?3)",
            params![repository_id, file.path, file.language.as_str()],
        )?;
        let file_id: i64 = transaction.query_row(
            "SELECT id FROM files WHERE repository_id = ?1 AND path = ?2",
            params![repository_id, file.path],
            |row| row.get(0),
        )?;
        let mut insert = transaction.prepare_cached(
            "INSERT INTO symbols(
                file_id, name, normalized_name, qualified_name,
                normalized_qualified_name, kind, namespace, start_line,
                start_column, end_line, end_column, parent
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        for symbol in file.symbols {
            insert.execute(params![
                file_id,
                symbol.name,
                symbol.name.to_ascii_lowercase(),
                symbol.qualified_name,
                symbol.qualified_name.to_ascii_lowercase(),
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

pub fn index_state(path: &Path, config: &Config) -> Result<IndexState> {
    if !path.exists() {
        return Ok(IndexState::Missing);
    }
    let connection = Connection::open(path)?;
    let metadata_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'index_metadata'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !metadata_exists {
        return Ok(IndexState::ConfigurationMismatch);
    }
    for table in ["repositories", "files", "symbols"] {
        let exists = connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Ok(IndexState::ConfigurationMismatch);
        }
    }

    let expected = [
        ("version", INDEX_VERSION.to_string()),
        ("root", config.root.to_string_lossy().into_owned()),
        ("languages", configured_languages(config)),
    ];
    for (key, expected_value) in expected {
        if metadata_value(&connection, key)?.as_deref() != Some(expected_value.as_str()) {
            return Ok(IndexState::ConfigurationMismatch);
        }
    }
    let Some(created_at) =
        metadata_value(&connection, "created_at")?.and_then(|value| value.parse::<u64>().ok())
    else {
        return Ok(IndexState::ConfigurationMismatch);
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let age = Duration::from_secs(now.saturating_sub(created_at));
    if config.stale_after.is_zero() {
        return Ok(IndexState::Fresh);
    }
    let rebuild_after = config.automatic_rebuild_after();
    if age > rebuild_after {
        Ok(IndexState::Rebuild)
    } else if age > config.stale_after {
        Ok(IndexState::Warn)
    } else {
        Ok(IndexState::Fresh)
    }
}

fn metadata_value(connection: &Connection, key: &str) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT value FROM index_metadata WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn test_config(root: &Path, stale_after: Duration) -> Config {
        Config {
            root: root.to_path_buf(),
            index_path: root.join("index.sqlite"),
            languages: HashSet::from([SupportedLanguage::Rust]),
            stale_after,
        }
    }

    #[test]
    fn missing_index_is_reported() {
        let temporary = tempfile::TempDir::new().unwrap();
        let config = test_config(temporary.path(), Duration::from_secs(6 * 60 * 60));
        assert_eq!(
            index_state(&config.index_path, &config).unwrap(),
            IndexState::Missing
        );
    }

    #[test]
    fn index_age_controls_warning_and_rebuild_states() {
        let temporary = tempfile::TempDir::new().unwrap();
        let config = test_config(temporary.path(), Duration::from_secs(10));
        rebuild(&config).unwrap();
        let connection = open_database(&config.index_path).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(
            index_state(&config.index_path, &config).unwrap(),
            IndexState::Fresh
        );
        connection
            .execute(
                "UPDATE index_metadata SET value = ?1 WHERE key = 'created_at'",
                [now - 11],
            )
            .unwrap();
        assert_eq!(
            index_state(&config.index_path, &config).unwrap(),
            IndexState::Warn
        );
        connection
            .execute(
                "UPDATE index_metadata SET value = ?1 WHERE key = 'created_at'",
                [now - 31],
            )
            .unwrap();
        assert_eq!(
            index_state(&config.index_path, &config).unwrap(),
            IndexState::Rebuild
        );

        let disabled_config = test_config(temporary.path(), Duration::ZERO);
        assert_eq!(
            index_state(&disabled_config.index_path, &disabled_config).unwrap(),
            IndexState::Fresh
        );
    }

    #[test]
    fn index_version_is_part_of_the_configuration() {
        let temporary = tempfile::TempDir::new().unwrap();
        let config = test_config(temporary.path(), Duration::from_secs(60));
        rebuild(&config).unwrap();
        let connection = open_database(&config.index_path).unwrap();
        let version: String = connection
            .query_row(
                "SELECT value FROM index_metadata WHERE key = 'version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "9");
        let columns = connection
            .prepare("PRAGMA table_info(symbols)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "qualified_name"));
        assert!(
            columns
                .iter()
                .any(|column| column == "normalized_qualified_name")
        );
        connection
            .execute(
                "UPDATE index_metadata SET value = '0' WHERE key = 'version'",
                [],
            )
            .unwrap();

        assert_eq!(
            index_state(&config.index_path, &config).unwrap(),
            IndexState::ConfigurationMismatch
        );
    }
}
