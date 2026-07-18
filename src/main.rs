use std::{env, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use code_search::{
    config::Config,
    index::{index_exists, index_is_stale, open_database, rebuild},
    search::{canonical_search_origin, distinct_symbol_kinds, require_query, search_filtered},
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Local code symbol search",
    after_help = "Examples:\n  code-search DatabaseContext\n  code-search GzipDecompress -f '\\.cs$'\n  code-search --type\n  code-search --index\n  code-search --status\n\nEnvironment:\n  CODE_SEARCH_ROOT=/path/to/code                         Required repository directory\n  CODE_SEARCH_INDEX=/path/to/index.sqlite                Optional exact database path\n  CODE_SEARCH_LANGUAGES=rust,javascript,typescript,csharp Optional languages (default: all)\n  CODE_SEARCH_FETCH_STALE_DAYS=3                          Fetch-age threshold; 0 disables Git state"
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
    /// Filter symbol kind; omit TYPE to list indexed kinds.
    #[arg(short = 't', long = "type", value_name = "TYPE", num_args = 0..=1, default_missing_value = "")]
    symbol_type: Option<String>,
    /// Prefer commit-pinned URLs; fall back to branch URLs.
    #[arg(long)]
    commit_url: bool,
    /// Omit repository URLs.
    #[arg(short, long)]
    quiet: bool,
    /// Include containing namespaces.
    #[arg(short, long)]
    verbose: bool,
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

    let list_types = cli.symbol_type.as_deref() == Some("");
    if cli.query.is_none() && !list_types {
        bail!("query required (or use --type to list indexed kinds)");
    }
    if cli.index {
        rebuild(&config)?;
    } else if !index_exists(&config.index_path)? {
        eprintln!("No index found.");
        eprintln!("Creating SQLite index at {}.", config.index_path.display());
        run_index(&config)?;
    }
    let connection = open_database(&config.index_path)?;
    let kinds = distinct_symbol_kinds(&connection)?;
    if list_types {
        for kind in kinds {
            println!("{kind}");
        }
        return Ok(());
    }

    let Some(query) = cli.query else {
        bail!("query required (or use --type to list indexed kinds)");
    };
    require_query(&query)?;
    let symbol_type = cli
        .symbol_type
        .as_deref()
        .map(|kind| kind.trim().to_ascii_lowercase());
    if let Some(kind) = symbol_type.as_deref()
        && !kinds.iter().any(|available| available == kind)
    {
        bail!(
            "unknown type '{kind}'; available types: {}",
            kinds.join(", ")
        );
    }
    let from = canonical_search_origin(
        &cli.from
            .unwrap_or(env::current_dir().context("could not determine current directory")?),
    )?;
    if index_is_stale(&connection)? {
        eprintln!(
            "warning: the code-search index is more than one day old; re-index with: {}",
            reindex_command(&config)
        );
    }
    let results = search_filtered(
        &connection,
        &query,
        &from,
        cli.limit,
        cli.filter.as_deref(),
        symbol_type.as_deref(),
        (config.fetch_stale_days > 0).then_some(config.fetch_stale_days),
    )?;
    for result in results {
        let parent = result
            .parent
            .as_deref()
            .map(|parent| format!(" in {parent}"))
            .unwrap_or_default();
        let git_state = result
            .git_state
            .as_deref()
            .map(|state| format!(" {state}"))
            .unwrap_or_default();
        println!(
            "{}  {}{}  {}{}\n  {}:{}",
            result.kind,
            result.name,
            parent,
            result.match_score,
            git_state,
            result.local_path.display(),
            result.start_line
        );
        if cli.verbose
            && let Some(namespace) = result.namespace.as_deref()
        {
            println!("  namespace {namespace}");
        }
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
