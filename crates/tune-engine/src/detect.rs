//! Guessing a repository's test command from its files.

use std::{fs, path::Path};

use crate::json;

/// The order in which project files are looked for, for messages.
pub(crate) const ORDER: &str = "Cargo.toml, package.json with a test script, pyproject.toml / pytest.ini / setup.cfg, go.mod, Makefile with a test target";

/// The kind of project a guess is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    /// `Cargo.toml`: `cargo test`.
    Rust,
    /// `package.json` with `scripts.test`: `npm test`, `pnpm test` or
    /// `yarn test`.
    Node,
    /// `pyproject.toml`, `pytest.ini` or `setup.cfg`: `python3 -m pytest`.
    Python,
    /// `go.mod`: `go test ./...`.
    Go,
    /// A `Makefile` with a `test` target: `make test`.
    Make,
}

/// A guess at how a repository runs its tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    /// The kind of project.
    pub kind: ProjectKind,
    /// The file the guess came from, for example `Cargo.toml`.
    pub marker: &'static str,
    /// Commands a fresh worktree needs before the tests can run (installing
    /// packages), in order; often none.
    pub setup: Vec<Vec<String>>,
    /// The test command.
    pub check: Vec<String>,
    /// The files of other kinds that also matched, in [`detect`]'s order;
    /// the first match wins.
    pub also: Vec<&'static str>,
}

/// Guesses the test command of the project at `root`, looking in this
/// order and taking the first match:
///
/// 1. `Cargo.toml`: `cargo test`.
/// 2. `package.json` whose `scripts.test` is set and is not npm's
///    placeholder (`no test specified`): `pnpm test` after
///    `pnpm install --frozen-lockfile` when there is a `pnpm-lock.yaml`,
///    `yarn test` after `yarn install --frozen-lockfile` when there is a
///    `yarn.lock`, else `npm test` after `npm ci` (with a
///    `package-lock.json`) or `npm install`.
/// 3. `pyproject.toml`, `pytest.ini` or `setup.cfg`: `python3 -m pytest`.
/// 4. `go.mod`: `go test ./...`.
/// 5. `Makefile` with a `test` target: `make test`.
#[must_use]
pub fn detect(root: &Path) -> Option<Detected> {
    let guesses: [fn(&Path) -> Option<Detected>; 5] = [rust, node, python, go, make];
    let mut found = guesses.into_iter().filter_map(|guess| guess(root));
    let mut first = found.next()?;
    first.also = found.map(|other| other.marker).collect();
    Some(first)
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn guess(kind: ProjectKind, marker: &'static str, setup: &[&[&str]], check: &[&str]) -> Detected {
    Detected {
        kind,
        marker,
        setup: setup.iter().map(|words| argv(words)).collect(),
        check: argv(check),
        also: Vec::new(),
    }
}

fn rust(root: &Path) -> Option<Detected> {
    root.join("Cargo.toml")
        .is_file()
        .then(|| guess(ProjectKind::Rust, "Cargo.toml", &[], &["cargo", "test"]))
}

fn node(root: &Path) -> Option<Detected> {
    let text = fs::read_to_string(root.join("package.json")).ok()?;
    let value = json::parse(&text, None).ok()?;
    let script = value.get("scripts")?.get("test")?.as_str()?;
    if script.trim().is_empty() || script.contains("no test specified") {
        return None;
    }
    let found = if root.join("pnpm-lock.yaml").is_file() {
        guess(
            ProjectKind::Node,
            "package.json",
            &[&["pnpm", "install", "--frozen-lockfile"]],
            &["pnpm", "test"],
        )
    } else if root.join("yarn.lock").is_file() {
        guess(
            ProjectKind::Node,
            "package.json",
            &[&["yarn", "install", "--frozen-lockfile"]],
            &["yarn", "test"],
        )
    } else if root.join("package-lock.json").is_file() {
        guess(
            ProjectKind::Node,
            "package.json",
            &[&["npm", "ci"]],
            &["npm", "test"],
        )
    } else {
        guess(
            ProjectKind::Node,
            "package.json",
            &[&["npm", "install"]],
            &["npm", "test"],
        )
    };
    Some(found)
}

fn python(root: &Path) -> Option<Detected> {
    ["pyproject.toml", "pytest.ini", "setup.cfg"]
        .into_iter()
        .find(|name| root.join(name).is_file())
        .map(|marker| {
            guess(
                ProjectKind::Python,
                marker,
                &[],
                &["python3", "-m", "pytest"],
            )
        })
}

fn go(root: &Path) -> Option<Detected> {
    root.join("go.mod")
        .is_file()
        .then(|| guess(ProjectKind::Go, "go.mod", &[], &["go", "test", "./..."]))
}

fn make(root: &Path) -> Option<Detected> {
    let text = fs::read_to_string(root.join("Makefile")).ok()?;
    text.lines()
        .any(has_test_target)
        .then(|| guess(ProjectKind::Make, "Makefile", &[], &["make", "test"]))
}

/// Whether a Makefile line is a rule with `test` among its targets
/// (`test:`, `unit test: deps`), not a recipe line, a variable
/// (`test := x`) or `.PHONY: test`.
fn has_test_target(line: &str) -> bool {
    if line.starts_with(['\t', ' ', '#']) {
        return false;
    }
    let Some((targets, rest)) = line.split_once(':') else {
        return false;
    };
    !rest.starts_with('=') && targets.split_whitespace().any(|target| target == "test")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makefile_rules_are_told_from_other_lines() {
        assert!(has_test_target("test:"));
        assert!(has_test_target("unit test: build"));
        assert!(has_test_target("test : build"));
        assert!(!has_test_target(".PHONY: test"));
        assert!(!has_test_target("test := 1"));
        assert!(!has_test_target("\ttest: x"));
        assert!(!has_test_target("test-unit:"));
        assert!(!has_test_target("# test:"));
    }
}
