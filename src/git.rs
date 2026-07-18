use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone)]
pub struct Repository {
    pub root: PathBuf,
    pub remote: Option<String>,
    pub revision: String,
    pub branch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TrackedFile {
    pub path: String,
    pub blob_id: String,
    pub dirty: bool,
}

fn is_git_metadata(entry: &DirEntry) -> bool {
    entry.file_name() == OsStr::new(".git")
}

pub fn discover_repositories(root: &Path) -> Result<Vec<Repository>> {
    let mut roots = HashSet::new();

    if git_output(root, &["rev-parse", "--show-toplevel"]).is_ok() {
        let top = git_output(root, &["rev-parse", "--show-toplevel"])?;
        let top = PathBuf::from(top.trim());
        if top.starts_with(root) || root.starts_with(&top) {
            roots.insert(top);
        }
    }

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_git_metadata(entry))
        .filter_map(Result::ok)
    {
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
            let branch = remote_branch(&repo_root);
            Ok(Repository {
                root: repo_root,
                remote,
                revision,
                branch,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    repositories.sort_by(|left, right| left.root.cmp(&right.root));
    Ok(repositories)
}

fn remote_branch(repository: &Path) -> Option<String> {
    let origin_head = git_output(
        repository,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|branch| branch.trim().strip_prefix("origin/").map(str::to_owned));
    if origin_head.is_some() {
        return origin_head;
    }

    let upstream = git_output(
        repository,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok()
    .and_then(|branch| branch.trim().strip_prefix("origin/").map(str::to_owned));
    if upstream.is_some() {
        return upstream;
    }

    git_output(repository, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|branch| branch.trim().to_owned())
        .filter(|branch| !branch.is_empty())
}

pub fn tracked_files(repository: &Repository) -> Result<Vec<TrackedFile>> {
    let dirty_paths = git_output_bytes(&repository.root, &["diff-files", "--name-only", "-z"])?
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<HashSet<_>>();
    let deleted_paths = git_output_bytes(&repository.root, &["ls-files", "--deleted", "-z"])?
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<HashSet<_>>();
    let output = git_output_bytes(&repository.root, &["ls-files", "-s", "-z"])?;
    let mut files = Vec::new();
    for entry in output
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = String::from_utf8_lossy(entry);
        let Some((metadata, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut fields = metadata.split_whitespace();
        let _mode = fields.next();
        let Some(blob_id) = fields.next() else {
            continue;
        };
        let stage = fields.next().unwrap_or("0");
        if stage == "0" && !deleted_paths.contains(path) {
            files.push(TrackedFile {
                path: path.to_owned(),
                blob_id: blob_id.to_owned(),
                dirty: dirty_paths.contains(path),
            });
        }
    }
    if repository.revision.is_empty() {
        let indexed_paths = files
            .iter()
            .map(|file| file.path.clone())
            .collect::<HashSet<_>>();
        let untracked = git_output_bytes(
            &repository.root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        for path in untracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path = String::from_utf8_lossy(path).into_owned();
            if indexed_paths.contains(path.as_str()) {
                continue;
            }
            let blob_id = git_output(&repository.root, &["hash-object", "--", &path])?;
            files.push(TrackedFile {
                path,
                blob_id: blob_id.trim().to_owned(),
                dirty: true,
            });
        }
    }
    Ok(files)
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
    segment
        .bytes()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                vec![byte as char]
            } else {
                format!("%{byte:02X}").chars().collect()
            }
        })
        .collect()
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
}
