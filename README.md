# cfind

`cfind` is a local, Git-aware symbol indexer. It parses tracked Rust,
JavaScript, TypeScript, and C# files with Tree-sitter and returns both local
locations and commit-pinned GitHub or GitLab links.

Only files reported by `git ls-files` are indexed. Ignored dependencies, build
outputs, and other untracked files are excluded automatically.

## Install

```bash
cargo install --path .
```

## Configure

Configuration is environment-based:

```bash
export CFIND_ROOT="$HOME/code"
export CFIND_LANGUAGES="rust,javascript,typescript,csharp"
export CFIND_FETCH_STALE_DAYS=3
```

`CFIND_ROOT` is required; the tool exits without creating or opening an
index when it is unset or empty. `CFIND_LANGUAGES` defaults to all
supported languages. Indexes are stored in the operating system's user data
directory:

- Linux: `$XDG_DATA_HOME/cfind/indexes`, or
  `~/.local/share/cfind/indexes` when `XDG_DATA_HOME` is unset
- macOS: `~/Library/Application Support/cfind/indexes`
- Windows: `%LOCALAPPDATA%\cfind\indexes`

Each canonical `CFIND_ROOT` gets a stable, independent database in that
directory. Generated database names end in `.sqlite`.

The root also acts as the workspace selector. For example, these commands use
independent databases without any additional configuration:

```bash
CFIND_ROOT="$HOME/code/rust" cfind --index
CFIND_ROOT="$HOME/code" cfind --index

CFIND_ROOT="$HOME/code/rust" cfind DatabaseContext
CFIND_ROOT="$HOME/code" cfind DatabaseContext
```

Set `CFIND_INDEX` to override the database path explicitly.

Language aliases such as `rs`, `js`, `ts`, `cs`, and `c#` are accepted.

`CFIND_FETCH_STALE_DAYS` defaults to `3`. Results from a repository whose
last fetch is older than that threshold, whose current branch is not the cached
origin default branch, or whose fetch state is unknown include a compact
`local-state(...)` suffix. Fresh results include no state suffix. Set the value
to `0` to disable Git-state collection and output.

## Use

```bash
cfind --index
cfind DatabaseContext
cfind DatabaseContext --index
cfind DatabaseContext --from "$HOME/code/marketplace/api" --limit 10
cfind GzipDecompress -f '\.cs$'
cfind Config -f '^src/.*\.rs$'
cfind --type
cfind DatabaseContext --type class
cfind DatabaseContext --type class --verbose
cfind DatabaseContext --commit-url
cfind DatabaseContext --quiet
cfind --status
```

`--index` by itself reports indexing details and exits. With a query, it
refreshes the index silently before returning search results.

If no index exists for the selected root, the tool reports the new database
path, builds the index, and then continues with the requested search.
Both automatic and explicit indexing report the database path before indexing
starts.

Use `--filter` to restrict results by repository-relative file path using a
regular expression. Quote the expression so the shell passes it unchanged. For
example, `--filter '\.cs$'` matches C# files anywhere in a repository.
Searches return at most 10 results by default; use `--limit` to change that.
Pass `--quiet` to omit repository URLs from results, including when
`--commit-url` is also present.

Use `--type class` (or another indexed kind) to restrict symbol kinds. Run
`cfind --type` without a query or value to list every distinct kind in the
current index. Unknown kinds return an error containing the available values.

C# namespace declarations are indexed as searchable `namespace` symbols.
Containing namespaces are stored on other C# symbols and included in output
with `--verbose`.

Indexing is incremental. Git blob IDs identify unchanged files, tracked files
with uncommitted changes are re-parsed, changed files are parsed in parallel,
and all symbol updates are committed in batched SQLite transactions. Each index
stores the CLI version that created it; a version mismatch automatically forces
a complete re-index before searching.

Search ranking prioritizes exact names. If multiple symbols have the exact same
name, the result with the shortest directory distance from `--from` (the current
directory by default) appears first. Remaining candidates are ranked by edit
and Jaro-Winkler name similarity and then path proximity. This includes close
non-subsequence names such as `DatabaseEntity` and `MarketplaceContext` for a
`DatabaseContext` query. Each result includes a compact match score from `0` to
`10000`; exact names score `10000`.

GitHub and GitLab links use the repository's default or tracked branch to keep
normal output compact. Pass `--commit-url` to prefer an immutable URL using the
commit that was current during indexing; it falls back to the branch URL when a
commit URL is unavailable. URLs are omitted when neither form is available.
Re-run `cfind --index` after changing branches or commits to refresh links
and symbols. Searches also print a warning with the re-index command when the
last successful indexing run was more than one day ago.
