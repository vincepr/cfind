pub mod config;
pub mod git;
pub mod index;
pub mod language;
pub mod search;

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub parent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub name: String,
    pub kind: String,
    pub match_score: u16,
    pub namespace: Option<String>,
    pub parent: Option<String>,
    pub local_path: PathBuf,
    pub relative_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub remote_url: Option<String>,
    pub commit_url: Option<String>,
    pub git_state: Option<String>,
}
