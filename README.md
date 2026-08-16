# cfind

`cfind` is a local, Git-aware symbol indexer. It parses tracked Rust,
JavaScript, TypeScript, and C# files with Tree-sitter and returns both local
locations and compact branch-based GitHub or GitLab links.

Only files reported by `git ls-files` are indexed. Ignored dependencies, build
outputs, and other untracked files are excluded automatically. When looking for
repositories, `node_modules`, `obj`, `bin`, and `target` directories inside a
repository are not descended into, so a repository vendored below one of them is
not indexed separately.

A file whose syntax tree would be disproportionately expensive — generated
sources such as inline byte arrays with millions of literals — is skipped after
a fixed number of parse steps and reported on stderr. The budget counts parse
steps rather than time, so the same sources are indexed on every machine.

## Install

```bash
cargo install --path .
```

## Configure

Configuration is environment-based:

```bash
export CFIND_ROOT="$HOME/code"
export CFIND_LANGUAGES="rust,javascript,typescript,csharp"
export CFIND_STALE_AFTER_HOURS=6
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

`CFIND_STALE_AFTER_HOURS` is the single freshness setting and defaults to `6`.
Searches warn when the index is older than that threshold and automatically
rebuild it after three times that age (18 hours by default). Set the value to
`0` to disable Git state annotations, index-age warnings, and automatic
age-based rebuilding.

Results from a repository with an `origin` remote carry `fetch:Nd`, the whole
days since it was last fetched (`fetch:?` when that is unknown). `branch:<name>`
follows when the checkout is not on the cached origin default branch, and
`branch:?` when neither branch can be determined. A repository without an
`origin` remote has nothing to compare against and carries no marker.

## Use

```bash
cfind --index
cfind DatabaseContext
cfind Acme Data DatabaseContext
cfind "Acme Data DatabaseContext"
cfind Acme.Data.DatabaseContext
cfind DatabaseContext --index
cfind DatabaseContext --from "$HOME/code/marketplace/api" --limit 10
cfind GzipDecompress -f '\.cs$'
cfind Config -f '^src/.*\.rs$'
cfind --type
cfind DatabaseContext --type class
cfind DatabaseContext --collapse none
cfind DatabaseContext --qualified
cfind Database --collapse type
cfind DatabaseContext --commit-url
cfind DatabaseContext --quiet
cfind --status
```

`--index` by itself reports indexing details and exits. With a query, it
refreshes the index silently before returning search results.

If no index exists for the selected root, the tool reports the new database
path, builds the index, and then continues with the requested search.
Both automatic and explicit indexing report the database path before indexing
starts. Automatic indexing writes its progress to stderr so search stdout
contains only results.

Use `--filter` to restrict results by repository-relative file path using a
regular expression. Quote the expression so the shell passes it unchanged. For
example, `--filter '\.cs$'` matches C# files anywhere in a repository.
Searches return at most 7 results by default; use `--limit` to change that.
Pass `--quiet` to omit repository URLs from results, including when
`--commit-url` is also present.

`--collapse` chooses how coarse a row is:

- `repo` (default) keeps the best-ranked match per repository, so the default
  limit of 7 surfaces 7 different repositories instead of 7 symbols from one.
- `type` keeps one row per declaring type, so members fold into the type they
  live in. The type's own row names the group when it matched the query too;
  otherwise its best member does, and the header still says which type that
  member is in. Only this mode prints a `matches=N` count.
- `none` prints every match.

Ranking is always symbol-level; only the grouping of printed rows differs.

Use `--type class` (or another indexed kind) to restrict symbol kinds. Run
`cfind --type` without a query or value to list every distinct kind in the
current index. Unknown kinds return an error containing the available values.
C# class, record, and struct declarations all use the `class` kind so searches
can treat them as the same type-level concept. Rust structs retain the `struct`
kind.

C# namespace declarations are indexed as searchable `namespace` symbols.
Containing namespaces and the full chain of enclosing indexed definitions are
stored as qualified names and searched alongside short names. Pass
`--qualified` to print the qualified name when it differs from the short name;
the file path usually implies it, so it is omitted by default.
Qualification uses language-appropriate separators (`.` for C#, JavaScript,
and TypeScript; `::` for Rust). Rust `impl` blocks are not indexed definitions,
so cfind does not invent an implementing-type qualification for methods inside
them.

A query may contain multiple whitespace-separated terms. Quoted and unquoted
whitespace have the same meaning: every term is scored against both the short
and qualified name, and candidates matching every term rank ahead of partial
matches. Qualified-name matches receive a small ranking discount so an exact
short-name match remains strongest. Terms containing only punctuation are
ignored; a query with no letters or numbers is rejected.

Indexing builds a fresh SQLite database beside the current index, parses tracked
source files in parallel, and replaces the old index only after the new one is
complete. Each index records its canonical root, normalized language set,
format version, and creation time. A configuration or version mismatch
automatically triggers a fresh rebuild before searching.

Search ranking uses explicit match tiers: exact name, prefix, word-boundary
substring, ordinary substring, ordered subsequence, and a typo match using
optimal string alignment distance. Matching is deliberately loose and lets
ranking sort it out:

- Two-character terms such as `16` match only at a word boundary, so
  `cfind Database 16` counts the numeric term as covered by `Database_16`
  without matching it anywhere else.
- Any ordered subsequence of three or more characters matches, scored by
  alignment quality and length coverage: word-boundary hits earn a bonus, gaps
  cost, and a tight match in a short name beats a scattered one in a long name,
  so `artcl` ranks `Article` above `AddReturnClient` and `DbCtx` finds
  `DbContext`.
- Terms of four or more characters tolerate up to two edits, scored by how much
  of the name the edits leave untouched, so `Artikel` still finds `Article`.

Each result opens with the band its match falls into - `exact`, `prefix`,
`substring`, `abbrev`, `typo`, or `weak` - so no score scale has to be
interpreted. Complete multi-term coverage, exact short-name matches, and score
are compared first. Equal scores prefer the outermost declaration - a namespace,
then a type, then a member - so a class outranks a like-named property, and
production code ahead of its tests. Next comes vicinity to `--from` (the current
directory by default): results sharing more of its path rank higher, and within
its own subtree the closest one wins. Results outside that subtree are equally
distant, so an unrelated repository is never preferred merely for sitting at a
shallower path. Paths and source lines provide deterministic final tie-breakers.

Repeated declarations of the same normalized namespace within one repository are
collapsed after ranking and before `--limit` is applied, at every `--collapse`
level. The best or nearest
declaration is retained. Identically named namespaces in separate repositories
and all non-namespace symbols remain separate results.

A valid search that has no remaining results, including after `--filter` or
`--type`, writes no result output to stdout, explains the miss on stderr, and
exits with status `1`. Listing kinds with `cfind --type` remains a successful
operation.

GitHub and GitLab links use the repository's default or tracked branch to keep
normal output compact, anchored to the symbol's declaration line. Pass
`--commit-url` to prefer an immutable URL using the commit that was current
during indexing; it spans the symbol's full line range and falls back to the
branch URL when a commit URL is unavailable. URLs are omitted when neither form is available.
Re-run `cfind --index` after changing branches or commits to refresh links and
symbols immediately. The configured age policy otherwise warns and eventually
rebuilds the index automatically.
