use std::{collections::HashSet, fs, path::Path, process::Command, time::Duration};

use cfind::{
    config::{Config, SupportedLanguage},
    index::{open_database, rebuild},
    search::search,
};
use tempfile::TempDir;

#[test]
fn workspaces_have_independent_indexes_and_ignore_untracked_files() {
    let temporary = TempDir::new().unwrap();
    let rust_workspace = temporary.path().join("rust-projects");
    let csharp_workspace = temporary.path().join("csharp-projects");
    create_repository(
        &rust_workspace,
        "src/lib.rs",
        "pub struct DatabaseContext;\n",
    );
    create_repository(
        &csharp_workspace,
        "Context.cs",
        "public record MarketplaceContext(int Id);\n",
    );
    fs::write(
        rust_workspace.join("src/untracked.rs"),
        "pub struct MustNotBeIndexed;\n",
    )
    .unwrap();

    let rust_config = config(&rust_workspace, SupportedLanguage::Rust);
    let csharp_config = config(&csharp_workspace, SupportedLanguage::CSharp);
    rebuild(&rust_config).unwrap();
    rebuild(&csharp_config).unwrap();

    assert_ne!(rust_config.index_path, csharp_config.index_path);
    let rust_database = open_database(&rust_config.index_path).unwrap();
    let rust_results = search(&rust_database, "DatabaseContext", &rust_workspace, 10).unwrap();
    assert_eq!(rust_results.len(), 1);
    assert_eq!(rust_results[0].name, "DatabaseContext");
    assert!(
        search(&rust_database, "MustNotBeIndexed", &rust_workspace, 10)
            .unwrap()
            .iter()
            .all(|result| result.name != "MustNotBeIndexed")
    );

    let csharp_database = open_database(&csharp_config.index_path).unwrap();
    assert!(
        search(&csharp_database, "DatabaseContext", &csharp_workspace, 10)
            .unwrap()
            .iter()
            .all(|result| result.name != "DatabaseContext")
    );
    assert_eq!(
        search(
            &csharp_database,
            "MarketplaceContext",
            &csharp_workspace,
            10
        )
        .unwrap()[0]
            .name,
        "MarketplaceContext"
    );
}

#[test]
fn reindexes_uncommitted_changes_to_tracked_files() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    create_repository(&workspace, "src/lib.rs", "pub struct OriginalContext;\n");
    let config = config(&workspace, SupportedLanguage::Rust);
    rebuild(&config).unwrap();

    fs::write(workspace.join("src/lib.rs"), "pub struct UpdatedContext;\n").unwrap();
    let stats = rebuild(&config).unwrap();
    assert_eq!(stats.parsed_files, 1);

    let database = open_database(&config.index_path).unwrap();
    assert!(
        search(&database, "OriginalContext", &workspace, 10)
            .unwrap()
            .iter()
            .all(|result| result.name != "OriginalContext")
    );
    assert_eq!(
        search(&database, "UpdatedContext", &workspace, 10).unwrap()[0].name,
        "UpdatedContext"
    );
}

#[test]
fn fresh_rebuild_reparses_unchanged_files() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    create_repository(
        &workspace,
        "Context.cs",
        "namespace Acme.Data;\npublic class DatabaseContext {}\n",
    );
    let config = config(&workspace, SupportedLanguage::CSharp);
    rebuild(&config).unwrap();

    let database = open_database(&config.index_path).unwrap();
    database
        .execute("UPDATE symbols SET namespace = NULL", [])
        .unwrap();
    drop(database);

    let stats = rebuild(&config).unwrap();
    assert_eq!(stats.parsed_files, 1);
    let database = open_database(&config.index_path).unwrap();
    let results = search(&database, "DatabaseContext", &workspace, 10).unwrap();
    assert_eq!(results[0].namespace.as_deref(), Some("Acme.Data"));
}

fn config(root: &Path, language: SupportedLanguage) -> Config {
    Config {
        root: root.to_path_buf(),
        index_path: root.join(".cfind.sqlite3"),
        languages: HashSet::from([language]),
        stale_after: Duration::from_secs(6 * 60 * 60),
    }
}

fn create_repository(root: &Path, relative_path: &str, source: &str) {
    fs::create_dir_all(root.join(Path::new(relative_path).parent().unwrap())).unwrap();
    run_git(root.parent().unwrap(), &["init", root.to_str().unwrap()]);
    fs::write(root.join(relative_path), source).unwrap();
    run_git(root, &["add", relative_path]);
    run_git(
        root,
        &[
            "-c",
            "user.name=Cfind Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "fixture",
        ],
    );
}

fn run_git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
