use std::{env, path::PathBuf};

use anyhow::{Context, Result, bail};
use cfind::{
    CollapsedResult, SearchResult,
    config::Config,
    index::{IndexState, index_state, open_database, rebuild},
    search::{
        Collapse, SearchOptions, canonical_search_origin, collapsed_search, distinct_symbol_kinds,
        match_tier, query_terms,
    },
};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Local code symbol search",
    after_help = "Path regex (-f): Rust regex crate, unanchored, matched against\nrepository-relative paths. No lookaround or backreferences.\n  -f '\\.cs$'              C# files\n  -f '^src/.*\\.rs$'       Rust files under src/\n  -f '\\.(cs|rs)$'         either extension\n  -f '(?i)payment'        case-insensitive, anywhere in the path\n\nEnvironment:\n  CFIND_ROOT=/path/to/code                         Required repository directory\n  CFIND_INDEX=/path/to/index.sqlite                Optional exact database path\n  CFIND_LANGUAGES=rust,javascript,typescript,csharp Optional languages (default: all)\n  CFIND_STALE_AFTER_HOURS=6                         Index warn age; rebuild 3x; 0 disables all three"
)]
struct Cli {
    /// Symbol name terms (fuzzy and qualified matching supported).
    #[arg(value_name = "QUERY")]
    query: Vec<String>,
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
    #[arg(short, long, default_value_t = 7)]
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
    /// Result granularity: one row per repository, per declaring type, or per match.
    #[arg(short, long, value_enum, default_value_t = CollapseArg::Repo)]
    collapse: CollapseArg,
    /// Print each symbol's fully qualified name.
    #[arg(long)]
    qualified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum CollapseArg {
    /// Every match on its own row.
    None,
    /// One row per declaring type; members fold into their type.
    Type,
    /// One row per repository.
    Repo,
}

impl From<CollapseArg> for Collapse {
    fn from(value: CollapseArg) -> Self {
        match value {
            CollapseArg::None => Collapse::None,
            CollapseArg::Type => Collapse::Type,
            CollapseArg::Repo => Collapse::Repository,
        }
    }
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
    if cli.index && cli.query.is_empty() {
        return run_index(&config);
    }
    if cli.status {
        return run_status(&config);
    }

    let list_types = cli.symbol_type.as_deref() == Some("");
    if cli.query.is_empty() && !list_types {
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

    let terms = query_terms(&cli.query)?;
    let query_label = terms.join(" ");
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
    let annotate_git_state = !config.stale_after.is_zero();
    let output = Output {
        quiet: cli.quiet,
        commit_url: cli.commit_url,
        qualified: cli.qualified,
        // Only an explicit collapse needs to say how much each row stands for.
        match_counts: cli.collapse == CollapseArg::Type,
    };
    let results = collapsed_search(
        &connection,
        &terms,
        SearchOptions {
            path_filter: cli.filter.as_deref(),
            symbol_kind: symbol_type.as_deref(),
            annotate_git_state,
            collapse: cli.collapse.into(),
            ..SearchOptions::new(&from, cli.limit)
        },
    )?;
    if results.is_empty() {
        bail!("no symbols matched '{query_label}' with the selected filters");
    }
    for result in results {
        print_result(&result, &output);
    }
    Ok(())
}

/// Presentation switches; output is agent-facing, so every line has to earn its tokens.
struct Output {
    quiet: bool,
    commit_url: bool,
    qualified: bool,
    match_counts: bool,
}

fn print_result(result: &CollapsedResult, output: &Output) {
    let representative = &result.representative;
    if output.match_counts {
        println!(
            "{}  matches={}",
            result_header(representative),
            result.match_count
        );
    } else {
        println!("{}", result_header(representative));
    }
    println!(
        "  {}:{}",
        representative.local_path.display(),
        representative.start_line
    );
    print_url(representative, output);
    print_result_footer(representative, output);
}

fn result_header(result: &SearchResult) -> String {
    let parent = result
        .parent
        .as_deref()
        .map(|parent| format!(" in {parent}"))
        .unwrap_or_default();
    let git_state = result
        .git_state
        .as_deref()
        .map(|state| format!("  {state}"))
        .unwrap_or_default();
    format!(
        "{}  {}  {}{}{}",
        match_tier(result.match_score),
        result.kind,
        result.name,
        parent,
        git_state
    )
}

fn print_url(result: &SearchResult, output: &Output) {
    let url = if output.quiet {
        None
    } else if output.commit_url {
        result
            .commit_url
            .as_deref()
            .or(result.remote_url.as_deref())
    } else {
        result.remote_url.as_deref()
    };
    if let Some(url) = url {
        println!("  {url}");
    }
}

fn print_result_footer(result: &SearchResult, output: &Output) {
    if output.qualified && result.qualified_name != result.name {
        println!("  {}", result.qualified_name);
    }
    println!();
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
    let hours = config.stale_after.as_secs() / (60 * 60);
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
