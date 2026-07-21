use std::{
    cmp::Ordering,
    collections::HashSet,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::Connection;
use strsim::osa_distance;

use crate::{
    SearchResult,
    git::{remote_branch_file_url, remote_file_url},
};

struct RankedResult {
    result: SearchResult,
    repository_root: String,
    full_coverage: bool,
    covered_terms: usize,
    exact_short_terms: usize,
    similarity: u32,
    path_distance: usize,
}

pub fn search(
    connection: &Connection,
    query: &str,
    from: &Path,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    search_filtered(connection, query, from, limit, None, None, None)
}

pub fn distinct_symbol_kinds(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare("SELECT DISTINCT kind FROM symbols ORDER BY kind")?;
    let kinds = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(kinds)
}

pub fn search_filtered(
    connection: &Connection,
    query: &str,
    from: &Path,
    limit: usize,
    path_filter: Option<&str>,
    symbol_kind: Option<&str>,
    stale_after: Option<Duration>,
) -> Result<Vec<SearchResult>> {
    search_filtered_terms(
        connection,
        &[query.to_owned()],
        from,
        limit,
        path_filter,
        symbol_kind,
        stale_after,
    )
}

pub fn search_filtered_terms(
    connection: &Connection,
    query_parts: &[String],
    from: &Path,
    limit: usize,
    path_filter: Option<&str>,
    symbol_kind: Option<&str>,
    stale_after: Option<Duration>,
) -> Result<Vec<SearchResult>> {
    let terms = query_terms(query_parts)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let canonical_terms = terms
        .iter()
        .map(|term| canonical_name(term))
        .collect::<Vec<_>>();
    let path_filter = path_filter
        .map(Regex::new)
        .transpose()
        .context("invalid --filter regex")?;
    let mut statement = connection.prepare(
        "SELECT s.name, s.qualified_name, s.kind, s.parent, s.namespace,
                s.start_line, s.end_line,
                f.path, r.root, r.remote, r.revision, r.branch,
                r.origin_branch, r.current_branch, r.last_fetch_at
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         JOIN repositories r ON r.id = f.repository_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, usize>(5)?,
            row.get::<_, usize>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<String>>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, Option<u64>>(14)?,
        ))
    })?;

    let mut ranked = Vec::new();
    for row in rows {
        let (
            name,
            qualified_name,
            kind,
            parent,
            namespace,
            start_line,
            end_line,
            relative_path,
            root,
            remote,
            revision,
            branch,
            origin_branch,
            current_branch,
            last_fetch_at,
        ) = row?;
        if symbol_kind.is_some_and(|filter| kind != filter) {
            continue;
        }
        if path_filter
            .as_ref()
            .is_some_and(|filter| !filter.is_match(&relative_path))
        {
            continue;
        }
        let mut covered_terms = 0;
        let mut exact_short_terms = 0;
        let short_name = canonical_name(&name);
        let qualified = canonical_name(&qualified_name);
        let mut total_similarity = 0_u64;
        for term in &canonical_terms {
            let short_score = canonical_similarity(term, &short_name);
            if short_score == 10_000 {
                exact_short_terms += 1;
            }
            let qualified_score = if qualified_name == name {
                0
            } else {
                match canonical_similarity(term, &qualified) {
                    10_000 => 9_900,
                    score => score * 95 / 100,
                }
            };
            let term_score = short_score.max(qualified_score);
            if term_score > 0 {
                covered_terms += 1;
                total_similarity += u64::from(term_score);
            }
        }
        if covered_terms == 0 {
            continue;
        }
        let similarity = (total_similarity / terms.len() as u64) as u32;
        let local_path = Path::new(&root).join(&relative_path);
        ranked.push(RankedResult {
            path_distance: path_distance(from, &local_path),
            result: SearchResult {
                name,
                qualified_name,
                kind,
                match_score: similarity.min(10_000) as u16,
                namespace,
                parent,
                local_path,
                relative_path: relative_path.clone(),
                start_line,
                end_line,
                remote_url: branch.as_deref().and_then(|branch| {
                    remote_branch_file_url(remote.as_deref(), branch, &relative_path)
                }),
                commit_url: remote_file_url(
                    remote.as_deref(),
                    &revision,
                    &relative_path,
                    start_line,
                    end_line,
                ),
                git_state: stale_after.and_then(|stale_after| {
                    stale_git_state(
                        remote.as_deref(),
                        origin_branch.as_deref(),
                        current_branch.as_deref(),
                        last_fetch_at,
                        stale_after,
                    )
                }),
            },
            repository_root: root,
            full_coverage: covered_terms == terms.len(),
            covered_terms,
            exact_short_terms,
            similarity,
        });
    }
    ranked.sort_by(compare_ranked);
    let mut namespaces = HashSet::new();
    let mut results = Vec::with_capacity(limit.min(ranked.len()));
    for item in ranked {
        if item.result.kind == "namespace"
            && !namespaces.insert((item.repository_root, item.result.name.to_ascii_lowercase()))
        {
            continue;
        }
        results.push(item.result);
        if results.len() == limit {
            break;
        }
    }
    Ok(results)
}

fn stale_git_state(
    remote: Option<&str>,
    origin_branch: Option<&str>,
    current_branch: Option<&str>,
    last_fetch_at: Option<u64>,
    stale_after: Duration,
) -> Option<String> {
    if remote.is_none() || stale_after.is_zero() {
        return None;
    }
    let mut reasons = Vec::new();
    match (current_branch, origin_branch) {
        (Some(current), Some(origin)) if current == origin => {}
        (_, Some(_)) => reasons.push("not-origin-branch".to_owned()),
        (_, None) => reasons.push("origin-branch-unknown".to_owned()),
    }

    match last_fetch_at {
        Some(fetched_at) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(fetched_at, |duration| duration.as_secs());
            let age_seconds = now.saturating_sub(fetched_at);
            if age_seconds > stale_after.as_secs() {
                reasons.push(format!("fetch>{}d", age_seconds / (24 * 60 * 60)));
            }
        }
        None => reasons.push("fetch-unknown".to_owned()),
    }

    (!reasons.is_empty()).then(|| format!("local-state({})", reasons.join(",")))
}

fn compare_ranked(left: &RankedResult, right: &RankedResult) -> Ordering {
    right
        .full_coverage
        .cmp(&left.full_coverage)
        .then_with(|| right.covered_terms.cmp(&left.covered_terms))
        .then_with(|| right.exact_short_terms.cmp(&left.exact_short_terms))
        .then_with(|| right.similarity.cmp(&left.similarity))
        .then_with(|| left.path_distance.cmp(&right.path_distance))
        .then_with(|| left.result.local_path.cmp(&right.result.local_path))
        .then_with(|| left.result.start_line.cmp(&right.result.start_line))
}

#[cfg(test)]
fn name_similarity(query: &str, candidate: &str) -> u32 {
    let query = canonical_name(query);
    let candidate = canonical_name(candidate);
    canonical_similarity(&query, &candidate)
}

fn canonical_similarity(query: &CanonicalName, candidate: &CanonicalName) -> u32 {
    if query.text.is_empty() || candidate.text.is_empty() {
        return 0;
    }
    if query.text == candidate.text {
        return 10_000;
    }

    let query_chars = &query.characters;
    let candidate_chars = &candidate.characters;
    if candidate_chars.starts_with(query_chars) {
        return 9_000 + length_closeness(query_chars.len(), candidate_chars.len(), 900);
    }
    if query_chars.len() >= 3
        && let Some(position) = find_subslice(candidate_chars, query_chars)
    {
        let closeness = length_closeness(query_chars.len(), candidate_chars.len(), 900);
        return if candidate.boundaries.contains(&position) {
            8_000 + closeness
        } else {
            7_000 + closeness
        };
    }
    if query_chars.len() >= 3
        && ordered_boundary_subsequence(query_chars, candidate_chars, &candidate.boundaries)
    {
        return 6_000 + length_closeness(query_chars.len(), candidate_chars.len(), 500);
    }

    let typo_limit = match query_chars.len() {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    };
    if typo_limit == 0 || query_chars.len().abs_diff(candidate_chars.len()) > typo_limit {
        return 0;
    }
    let distance = osa_distance(&query.text, &candidate.text);
    if distance > typo_limit {
        return 0;
    }
    5_000 + ((typo_limit - distance) as u32 * 250)
}

struct CanonicalName {
    text: String,
    characters: Vec<char>,
    boundaries: Vec<usize>,
}

fn canonical_name(value: &str) -> CanonicalName {
    let mut text = String::new();
    let mut boundaries = Vec::new();
    let mut previous: Option<char> = None;
    let mut position = 0;
    for character in value.chars() {
        if !character.is_alphanumeric() {
            previous = None;
            continue;
        }
        if previous.is_none()
            || previous.is_some_and(|previous| {
                (previous.is_lowercase() && character.is_uppercase())
                    || (previous.is_alphabetic() != character.is_alphabetic())
            })
        {
            boundaries.push(position);
        }
        for lowercase in character.to_lowercase() {
            text.push(lowercase);
            position += 1;
        }
        previous = Some(character);
    }
    let characters = text.chars().collect();
    CanonicalName {
        text,
        characters,
        boundaries,
    }
}

fn length_closeness(query_length: usize, candidate_length: usize, range: u32) -> u32 {
    (query_length.min(candidate_length) as u32 * range) / candidate_length.max(1) as u32
}

fn find_subslice(candidate: &[char], query: &[char]) -> Option<usize> {
    candidate
        .windows(query.len())
        .position(|window| window == query)
}

fn ordered_boundary_subsequence(query: &[char], candidate: &[char], boundaries: &[usize]) -> bool {
    let mut best = vec![None; query.len() + 1];
    best[0] = Some(0);
    for (position, candidate_character) in candidate.iter().enumerate() {
        let boundary_score = usize::from(boundaries.contains(&position));
        for query_position in (0..query.len()).rev() {
            if candidate_character == &query[query_position]
                && let Some(score) = best[query_position]
            {
                best[query_position + 1] =
                    best[query_position + 1].max(Some(score + boundary_score));
            }
        }
    }
    best[query.len()].unwrap_or(0) >= 2
}

fn path_distance(from: &Path, target: &Path) -> usize {
    let from = if from.is_file() {
        from.parent().unwrap_or(from)
    } else {
        from
    };
    let target = target.parent().unwrap_or(target);
    let from_components = from.components().collect::<Vec<_>>();
    let target_components = target.components().collect::<Vec<_>>();
    let common = from_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    from_components.len() + target_components.len() - (2 * common)
}

pub fn query_terms(query_parts: &[String]) -> Result<Vec<String>> {
    let terms = query_parts
        .iter()
        .flat_map(|part| part.split_whitespace())
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if query_parts.iter().all(|part| part.trim().is_empty()) {
        anyhow::bail!("search query cannot be empty");
    }
    if terms.is_empty() {
        anyhow::bail!("search query must contain a letter or number");
    }
    Ok(terms)
}

pub fn canonical_search_origin(path: &Path) -> Result<std::path::PathBuf> {
    path.canonicalize()
        .with_context(|| format!("search origin does not exist: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_distance_prefers_nearby_directories() {
        let from = Path::new("/code/shop/api");
        assert!(
            path_distance(from, Path::new("/code/shop/api/db/context.cs"))
                < path_distance(from, Path::new("/code/other/db/context.cs"))
        );
    }

    #[test]
    fn matching_uses_explainable_score_tiers() {
        let exact = name_similarity("DatabaseContext", "DatabaseContext");
        let prefix = name_similarity("Database", "DatabaseContext");
        let boundary_substring = name_similarity("Context", "DatabaseContext");
        let ordinary_substring = name_similarity("base", "DatabaseContext");
        let abbreviation = name_similarity("DbCtx", "DatabaseContext");
        let typo = name_similarity("DatabsaeContext", "DatabaseContext");

        assert_eq!(exact, 10_000);
        assert!((9_000..10_000).contains(&prefix));
        assert!((8_000..9_000).contains(&boundary_substring));
        assert!((7_000..8_000).contains(&ordinary_substring));
        assert!((6_000..7_000).contains(&abbreviation));
        assert!((5_000..6_000).contains(&typo));
        assert!(exact > prefix);
        assert!(prefix > boundary_substring);
        assert!(boundary_substring > ordinary_substring);
        assert!(ordinary_substring > abbreviation);
        assert!(abbreviation > typo);
    }

    #[test]
    fn typo_matching_is_bounded_by_query_length_and_distance() {
        assert_eq!(osa_distance("Cot", "Cat"), 1);
        assert_eq!(name_similarity("Cot", "Cat"), 0);

        assert_eq!(osa_distance("Artcle", "Article"), 1);
        assert!(name_similarity("Artcle", "Article") > 0);

        assert_eq!(osa_distance("abxdefyhij", "abcdefghij"), 2);
        assert!(name_similarity("abxdefyhij", "abcdefghij") > 0);

        assert_eq!(osa_distance("abxdefyhiz", "abcdefghij"), 3);
        assert_eq!(name_similarity("abxdefyhiz", "abcdefghij"), 0);

        assert_eq!(osa_distance("DatabaseContexts", "DatabaseContext"), 1);
        assert!(name_similarity("DatabaseContexts", "DatabaseContext") > 0);
    }

    #[test]
    fn matching_rejects_unrelated_and_short_coincidental_names() {
        assert_eq!(
            name_similarity("DefinitelyNoSuchSymbolQzx", "ArticleDto"),
            0
        );
        assert_eq!(name_similarity("id", "Build"), 0);
        assert_eq!(name_similarity("--", "DatabaseContext"), 0);
    }

    #[test]
    fn query_terms_make_quoted_whitespace_equivalent_and_ignore_punctuation() {
        assert_eq!(
            query_terms(&[
                "Acme Tools".to_owned(),
                "--".to_owned(),
                "Widget".to_owned()
            ])
            .unwrap(),
            ["Acme", "Tools", "Widget"]
        );
        assert!(query_terms(&["::".to_owned()]).is_err());
    }

    #[test]
    fn zero_limit_returns_no_results() {
        let connection = Connection::open_in_memory().unwrap();
        assert!(
            search_filtered_terms(
                &connection,
                &["Anything".to_owned()],
                Path::new("/code"),
                0,
                None,
                None,
                None,
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn multi_term_ranking_is_order_independent_and_keeps_partial_matches() {
        let connection = search_fixture();
        let forward = search_filtered_terms(
            &connection,
            &["Acme".to_owned(), "Widget".to_owned()],
            Path::new("/work"),
            10,
            None,
            Some("class"),
            None,
        )
        .unwrap();
        let reverse = search_filtered_terms(
            &connection,
            &["Widget".to_owned(), "Acme".to_owned()],
            Path::new("/work"),
            10,
            None,
            Some("class"),
            None,
        )
        .unwrap();

        let forward_names = forward
            .iter()
            .map(|result| result.name.as_str())
            .collect::<Vec<_>>();
        let reverse_names = reverse
            .iter()
            .map(|result| result.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(forward_names, reverse_names);
        assert_eq!(forward_names[0], "Widget");
        assert!(forward_names.contains(&"AcmeOnly"));
        assert_eq!(forward_names.last(), Some(&"AcmeOnly"));

        let short_exact = search_filtered_terms(
            &connection,
            &["Widget".to_owned()],
            Path::new("/work"),
            10,
            None,
            Some("class"),
            None,
        )
        .unwrap();
        assert_eq!(short_exact[0].name, "Widget");
        assert_eq!(short_exact[1].name, "Tools");
    }

    fn search_fixture() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE repositories (
                     id INTEGER PRIMARY KEY,
                     root TEXT NOT NULL,
                     remote TEXT,
                     revision TEXT NOT NULL,
                     branch TEXT,
                     origin_branch TEXT,
                     current_branch TEXT,
                     last_fetch_at INTEGER
                 );
                 CREATE TABLE files (
                     id INTEGER PRIMARY KEY,
                     repository_id INTEGER NOT NULL,
                     path TEXT NOT NULL
                 );
                 CREATE TABLE symbols (
                     file_id INTEGER NOT NULL,
                     name TEXT NOT NULL,
                     qualified_name TEXT NOT NULL,
                     kind TEXT NOT NULL,
                     parent TEXT,
                     namespace TEXT,
                     start_line INTEGER NOT NULL,
                     end_line INTEGER NOT NULL
                 );
                 INSERT INTO repositories(id, root, revision)
                     VALUES (1, '/work/repo', 'abc123');",
            )
            .unwrap();
        for (id, name, qualified_name) in [
            (1, "Widget", "Acme.Tools.Widget"),
            (2, "Tools", "Acme.Widget.Tools"),
            (3, "AcmeOnly", "AcmeOnly"),
        ] {
            connection
                .execute(
                    "INSERT INTO files(id, repository_id, path) VALUES (?1, 1, ?2)",
                    rusqlite::params![id, format!("src/{name}.cs")],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO symbols(
                         file_id, name, qualified_name, kind, start_line, end_line
                     ) VALUES (?1, ?2, ?3, 'class', 1, 1)",
                    rusqlite::params![id, name, qualified_name],
                )
                .unwrap();
        }
        connection
    }

    #[test]
    fn regex_filter_matches_nested_file_extensions() {
        let filter = Regex::new(r"\.cs$").unwrap();
        assert!(filter.is_match("algorithms/Deflate/GzipDecompress.cs"));
        assert!(!filter.is_match("src/search.rs"));
    }

    #[test]
    fn git_state_is_only_returned_when_stale() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(
            stale_git_state(
                Some("git@example.com:acme/shop.git"),
                Some("main"),
                Some("main"),
                Some(now - 2 * 24 * 60 * 60),
                Duration::from_secs(3 * 24 * 60 * 60),
            ),
            None
        );
        assert_eq!(
            stale_git_state(
                Some("git@example.com:acme/shop.git"),
                Some("main"),
                Some("feature/payments"),
                Some(now - 5 * 24 * 60 * 60),
                Duration::from_secs(3 * 24 * 60 * 60),
            ),
            Some("local-state(not-origin-branch,fetch>5d)".to_owned())
        );
        assert_eq!(
            stale_git_state(
                Some("git@example.com:acme/shop.git"),
                Some("main"),
                Some("feature/payments"),
                None,
                Duration::ZERO,
            ),
            None
        );
    }
}
