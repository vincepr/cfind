use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

#[test]
fn help_documents_required_environment_and_path_filters() {
    let output = Command::new(env!("CARGO_BIN_EXE_code-search"))
        .arg("--help")
        .env_remove("CODE_SEARCH_ROOT")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("CODE_SEARCH_ROOT=/path/to/code"),
        "{stdout}"
    );
    assert!(stdout.contains("Required repository directory"), "{stdout}");
    assert!(stdout.contains(r"-f '\.cs$'"), "{stdout}");
    assert!(stdout.contains("Path regex"), "{stdout}");
    assert!(stdout.contains(r"'\.(cs|rs)$'"), "{stdout}");
    assert!(stdout.contains("[default: 10]"), "{stdout}");
    assert!(stdout.contains("--quiet"), "{stdout}");
    assert!(
        stdout.contains("CODE_SEARCH_LANGUAGES=rust,javascript,typescript,csharp"),
        "{stdout}"
    );
    assert!(
        stdout.contains("CODE_SEARCH_INDEX=/path/to/index.sqlite"),
        "{stdout}"
    );
    assert!(stdout.contains("--commit-url"), "{stdout}");
}

#[test]
fn missing_root_exits_without_creating_an_index() {
    let temporary = TempDir::new().unwrap();
    let index_path = temporary.path().join("must-not-exist.sqlite");
    let output = Command::new(env!("CARGO_BIN_EXE_code-search"))
        .arg("Anything")
        .env_remove("CODE_SEARCH_ROOT")
        .env("CODE_SEARCH_INDEX", &index_path)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(!index_path.exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CODE_SEARCH_ROOT is required"), "{stderr}");
    assert!(stderr.contains("export CODE_SEARCH_ROOT"), "{stderr}");
}

#[test]
fn search_creates_a_missing_index_and_then_returns_results() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(
        workspace.join("src/lib.rs"),
        "pub struct AutoIndexedSymbol;\n",
    )
    .unwrap();

    let output = code_search_command(&workspace, &index_path)
        .arg("AutoIndexedSymbol")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("Indexed 1 symbols"), "{stdout}");
    assert!(stdout.contains("AutoIndexedSymbol"), "{stdout}");
    assert!(stdout.contains("10000"), "{stdout}");
    assert!(stderr.contains("No index found."), "{stderr}");
    assert!(stderr.contains("Creating SQLite index at"), "{stderr}");
    assert!(stderr.contains("Indexing"), "{stderr}");
    assert!(
        stderr.contains(&index_path.display().to_string()),
        "{stderr}"
    );

    let output = code_search_command(&workspace, &index_path)
        .arg("--index")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Indexing"), "{stderr}");
    assert!(
        stderr.contains(&index_path.display().to_string()),
        "{stderr}"
    );

    let output = code_search_command(&workspace, &index_path)
        .args(["AutoIndexedSymbol", "--index"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("AutoIndexedSymbol"), "{stdout}");
    assert!(!stdout.contains("Indexed "), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn branch_urls_are_default_and_commit_urls_are_opt_in() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(workspace.join("src/lib.rs"), "pub struct RemoteSymbol;\n").unwrap();
    run_git(&workspace, &["add", "src/lib.rs"]);
    run_git(
        &workspace,
        &[
            "-c",
            "user.name=Code Search Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "fixture",
        ],
    );
    run_git(&workspace, &["branch", "-M", "main"]);
    run_git(
        &workspace,
        &["remote", "add", "origin", "git@github.com:acme/example.git"],
    );

    let output = code_search_command(&workspace, &index_path)
        .arg("RemoteSymbol")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RemoteSymbol"), "{stdout}");
    assert!(
        stdout.contains("https://github.com/acme/example/blob/main/src/lib.rs"),
        "{stdout}"
    );
    assert!(!stdout.contains("#L"), "{stdout}");

    let output = code_search_command(&workspace, &index_path)
        .args(["RemoteSymbol", "--commit-url"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("https://github.com/acme/example/blob/"),
        "{stdout}"
    );
    assert!(!stdout.contains("/blob/main/"), "{stdout}");

    let output = code_search_command(&workspace, &index_path)
        .args(["RemoteSymbol", "--commit-url", "--quiet"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RemoteSymbol"), "{stdout}");
    assert!(!stdout.contains("https://"), "{stdout}");
}

#[test]
fn search_filters_results_by_path_regex() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(
        workspace.join("src/shared.rs"),
        "pub struct SharedSymbol;\n",
    )
    .unwrap();
    fs::write(
        workspace.join("src/Shared.cs"),
        "public class SharedSymbol {}\n",
    )
    .unwrap();

    let output = code_search_command(&workspace, &index_path)
        .args(["SharedSymbol", "--filter", r"\.cs$"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("src/Shared.cs"), "{stdout}");
    assert!(!stdout.contains("src/shared.rs"), "{stdout}");
}

fn code_search_command(workspace: &Path, index_path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_code-search"));
    command
        .current_dir(workspace)
        .env("CODE_SEARCH_ROOT", workspace)
        .env("CODE_SEARCH_INDEX", index_path);
    command
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
