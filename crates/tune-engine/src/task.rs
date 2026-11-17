//! Task files (`tune.task.v1`, and `memprove.eval.task.v1`, whose members
//! are the same): reading and checking them.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};

use crate::{
    CommandSpec, EvalError, LEGACY_TASK_SCHEMA, TASK_SCHEMA, Task, TaskRef,
    json::{self, Reader},
};

/// The agent's time limit when a task names none.
pub const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 1800;
/// The check's time limit when it names none.
pub const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 900;
/// A setup command's time limit when it names none.
pub const DEFAULT_SETUP_TIMEOUT_SECS: u64 = 600;
/// The file in a task directory that is a baseline, not a task.
pub const BASELINE_FILE: &str = "baseline.json";

const TASK_MEMBERS: &[&str] = &[
    "schema",
    "id",
    "title",
    "base",
    "prompt",
    "setup",
    "check",
    "check_patch",
    "agent_timeout_secs",
    "tags",
];
const COMMAND_MEMBERS: &[&str] = &["argv", "timeout_secs"];

/// Whether `id` is a valid task id: 1 to 64 of `a-z`, `0-9`, `-`, `_`.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

/// Reads one task file; `file` is absolute or relative to `repo_root`.
///
/// # Errors
/// [`EvalError::Io`] or [`EvalError::Invalid`].
pub fn load_task(repo_root: &Path, file: &Path) -> Result<(Task, TaskRef), EvalError> {
    let path = if file.is_absolute() {
        file.to_path_buf()
    } else {
        repo_root.join(file)
    };
    let bytes = fs::read(&path).map_err(|error| EvalError::io(&path, error))?;
    let source = path
        .strip_prefix(repo_root)
        .map_or_else(|_| path.clone(), Path::to_path_buf);
    let text = String::from_utf8(bytes.clone())
        .map_err(|_| EvalError::invalid(Some(&source), "is not UTF-8"))?;
    let value = json::parse(&text, Some(&source))?;
    let reader = Reader::new(&value, Some(&source), "")?;
    reader.deny_unknown(TASK_MEMBERS)?;
    let schema = reader.str("schema")?;
    if schema != TASK_SCHEMA && schema != LEGACY_TASK_SCHEMA {
        return Err(reader.error("schema", &format!("must be {TASK_SCHEMA}, not {schema}")));
    }
    let id = match reader.opt_str("id")? {
        Some(id) => id.to_owned(),
        None => source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    if !valid_id(&id) {
        return Err(reader.error(
            "id",
            &format!(
                "must be 1 to 64 of a-z, 0-9, - and _ (the file name when absent), not {id:?}"
            ),
        ));
    }
    let prompt = reader.str("prompt")?;
    if prompt.trim().is_empty() {
        return Err(reader.error("prompt", "must not be empty"));
    }
    let check = command(
        &reader,
        reader
            .get("check")
            .ok_or_else(|| reader.error("check", "is required"))?,
        "check",
        DEFAULT_CHECK_TIMEOUT_SECS,
    )?;
    let setup = reader
        .array("setup")?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            command(
                &reader,
                value,
                &format!("setup[{index}]"),
                DEFAULT_SETUP_TIMEOUT_SECS,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let check_patch = reader
        .opt_str("check_patch")?
        .map(|patch| check_patch(&reader, repo_root, patch))
        .transpose()?;
    let agent_timeout = reader
        .opt_u64("agent_timeout_secs")?
        .unwrap_or(DEFAULT_AGENT_TIMEOUT_SECS);
    if agent_timeout == 0 {
        return Err(reader.error("agent_timeout_secs", "must be at least 1"));
    }
    let task = Task {
        id: id.clone(),
        title: reader.opt_str("title")?.map(str::to_owned),
        base: reader.opt_str("base")?.map(str::to_owned),
        prompt: prompt.to_owned(),
        setup,
        check,
        check_patch,
        agent_timeout: Duration::from_secs(agent_timeout),
        tags: reader.strs("tags")?,
        source: source.clone(),
    };
    let reference = TaskRef {
        id,
        title: task.title.clone(),
        source: source.to_string_lossy().into_owned(),
        sha256: crate::hex(&Sha256::digest(&bytes)),
    };
    Ok((task, reference))
}

fn command(
    task: &Reader<'_>,
    value: &json::JsonValue,
    at: &str,
    default_timeout: u64,
) -> Result<CommandSpec, EvalError> {
    let reader = Reader::new(value, task.path(), at)?;
    reader.deny_unknown(COMMAND_MEMBERS)?;
    let argv = reader.strs("argv")?;
    if argv.is_empty() || argv[0].is_empty() {
        return Err(reader.error("argv", "must name a program: a non-empty array of strings"));
    }
    let timeout = reader.opt_u64("timeout_secs")?.unwrap_or(default_timeout);
    if timeout == 0 {
        return Err(reader.error("timeout_secs", "must be at least 1"));
    }
    Ok(CommandSpec {
        argv,
        timeout: Duration::from_secs(timeout),
    })
}

fn check_patch(task: &Reader<'_>, repo_root: &Path, patch: &str) -> Result<PathBuf, EvalError> {
    let relative = PathBuf::from(patch);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(task.error(
            "check_patch",
            "must be a path relative to the repository root, without ..",
        ));
    }
    if !repo_root.join(&relative).is_file() {
        return Err(task.error("check_patch", &format!("names no file: {patch}")));
    }
    Ok(relative)
}

/// Reads the tasks named by `paths`: files as they are, directories as
/// their `*.json` files other than `baseline.json`, sorted by name.
/// Duplicate ids are refused.
///
/// # Errors
/// As [`load_task`], [`EvalError::Invalid`] for a duplicate id or when
/// `paths` hold no task.
pub fn load_suite(repo_root: &Path, paths: &[PathBuf]) -> Result<Vec<(Task, TaskRef)>, EvalError> {
    let mut files = Vec::new();
    for path in paths {
        let full = if path.is_absolute() {
            path.clone()
        } else {
            repo_root.join(path)
        };
        if full.is_dir() {
            let mut found: Vec<PathBuf> = fs::read_dir(&full)
                .map_err(|error| EvalError::io(&full, error))?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|file| {
                    file.extension()
                        .is_some_and(|extension| extension == "json")
                        && file.file_name().is_some_and(|name| name != BASELINE_FILE)
                        && file.is_file()
                })
                .collect();
            found.sort();
            files.extend(found);
        } else {
            files.push(full);
        }
    }
    if files.is_empty() {
        let named: Vec<String> = paths
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        return Err(EvalError::invalid(
            None,
            format!(
                "no task files in {}; docs/tasks.md says how to write one",
                named.join(", ")
            ),
        ));
    }
    let mut suite: Vec<(Task, TaskRef)> = Vec::with_capacity(files.len());
    for file in files {
        let (task, reference) = load_task(repo_root, &file)?;
        if let Some((other, _)) = suite.iter().find(|(other, _)| other.id == task.id) {
            return Err(EvalError::invalid(
                Some(&task.source),
                format!(
                    "repeats the task id {} of {}",
                    task.id,
                    other.source.display()
                ),
            ));
        }
        suite.push((task, reference));
    }
    Ok(suite)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, name: &str, text: &str) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, text).expect("write");
        path
    }

    #[test]
    fn reads_a_full_task_and_defaults_a_minimal_one() {
        let root = tempfile::tempdir().expect("tempdir");
        write(root.path(), "evals/a.check.patch", "patch");
        write(
            root.path(),
            "evals/a.json",
            r#"{"schema": "tune.task.v1", "id": "a", "title": "A", "base": "HEAD~1",
                "prompt": "do it", "setup": [{"argv": ["npm", "ci"], "timeout_secs": 60}],
                "check": {"argv": ["cargo", "test"], "timeout_secs": 30},
                "check_patch": "evals/a.check.patch", "agent_timeout_secs": 120, "tags": ["x"]}"#,
        );
        write(
            root.path(),
            "evals/min-task.json",
            r#"{"schema": "tune.task.v1", "prompt": "p", "check": {"argv": ["true"]}}"#,
        );
        write(root.path(), "evals/baseline.json", "{}");
        let suite = load_suite(root.path(), &[PathBuf::from("evals")]).expect("suite");
        let ids: Vec<&str> = suite.iter().map(|(task, _)| task.id.as_str()).collect();
        assert_eq!(ids, ["a", "min-task"]);
        let (full, reference) = &suite[0];
        assert_eq!(full.setup[0].timeout, Duration::from_secs(60));
        assert_eq!(full.check_patch, Some(PathBuf::from("evals/a.check.patch")));
        assert_eq!(full.agent_timeout, Duration::from_secs(120));
        assert_eq!(reference.source, "evals/a.json");
        assert_eq!(reference.sha256.len(), 64);
        let (minimal, _) = &suite[1];
        assert_eq!(minimal.base, None);
        assert_eq!(
            minimal.check.timeout,
            Duration::from_secs(DEFAULT_CHECK_TIMEOUT_SECS)
        );
        assert_eq!(
            minimal.agent_timeout,
            Duration::from_secs(DEFAULT_AGENT_TIMEOUT_SECS)
        );
    }

    #[test]
    fn reads_a_task_file_written_for_memprove_eval() {
        let root = tempfile::tempdir().expect("tempdir");
        write(
            root.path(),
            "evals/old.json",
            r#"{"schema": "memprove.eval.task.v1", "prompt": "p", "check": {"argv": ["true"]}}"#,
        );
        let (task, _) = load_task(root.path(), Path::new("evals/old.json")).expect("reads");
        assert_eq!(task.id, "old");
        assert_eq!(task.prompt, "p");
    }

    #[test]
    fn refuses_what_is_wrong_naming_the_member() {
        let root = tempfile::tempdir().expect("tempdir");
        let cases = [
            (
                r#"{"schema": "x", "prompt": "p", "check": {"argv": ["t"]}}"#,
                "`schema` must be tune.task.v1, not x",
            ),
            (
                r#"{"schema": "tune.task.v1", "prompt": "p", "check": {"argv": []}}"#,
                "`check.argv` must name a program",
            ),
            (
                r#"{"schema": "tune.task.v1", "prompt": " ", "check": {"argv": ["t"]}}"#,
                "`prompt` must not be empty",
            ),
            (
                r#"{"schema": "tune.task.v1", "prompt": "p", "check": {"argv": ["t"]}, "cheque": 1}"#,
                "`cheque` is not a known member",
            ),
            (
                r#"{"schema": "tune.task.v1", "id": "Bad Id", "prompt": "p", "check": {"argv": ["t"]}}"#,
                "`id` must be 1 to 64",
            ),
            (
                r#"{"schema": "tune.task.v1", "prompt": "p", "check": {"argv": ["t"]}, "check_patch": "../x"}"#,
                "`check_patch` must be a path relative",
            ),
            (
                r#"{"schema": "tune.task.v1", "prompt": "p", "check": {"argv": ["t"]}, "check_patch": "nope.patch"}"#,
                "`check_patch` names no file",
            ),
            (
                r#"{"schema": "tune.task.v1", "prompt": "p", "check": {"argv": ["t"], "timeout_secs": 0}}"#,
                "`check.timeout_secs` must be at least 1",
            ),
            (
                r#"{"schema": "tune.task.v1", "prompt": "p"}"#,
                "`check` is required",
            ),
        ];
        for (text, expected) in cases {
            write(root.path(), "t.json", text);
            let error = load_task(root.path(), Path::new("t.json"))
                .expect_err(text)
                .to_string();
            assert!(error.starts_with("t.json: "), "{error}");
            assert!(error.contains(expected), "{text}: {error}");
        }
    }

    #[test]
    fn refuses_a_repeated_id_and_an_empty_suite() {
        let root = tempfile::tempdir().expect("tempdir");
        let task =
            r#"{"schema": "tune.task.v1", "id": "same", "prompt": "p", "check": {"argv": ["t"]}}"#;
        write(root.path(), "evals/one.json", task);
        write(root.path(), "evals/two.json", task);
        let error = load_suite(root.path(), &[PathBuf::from("evals")])
            .expect_err("repeat")
            .to_string();
        assert_eq!(
            error,
            "evals/two.json: repeats the task id same of evals/one.json"
        );
        fs::create_dir_all(root.path().join("empty")).expect("dir");
        let error = load_suite(root.path(), &[PathBuf::from("empty")])
            .expect_err("empty")
            .to_string();
        assert!(error.starts_with("no task files in empty"), "{error}");
    }
}
