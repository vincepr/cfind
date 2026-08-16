use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use strsim::osa_distance;

use crate::{
    RoughSearchResult, SearchResult,
    git::{remote_branch_file_url, remote_file_url},
};

struct RankedResult {
    result: SearchResult,
    repository_root: String,
    full_coverage: bool,
    covered_terms: usize,
    exact_short_terms: usize,
    similarity: u32,
    is_test: bool,
    proximity: (usize, usize),
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
    let ranked = ranked_filtered_terms(
        connection,
        &terms,
        from,
        path_filter,
        symbol_kind,
        stale_after,
    )?;
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

/// Keeps the best-ranked match per repository so one query surfaces several repositories.
pub fn repository_search_filtered_terms(
    connection: &Connection,
    query_parts: &[String],
    from: &Path,
    limit: usize,
    path_filter: Option<&str>,
    symbol_kind: Option<&str>,
    stale_after: Option<Duration>,
) -> Result<Vec<RoughSearchResult>> {
    let terms = query_terms(query_parts)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let ranked = ranked_filtered_terms(
        connection,
        &terms,
        from,
        path_filter,
        symbol_kind,
        stale_after,
    )?;
    let mut repositories: HashMap<String, usize> = HashMap::new();
    let mut results: Vec<RoughSearchResult> = Vec::new();
    for item in ranked {
        // Count matches beyond the limit too, so the reported size hint stays complete.
        if let Some(index) = repositories.get(&item.repository_root).copied() {
            results[index].match_count += 1;
            continue;
        }
        repositories.insert(item.repository_root, results.len());
        results.push(RoughSearchResult {
            representative: item.result,
            match_count: 1,
            shared_directory: None,
        });
    }
    results.truncate(limit);
    Ok(results)
}

pub fn rough_search_filtered_terms(
    connection: &Connection,
    query_parts: &[String],
    from: &Path,
    limit: usize,
    path_filter: Option<&str>,
    symbol_kind: Option<&str>,
    stale_after: Option<Duration>,
) -> Result<Vec<RoughSearchResult>> {
    let terms = query_terms(query_parts)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let ranked = ranked_filtered_terms(
        connection,
        &terms,
        from,
        path_filter,
        symbol_kind,
        stale_after,
    )?;
    let matching_namespaces = ranked
        .iter()
        .filter(|item| item.result.kind == "namespace")
        .map(|item| {
            (
                item.repository_root.clone(),
                item.result.name.to_ascii_lowercase(),
            )
        })
        .collect::<HashSet<_>>();
    let enclosing_types = ranked
        .iter()
        .filter_map(|item| {
            enclosing_qualified_name(&item.result)
                .map(|qualified_name| (item.repository_root.clone(), qualified_name.to_owned()))
        })
        .collect::<HashSet<_>>();
    let mut namespace_groups: HashMap<(String, String), usize> = HashMap::new();
    let mut type_groups: HashMap<(String, String), usize> = HashMap::new();
    let mut results: Vec<RoughSearchResult> = Vec::new();
    for mut item in ranked {
        let namespace_name = if item.result.kind == "namespace" {
            Some(item.result.name.as_str())
        } else {
            item.result.namespace.as_deref()
        };
        let namespace_key =
            namespace_name.map(|name| (item.repository_root.clone(), name.to_ascii_lowercase()));
        if let Some(key) = namespace_key
            && matching_namespaces.contains(&key)
        {
            let directory = item
                .result
                .local_path
                .parent()
                .unwrap_or(&item.result.local_path)
                .to_path_buf();
            if let Some(index) = namespace_groups.get(&key).copied() {
                let group = &mut results[index];
                group.match_count += 1;
                group.shared_directory = Some(common_path(
                    group.shared_directory.as_deref().unwrap_or(&directory),
                    &directory,
                ));
                if item.result.kind == "namespace" && group.representative.kind != "namespace" {
                    item.result.match_score = item
                        .result
                        .match_score
                        .max(group.representative.match_score);
                    group.representative = item.result;
                }
            } else {
                let index = results.len();
                namespace_groups.insert(key, index);
                results.push(RoughSearchResult {
                    representative: item.result,
                    match_count: 1,
                    shared_directory: Some(directory),
                });
            }
        } else if let Some(key) = rough_type_key(&item, &enclosing_types) {
            if let Some(index) = type_groups.get(&key).copied() {
                let group = &mut results[index];
                group.match_count += 1;
                if item.result.qualified_name == key.1 {
                    item.result.match_score = item
                        .result
                        .match_score
                        .max(group.representative.match_score);
                    group.representative = item.result;
                }
            } else {
                let index = results.len();
                let enclosing_name = key.1.clone();
                type_groups.insert(key, index);
                let representative = if item.result.qualified_name == enclosing_name {
                    item.result
                } else {
                    enclosing_representative(
                        connection,
                        &item.repository_root,
                        &enclosing_name,
                        &item.result.relative_path,
                        item.result.match_score,
                        stale_after,
                    )?
                    .unwrap_or(item.result)
                };
                results.push(RoughSearchResult {
                    representative,
                    match_count: 1,
                    shared_directory: None,
                });
            }
        } else {
            results.push(RoughSearchResult {
                representative: item.result,
                match_count: 1,
                shared_directory: None,
            });
        }
    }
    results.truncate(limit);
    Ok(results)
}

fn enclosing_representative(
    connection: &Connection,
    repository_root: &str,
    qualified_name: &str,
    preferred_path: &str,
    match_score: u16,
    stale_after: Option<Duration>,
) -> Result<Option<SearchResult>> {
    let mut statement = connection.prepare(
        "SELECT s.name, s.qualified_name, s.kind, s.parent, s.namespace,
                s.start_line, s.end_line,
                f.path, r.remote, r.revision, r.branch,
                r.origin_branch, r.current_branch, r.last_fetch_at
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         JOIN repositories r ON r.id = f.repository_id
         WHERE r.root = ?1 AND s.qualified_name = ?2
         ORDER BY CASE WHEN f.path = ?3 THEN 0 ELSE 1 END, f.path, s.start_line
         LIMIT 1",
    )?;
    let row = statement
        .query_row(
            params![repository_root, qualified_name, preferred_path],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, usize>(5)?,
                    row.get::<_, usize>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<u64>>(13)?,
                ))
            },
        )
        .optional()?;
    let Some((
        name,
        qualified_name,
        kind,
        parent,
        namespace,
        start_line,
        end_line,
        relative_path,
        remote,
        revision,
        branch,
        origin_branch,
        current_branch,
        last_fetch_at,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(SearchResult {
        name,
        qualified_name,
        kind,
        match_score,
        namespace,
        parent,
        local_path: Path::new(repository_root).join(&relative_path),
        relative_path: relative_path.clone(),
        start_line,
        end_line,
        remote_url: branch
            .as_deref()
            .and_then(|branch| remote_branch_file_url(remote.as_deref(), branch, &relative_path)),
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
    }))
}

fn rough_type_key(
    item: &RankedResult,
    enclosing_types: &HashSet<(String, String)>,
) -> Option<(String, String)> {
    let own_key = (
        item.repository_root.clone(),
        item.result.qualified_name.clone(),
    );
    if enclosing_types.contains(&own_key) {
        return Some(own_key);
    }
    enclosing_qualified_name(&item.result)
        .map(|qualified_name| (item.repository_root.clone(), qualified_name.to_owned()))
}

fn enclosing_qualified_name(result: &SearchResult) -> Option<&str> {
    result.parent.as_ref()?;
    let enclosing = result.qualified_name.strip_suffix(&result.name)?;
    enclosing
        .strip_suffix("::")
        .or_else(|| enclosing.strip_suffix('.'))
}

fn ranked_filtered_terms(
    connection: &Connection,
    terms: &[String],
    from: &Path,
    path_filter: Option<&str>,
    symbol_kind: Option<&str>,
    stale_after: Option<Duration>,
) -> Result<Vec<RankedResult>> {
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
            proximity: proximity(from, &local_path),
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
            is_test: is_test_path(&relative_path),
        });
    }
    ranked.sort_by(compare_ranked);
    Ok(ranked)
}

fn common_path(left: &Path, right: &Path) -> PathBuf {
    left.components()
        .zip(right.components())
        .take_while(|(left, right)| left == right)
        .map(|(component, _)| component.as_os_str())
        .collect()
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
        .then_with(|| kind_rank(&left.result.kind).cmp(&kind_rank(&right.result.kind)))
        .then_with(|| left.is_test.cmp(&right.is_test))
        .then_with(|| right.proximity.0.cmp(&left.proximity.0))
        .then_with(|| left.proximity.1.cmp(&right.proximity.1))
        .then_with(|| left.result.local_path.cmp(&right.result.local_path))
        .then_with(|| left.result.start_line.cmp(&right.result.start_line))
}

/// Production code wins ties against its tests. Matches a `test`/`tests` directory or a file stem
/// ending in `test`, `tests`, or `spec`, which covers the C#, Rust, and TypeScript conventions.
fn is_test_path(relative_path: &str) -> bool {
    let (directories, file) = relative_path
        .rsplit_once('/')
        .unwrap_or(("", relative_path));
    let stem = file
        .rsplit_once('.')
        .map_or(file, |(stem, _)| stem)
        .to_ascii_lowercase();
    stem.ends_with("test")
        || stem.ends_with("tests")
        || stem.ends_with("spec")
        || directories.split('/').any(|directory| {
            directory.eq_ignore_ascii_case("test") || directory.eq_ignore_ascii_case("tests")
        })
}

/// Equally scored declarations answer "where does this live" best from the outside in.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "namespace" => 0,
        "class" | "struct" | "interface" | "enum" | "record" | "trait" | "type" | "delegate" => 1,
        _ => 2,
    }
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
    // Two-character terms only match at a word boundary; shorter ones would match nearly anything.
    if query_chars.len() >= 2
        && candidate
            .boundaries
            .iter()
            .any(|&position| candidate_chars[position..].starts_with(query_chars.as_slice()))
    {
        return 8_000 + length_closeness(query_chars.len(), candidate_chars.len(), 900);
    }
    if query_chars.len() >= 3 && find_subslice(candidate_chars, query_chars).is_some() {
        return 7_000 + length_closeness(query_chars.len(), candidate_chars.len(), 900);
    }
    if query_chars.len() >= 3 && is_subsequence(query_chars, candidate_chars) {
        // Alignment alone saturates on names with many word boundaries, so average it with how
        // much of the candidate the query covers: a tight match in a short name wins.
        let alignment = subsequence_quality(query_chars, candidate_chars, &candidate.boundaries);
        let coverage = length_closeness(query_chars.len(), candidate_chars.len(), 999);
        return 6_000 + (alignment + coverage) / 2;
    }

    // A typo match keeps the query intact except for up to two edits, so it stays below any
    // subsequence match; the score grows with how much of a long name the edits leave untouched.
    const TYPO_LIMIT: usize = 2;
    if query_chars.len() < 4 || query_chars.len().abs_diff(candidate_chars.len()) > TYPO_LIMIT {
        return 0;
    }
    let distance = osa_distance(&query.text, &candidate.text);
    if distance == 0 || distance > TYPO_LIMIT {
        return 0;
    }
    let longest = query_chars.len().max(candidate_chars.len()) as u32;
    5_000 + (999 * (longest - distance as u32)) / longest
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

/// Cheap allocation-free guard so only real subsequence matches pay for the alignment scoring.
fn is_subsequence(query: &[char], candidate: &[char]) -> bool {
    let mut remaining = query.iter();
    let mut wanted = remaining.next();
    for character in candidate {
        if wanted == Some(character) {
            wanted = remaining.next();
        }
    }
    wanted.is_none()
}

/// Alignment quality from 1 to 999 for a subsequence match: word-boundary hits earn a bonus and
/// gaps cost, so a tight, well-aligned match such as `artcl` in `Article` outranks a scattered one.
fn subsequence_quality(query: &[char], candidate: &[char], boundaries: &[usize]) -> u32 {
    const MATCH: i32 = 16;
    const GAP: i32 = 4;
    const START_BONUS: i32 = 22;
    const BOUNDARY_BONUS: i32 = 10;
    const UNMATCHED: i32 = i32::MIN / 4;
    const MAX_TAIL_PENALTY: usize = 12;

    let bonus = |position: usize| match position {
        0 => START_BONUS,
        _ if boundaries.binary_search(&position).is_ok() => BOUNDARY_BONUS,
        _ => 0,
    };
    // best[i] is the score of the best alignment ending with the current query character at i.
    let mut best = candidate
        .iter()
        .enumerate()
        .map(|(position, character)| {
            if character == &query[0] {
                MATCH + bonus(position)
            } else {
                UNMATCHED
            }
        })
        .collect::<Vec<_>>();
    let mut next = vec![UNMATCHED; candidate.len()];
    for query_character in &query[1..] {
        next.fill(UNMATCHED);
        // Carries the best earlier alignment, gap-adjusted so distant predecessors cost more.
        let mut carried = UNMATCHED;
        for position in 0..candidate.len() {
            if position > 0 && best[position - 1] > UNMATCHED {
                carried = carried.max(best[position - 1] + GAP * (position as i32 - 1));
            }
            if &candidate[position] == query_character && carried > UNMATCHED {
                next[position] = MATCH + bonus(position) + carried - GAP * (position as i32 - 1);
            }
        }
        std::mem::swap(&mut best, &mut next);
    }
    let mut score = UNMATCHED;
    let mut end = None;
    for (position, &candidate_score) in best.iter().enumerate() {
        if candidate_score > score {
            score = candidate_score;
            end = Some(position);
        }
    }
    let Some(end) = end else { return 1 };
    let tail = (candidate.len() - 1 - end).min(MAX_TAIL_PENALTY) as i32;
    let ideal = MATCH * query.len() as i32 + START_BONUS;
    (999 * (score - tail).max(1) / ideal).clamp(1, 999) as u32
}

/// Vicinity of a result to the search origin, best first: how many leading path components the two
/// share, then - only for results inside the origin's own subtree - how far below it they sit.
/// Results outside that subtree compare equal, so an unrelated repository is never preferred just
/// for sitting at a shallower path than another.
fn proximity(from: &Path, target: &Path) -> (usize, usize) {
    let from = if from.is_file() {
        from.parent().unwrap_or(from)
    } else {
        from
    };
    let target = target.parent().unwrap_or(target);
    let from_components = from.components().collect::<Vec<_>>();
    let target_components = target.components().collect::<Vec<_>>();
    let shared = from_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    let depth_below = if shared == from_components.len() {
        target_components.len() - shared
    } else {
        0
    };
    (shared, depth_below)
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
    fn proximity_prefers_nearby_directories_without_penalizing_deep_unrelated_ones() {
        let from = Path::new("/code/shop/api");
        let inside = proximity(from, Path::new("/code/shop/api/db/context.cs"));
        let nearer = proximity(from, Path::new("/code/shop/api/context.cs"));
        let outside = proximity(from, Path::new("/code/other/db/context.cs"));
        let deeply_outside = proximity(from, Path::new("/code/other/a/b/c/d/context.cs"));

        assert_eq!(nearer.0, inside.0);
        assert!(nearer.1 < inside.1, "closer inside the origin wins");
        assert!(inside.0 > outside.0, "sharing more of the origin wins");
        // Unrelated results compare equal, so later tie-breakers decide instead of path depth.
        assert_eq!(outside, deeply_outside);
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
    fn test_paths_are_recognized_without_flagging_similar_names() {
        assert!(is_test_path("tests/OrderApi.Tests/OrderDownloaderTests.cs"));
        assert!(is_test_path("src/Common/PriceUploaderTest.cs"));
        assert!(is_test_path("src/search/index_test.rs"));
        assert!(is_test_path("src/app/order.spec.ts"));
        assert!(!is_test_path("src/Common/LatestPrices.cs"));
        assert!(!is_test_path("src/Common/TestContainerFactory.cs"));
    }

    #[test]
    fn subsequence_matches_rank_by_alignment_quality() {
        // Every subsequence matches, but a tight one must beat a scattered one across a long name.
        let tight = name_similarity("artcl", "Article");
        let scattered = name_similarity("artcl", "AddReturnClient");
        assert!((6_000..7_000).contains(&tight));
        assert!((6_000..7_000).contains(&scattered));
        assert!(tight > scattered, "{tight} vs {scattered}");

        let context = name_similarity("Cntxt", "Context");
        let nested_context = name_similarity("Cntxt", "DataContextTests");
        assert!(context > nested_context, "{context} vs {nested_context}");
        assert!(name_similarity("psej", "PriceStockExportJob") > 6_000);
    }

    #[test]
    fn typo_matching_allows_two_edits_from_four_characters() {
        // "Artikel" is not a subsequence of "Article"; it is a substitution plus a transposition.
        let typo = name_similarity("Artikel", "Article");
        assert!((5_000..6_000).contains(&typo), "{typo}");
        assert_eq!(name_similarity("Cot", "Cat"), 0);
        assert_eq!(name_similarity("Artikel", "Warehouse"), 0);
    }

    #[test]
    fn two_character_terms_match_only_at_word_boundaries() {
        assert!((8_000..9_000).contains(&name_similarity("16", "Database_16")));
        assert!((8_000..9_000).contains(&name_similarity("16", "LaikaDatabase16")));
        assert!(name_similarity("16", "Database_16") > name_similarity("16", "LaikaDatabase16"));
        assert_eq!(name_similarity("16", "Database19"), 0);
        assert_eq!(name_similarity("as", "DatabaseContext"), 0);
        assert_eq!(name_similarity("se", "Database"), 0);
    }

    #[test]
    fn a_boundary_occurrence_outranks_an_earlier_ordinary_one() {
        // "cat" appears at index 1 and again at the "Catalog" boundary.
        assert!((8_000..9_000).contains(&name_similarity("cat", "ScatterCatalog")));
        assert!((7_000..8_000).contains(&name_similarity("cat", "Scatter")));
        assert!((8_000..9_000).contains(&name_similarity("Payment", "ProcessPaymentJob")));
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
