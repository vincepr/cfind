use std::{env, path::PathBuf};

use anyhow::{Context, Result, bail};
use cfind::{
    config::Config,
    index::{IndexState, index_state, open_database, rebuild},
    search::{canonical_search_origin, distinct_symbol_kinds, require_query, search_filtered},
};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Local code symbol search",
    after_help = "Examples:\n  cfind DatabaseContext\n  cfind GzipDecompress -f '\\.cs$'\n  cfind --type\n  cfind --index\n  cfind --status\n\nEnvironment:\n  CFIND_ROOT=/path/to/code                         Required repository directory\n  CFIND_INDEX=/path/to/index.sqlite                Optional exact database path\n  CFIND_LANGUAGES=rust,javascript,typescript,csharp Optional languages (default: all)\n  CFIND_FETCH_STALE_DAYS=3                          Fetch-age threshold; 0 disables Git state\n  CFIND_WARN_AFTER_HOURS=6                          Warn about index age; rebuild after 3x"
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
    let warn_about_age = if cli.index {
        rebuild(&config)?;
        false
    } else {
        ensure_index(&config)?
    };
    let connection = open_database(&config.index_path)?;
    if warn_about_age {
        eprintln!(
            "warning: the cfind index is older than {}; re-index with: {}",
            warning_period(&config),
            reindex_command(&config)
        );
    }
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
        if cli.verbose
            && let Some(namespace) = result.namespace.as_deref()
        {
            println!("  {namespace}");
        }
        println!();
    }
    Ok(())
}

fn run_status(config: &Config) -> Result<()> {
    match index_state(&config.index_path, config)? {
        IndexState::Missing => {
            println!("No index at {}", config.index_path.display());
            return Ok(());
        }
        IndexState::ConfigurationMismatch => {
            println!(
                "Index configuration does not match at {}",
                config.index_path.display()
            );
            return Ok(());
        }
        IndexState::Fresh | IndexState::Warn | IndexState::Rebuild => {}
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
    let stats = rebuild_with_progress(config)?;
    println!("{}", index_summary(&stats));
    Ok(())
}

fn run_automatic_index(config: &Config) -> Result<()> {
    let stats = rebuild_with_progress(config)?;
    eprintln!("{}", index_summary(&stats));
    Ok(())
}

fn ensure_index(config: &Config) -> Result<bool> {
    match index_state(&config.index_path, config)? {
        IndexState::Missing => {
            eprintln!("No index found.");
            eprintln!("Creating SQLite index at {}.", config.index_path.display());
            run_automatic_index(config)?;
        }
        IndexState::ConfigurationMismatch => {
            eprintln!("Index configuration changed; rebuilding SQLite index.");
            run_automatic_index(config)?;
        }
        IndexState::Rebuild => {
            eprintln!("Index age exceeded the automatic rebuild threshold; rebuilding.");
            run_automatic_index(config)?;
        }
        IndexState::Warn => return Ok(true),
        IndexState::Fresh => {}
    }
    Ok(false)
}

fn warning_period(config: &Config) -> String {
    let hours = config.warn_after.as_secs() / (60 * 60);
    if hours == 1 {
        "1 hour".to_owned()
    } else {
        format!("{hours} hours")
    }
}

fn rebuild_with_progress(config: &Config) -> Result<cfind::index::IndexStats> {
    eprintln!(
        "Indexing {} and writing to {}.",
        config.root.display(),
        config.index_path.display()
    );
    rebuild(config)
}

fn index_summary(stats: &cfind::index::IndexStats) -> String {
    format!(
        "Indexed {} symbols from {} source files in {} repositories ({} parsed) in {} ms.",
        stats.symbols,
        stats.tracked_source_files,
        stats.repositories,
        stats.parsed_files,
        stats.elapsed_ms
    )
}

#[cfg(not(target_os = "windows"))]
fn reindex_command(config: &Config) -> String {
    fn quote(value: &std::path::Path) -> String {
        format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
    }

    format!(
        "CFIND_ROOT={} CFIND_INDEX={} cfind --index",
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
        "$env:CFIND_ROOT={}; $env:CFIND_INDEX={}; cfind --index",
        quote(&config.root),
        quote(&config.index_path)
    )
}
