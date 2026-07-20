use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, bail};
use walkdir::{DirEntry, WalkDir};

use crate::config::SupportedLanguage;

#[derive(Debug, Clone)]
pub struct Repository {
    pub root: PathBuf,
    pub remote: Option<String>,
    pub revision: String,
    pub branch: Option<String>,
    pub origin_branch: Option<String>,
    pub current_branch: Option<String>,
    pub last_fetch_at: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct TrackedFile {
    pub path: String,
}

fn is_git_metadata(entry: &DirEntry) -> bool {
    entry.file_name() == OsStr::new(".git")
}

pub fn discover_repositories(root: &Path) -> Result<Vec<Repository>> {
    let mut roots = HashSet::new();

    if let Ok(top) = git_output(root, &["rev-parse", "--show-toplevel"]) {
        let top = PathBuf::from(top.trim());
        if top.starts_with(root) || root.starts_with(&top) {
            roots.insert(top);
        }
    }

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_git_metadata(entry))
    {
        let entry = entry
            .with_context(|| format!("could not inspect repositories under {}", root.display()))?;
        let path = entry.path();
        if path.join(".git").exists() {
            roots.insert(path.to_path_buf());
        }
    }

    let mut repositories = roots
        .into_iter()
        .map(|repo_root| {
            let revision = match git_output(&repo_root, &["rev-parse", "--verify", "HEAD"]) {
                Ok(revision) => revision.trim().to_owned(),
                Err(_) if git_output(&repo_root, &["symbolic-ref", "-q", "HEAD"]).is_ok() => {
                    // A repository can have an index and staged files before its
                    // first commit. There is no revision for remote links yet.
                    String::new()
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("could not read HEAD for {}", repo_root.display())
                    });
                }
            };
            let remote = git_output(&repo_root, &["remote", "get-url", "origin"])
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            let origin_branch = origin_branch(&repo_root);
            let tracked_branch = origin_branch
                .is_none()
                .then(|| tracked_branch(&repo_root))
                .flatten();
            let current_branch = current_branch_name(&repo_root);
            let branch = origin_branch
                .clone()
                .or(tracked_branch)
                .or_else(|| current_branch.clone());
            let last_fetch_at = last_fetch_at(&repo_root);
            Ok(Repository {
                root: repo_root,
                remote,
                revision,
                branch,
                origin_branch,
                current_branch,
                last_fetch_at,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    repositories.sort_by(|left, right| left.root.cmp(&right.root));
    Ok(repositories)
}

fn origin_branch(repository: &Path) -> Option<String> {
    let origin_head = git_output(
        repository,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|branch| branch.trim().strip_prefix("origin/").map(str::to_owned));
    if origin_head.is_some() {
        return origin_head;
    }

    git_output(
        repository,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/remotes/origin/main",
            "refs/remotes/origin/master",
        ],
    )
    .ok()
    .and_then(|branches| {
        branches
            .lines()
            .next()
            .and_then(|branch| branch.strip_prefix("origin/"))
            .map(str::to_owned)
    })
}

fn tracked_branch(repository: &Path) -> Option<String> {
    git_output(
        repository,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok()
    .and_then(|branch| branch.trim().strip_prefix("origin/").map(str::to_owned))
}

fn current_branch_name(repository: &Path) -> Option<String> {
    git_output(repository, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|branch| branch.trim().to_owned())
        .filter(|branch| !branch.is_empty())
}

fn last_fetch_at(repository: &Path) -> Option<u64> {
    git_metadata_time(repository, "FETCH_HEAD")
        // A clone creates the cached origin refs without creating FETCH_HEAD.
        // origin/HEAD's timestamp is therefore the best local clone-time
        // fallback until the first explicit fetch.
        .or_else(|| git_metadata_time(repository, "refs/remotes/origin/HEAD"))
}

fn git_metadata_time(repository: &Path, git_path: &str) -> Option<u64> {
    let path = git_output(repository, &["rev-parse", "--git-path", git_path])
        .ok()
        .map(|path| PathBuf::from(path.trim()))?;
    let path = if path.is_absolute() {
        path
    } else {
        repository.join(path)
    };
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

pub fn tracked_files(repository: &Repository) -> Result<Vec<TrackedFile>> {
    let deleted_paths = git_output_bytes(&repository.root, &["ls-files", "--deleted", "-z"])?
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| git_path(path, &repository.root))
        .filter_map(Result::transpose)
        .collect::<Result<HashSet<_>>>()?;
    let output = git_output_bytes(&repository.root, &["ls-files", "-s", "-z"])?;
    let mut files = Vec::new();
    for entry in output
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let Some(separator) = entry.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let metadata = std::str::from_utf8(&entry[..separator])
            .context("git ls-files returned non-UTF-8 metadata")?;
        let Some(path) = git_path(&entry[separator + 1..], &repository.root)? else {
            continue;
        };
        let mut fields = metadata.split_whitespace();
        let _mode = fields.next();
        let Some(_blob_id) = fields.next() else {
            continue;
        };
        let stage = fields.next().unwrap_or("0");
        if stage == "0" && !deleted_paths.contains(&path) {
            files.push(TrackedFile { path });
        }
    }
    Ok(files)
}

fn git_path(path: &[u8], repository: &Path) -> Result<Option<String>> {
    match std::str::from_utf8(path) {
        Ok(path) => Ok(Some(path.to_owned())),
        Err(_) if has_supported_source_extension(path) => bail!(
            "tracked source path is not valid UTF-8 in {}; cfind cannot index it",
            repository.display()
        ),
        Err(_) => Ok(None),
    }
}

fn has_supported_source_extension(path: &[u8]) -> bool {
    let extension = path.rsplit(|byte| *byte == b'.').next().unwrap_or_default();
    std::str::from_utf8(extension)
        .ok()
        .and_then(SupportedLanguage::from_extension)
        .is_some()
}

pub fn remote_file_url(
    remote: Option<&str>,
    revision: &str,
    relative_path: &str,
    start_line: usize,
    end_line: usize,
) -> Option<String> {
    remote_url(
        remote,
        revision,
        relative_path,
        Some((start_line, end_line)),
    )
}

pub fn remote_branch_file_url(
    remote: Option<&str>,
    branch: &str,
    relative_path: &str,
) -> Option<String> {
    remote_url(remote, branch, relative_path, None)
}

fn remote_url(
    remote: Option<&str>,
    reference: &str,
    relative_path: &str,
    lines: Option<(usize, usize)>,
) -> Option<String> {
    if reference.is_empty() {
        return None;
    }
    let remote = normalize_remote(remote?)?;
    let separator = if remote.host.contains("gitlab") {
        "/-/blob/"
    } else if remote.host.contains("github") {
        "/blob/"
    } else {
        return None;
    };
    let encoded_path = relative_path
        .split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/");
    let line_fragment = match lines {
        Some((start_line, end_line)) if start_line == end_line => format!("#L{start_line}"),
        Some((start_line, end_line)) => format!("#L{start_line}-L{end_line}"),
        None => String::new(),
    };
    Some(format!(
        "https://{}{}{}{}/{}{}",
        remote.host, remote.path, separator, reference, encoded_path, line_fragment
    ))
}

struct NormalizedRemote {
    host: String,
    path: String,
}

fn normalize_remote(value: &str) -> Option<NormalizedRemote> {
    let without_scheme = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .or_else(|| value.strip_prefix("ssh://"))
        .unwrap_or(value);
    let without_user = without_scheme
        .strip_prefix("git@")
        .unwrap_or(without_scheme);
    let (host, path) = if let Some((host, path)) = without_user.split_once(':') {
        if !host.contains('/') {
            (host, path)
        } else {
            without_user.split_once('/')?
        }
    } else {
        without_user.split_once('/')?
    };
    let path = format!("/{}", path.trim_matches('/').trim_end_matches(".git"));
    Some(NormalizedRemote {
        host: host.to_ascii_lowercase(),
        path,
    })
}

fn percent_encode_segment(segment: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn git_output(repository: &Path, args: &[&str]) -> Result<String> {
    String::from_utf8(git_output_bytes(repository, args)?).context("Git returned non-UTF-8 output")
}

fn git_output_bytes(repository: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .with_context(|| format!("could not execute git in {}", repository.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            repository.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_github_url_from_ssh_remote() {
        assert_eq!(
            remote_file_url(
                Some("git@github.com:acme/example.git"),
                "abc123",
                "src/a file.rs",
                3,
                8
            ),
            Some("https://github.com/acme/example/blob/abc123/src/a%20file.rs#L3-L8".to_owned())
        );
    }

    #[test]
    fn builds_gitlab_url_from_https_remote() {
        assert_eq!(
            remote_file_url(
                Some("https://gitlab.com/acme/example.git"),
                "abc123",
                "src/lib.rs",
                4,
                4
            ),
            Some("https://gitlab.com/acme/example/-/blob/abc123/src/lib.rs#L4".to_owned())
        );
    }

    #[test]
    fn rejects_non_utf8_git_paths() {
        let error = git_path(b"src/invalid-\xff.rs", Path::new("/work/example")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("tracked source path is not valid UTF-8")
        );
        assert!(error.to_string().contains("/work/example"));
        assert_eq!(
            git_path(b"assets/invalid-\xff.png", Path::new("/work/example")).unwrap(),
            None
        );
    }
}
