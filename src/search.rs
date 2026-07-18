use std::{
    cmp::Ordering,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::Connection;
use strsim::{jaro_winkler, normalized_levenshtein};

use crate::{
    SearchResult,
    git::{remote_branch_file_url, remote_file_url},
};

struct RankedResult {
    result: SearchResult,
    exactness: u8,
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
    fetch_stale_days: Option<u64>,
) -> Result<Vec<SearchResult>> {
    let query_normalized = query.to_ascii_lowercase();
    let path_filter = path_filter
        .map(Regex::new)
        .transpose()
        .context("invalid --filter regex")?;
    let mut statement = connection.prepare(
        "SELECT s.name, s.kind, s.parent, s.namespace, s.start_line, s.end_line,
                f.path, r.root, r.remote, r.revision, r.branch,
                r.origin_branch, r.current_branch, r.last_fetch_at,
                r.git_state_collected
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         JOIN repositories r ON r.id = f.repository_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, usize>(4)?,
            row.get::<_, usize>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<String>>(12)?,
            row.get::<_, Option<u64>>(13)?,
            row.get::<_, bool>(14)?,
        ))
    })?;

    let mut ranked = Vec::new();
    for row in rows {
        let (
            name,
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
            git_state_collected,
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
        let normalized = name.to_ascii_lowercase();
        let exactness = if normalized == query_normalized {
            3
        } else if normalized.starts_with(&query_normalized) {
            2
        } else if normalized.contains(&query_normalized) {
            1
        } else {
            0
        };
        let similarity = name_similarity(&query_normalized, &normalized);
        if exactness == 0 && similarity < 3_500 {
            continue;
        }
        let local_path = Path::new(&root).join(&relative_path);
        ranked.push(RankedResult {
            path_distance: path_distance(from, &local_path),
            result: SearchResult {
                name,
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
                git_state: fetch_stale_days.and_then(|days| {
                    stale_git_state(
                        remote.as_deref(),
                        origin_branch.as_deref(),
                        current_branch.as_deref(),
                        last_fetch_at,
                        days,
                        git_state_collected,
                    )
                }),
            },
            exactness,
            similarity,
        });
    }
    ranked.sort_by(compare_ranked);
    Ok(ranked
        .into_iter()
        .take(limit)
        .map(|item| item.result)
        .collect())
}

fn stale_git_state(
    remote: Option<&str>,
    origin_branch: Option<&str>,
    current_branch: Option<&str>,
    last_fetch_at: Option<u64>,
    stale_days: u64,
    collected: bool,
) -> Option<String> {
    if !collected || remote.is_none() || stale_days == 0 {
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
            if age_seconds > stale_days.saturating_mul(24 * 60 * 60) {
                reasons.push(format!("fetch>{}d", age_seconds / (24 * 60 * 60)));
            }
        }
        None => reasons.push("fetch-unknown".to_owned()),
    }

    (!reasons.is_empty()).then(|| format!("local-state({})", reasons.join(",")))
}

fn compare_ranked(left: &RankedResult, right: &RankedResult) -> Ordering {
    right
        .exactness
        .cmp(&left.exactness)
        .then_with(|| {
            if left.exactness == 3 && right.exactness == 3 {
                left.path_distance.cmp(&right.path_distance)
            } else {
                right.similarity.cmp(&left.similarity)
            }
        })
        .then_with(|| left.path_distance.cmp(&right.path_distance))
        .then_with(|| left.result.local_path.cmp(&right.result.local_path))
        .then_with(|| left.result.start_line.cmp(&right.result.start_line))
}

fn name_similarity(query: &str, candidate: &str) -> u32 {
    let edit = normalized_levenshtein(query, candidate);
    let jaro = jaro_winkler(query, candidate);
    (edit.max(jaro) * 10_000.0).round() as u32
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

pub fn require_query(query: &str) -> Result<()> {
    if query.trim().is_empty() {
        anyhow::bail!("search query cannot be empty");
    }
    Ok(())
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
    fn similarity_includes_non_subsequence_matches() {
        let query = "databasecontext";
        assert!(name_similarity(query, "databaseentity") >= 3_500);
        assert!(name_similarity(query, "marketplacecontext") >= 3_500);
        assert!(
            name_similarity(query, "databaseentity") > name_similarity(query, "marketplacecontext")
        );
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
                3,
                true,
            ),
            None
        );
        assert_eq!(
            stale_git_state(
                Some("git@example.com:acme/shop.git"),
                Some("main"),
                Some("feature/payments"),
                Some(now - 5 * 24 * 60 * 60),
                3,
                true,
            ),
            Some("local-state(not-origin-branch,fetch>5d)".to_owned())
        );
        assert_eq!(
            stale_git_state(
                Some("git@example.com:acme/shop.git"),
                Some("main"),
                Some("feature/payments"),
                None,
                0,
                false,
            ),
            None
        );
    }
}
