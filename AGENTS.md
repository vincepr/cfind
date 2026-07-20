# Agent Guidelines for cfind

These rules apply to all contributors and coding agents working in this repository.

## Project Goals

- Keep `cfind` a fast, local, agent-oriented symbol search CLI.
- Prefer small, explicit, maintainable implementations over speculative abstractions.
- Keep runtime dependencies and compile times low. Use the standard library when it is sufficient;
  add a crate only when it provides substantial value that would be risky or costly to reproduce.
- Preserve predictable, compact output. Treat CLI text, ordering, exit behavior, and stderr/stdout
  placement as public interfaces consumed by coding agents.
- Support Linux, macOS, and Windows. Do not introduce platform-specific path, shell, or Git
  assumptions without an appropriate platform implementation.
- Prefer safe automatic recovery for disposable index state. A missing or incompatible index should
  rebuild cleanly rather than leave the user to repair SQLite manually.

## Repository Architecture

- `src/main.rs` owns the flat CLI and user-facing output.
- `src/config.rs` owns environment configuration and platform-specific index locations.
- `src/git.rs` owns repository discovery and local Git metadata. Normal indexing and searching must
  not make network calls or require remote authentication.
- `src/index.rs` owns SQLite schema, configuration metadata, and fresh index replacement.
- `src/search.rs` owns filtering and ranking.
- `src/language/mod.rs` owns shared Tree-sitter parsing and traversal.
- Each supported language owns its grammar metadata and symbol rules in
  `src/language/<language>.rs` by implementing `LanguageAdapter`.

Do not add a heuristic fallback for arbitrary Tree-sitter grammars. A language is supported only
when it has an explicit adapter and tests demonstrating useful symbol extraction.

## Rust Style

- Follow idiomatic Rust and `rustfmt`; use four-space indentation and no tabs.
- Use descriptive snake_case names for functions, variables, and modules; PascalCase for types and
  traits; and SCREAMING_SNAKE_CASE for constants.
- Keep functions and types focused. Prefer borrowing over ownership and early returns over deeply
  nested control flow.
- Prefer iterators when they are clearer, but do not force combinators when a loop is easier to read.
- Avoid unnecessary allocation in indexing and search hot paths. Use parallelism only for work large
  enough to benefit from it.
- Do not use `unsafe` unless it is essential and its invariants are documented.
- Do not use `unwrap()` in production code. `expect()` is acceptable only for a documented invariant;
  tests may use either.
- Propagate fallible operations with `Result` and `?`. Add actionable context at filesystem, Git,
  SQLite, and parsing boundaries.
- Add comments for non-obvious invariants and cross-platform behavior, not for code that explains
  itself. Keep existing comments accurate.
- Document public APIs when the contract is not already obvious from the name and types. Avoid large
  boilerplate documentation on narrow internal helpers.
- Do not use emoji or decorative Unicode in source, documentation, diagnostics, or CLI output.

## CLI and Configuration

- `CFIND_ROOT` is required. `CFIND_INDEX`, `CFIND_LANGUAGES`, and
  `CFIND_STALE_AFTER_HOURS` are optional.
- Keep `--help` concise but sufficient for an agent to discover every option and environment value.
- Preserve the flat query-first interface. Do not introduce nested subcommands without an explicit
  product decision.
- Search results go to stdout. Progress, warnings, and errors go to stderr when mixing them into
  stdout would make results harder to consume.
- Keep every result visually separated and avoid conditional formatting that makes fields ambiguous.
- Use repository-relative paths for filtering and absolute local paths for returned locations.
- Treat `--quiet`, `--verbose`, `--commit-url`, `--filter`, `--type`, and `--limit` as composable.
- Do not silently retain compatibility aliases for renamed flags or environment variables unless the
  user asks for a migration period.

## Index and Git Invariants

- Generated database filenames end in `.sqlite` and remain isolated by canonical `CFIND_ROOT`.
- Update the index format version whenever stored data semantics require existing indexes to rebuild.
- Replace incompatible indexes safely and record completion only after a successful indexing run.
- Build replacement indexes beside the active database and install them only after indexing
  succeeds. Failed indexing must leave the previous complete index available.
- Index only tracked, supported source files. Do not traverse ignored dependencies or untracked files
  as a fallback.
- Keep normal Git inspection local. Any future network freshness feature must be explicit and must not
  run once per repository during ordinary searches.
- Remote URLs should remain compact by default; commit-pinned URLs are opt-in.

## Language Adapters

- Put aliases, extensions, grammar selection, symbol-kind mapping, and language-specific syntax rules
  in the relevant adapter module.
- Keep shared traversal and `Symbol` construction in `src/language/mod.rs`.
- Normalize Tree-sitter node kinds into concise, stable symbol kinds used by `--type`.
- Add adapter tests for representative declarations and for special cases that exclude false
  positives.
- Preserve one-based source lines in indexed and displayed results.

## Testing

- Every behavior change and bug fix must update or add a regression test.
- Keep private-helper tests beside their module. Put public CLI and workspace behavior in `tests/`.
- Use temporary repositories and databases for integration tests. Do not depend on the developer's
  repositories, global Git configuration, network access, or existing index cache.
- Test both successful behavior and actionable failures for new user-facing inputs.
- Do not weaken tests to make an implementation pass.

Before handing off Rust changes, run:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
git diff --check
```

## Version Control and Security

- Preserve unrelated user changes in a dirty worktree.
- Never commit generated build output, temporary databases, credentials, tokens, or `.env` files.
- Do not leave debug output, commented-out code, or temporary compatibility paths.
- Use clear Conventional Commit messages such as `feat:`, `fix:`, `refactor:`, `test:`, and `docs:`.
- Commit, push, tag, publish, or open a pull request only when the user explicitly requests it.
- When creating a pull request, include Summary, Changes, Verification, and Risks/Notes.
