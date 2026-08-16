use std::{
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use cfind::index::open_database;
use tempfile::TempDir;

#[test]
fn help_documents_required_environment_and_path_filters() {
    let output = Command::new(env!("CARGO_BIN_EXE_cfind"))
        .arg("--help")
        .env_remove("CFIND_ROOT")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("CFIND_ROOT=/path/to/code"), "{stdout}");
    assert!(stdout.contains("Required repository directory"), "{stdout}");
    assert!(stdout.contains(r"-f '^src/.*\.rs$'"), "{stdout}");
    assert!(stdout.contains("Path regex"), "{stdout}");
    assert!(stdout.contains(r"'\.(cs|rs)$'"), "{stdout}");
    assert!(stdout.contains("[default: 7]"), "{stdout}");
    assert!(stdout.contains("--quiet"), "{stdout}");
    assert!(
        stdout.contains("omit TYPE to list indexed kinds"),
        "{stdout}"
    );
    assert!(!stdout.contains("--verbose"), "{stdout}");
    assert!(
        stdout.contains("No lookaround or backreferences"),
        "{stdout}"
    );
    assert!(
        stdout.contains("CFIND_LANGUAGES=rust,javascript,typescript,csharp"),
        "{stdout}"
    );
    assert!(
        stdout.contains("CFIND_INDEX=/path/to/index.sqlite"),
        "{stdout}"
    );
    assert!(stdout.contains("--commit-url"), "{stdout}");
    assert!(stdout.contains("--collapse"), "{stdout}");
    assert!(
        stdout.contains("one row per repository, per declaring type, or per match"),
        "{stdout}"
    );
    assert!(stdout.contains("CFIND_STALE_AFTER_HOURS=6"), "{stdout}");
    assert!(stdout.contains("0 disables"), "{stdout}");
    assert!(stdout.contains("rebuild 3x"), "{stdout}");
    assert!(
        stdout.contains("Print each symbol's fully qualified name"),
        "{stdout}"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cfind"))
        .arg("--verbose")
        .env_remove("CFIND_ROOT")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--verbose'"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn missing_root_exits_without_creating_an_index() {
    let temporary = TempDir::new().unwrap();
    let index_path = temporary.path().join("must-not-exist.sqlite");
    let output = Command::new(env!("CARGO_BIN_EXE_cfind"))
        .arg("Anything")
        .env_remove("CFIND_ROOT")
        .env("CFIND_INDEX", &index_path)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(!index_path.exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CFIND_ROOT is required"), "{stderr}");
    assert!(stderr.contains("export CFIND_ROOT"), "{stderr}");
}

#[test]
fn status_reports_a_missing_index_without_creating_it() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir(&workspace).unwrap();

    let output = cfind_command(&workspace, &index_path)
        .arg("--status")
        .env("CFIND_LANGUAGES", "rust")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("No index at {}\n", index_path.display())
    );
    assert!(output.stderr.is_empty());
    assert!(!index_path.exists());
}

#[test]
fn status_reports_an_index_configuration_mismatch() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(workspace.join("src/lib.rs"), "pub struct StatusSymbol;\n").unwrap();
    run_git(&workspace, &["add", "src/lib.rs"]);

    let output = cfind_command(&workspace, &index_path)
        .arg("--index")
        .env("CFIND_LANGUAGES", "rust")
        .output()
        .unwrap();
    assert!(output.status.success());

    let output = cfind_command(&workspace, &index_path)
        .arg("--status")
        .env("CFIND_LANGUAGES", "csharp")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "Index configuration does not match at {}\n",
            index_path.display()
        )
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn status_reports_the_index_path_and_exact_counts() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(
        workspace.join("src/lib.rs"),
        "pub struct StatusSymbol;\npub fn status_function() {}\n",
    )
    .unwrap();
    run_git(&workspace, &["add", "src/lib.rs"]);

    let output = cfind_command(&workspace, &index_path)
        .arg("--index")
        .env("CFIND_LANGUAGES", "rust")
        .output()
        .unwrap();
    assert!(output.status.success());

    let output = cfind_command(&workspace, &index_path)
        .arg("--status")
        .env("CFIND_LANGUAGES", "rust")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "Index: {}\nRepositories: 1\nFiles: 1\nSymbols: 2\n",
            index_path.display()
        )
    );
    assert!(output.stderr.is_empty());
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
    run_git(&workspace, &["add", "src/lib.rs"]);

    let output = cfind_command(&workspace, &index_path)
        .arg("AutoIndexedSymbol")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains("Indexed "), "{stdout}");
    assert!(stdout.contains("AutoIndexedSymbol"), "{stdout}");
    assert!(
        stdout.contains("exact  struct  AutoIndexedSymbol"),
        "{stdout}"
    );
    assert!(stderr.contains("No index found."), "{stderr}");
    assert!(stderr.contains("Creating SQLite index at"), "{stderr}");
    assert!(stderr.contains("Indexing"), "{stderr}");
    assert!(stderr.contains("Indexed 1 symbols"), "{stderr}");
    assert!(
        stderr.contains(&index_path.display().to_string()),
        "{stderr}"
    );

    let output = cfind_command(&workspace, &index_path)
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

    let output = cfind_command(&workspace, &index_path)
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
fn invalid_configuration_fails_before_opening_the_index() {
    let temporary = TempDir::new().unwrap();
    let root_file = temporary.path().join("not-a-directory");
    fs::write(&root_file, "not a repository root").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cfind"))
        .arg("Anything")
        .env("CFIND_ROOT", &root_file)
        .env(
            "CFIND_INDEX",
            temporary.path().join("must-not-exist.sqlite"),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("search root is not a directory"),
        "{stderr}"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cfind"))
        .arg("Anything")
        .env("CFIND_ROOT", temporary.path())
        .env("CFIND_INDEX", "")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CFIND_INDEX must not be empty"), "{stderr}");

    let output = Command::new(env!("CARGO_BIN_EXE_cfind"))
        .arg("Anything")
        .env("CFIND_ROOT", temporary.path())
        .env("CFIND_STALE_AFTER_HOURS", "soon")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CFIND_STALE_AFTER_HOURS must be a non-negative number of hours"),
        "{stderr}"
    );
}

#[test]
fn language_configuration_change_rebuilds_the_index() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(workspace.join("src/lib.rs"), "pub struct RustSymbol;\n").unwrap();
    fs::write(
        workspace.join("src/Other.cs"),
        "public class CSharpSymbol {}\n",
    )
    .unwrap();
    run_git(&workspace, &["add", "src/lib.rs", "src/Other.cs"]);

    let output = cfind_command(&workspace, &index_path)
        .arg("CSharpSymbol")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("CSharpSymbol"));

    let output = cfind_command(&workspace, &index_path)
        .arg("RustSymbol")
        .env("CFIND_LANGUAGES", "rust")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Index configuration changed"), "{stderr}");
    let database = open_database(&index_path).unwrap();
    let languages: String = database
        .query_row(
            "SELECT value FROM index_metadata WHERE key = 'languages'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(languages, "rust");
    let csharp_files: usize = database
        .query_row(
            "SELECT COUNT(*) FROM files WHERE language = 'csharp'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(csharp_files, 0);
}

#[test]
fn index_format_change_rebuilds_before_searching() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(workspace.join("src/lib.rs"), "pub struct FormatSymbol;\n").unwrap();
    run_git(&workspace, &["add", "src/lib.rs"]);

    let output = cfind_command(&workspace, &index_path)
        .arg("FormatSymbol")
        .env("CFIND_LANGUAGES", "rust")
        .output()
        .unwrap();
    assert!(output.status.success());
    let database = open_database(&index_path).unwrap();
    database
        .execute(
            "UPDATE index_metadata SET value = '7' WHERE key = 'version'",
            [],
        )
        .unwrap();
    drop(database);

    let output = cfind_command(&workspace, &index_path)
        .arg("FormatSymbol")
        .env("CFIND_LANGUAGES", "rust")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Index configuration changed"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let database = open_database(&index_path).unwrap();
    let version: String = database
        .query_row(
            "SELECT value FROM index_metadata WHERE key = 'version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "12");
}

#[test]
fn old_index_warns_then_rebuilds_after_three_warning_periods() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(workspace.join("src/lib.rs"), "pub struct OriginalSymbol;\n").unwrap();
    run_git(&workspace, &["add", "src/lib.rs"]);

    let output = cfind_command(&workspace, &index_path)
        .arg("OriginalSymbol")
        .env("CFIND_STALE_AFTER_HOURS", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    fs::write(workspace.join("src/lib.rs"), "pub struct UpdatedSymbol;\n").unwrap();

    set_index_creation_age(&index_path, 60 * 60 + 1);
    let output = cfind_command(&workspace, &index_path)
        .arg("OriginalSymbol")
        .env("CFIND_STALE_AFTER_HOURS", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("OriginalSymbol"));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("older than 1 hour"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    set_index_creation_age(&index_path, 3 * 60 * 60 + 1);
    let output = cfind_command(&workspace, &index_path)
        .arg("UpdatedSymbol")
        .env("CFIND_STALE_AFTER_HOURS", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("UpdatedSymbol"));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("automatic rebuild threshold"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn relative_index_path_is_resolved_from_the_working_directory() {
    let temporary = TempDir::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cfind"))
        .arg("--index")
        .current_dir(temporary.path())
        .env("CFIND_ROOT", temporary.path())
        .env("CFIND_INDEX", "index.sqlite")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(temporary.path().join("index.sqlite").exists());
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
            "user.name=Cfind Test",
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
    run_git(
        &workspace,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    run_git(
        &workspace,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    let output = cfind_command(&workspace, &index_path)
        .arg("RemoteSymbol")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RemoteSymbol"), "{stdout}");
    // Freshly created origin refs, checked out on the origin default branch.
    assert!(stdout.contains("fetch:0d"), "{stdout}");
    assert!(!stdout.contains("branch:"), "{stdout}");
    assert!(
        stdout.contains("https://github.com/acme/example/blob/main/src/lib.rs#L1"),
        "{stdout}"
    );

    let output = cfind_command(&workspace, &index_path)
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

    let output = cfind_command(&workspace, &index_path)
        .args(["RemoteSymbol", "--commit-url", "--quiet"])
        .env("CFIND_STALE_AFTER_HOURS", "0")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RemoteSymbol"), "{stdout}");
    assert!(!stdout.contains("https://"), "{stdout}");
    // CFIND_STALE_AFTER_HOURS=0 switches Git state annotation off entirely.
    assert!(!stdout.contains("fetch:"), "{stdout}");
    assert!(!stdout.contains("branch:"), "{stdout}");
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
        "namespace Acme.Data;\npublic class SharedSymbol {}\npublic record SharedRecord;\npublic struct SharedValue {}\n",
    )
    .unwrap();
    run_git(&workspace, &["add", "src/shared.rs", "src/Shared.cs"]);
    run_git(
        &workspace,
        &["remote", "add", "origin", "git@github.com:acme/shared.git"],
    );

    let output = cfind_command(&workspace, &index_path)
        .args(["SharedSymbol", "--filter", r"\.cs$"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("src/Shared.cs"), "{stdout}");
    assert!(!stdout.contains("src/shared.rs"), "{stdout}");
    // The qualified name is opt-in; the path already implies it.
    assert!(!stdout.contains("Acme.Data.SharedSymbol"), "{stdout}");

    for name in ["SharedRecord", "SharedValue"] {
        let output = cfind_command(&workspace, &index_path)
            .args([name, "--type", "class", "--quiet"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(&format!("class  {name}")), "{stdout}");
    }
    let output = cfind_command(&workspace, &index_path)
        .args(["SharedSymbol", "--filter", r"\.cs$", "--qualified"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let qualified_stdout = String::from_utf8_lossy(&output.stdout);
    let path = qualified_stdout.find("src/Shared.cs:2").unwrap();
    let url = qualified_stdout
        .find("https://github.com/acme/shared/")
        .unwrap();
    let qualified_name = qualified_stdout
        .rfind("\n  Acme.Data.SharedSymbol")
        .unwrap();
    assert!(path < url && url < qualified_name, "{qualified_stdout}");
    assert!(qualified_stdout.ends_with("\n\n"), "{qualified_stdout}");

    let output = cfind_command(&workspace, &index_path)
        .arg("--type")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "class\nnamespace\nstruct\n"
    );

    let output = cfind_command(&workspace, &index_path)
        .args(["SharedSymbol", "--type", "class", "--qualified"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("src/Shared.cs"), "{stdout}");
    assert!(!stdout.contains("src/shared.rs"), "{stdout}");
    assert!(
        stdout.contains("\n  Acme.Data.SharedSymbol\n\n"),
        "{stdout}"
    );

    let output = cfind_command(&workspace, &index_path)
        .args(["SharedSymbol", "--type", "struct"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.matches("SharedSymbol").count(), 1, "{stdout}");
    assert!(!stdout.contains("\n  SharedSymbol\n"), "{stdout}");

    let output = cfind_command(&workspace, &index_path)
        .args(["SharedSymbol", "--type", "class", "--quiet", "--qualified"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("https://github.com"), "{stdout}");
    assert!(
        stdout.contains("\n  Acme.Data.SharedSymbol\n\n"),
        "{stdout}"
    );

    let output = cfind_command(&workspace, &index_path)
        .args(["Acme.Data", "--type", "namespace"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("namespace  Acme.Data"), "{stdout}");

    let output = cfind_command(&workspace, &index_path)
        .args(["SharedSymbol", "--type", "component"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown type 'component'"), "{stderr}");
    assert!(
        stderr.contains("available types: class, namespace, struct"),
        "{stderr}"
    );
}

#[test]
fn no_match_is_a_quiet_stdout_failure_even_after_filtering() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(workspace.join("src/lib.rs"), "pub struct PresentSymbol;\n").unwrap();
    fs::write(
        workspace.join("src/Other.cs"),
        "public class OtherClass {}\n",
    )
    .unwrap();
    run_git(&workspace, &["add", "src/lib.rs", "src/Other.cs"]);

    let output = cfind_command(&workspace, &index_path)
        .arg("DefinitelyNoSuchSymbolQzx")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no symbols matched"), "{stderr}");
    assert!(stderr.contains("DefinitelyNoSuchSymbolQzx"), "{stderr}");

    let output = cfind_command(&workspace, &index_path)
        .args(["PresentSymbol", "--filter", r"\.cs$"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no symbols matched"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = cfind_command(&workspace, &index_path)
        .args(["PresentSymbol", "--type", "class"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no symbols matched"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = cfind_command(&workspace, &index_path)
        .arg("::")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("must contain a letter or number"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn qualified_multi_term_queries_rank_the_leaf_and_accept_options_anywhere() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(
        workspace.join("src/Payments.cs"),
        "namespace Acme.Tools { public class Container { public class PaymentProcessor {} } }\n",
    )
    .unwrap();
    run_git(&workspace, &["add", "src/Payments.cs"]);

    let output = cfind_command(&workspace, &index_path)
        .args([
            "--limit",
            "1",
            "Acme",
            "Tools",
            "PaymentProcessor",
            "--type",
            "class",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let unquoted = String::from_utf8_lossy(&output.stdout);
    assert!(unquoted.contains("class  PaymentProcessor"), "{unquoted}");
    assert!(
        !unquoted.contains("Acme.Tools.Container.PaymentProcessor"),
        "{unquoted}"
    );

    let quoted = cfind_command(&workspace, &index_path)
        .arg("Acme Tools PaymentProcessor")
        .args(["--limit", "1", "--type", "class"])
        .output()
        .unwrap();
    assert!(quoted.status.success());
    assert_eq!(output.stdout, quoted.stdout);

    let qualified = cfind_command(&workspace, &index_path)
        .arg("Acme.Tools.Container.PaymentProcessor")
        .args(["--limit", "1", "--type", "class"])
        .output()
        .unwrap();
    assert!(qualified.status.success());
    assert!(
        String::from_utf8_lossy(&qualified.stdout).contains("class  PaymentProcessor"),
        "{}",
        String::from_utf8_lossy(&qualified.stdout)
    );
}

#[test]
fn results_outside_the_search_origin_are_not_ordered_by_path_depth() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    let deep = workspace.join("acme/services/export/jobs");
    let shallow = workspace.join("zzz");
    let origin = workspace.join("elsewhere");
    fs::create_dir_all(&deep).unwrap();
    fs::create_dir_all(&shallow).unwrap();
    fs::create_dir_all(&origin).unwrap();
    for repository in [&deep, &shallow] {
        run_git(&workspace, &["init", repository.to_str().unwrap()]);
        fs::write(repository.join("Export.cs"), "public class ExportJob {}\n").unwrap();
        run_git(repository, &["add", "Export.cs"]);
    }

    let output = cfind_command(&workspace, &index_path)
        .args(["ExportJob", "--limit", "2", "--quiet"])
        .arg("--from")
        .arg(&origin)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let deep_position = stdout.find(&deep.display().to_string()).unwrap();
    let shallow_position = stdout.find(&shallow.display().to_string()).unwrap();
    assert!(deep_position < shallow_position, "{stdout}");
}

#[test]
fn type_declarations_win_ties_against_their_members() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    // The property is declared first, so only the kind tie-break can put the class ahead of it.
    fs::write(
        workspace.join("src/Ordering.cs"),
        "namespace Acme;\npublic class Holder {\n    public string Ordering { get; set; }\n}\npublic class Ordering {}\n",
    )
    .unwrap();
    run_git(&workspace, &["add", "src/Ordering.cs"]);

    let output = cfind_command(&workspace, &index_path)
        .args(["Ordering", "--collapse", "none", "--limit", "2", "--quiet"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let class = stdout.find("class  Ordering").unwrap();
    let property = stdout.find("property  Ordering in Holder").unwrap();
    assert!(class < property, "{stdout}");
}

#[test]
fn short_numeric_terms_still_contribute_to_multi_term_coverage() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(workspace.join("src")).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(
        workspace.join("src/Migrations.cs"),
        "namespace Acme.Migrations;\npublic class Database_16 {}\npublic class LaikaDatabase16 {}\npublic class DatabaseOptions {\n    public string Database { get; set; }\n}\n",
    )
    .unwrap();
    run_git(&workspace, &["add", "src/Migrations.cs"]);

    let output = cfind_command(&workspace, &index_path)
        .args([
            "Database",
            "16",
            "--collapse",
            "none",
            "--limit",
            "3",
            "--quiet",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let database_16 = stdout.find("class  Database_16").unwrap();
    let laika_database_16 = stdout.find("class  LaikaDatabase16").unwrap();
    // Matching both terms outranks an exact match on "Database" alone.
    let exact_property = stdout
        .find("property  Database in DatabaseOptions")
        .unwrap_or(usize::MAX);
    assert!(database_16 < laika_database_16, "{stdout}");
    assert!(laika_database_16 < exact_property, "{stdout}");
}

#[test]
fn namespace_results_are_deduplicated_per_repository_before_limit() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let first = workspace.join("first");
    let second = workspace.join("second");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    fs::create_dir_all(first.join("near")).unwrap();
    fs::create_dir_all(first.join("far")).unwrap();
    run_git(&workspace, &["init", first.to_str().unwrap()]);
    run_git(&workspace, &["init", second.to_str().unwrap()]);
    fs::write(first.join("a.cs"), "namespace Acme.Shared;\n").unwrap();
    fs::write(first.join("b.cs"), "namespace Acme.Shared;\n").unwrap();
    fs::write(first.join("case.cs"), "namespace ACME.SHARED;\n").unwrap();
    fs::write(first.join("joined.cs"), "namespace AcmeShared;\n").unwrap();
    fs::write(first.join("c.cs"), "namespace Acme.SharedOther;\n").unwrap();
    fs::write(first.join("near/location.cs"), "namespace Acme.Location;\n").unwrap();
    fs::write(first.join("far/location.cs"), "namespace Acme.Location;\n").unwrap();
    fs::write(
        first.join("methods.cs"),
        "class First { void Run() {} } class Second { void Run() {} }\n",
    )
    .unwrap();
    fs::write(
        first.join("ctor-a.cs"),
        "partial class Duplicate { Duplicate() {} }\n",
    )
    .unwrap();
    fs::write(
        first.join("ctor-b.cs"),
        "partial class Duplicate { Duplicate() {} }\n",
    )
    .unwrap();
    fs::write(second.join("d.cs"), "namespace Acme.Shared;\n").unwrap();
    run_git(
        &first,
        &[
            "add",
            "a.cs",
            "b.cs",
            "case.cs",
            "joined.cs",
            "c.cs",
            "near/location.cs",
            "far/location.cs",
            "methods.cs",
            "ctor-a.cs",
            "ctor-b.cs",
        ],
    );
    run_git(&second, &["add", "d.cs"]);

    let output = cfind_command(&workspace, &index_path)
        .args([
            "Acme",
            "--collapse",
            "none",
            "--type",
            "namespace",
            "--limit",
            "10",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.matches("namespace  Acme.Shared\n").count(),
        2,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("namespace  Acme.SharedOther\n").count(),
        1,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("namespace  AcmeShared\n").count(),
        1,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("namespace  Acme.Location\n").count(),
        1,
        "{stdout}"
    );
    assert!(!stdout.contains("namespace  ACME.SHARED"), "{stdout}");
    assert!(stdout.contains("first/a.cs"), "{stdout}");
    assert!(!stdout.contains("first/b.cs"), "{stdout}");

    let output = cfind_command(&workspace, &index_path)
        .args([
            "Acme.Shared",
            "--collapse",
            "none",
            "--type",
            "namespace",
            "--limit",
            "4",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Acme.SharedOther"), "{stdout}");
    assert!(stdout.contains("Acme.Shared"), "{stdout}");

    let from = first.join("far");
    let output = cfind_command(&workspace, &index_path)
        .args([
            "Acme.Location",
            "--collapse",
            "none",
            "--type",
            "namespace",
            "--limit",
            "1",
        ])
        .arg("--from")
        .arg(&from)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&first.join("far/location.cs").display().to_string()),
        "{stdout}"
    );

    let output = cfind_command(&workspace, &index_path)
        .args(["Run", "--collapse", "none", "--type", "method"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .matches("method  Run")
            .count(),
        2
    );

    let output = cfind_command(&workspace, &index_path)
        .args(["Duplicate", "--collapse", "none", "--type", "constructor"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .matches("constructor  Duplicate")
            .count(),
        2
    );
}

#[test]
fn repository_collapse_returns_one_result_per_repository() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    for (name, source) in [
        (
            "alpha",
            "namespace Acme.Alpha;\npublic class PaymentService {\n    public PaymentService() {}\n    public void ProcessPayment() {}\n}\n",
        ),
        (
            "beta",
            "namespace Acme.Beta;\npublic class PaymentGateway {}\n",
        ),
        (
            "gamma",
            "namespace Acme.Gamma;\npublic class PaymentQueue {}\n",
        ),
    ] {
        let repository = workspace.join(name);
        fs::create_dir_all(&repository).unwrap();
        run_git(&workspace, &["init", repository.to_str().unwrap()]);
        fs::write(repository.join("Payments.cs"), source).unwrap();
        run_git(&repository, &["add", "Payments.cs"]);
    }

    let output = cfind_command(&workspace, &index_path)
        .args(["Payment", "--limit", "10", "--quiet"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in ["alpha", "beta", "gamma"] {
        assert_eq!(
            stdout
                .matches(&format!("{}/{name}/Payments.cs:", workspace.display()))
                .count(),
            1,
            "{stdout}"
        );
    }
    assert!(stdout.contains("class  PaymentService"), "{stdout}");
    assert!(!stdout.contains("constructor  "), "{stdout}");
    // Match counts belong to --collapse type, where folding is the point.
    assert!(!stdout.contains("matches="), "{stdout}");

    let limited = cfind_command(&workspace, &index_path)
        .args(["Payment", "--limit", "2", "--quiet"])
        .output()
        .unwrap();
    assert!(limited.status.success());
    let limited_stdout = String::from_utf8_lossy(&limited.stdout);
    assert_eq!(
        limited_stdout.matches("/Payments.cs:").count(),
        2,
        "{limited_stdout}"
    );

    let detailed = cfind_command(&workspace, &index_path)
        .args(["Payment", "--collapse", "none", "--limit", "10", "--quiet"])
        .output()
        .unwrap();
    assert!(detailed.status.success());
    let detailed_stdout = String::from_utf8_lossy(&detailed.stdout);
    assert_eq!(
        detailed_stdout
            .matches(&format!(
                "{}:",
                workspace.join("alpha/Payments.cs").display()
            ))
            .count(),
        3,
        "{detailed_stdout}"
    );
    assert!(
        detailed_stdout.contains("constructor  PaymentService"),
        "{detailed_stdout}"
    );
    assert!(!detailed_stdout.contains("matches="), "{detailed_stdout}");

    let invalid = cfind_command(&workspace, &index_path)
        .args(["Payment", "--collapse", "bogus"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(stderr.contains("invalid value 'bogus'"), "{stderr}");
    assert!(
        stderr.contains("[possible values: none, type, repo]"),
        "{stderr}"
    );
}

#[test]
fn type_collapse_keeps_namespaces_and_types_as_separate_rows() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let source_directory = workspace.join("src/services");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(&source_directory).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(
        source_directory.join("Payments.cs"),
        "namespace Acme.Services;\npublic class PaymentService {\n    public PaymentService() {}\n    public void Connect() {}\n}\n",
    )
    .unwrap();
    fs::write(
        source_directory.join("Refunds.cs"),
        "namespace Acme.Services;\npublic class RefundService {\n    public void Disconnect() {}\n}\n",
    )
    .unwrap();
    run_git(
        &workspace,
        &["add", "src/services/Payments.cs", "src/services/Refunds.cs"],
    );

    let output = cfind_command(&workspace, &index_path)
        .args([
            "Acme.Services",
            "--collapse",
            "type",
            "--limit",
            "10",
            "--quiet",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // A namespace is one row of its own; members fold into their declaring type,
    // not into the namespace.
    assert_eq!(
        stdout.matches("namespace  Acme.Services").count(),
        1,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("class  PaymentService  matches=3").count(),
        1,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("class  RefundService  matches=2").count(),
        1,
        "{stdout}"
    );
    assert!(stdout.contains("Payments.cs"), "{stdout}");
    assert!(stdout.contains("Refunds.cs"), "{stdout}");

    let output = cfind_command(&workspace, &index_path)
        .args(["Acme.Services", "Payment", "--collapse", "type", "--quiet"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Type collapse never folds a type into the namespace above it, so the
    // namespace stands alone and each type keeps its own row.
    assert_eq!(
        stdout
            .matches("namespace  Acme.Services  matches=2")
            .count(),
        1,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("class  PaymentService  matches=3").count(),
        1,
        "{stdout}"
    );
}

#[test]
fn type_collapse_folds_members_into_types_before_applying_the_limit() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let source_path = workspace.join("src/Payments.cs");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(
        &source_path,
        "namespace Acme.Services;\npublic class PaymentService {\n    public PaymentService() {}\n    public void ProcessPayment() {}\n    public string PaymentStatus { get; }\n}\npublic class PaymentGateway {\n    public PaymentGateway() {}\n    public void SendPayment() {}\n}\n",
    )
    .unwrap();
    run_git(&workspace, &["add", "src/Payments.cs"]);

    let detailed = cfind_command(&workspace, &index_path)
        .args(["Payment", "--collapse", "none", "--limit", "10", "--quiet"])
        .output()
        .unwrap();
    assert!(detailed.status.success());
    let detailed_stdout = String::from_utf8_lossy(&detailed.stdout);
    assert!(detailed_stdout.contains("constructor  PaymentService"));
    assert!(detailed_stdout.contains("method  ProcessPayment"));
    assert!(!detailed_stdout.contains("matches="));

    let output = cfind_command(&workspace, &index_path)
        .args(["Payment", "--collapse", "type", "--limit", "2", "--quiet"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.matches("class  PaymentService  matches=").count(),
        1,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("class  PaymentGateway  matches=").count(),
        1,
        "{stdout}"
    );
    assert!(stdout.contains("matches=4"), "{stdout}");
    assert!(stdout.contains("matches=3"), "{stdout}");
    assert!(!stdout.contains("constructor  "), "{stdout}");
    assert!(!stdout.contains("method  "), "{stdout}");
    assert!(
        stdout.contains(&format!("{}:2", source_path.display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("{}:7", source_path.display())),
        "{stdout}"
    );
}

#[test]
fn type_collapse_names_a_group_by_its_best_member_when_the_type_does_not_match() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let source_path = workspace.join("src/Handlers.ts");
    let index_path = temporary.path().join("indexes/workspace.sqlite");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    run_git(temporary.path(), &["init", workspace.to_str().unwrap()]);
    fs::write(
        &source_path,
        "class RequestHandler {\n  getUser() {}\n  getOrder() {}\n}\n",
    )
    .unwrap();
    run_git(&workspace, &["add", "src/Handlers.ts"]);

    let output = cfind_command(&workspace, &index_path)
        .args(["get", "--collapse", "type", "--quiet"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The type itself does not match "get", so its best member names the group
    // and the header still says which type the members live in.
    assert!(
        stdout.contains("method  getUser in RequestHandler"),
        "{stdout}"
    );
    assert!(stdout.contains("matches=2"), "{stdout}");
    assert!(
        stdout.contains(&format!("{}:2", source_path.display())),
        "{stdout}"
    );
    assert!(!stdout.contains("getOrder"), "{stdout}");
}

fn cfind_command(workspace: &Path, index_path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cfind"));
    command
        .current_dir(workspace)
        .env("CFIND_ROOT", workspace)
        .env("CFIND_INDEX", index_path)
        .env_remove("CFIND_STALE_AFTER_HOURS");
    command
}

fn set_index_creation_age(index_path: &Path, age_seconds: u64) {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - age_seconds;
    let database = open_database(index_path).unwrap();
    database
        .execute(
            "UPDATE index_metadata SET value = ?1 WHERE key = 'created_at'",
            [created_at],
        )
        .unwrap();
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
