//! Writing task files: the text of one, the timeouts a written task gets,
//! and where they go.

use std::{path::Path, time::Duration};

use crate::{
    CommandSpec, TASK_SCHEMA, Task,
    detect::detect,
    json::{Out, string},
};

/// Where task files are written, relative to the repository root.
pub const TASKS_DIR: &str = ".tune/tasks";

/// How long the agent gets for a written task.
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(1800);
/// How long a written task's check gets.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(1800);
/// How long each setup command of a written task gets.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(900);

/// The repository's test command, guessed from its files (`Cargo.toml`,
/// `package.json`, `pyproject.toml`, `go.mod`, ...); `None` when there is
/// no guess. See [`detect`] for the rules and the setup the command needs.
#[must_use]
pub fn detect_check(repo_root: &Path) -> Option<Vec<String>> {
    detect(repo_root).map(|found| found.check)
}

/// The text of a task file for `task`, as `mine` writes it: members in the
/// order of the format, the optional ones left out when empty.
/// [`Task::source`] is not written.
#[must_use]
pub fn render_task(task: &Task) -> String {
    let command = |spec: &CommandSpec| {
        Out::Object(vec![
            ("argv", Out::strs(&spec.argv)),
            ("timeout_secs", Out::uint(spec.timeout.as_secs())),
        ])
    };
    let mut members = vec![
        ("schema", Out::str(TASK_SCHEMA)),
        ("id", Out::str(&task.id)),
    ];
    if let Some(title) = &task.title {
        members.push(("title", Out::str(title)));
    }
    if let Some(base) = &task.base {
        members.push(("base", Out::str(base)));
    }
    members.push(("prompt", Out::str(&task.prompt)));
    if !task.setup.is_empty() {
        members.push((
            "setup",
            Out::Array(task.setup.iter().map(command).collect()),
        ));
    }
    members.push(("check", command(&task.check)));
    if let Some(patch) = &task.check_patch {
        members.push(("check_patch", Out::Raw(string(&slashed(patch)))));
    }
    members.push((
        "agent_timeout_secs",
        Out::uint(task.agent_timeout.as_secs()),
    ));
    if !task.tags.is_empty() {
        members.push(("tags", Out::strs(&task.tags)));
    }
    Out::Object(members).render()
}

/// `path` with `/` between its parts, as task files name paths.
pub(crate) fn slashed(path: &Path) -> String {
    path.iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The setup commands `setup` as task commands.
pub(crate) fn setup_commands(setup: &[Vec<String>]) -> Vec<CommandSpec> {
    setup
        .iter()
        .map(|argv| CommandSpec {
            argv: argv.clone(),
            timeout: SETUP_TIMEOUT,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Detected, ProjectKind};

    #[test]
    fn a_task_renders_its_members_in_the_format_order() {
        let found = Detected {
            kind: ProjectKind::Node,
            marker: "package.json",
            setup: vec![vec!["npm".to_owned(), "ci".to_owned()]],
            check: vec!["npm".to_owned(), "test".to_owned()],
            also: Vec::new(),
        };
        let task = Task {
            id: "smoke".to_owned(),
            title: Some("the test suite passes".to_owned()),
            base: None,
            prompt: "Make the tests pass.".to_owned(),
            setup: setup_commands(&found.setup),
            check: CommandSpec {
                argv: found.check.clone(),
                timeout: CHECK_TIMEOUT,
            },
            check_patch: None,
            agent_timeout: AGENT_TIMEOUT,
            tags: vec!["smoke".to_owned()],
            source: Path::new(TASKS_DIR).join("smoke.json"),
        };
        let text = render_task(&task);
        let order = [
            "\"schema\"",
            "\"id\"",
            "\"title\"",
            "\"prompt\"",
            "\"setup\"",
            "\"check\"",
            "\"agent_timeout_secs\"",
            "\"tags\"",
        ];
        let positions: Vec<usize> = order
            .iter()
            .map(|member| text.find(member).expect(member))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
        assert!(!text.contains("\"base\""), "{text}");
        assert!(!text.contains("\"check_patch\""), "{text}");
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn paths_in_task_files_use_slashes() {
        assert_eq!(
            slashed(&Path::new("evals").join("a.check.patch")),
            "evals/a.check.patch"
        );
    }
}
