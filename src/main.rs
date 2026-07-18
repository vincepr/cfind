use std::{env, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use code_search::{
    config::Config,
    index::{index_exists, index_is_stale, open_database, rebuild},
    search::{canonical_search_origin, require_query, search_filtered},
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Local code symbol search",
    after_help = "Examples:\n  code-search DatabaseContext\n  code-search GzipDecompress -f '\\.cs$'\n  code-search --index\n  code-search --status\n\nEnvironment:\n  CODE_SEARCH_ROOT=/path/to/code                         Required repository directory\n  CODE_SEARCH_INDEX=/path/to/index.sqlite                Optional exact database path\n  CODE_SEARCH_LANGUAGES=rust,javascript,typescript,csharp Optional languages (default: all)"
)]
struct Cli {
    /// Symbol name (fuzzy matching supported).
    query: Option<String>,
    /// Rebuild first; exit with details when no query is given.
    #[arg(short, long, conflicts_with = "status")]
    index: bool,
    /// Show index path and counts, then exit.
    #[arg(short, long, conflicts_with = "index")]
    status: bool,
    /// Rank results from this directory.
    #[arg(long)]
    from: Option<PathBuf>,
    /// Maximum results.
    #[arg(short, long, default_value_t = 10)]
    limit: usize,
    /// Path regex (e.g. '\.cs$' or '\.(cs|rs)$').
    #[arg(short, long, value_name = "REGEX")]
    filter: Option<String>,
    /// Prefer commit-pinned URLs; fall back to branch URLs.
    #[arg(long)]
    commit_url: bool,
    /// Omit repository URLs.
    #[arg(short, long)]
    quiet: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::from_env()?;
    if cli.index && cli.query.is_none() {
        return run_index(&config);
    }
    if cli.status {
        return run_status(&config);
    }

    let Some(query) = cli.query else {
        bail!("query required (or use --index/--status); try `code-search --help`");
    };
    require_query(&query)?;
    if cli.index {
        rebuild(&config)?;
    } else if !index_exists(&config.index_path)? {
        eprintln!("No index found.");
        eprintln!("Creating SQLite index at {}.", config.index_path.display());
        run_index(&config)?;
    }
    let from = canonical_search_origin(
        &cli.from
            .unwrap_or(env::current_dir().context("could not determine current directory")?),
    )?;
    let connection = open_database(&config.index_path)?;
    if index_is_stale(&connection)? {
        eprintln!(
            "warning: the code-search index is more than one day old; re-index with: {}",
            reindex_command(&config)
        );
    }
    let results = search_filtered(&connection, &query, &from, cli.limit, cli.filter.as_deref())?;
    for result in results {
        let parent = result
            .parent
            .as_deref()
            .map(|parent| format!(" in {parent}"))
            .unwrap_or_default();
        println!(
            "{}  {}{}  {}\n  {}:{}",
            result.kind,
            result.name,
            parent,
            result.match_score,
            result.local_path.display(),
            result.start_line
        );
        let url = if cli.quiet {
            None
        } else if cli.commit_url {
            result.commit_url.or(result.remote_url)
        } else {
            result.remote_url
        };
        if let Some(url) = url {
            println!("  {url}");
        }
    }
    Ok(())
}

fn run_status(config: &Config) -> Result<()> {
    if !index_exists(&config.index_path)? {
        println!("No index at {}", config.index_path.display());
        return Ok(());
    }
    let connection = open_database(&config.index_path)?;
    let repositories: usize =
        connection.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;
    let files: usize = connection.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
    let symbols: usize =
        connection.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
    println!("Index: {}", config.index_path.display());
    println!("Repositories: {repositories}");
    println!("Files: {files}");
    println!("Symbols: {symbols}");
    Ok(())
}

fn run_index(config: &Config) -> Result<()> {
    eprintln!(
        "Indexing {} and writing to {}.",
        config.root.display(),
        config.index_path.display()
    );
    let stats = rebuild(config)?;
    println!(
        "Indexed {} symbols from {} source files in {} repositories ({} parsed, {} unchanged) in {} ms.",
        stats.symbols,
        stats.tracked_source_files,
        stats.repositories,
        stats.parsed_files,
        stats.unchanged_files,
        stats.elapsed_ms
    );
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn reindex_command(config: &Config) -> String {
    fn quote(value: &std::path::Path) -> String {
        format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
    }

    format!(
        "CODE_SEARCH_ROOT={} CODE_SEARCH_INDEX={} code-search --index",
        quote(&config.root),
        quote(&config.index_path)
    )
}

#[cfg(target_os = "windows")]
fn reindex_command(config: &Config) -> String {
    fn quote(value: &std::path::Path) -> String {
        format!("'{}'", value.to_string_lossy().replace('\'', "''"))
    }

    format!(
        "$env:CODE_SEARCH_ROOT={}; $env:CODE_SEARCH_INDEX={}; code-search --index",
        quote(&config.root),
        quote(&config.index_path)
    )
}
