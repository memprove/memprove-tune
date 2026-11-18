//! Narrowing the repository's test command to the tests a commit changed,
//! so that a mined task's check runs only what judges it.

use std::path::Path;

use crate::{detect::ProjectKind, git::git};

/// The check for a commit that changed `tests` (paths relative to the
/// root, `/`-separated), from the repository's test command `check` of
/// kind `kind`:
///
/// - Rust: when every test file belongs to a package (the nearest
///   `Cargo.toml` with a `[package]` name, read at `commit`),
///   `cargo test -p <package>` for each; when they are all in one package
///   and all are integration test files (`<package>/tests/<name>.rs`),
///   also `--test <name>` for each.
/// - Go: when every test file is a `_test.go` file, `go test` with the
///   package directory of each (`./dir`).
/// - Python: when every test file is a `.py` file that exists at
///   `commit`, the test command with those files appended.
/// - Otherwise, and for Node and Make: `None`, and the task's check is
///   `check` as it is.
pub(super) fn narrow(
    root: &Path,
    commit: &str,
    kind: ProjectKind,
    check: &[String],
    tests: &[String],
) -> Option<Vec<String>> {
    match kind {
        ProjectKind::Rust => rust(root, commit, tests),
        ProjectKind::Go => go(tests),
        ProjectKind::Python => python(root, commit, check, tests),
        ProjectKind::Node | ProjectKind::Make => None,
    }
}

fn exists(root: &Path, commit: &str, path: &str) -> bool {
    git(root, &["cat-file", "-e", &format!("{commit}:{path}")]).is_ok()
}

fn rust(root: &Path, commit: &str, tests: &[String]) -> Option<Vec<String>> {
    let mut packages: Vec<(String, String)> = Vec::new();
    let mut integration: Vec<String> = Vec::new();
    let mut all_integration = true;
    for test in tests {
        let (dir, name) = package_of(root, commit, test)?;
        let inside = test
            .strip_prefix(&dir)
            .map_or(test.as_str(), |rest| rest.trim_start_matches('/'));
        match inside
            .strip_prefix("tests/")
            .and_then(|file| file.strip_suffix(".rs"))
        {
            Some(stem) if !stem.contains('/') && exists(root, commit, test) => {
                if !integration.iter().any(|known| known == stem) {
                    integration.push(stem.to_owned());
                }
            }
            _ => all_integration = false,
        }
        if !packages.iter().any(|(known, _)| *known == dir) {
            packages.push((dir, name));
        }
    }
    let mut argv = vec!["cargo".to_owned(), "test".to_owned()];
    for (_, name) in &packages {
        argv.extend(["-p".to_owned(), name.clone()]);
    }
    if packages.len() == 1 && all_integration {
        for stem in integration {
            argv.extend(["--test".to_owned(), stem]);
        }
    }
    Some(argv)
}

/// The directory (relative, `""` for the root) and name of the Cargo
/// package that holds `path` at `commit`; `None` when the nearest
/// `Cargo.toml` names no package.
fn package_of(root: &Path, commit: &str, path: &str) -> Option<(String, String)> {
    let mut dir = path;
    loop {
        dir = dir.rsplit_once('/').map_or("", |(parent, _)| parent);
        let manifest = if dir.is_empty() {
            "Cargo.toml".to_owned()
        } else {
            format!("{dir}/Cargo.toml")
        };
        if let Ok(text) = git(root, &["show", &format!("{commit}:{manifest}")]) {
            return package_name(&text).map(|name| (dir.to_owned(), name));
        }
        if dir.is_empty() {
            return None;
        }
    }
}

/// The `name` of the `[package]` table of a `Cargo.toml`.
pub(super) fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "name"
        {
            let value = value.trim().trim_matches(['"', '\'']);
            return (!value.is_empty()).then(|| value.to_owned());
        }
    }
    None
}

fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

fn go(tests: &[String]) -> Option<Vec<String>> {
    let mut argv = vec!["go".to_owned(), "test".to_owned()];
    for test in tests {
        if !test.ends_with("_test.go") {
            return None;
        }
        let package = test
            .rsplit_once('/')
            .map_or_else(|| ".".to_owned(), |(dir, _)| format!("./{dir}"));
        if !argv.contains(&package) {
            argv.push(package);
        }
    }
    Some(argv)
}

fn python(root: &Path, commit: &str, check: &[String], tests: &[String]) -> Option<Vec<String>> {
    if tests
        .iter()
        .any(|test| !has_extension(test, "py") || !exists(root, commit, test))
    {
        return None;
    }
    let mut argv = check.to_vec();
    argv.extend(tests.iter().cloned());
    Some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_name_is_read_from_the_package_table() {
        let manifest = "[workspace]\nmembers = [\"a\"]\n\n[package]\nversion = \"1\"\nname = \"demo-core\"\n\n[dependencies]\nname = \"not this\"\n";
        assert_eq!(package_name(manifest).as_deref(), Some("demo-core"));
        assert_eq!(package_name("[workspace]\nmembers = []\n"), None);
    }

    #[test]
    fn go_tests_narrow_to_their_packages() {
        let tests = ["a/b/x_test.go".to_owned(), "y_test.go".to_owned()];
        assert_eq!(go(&tests).expect("go"), ["go", "test", "./a/b", "."]);
        assert_eq!(go(&["a/testdata/in.txt".to_owned()]), None);
    }
}
