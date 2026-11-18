//! What `scan` can say about a configuration without running anything:
//! paths and commands that no longer exist, files that disagree, text
//! copied from the README or left to a formatter, and files that are too
//! long. Every check is a heuristic that prefers saying nothing to saying
//! something wrong.

use std::collections::BTreeSet;

use crate::{
    files::{Entry, FileMap, INSTRUCTION_FILES},
    markdown,
    units::{FileInfo, Inventory, Kind, Load, Unit, resolve},
};

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Smell {
    /// A stable name for the kind of finding, such as `missing-path`.
    pub(crate) code: &'static str,
    /// The configuration file.
    pub(crate) file: String,
    /// The line in `file`, counted from 1, when the finding has one.
    pub(crate) line: Option<usize>,
    /// The unit it is in, when it is in one.
    pub(crate) unit: Option<String>,
    /// One sentence.
    pub(crate) message: String,
}

/// What the checks ask about the rest of the repository.
pub(crate) struct Repo<'a> {
    /// Whether a file or directory is at this path from the root.
    pub(crate) exists: &'a dyn Fn(&str) -> bool,
    /// The text of the file at this path from the root.
    pub(crate) read: &'a dyn Fn(&str) -> Option<String>,
}

/// The lines Claude Code's guidance gives as the target for one
/// `CLAUDE.md` ("target under 200 lines per CLAUDE.md file").
pub(crate) const INSTRUCTION_FILE_LINES: usize = 200;
/// The lines its guidance gives for one `SKILL.md`.
pub(crate) const SKILL_FILE_LINES: usize = 500;
/// The characters of a skill's description it lists before cutting.
pub(crate) const SKILL_DESCRIPTION_CHARS: usize = 1536;

/// A path a configuration file names in inline code or a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamedPath {
    /// The line in the file, counted from 1.
    pub(crate) line: usize,
    /// The path as written.
    pub(crate) written: String,
    /// The path from the repository root it stands for.
    pub(crate) path: String,
    /// Whether it exists now.
    pub(crate) exists: bool,
}

/// The repository paths the prose of `text` (a file in `dir`) names in
/// inline code and links. A word counts as a path only when it has a `/`
/// and starts with `./`, `../` or a name that exists in `dir` or at the
/// root: `origin/main` and `application/json` are not paths. A path that
/// is not there from `dir` but is from the root stands for the one at the
/// root.
pub(crate) fn named_paths(text: &str, dir: &str, repo: &Repo<'_>) -> Vec<NamedPath> {
    let mut found: Vec<NamedPath> = Vec::new();
    for (index, line) in markdown::lines(text).iter().enumerate() {
        if line.in_code || line.in_front_matter {
            continue;
        }
        let mut words = markdown::code_spans(line.text);
        words.extend(markdown::link_targets(line.text));
        for written in words {
            let Some(path) = as_path(written, dir, repo) else {
                continue;
            };
            let line = index + 1;
            if found
                .iter()
                .any(|named| named.line == line && named.path == path)
            {
                continue;
            }
            found.push(NamedPath {
                line,
                written: written.to_owned(),
                exists: (repo.exists)(&path),
                path,
            });
        }
    }
    found
}

/// The path from the repository root that `written` stands for in a file
/// in `dir`, when it reads as a path of this repository.
fn as_path(written: &str, dir: &str, repo: &Repo<'_>) -> Option<String> {
    let word = written.split('#').next().unwrap_or_default();
    let word = without_position(word).trim_end_matches('/');
    let odd = |c: char| c.is_whitespace() || "*?{}<>$|\\\"'`()[]=,;%!".contains(c);
    if !word.contains('/')
        || word.contains("://")
        || word.starts_with(['/', '~', '-', '@'])
        || word.chars().any(odd)
    {
        return None;
    }
    if word.starts_with("../") {
        return resolve(dir, word);
    }
    if word.starts_with("./") {
        return here_or_root(dir, word, repo);
    }
    let first = word.split('/').next().unwrap_or_default();
    if !dir.is_empty() && (repo.exists)(&format!("{dir}/{first}")) {
        return here_or_root(dir, word, repo);
    }
    if (repo.exists)(first) {
        return resolve("", word);
    }
    None
}

/// `word` from `dir`; when nothing is there and the same path from the
/// repository root exists, that one. A nested file often names what the
/// agent, who works at the root, should run or read (`./scripts/test` in
/// a skill), and a path that is there is not reported as gone.
fn here_or_root(dir: &str, word: &str, repo: &Repo<'_>) -> Option<String> {
    let here = resolve(dir, word)?;
    if dir.is_empty() || (repo.exists)(&here) {
        return Some(here);
    }
    match resolve("", word) {
        Some(root) if (repo.exists)(&root) => Some(root),
        _ => Some(here),
    }
}

/// `word` without a `:line`, `:line:column` or `:line-line` ending.
fn without_position(word: &str) -> &str {
    match word.split_once(':') {
        Some((path, position))
            if !position.is_empty()
                && position
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ':' || c == '-') =>
        {
            path
        }
        _ => word,
    }
}

/// One simple command a configuration file writes down.
struct Command<'a> {
    /// The line in the file, counted from 1.
    line: usize,
    /// The directory it runs in, from the repository root, when the text
    /// says so with `cd` and the directory can be told.
    dir: Option<String>,
    /// Its words.
    words: Vec<&'a str>,
}

/// The commands of a file in `dir`: the inline code of its prose and the
/// lines of its code blocks, split at `&&`, `||`, `;` and `|`.
fn commands<'a>(lines: &[markdown::Line<'a>], dir: &str) -> Vec<Command<'a>> {
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.in_front_matter {
            continue;
        }
        let texts = if line.in_code {
            let text = line.text.trim_start();
            if text.starts_with("```") || text.starts_with("~~~") {
                continue;
            }
            vec![line.text]
        } else {
            markdown::code_spans(line.text)
        };
        for text in texts {
            // Where the commands of this text run: `cd` moves it.
            let mut cwd = Some(dir.to_owned());
            for part in text.split(['&', ';', '|']) {
                let mut words: Vec<&str> = part.split_whitespace().collect();
                while words.first().is_some_and(|word| {
                    matches!(*word, "$" | ">" | "sudo" | "time")
                        || (word.contains('=') && !word.starts_with('-'))
                }) {
                    words.remove(0);
                }
                let Some(first) = words.first() else {
                    continue;
                };
                if *first == "cd" {
                    cwd = match (cwd, words.get(1)) {
                        (Some(cwd), Some(to)) if !to.contains(['$', '~', '`']) => resolve(&cwd, to),
                        _ => None,
                    };
                    continue;
                }
                found.push(Command {
                    line: index + 1,
                    dir: cwd.clone(),
                    words,
                });
            }
        }
    }
    found
}

/// The JavaScript package managers and the lockfile each one writes.
const MANAGERS: [(&str, &[&str]); 4] = [
    ("npm", &["package-lock.json", "npm-shrinkwrap.json"]),
    ("pnpm", &["pnpm-lock.yaml"]),
    ("yarn", &["yarn.lock"]),
    ("bun", &["bun.lock", "bun.lockb"]),
];

/// The file `name` in `dir` or, failing that, at the root: its path and
/// its text.
fn nearest(repo: &Repo<'_>, dir: &str, name: &str) -> Option<(String, String)> {
    let local = if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    };
    if let Some(text) = (repo.read)(&local) {
        return Some((local, text));
    }
    (repo.read)(name).map(|text| (name.to_owned(), text))
}

/// The scripts of the `package.json` text, when it can be read.
fn scripts(text: &str) -> Option<BTreeSet<String>> {
    let value = tune_engine::strict_json::parse_strict(text).ok()?;
    value.as_object()?;
    Some(
        value
            .get("scripts")
            .and_then(|scripts| scripts.as_object())
            .map(|members| members.iter().map(|(name, _)| name.clone()).collect())
            .unwrap_or_default(),
    )
}

/// The targets of the Makefile `text`, or `None` when they cannot be told
/// from this file alone (it includes another file or has pattern rules).
fn make_targets(text: &str) -> Option<BTreeSet<String>> {
    let mut targets = BTreeSet::new();
    for line in text.lines() {
        if line.starts_with('\t') || line.trim_start().starts_with('#') {
            continue;
        }
        let first = line.split_whitespace().next().unwrap_or_default();
        if matches!(first, "include" | "-include" | "sinclude") {
            return None;
        }
        let Some((names, rest)) = line.split_once(':') else {
            continue;
        };
        if rest.starts_with('=') || names.contains(['=', '?', '+']) {
            continue;
        }
        if names.contains(['%', '$']) {
            return None;
        }
        targets.extend(names.split_whitespace().map(str::to_owned));
    }
    Some(targets)
}

/// What a missing script or make target says, when `command` names one.
fn missing_command(command: &Command<'_>, repo: &Repo<'_>) -> Vec<String> {
    let Some(dir) = &command.dir else {
        return Vec::new();
    };
    let words = &command.words;
    let tool = words[0];
    if MANAGERS.iter().any(|(manager, _)| *manager == tool) {
        let (Some(run), Some(script)) = (words.get(1), words.get(2)) else {
            return Vec::new();
        };
        if !matches!(*run, "run" | "run-script") || script.starts_with('-') {
            return Vec::new();
        }
        let Some((path, text)) = nearest(repo, dir, "package.json") else {
            return Vec::new();
        };
        return match scripts(&text) {
            Some(known) if !known.contains(*script) => vec![format!(
                "runs `{tool} run {script}`, but {path} has no script `{script}`"
            )],
            _ => Vec::new(),
        };
    }
    if tool != "make" || words[1..].iter().any(|word| word.starts_with('-')) {
        return Vec::new();
    }
    let Some((path, text)) = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .find_map(|name| nearest(repo, dir, name))
    else {
        return Vec::new();
    };
    let Some(known) = make_targets(&text) else {
        return Vec::new();
    };
    let file = if path == "Makefile" {
        "the Makefile".to_owned()
    } else {
        path
    };
    words[1..]
        .iter()
        .filter(|word| !word.contains('=') && !known.contains(**word))
        .map(|target| format!("runs `make {target}`, but {file} has no target `{target}`"))
        .collect()
}

/// The one package manager the lockfiles at the root speak for, and that
/// lockfile.
fn lockfile_manager(repo: &Repo<'_>) -> Option<(&'static str, &'static str)> {
    let mut present = MANAGERS.iter().filter_map(|(manager, lockfiles)| {
        lockfiles
            .iter()
            .find(|lockfile| (repo.exists)(lockfile))
            .map(|lockfile| (*manager, *lockfile))
    });
    let only = present.next()?;
    present.next().is_none().then_some(only)
}

/// The words that state a formatting rule.
const FORMAT_WORDS: [&str; 16] = [
    "2 spaces",
    "4 spaces",
    "two spaces",
    "four spaces",
    "indent",
    "semicolon",
    "single quote",
    "double quote",
    "trailing comma",
    "line length",
    "line width",
    "print width",
    "max-len",
    "trailing whitespace",
    "final newline",
    "import order",
];

/// The files that configure a formatter.
const FORMATTER_FILES: [&str; 19] = [
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.yaml",
    ".prettierrc.yml",
    ".prettierrc.js",
    ".prettierrc.cjs",
    ".prettierrc.mjs",
    ".prettierrc.toml",
    "prettier.config.js",
    "prettier.config.cjs",
    "prettier.config.mjs",
    "prettier.config.ts",
    "biome.json",
    "biome.jsonc",
    "dprint.json",
    "rustfmt.toml",
    ".rustfmt.toml",
    ".clang-format",
    ".editorconfig",
];

/// The names a repository gives its README.
const README_FILES: [&str; 4] = ["README.md", "readme.md", "Readme.md", "README"];

/// The fewest characters of a line that counts when looking for copied
/// text, the fewest copied lines that count, and the share of a unit's
/// lines (in percent) that must be copied.
const COPIED_LINE_CHARS: usize = 20;
const COPIED_LINES: usize = 5;
const COPIED_PERCENT: usize = 60;

/// A finding and where it sorts: the file's place and its line.
struct Found {
    file: usize,
    smell: Smell,
}

/// Everything the checks of one file look at.
struct Subject<'a> {
    /// The place of the file in the inventory.
    index: usize,
    path: &'a str,
    dir: &'a str,
    lines: Vec<markdown::Line<'a>>,
    units: Vec<&'a Unit>,
}

impl Subject<'_> {
    fn unit_at(&self, line: usize) -> Option<&Unit> {
        self.units
            .iter()
            .find(|unit| unit.first_line <= line && line <= unit.last_line)
            .copied()
    }

    fn at_line(&self, code: &'static str, line: usize, message: String) -> Found {
        Found {
            file: self.index,
            smell: Smell {
                code,
                file: self.path.to_owned(),
                line: Some(line),
                unit: self.unit_at(line).map(|unit| unit.id.clone()),
                message,
            },
        }
    }

    fn whole(&self, code: &'static str, message: String) -> Found {
        Found {
            file: self.index,
            smell: Smell {
                code,
                file: self.path.to_owned(),
                line: None,
                unit: None,
                message,
            },
        }
    }

    /// The lines of `unit` that say something: no heading, no blank line,
    /// no front matter, trimmed.
    fn body(&self, unit: &Unit) -> Vec<&str> {
        self.lines[unit.first_line - 1..unit.last_line.min(self.lines.len())]
            .iter()
            .filter(|line| !line.in_front_matter)
            .filter(|line| line.in_code || !line.text.trim_start().starts_with('#'))
            .map(|line| line.text.trim())
            .filter(|text| !text.is_empty())
            .collect()
    }
}

/// Every finding for `files`, in the order of `inventory`'s files; in one
/// file, the findings about the whole file come first and the rest follow
/// by line.
pub(crate) fn smells(files: &FileMap, inventory: &Inventory, repo: &Repo<'_>) -> Vec<Smell> {
    let subjects: Vec<Subject<'_>> = inventory
        .files
        .iter()
        .enumerate()
        .filter_map(|(index, info)| {
            let text = files.get(&info.path).and_then(Entry::as_text)?;
            Some(Subject {
                index,
                path: &info.path,
                dir: info.path.rsplit_once('/').map_or("", |(dir, _)| dir),
                lines: markdown::lines(text),
                units: inventory
                    .units
                    .iter()
                    .filter(|unit| unit.file == info.path)
                    .collect(),
            })
        })
        .collect();

    let mut found: Vec<Found> = Vec::new();
    let copies = copies(files, &subjects, &mut found);
    for subject in &subjects {
        let info = &inventory.files[subject.index];
        whole_file(subject, info, files, &mut found);
        diverged(subject, files, inventory, &mut found);
        if matches!(info.load, Load::NotRead(_)) {
            found.push(subject.whole(
                "not-read",
                "is not read by Claude Code: it reads CLAUDE.md, and no CLAUDE.md imports this file"
                    .to_owned(),
            ));
        }
        let text = files
            .get(subject.path)
            .and_then(Entry::as_text)
            .unwrap_or_default();
        for named in named_paths(text, subject.dir, repo) {
            if !named.exists {
                found.push(subject.at_line(
                    "missing-path",
                    named.line,
                    format!("names `{}`, which does not exist", named.written),
                ));
            }
        }
        written_commands(subject, repo, &mut found);
    }
    duplicates(&subjects, &copies, &mut found);
    for subject in &subjects {
        copied_readme(subject, repo, &mut found);
        formatter_rules(subject, repo, &mut found);
        for (at, unit) in subject.units.iter().enumerate() {
            // The number of `#` of the heading a unit begins with.
            let level = |unit: &Unit| {
                subject.lines.get(unit.first_line - 1).map_or(0, |line| {
                    line.text
                        .trim_start()
                        .chars()
                        .take_while(|c| *c == '#')
                        .count()
                })
            };
            // A title whose sections are the units that follow is not
            // empty.
            let is_title = subject
                .units
                .get(at + 1)
                .is_some_and(|next| level(next) > level(unit));
            if unit.kind == Kind::Section
                && level(unit) > 0
                && !is_title
                && subject.body(unit).is_empty()
            {
                found.push(subject.at_line(
                    "empty-section",
                    unit.first_line,
                    "has a heading and nothing under it".to_owned(),
                ));
            }
        }
    }
    found.sort_by_key(|found| (found.file, found.smell.line.unwrap_or(0)));
    found.into_iter().map(|found| found.smell).collect()
}

/// The files whose whole text is that of an earlier file: each is named
/// once here, and its units are not named again one by one.
fn copies<'a>(
    files: &FileMap,
    subjects: &[Subject<'a>],
    found: &mut Vec<Found>,
) -> BTreeSet<&'a str> {
    let text = |subject: &Subject<'_>| {
        files
            .get(subject.path)
            .and_then(Entry::as_text)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let mut copies = BTreeSet::new();
    for (at, subject) in subjects.iter().enumerate() {
        let Some(mine) = text(subject) else {
            continue;
        };
        if let Some(first) = subjects[..at]
            .iter()
            .find(|earlier| text(earlier) == Some(mine))
        {
            copies.insert(subject.path);
            found.push(subject.whole("duplicate", format!("is a copy of {}", first.path)));
        }
    }
    copies
}

/// Findings about a file as a whole: its length and, for a skill, its
/// description.
fn whole_file(subject: &Subject<'_>, info: &FileInfo, files: &FileMap, found: &mut Vec<Found>) {
    let name = subject.path.rsplit('/').next().unwrap_or_default();
    let skill = subject.units.iter().find(|unit| unit.kind == Kind::Skill);
    let limit = if skill.is_some() {
        Some((SKILL_FILE_LINES, "SKILL.md"))
    } else if INSTRUCTION_FILES.contains(&name) {
        Some((INSTRUCTION_FILE_LINES, "CLAUDE.md"))
    } else {
        None
    };
    if let Some((limit, kind)) = limit
        && info.lines > limit
    {
        found.push(subject.whole(
            "long-file",
            format!(
                "has {} lines; Claude Code's guidance is under {limit} lines for one {kind}",
                info.lines
            ),
        ));
    }
    if skill.is_none() {
        return;
    }
    let text = files
        .get(subject.path)
        .and_then(Entry::as_text)
        .unwrap_or_default();
    let front = markdown::front_matter(text);
    let value = |key: &str| {
        front
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
    };
    if value("disable-model-invocation") == Some("true") {
        return;
    }
    let description = [value("description"), value("when_to_use")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let chars = description.chars().count();
    if chars == 0 {
        found.push(subject.whole(
            "skill-description",
            "has no description in its front matter, which is what Claude Code goes by to decide when to use it"
                .to_owned(),
        ));
    } else if chars > SKILL_DESCRIPTION_CHARS {
        found.push(subject.whole(
            "skill-description",
            format!(
                "has a description of {chars} characters; Claude Code lists the first {SKILL_DESCRIPTION_CHARS}"
            ),
        ));
    }
}

/// A `CLAUDE.md` beside an `AGENTS.md` that says something else, when
/// neither brings the other in.
fn diverged(subject: &Subject<'_>, files: &FileMap, inventory: &Inventory, found: &mut Vec<Found>) {
    if subject.path.rsplit('/').next() != Some("CLAUDE.md") {
        return;
    }
    let Some(sibling) = resolve(subject.dir, "AGENTS.md") else {
        return;
    };
    let text = |path: &str| files.get(path).and_then(Entry::as_text).map(str::trim);
    let (Some(mine), Some(theirs)) = (text(subject.path), text(&sibling)) else {
        return;
    };
    let brings = |from: &str, to: &str| {
        inventory
            .units
            .iter()
            .any(|unit| unit.file == from && unit.imports.iter().any(|import| import == to))
    };
    if mine == theirs || brings(subject.path, &sibling) || brings(&sibling, subject.path) {
        return;
    }
    found.push(subject.whole(
        "diverged-files",
        format!(
            "differs from {sibling} and neither imports the other: agents that read different files get different instructions"
        ),
    ));
}

/// Scripts and make targets that are gone, and package managers other
/// than the lockfile's.
fn written_commands(subject: &Subject<'_>, repo: &Repo<'_>, found: &mut Vec<Found>) {
    let expected = lockfile_manager(repo);
    // The units already told about a manager, so each hears it once.
    let mut told: Vec<(Option<String>, &str)> = Vec::new();
    for command in commands(&subject.lines, subject.dir) {
        for message in missing_command(&command, repo) {
            found.push(subject.at_line("missing-command", command.line, message));
        }
        let Some((manager, lockfile)) = expected else {
            continue;
        };
        let tool = command.words[0];
        if tool == manager || !MANAGERS.iter().any(|(other, _)| *other == tool) {
            continue;
        }
        let unit = subject.unit_at(command.line).map(|unit| unit.id.clone());
        if told
            .iter()
            .any(|(seen, other)| *seen == unit && *other == tool)
        {
            continue;
        }
        told.push((unit, tool));
        found.push(subject.at_line(
            "package-manager",
            command.line,
            format!("runs `{tool}`, but the repository's lockfile is {lockfile}"),
        ));
    }
}

/// Units that say the same as an earlier unit.
fn duplicates(subjects: &[Subject<'_>], copies: &BTreeSet<&str>, found: &mut Vec<Found>) {
    let mut seen: Vec<(Vec<&str>, &str)> = Vec::new();
    for subject in subjects {
        for unit in &subject.units {
            if unit.kind == Kind::Skill {
                continue;
            }
            let body = subject.body(unit);
            if body.is_empty() {
                continue;
            }
            match seen.iter().find(|(other, _)| *other == body) {
                Some((_, first)) if !copies.contains(subject.path) => {
                    found.push(subject.at_line(
                        "duplicate",
                        unit.first_line,
                        format!("says the same as {first}"),
                    ));
                }
                Some(_) => {}
                None => seen.push((body, &unit.id)),
            }
        }
    }
}

/// Units most of whose lines are in the README word for word.
fn copied_readme(subject: &Subject<'_>, repo: &Repo<'_>, found: &mut Vec<Found>) {
    let Some((name, readme)) = README_FILES
        .iter()
        .find_map(|name| (repo.read)(name).map(|text| (*name, text)))
    else {
        return;
    };
    let theirs: BTreeSet<&str> = readme.lines().map(str::trim).collect();
    for unit in &subject.units {
        if unit.kind == Kind::Skill {
            continue;
        }
        let lines: Vec<&str> = subject
            .body(unit)
            .into_iter()
            .filter(|line| line.chars().count() >= COPIED_LINE_CHARS)
            .collect();
        let copied = lines.iter().filter(|line| theirs.contains(**line)).count();
        if copied >= COPIED_LINES && copied * 100 >= lines.len() * COPIED_PERCENT {
            found.push(subject.at_line(
                "copied-readme",
                unit.first_line,
                format!(
                    "repeats {name}: {copied} of its {} lines are there word for word",
                    lines.len()
                ),
            ));
        }
    }
}

/// Units that state formatting rules a formatter's configuration is there
/// to enforce.
fn formatter_rules(subject: &Subject<'_>, repo: &Repo<'_>, found: &mut Vec<Found>) {
    let Some(formatter) = FORMATTER_FILES.iter().find(|name| (repo.exists)(name)) else {
        return;
    };
    for unit in &subject.units {
        if unit.kind == Kind::Skill {
            continue;
        }
        let prose: String = subject.lines
            [unit.first_line - 1..unit.last_line.min(subject.lines.len())]
            .iter()
            .filter(|line| !line.in_code && !line.in_front_matter)
            .flat_map(|line| [line.text, "\n"])
            .collect::<String>()
            .to_lowercase();
        let mut stated: Vec<&str> = FORMAT_WORDS
            .iter()
            .filter(|word| prose.contains(**word))
            .copied()
            .collect();
        if stated.len() < 2 {
            continue;
        }
        stated.sort_unstable();
        found.push(subject.at_line(
            "formatter-rules",
            unit.first_line,
            format!(
                "states formatting rules ({}) that {formatter} can enforce",
                stated.join(", ")
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::units::inventory;

    /// A repository in memory: configuration files and other files.
    struct Fake {
        config: FileMap,
        other: BTreeMap<String, String>,
    }

    impl Fake {
        fn new(config: &[(&str, &str)], other: &[(&str, &str)]) -> Self {
            Self {
                config: config
                    .iter()
                    .map(|(path, text)| ((*path).to_owned(), Entry::text(text)))
                    .collect(),
                other: other
                    .iter()
                    .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
                    .collect(),
            }
        }

        fn exists(&self, path: &str) -> bool {
            let under = format!("{path}/");
            self.config
                .keys()
                .chain(self.other.keys())
                .any(|file| file == path || file.starts_with(&under))
        }

        fn read(&self, path: &str) -> Option<String> {
            self.other.get(path).cloned()
        }

        fn smells(&self) -> Vec<Smell> {
            let read = |path: &str| self.read(path);
            let exists = |path: &str| self.exists(path);
            let found = inventory(&self.config, &read);
            smells(
                &self.config,
                &found,
                &Repo {
                    exists: &exists,
                    read: &read,
                },
            )
        }

        fn codes(&self) -> Vec<(&'static str, Option<usize>, String)> {
            self.smells()
                .into_iter()
                .map(|smell| (smell.code, smell.line, smell.message))
                .collect()
        }
    }

    #[test]
    fn a_path_that_is_gone_is_named_with_its_line_and_unit() {
        let fake = Fake::new(
            &[(
                "CLAUDE.md",
                "## Setup\nRun `scripts/setup.sh`, then read [the guide](docs/setup.md#install).\n## Code\nEdit `src/app/main.ts:42` and `./src/gone.ts`.\nBranch `origin/main`, type `application/json`, glob `src/**/*.ts`,\n`https://example.com/a/b`, `$HOME/x`, `a/b c/d`.\n```\ncat scripts/also-gone.sh\n```\n",
            )],
            &[
                ("scripts/build.sh", ""),
                ("docs/setup.md", ""),
                ("src/app/main.ts", ""),
            ],
        );
        let found = fake.smells();
        let missing: Vec<_> = found
            .iter()
            .filter(|smell| smell.code == "missing-path")
            .collect();
        assert_eq!(missing.len(), 2, "{found:#?}");
        assert_eq!(missing[0].line, Some(2));
        assert_eq!(missing[0].unit.as_deref(), Some("CLAUDE.md#setup"));
        assert_eq!(
            missing[0].message,
            "names `scripts/setup.sh`, which does not exist"
        );
        assert_eq!(missing[1].line, Some(4));
        assert_eq!(missing[1].unit.as_deref(), Some("CLAUDE.md#code"));
        assert!(missing[1].message.contains("`./src/gone.ts`"));
    }

    #[test]
    fn a_nested_file_names_paths_from_its_own_directory() {
        let fake = Fake::new(
            &[(
                "web/CLAUDE.md",
                "See `src/app.ts` and `src/gone.ts` and `docs/api.md`.\n",
            )],
            &[("web/src/app.ts", ""), ("docs/api.md", "")],
        );
        let exists = |path: &str| fake.exists(path);
        let read = |path: &str| fake.read(path);
        let repo = Repo {
            exists: &exists,
            read: &read,
        };
        let named = named_paths(
            fake.config["web/CLAUDE.md"].as_text().expect("text"),
            "web",
            &repo,
        );
        let seen: Vec<(&str, bool)> = named
            .iter()
            .map(|named| (named.path.as_str(), named.exists))
            .collect();
        assert_eq!(
            seen,
            [
                ("web/src/app.ts", true),
                ("web/src/gone.ts", false),
                ("docs/api.md", true)
            ]
        );
    }

    #[test]
    fn a_path_that_is_there_from_the_root_is_not_missing_in_a_nested_file() {
        // A skill tells the agent to run `./scripts/test`: a command for the
        // repository root, where the agent works, not for the skill's
        // directory.
        let fake = Fake::new(
            &[(
                ".claude/skills/tests/SKILL.md",
                "Run `./scripts/test` and `./scripts/gone`, with `./notes/own.md` at hand.\nThen `scripts/lint` and `notes/root-only.md`.\n",
            )],
            &[
                ("scripts/test", ""),
                ("scripts/lint", ""),
                ("notes/root-only.md", ""),
                (".claude/skills/tests/notes/own.md", ""),
            ],
        );
        let exists = |path: &str| fake.exists(path);
        let read = |path: &str| fake.read(path);
        let repo = Repo {
            exists: &exists,
            read: &read,
        };
        let named = named_paths(
            fake.config[".claude/skills/tests/SKILL.md"]
                .as_text()
                .expect("text"),
            ".claude/skills/tests",
            &repo,
        );
        let seen: Vec<(&str, bool)> = named
            .iter()
            .map(|named| (named.path.as_str(), named.exists))
            .collect();
        assert_eq!(
            seen,
            [
                ("scripts/test", true),
                (".claude/skills/tests/scripts/gone", false),
                (".claude/skills/tests/notes/own.md", true),
                ("scripts/lint", true),
                // Its first name is in the skill's directory, the file is
                // only at the root.
                ("notes/root-only.md", true),
            ]
        );
    }

    #[test]
    fn a_script_or_make_target_that_is_gone_is_named() {
        let fake = Fake::new(
            &[(
                "CLAUDE.md",
                "Run `npm run lint` and `npm run build`; `npm --prefix web run nope` is skipped.\n```sh\n$ npm test && npm run e2e\nmake test\nmake deploy VERSION=1\n```\n",
            )],
            &[
                (
                    "package.json",
                    r#"{"scripts": {"test": "jest", "build": "tsc"}}"#,
                ),
                ("package-lock.json", "{}"),
                (
                    "Makefile",
                    "CC = gcc\n.PHONY: test\ntest: build\n\t$(CC) x\nbuild lib:\n\ttrue\n",
                ),
            ],
        );
        assert_eq!(
            fake.codes(),
            [
                (
                    "missing-command",
                    Some(1),
                    "runs `npm run lint`, but package.json has no script `lint`".to_owned()
                ),
                (
                    "missing-command",
                    Some(3),
                    "runs `npm run e2e`, but package.json has no script `e2e`".to_owned()
                ),
                (
                    "missing-command",
                    Some(5),
                    "runs `make deploy`, but the Makefile has no target `deploy`".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn commands_are_not_checked_without_the_file_that_defines_them() {
        let fake = Fake::new(
            &[("CLAUDE.md", "Run `npm run lint` and `make deploy`.\n")],
            &[("Makefile", "include common.mk\ntest:\n\ttrue\n")],
        );
        assert_eq!(fake.codes(), []);
    }

    #[test]
    fn another_package_manager_than_the_lockfiles_is_named_once_per_unit() {
        let fake = Fake::new(
            &[(
                "CLAUDE.md",
                "Run `npm install`, `npm test`, `yarn test` and `pnpm test`.\n",
            )],
            &[
                ("pnpm-lock.yaml", ""),
                ("package.json", r#"{"scripts": {"test": "vitest"}}"#),
            ],
        );
        assert_eq!(
            fake.codes(),
            [
                (
                    "package-manager",
                    Some(1),
                    "runs `npm`, but the repository's lockfile is pnpm-lock.yaml".to_owned()
                ),
                (
                    "package-manager",
                    Some(1),
                    "runs `yarn`, but the repository's lockfile is pnpm-lock.yaml".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn two_instruction_files_that_differ_and_a_file_nobody_reads() {
        let both = Fake::new(
            &[
                ("CLAUDE.md", "## Style\nTabs.\n## Tests\nRun them.\n"),
                ("AGENTS.md", "## Style\nTabs.\n## Tests\nSkip them.\n"),
            ],
            &[],
        );
        let codes: Vec<&str> = both.smells().iter().map(|smell| smell.code).collect();
        assert_eq!(codes, ["diverged-files", "not-read", "duplicate"]);
        let messages: Vec<String> = both
            .smells()
            .into_iter()
            .map(|smell| smell.message)
            .collect();
        assert_eq!(
            messages[0],
            "differs from AGENTS.md and neither imports the other: agents that read different files get different instructions"
        );
        assert!(messages[1].starts_with("is not read by Claude Code"));
        assert_eq!(messages[2], "says the same as CLAUDE.md#style");

        let imported = Fake::new(
            &[
                ("CLAUDE.md", "@AGENTS.md\n"),
                ("AGENTS.md", "## Style\nTabs.\n## Tests\nRun them.\n"),
            ],
            &[],
        );
        assert_eq!(imported.codes(), []);
    }

    #[test]
    fn a_file_that_is_a_copy_of_another_is_named_once() {
        let text = "## Style\nTabs.\n## Tests\nRun them.\n";
        let fake = Fake::new(&[("CLAUDE.md", text), ("AGENTS.md", text)], &[]);
        let found = fake.codes();
        assert_eq!(found.len(), 2, "{found:#?}");
        assert_eq!(
            found[0],
            ("duplicate", None, "is a copy of CLAUDE.md".to_owned())
        );
        assert_eq!(found[1].0, "not-read");
    }

    #[test]
    fn a_command_after_cd_is_checked_in_that_directory() {
        let fake = Fake::new(
            &[(
                "CLAUDE.md",
                "Run `cd web && npm run dev` and `cd web && npm run nope`, or `cd $DIR && npm run x`.\n",
            )],
            &[
                ("package.json", r#"{"scripts": {}}"#),
                ("web/package.json", r#"{"scripts": {"dev": "vite"}}"#),
            ],
        );
        assert_eq!(
            fake.codes(),
            [(
                "missing-command",
                Some(1),
                "runs `npm run nope`, but web/package.json has no script `nope`".to_owned()
            )]
        );
    }

    #[test]
    fn text_copied_from_the_readme_is_named() {
        let copied = "This project is a parcel tracking service for small shops.\nIt exposes a REST API on port 8080 for all clients.\nOrders are stored in PostgreSQL through the orders module.\nThe worker process sends notification emails every hour.\nDeployments go through the staging cluster before production.\n";
        let fake = Fake::new(
            &[(
                "CLAUDE.md",
                &format!(
                    "## About\n{copied}One line that is only here, about the agent.\n## Rules\nNever edit the generated client by hand, regenerate it.\n"
                ),
            )],
            &[("README.md", &format!("# Parcel\n\n{copied}\n## License\n"))],
        );
        assert_eq!(
            fake.codes(),
            [(
                "copied-readme",
                Some(1),
                "repeats README.md: 5 of its 6 lines are there word for word".to_owned()
            )]
        );
    }

    #[test]
    fn style_rules_beside_a_formatter_configuration_are_named() {
        let text = "## Style\nIndent with 2 spaces.\nAlways use semicolons and single quotes.\n## Other\nUse tabs in Makefiles.\n";
        let with = Fake::new(&[("CLAUDE.md", text)], &[(".prettierrc", "{}")]);
        assert_eq!(
            with.codes(),
            [(
                "formatter-rules",
                Some(1),
                "states formatting rules (2 spaces, indent, semicolon, single quote) that .prettierrc can enforce".to_owned()
            )]
        );
        let without = Fake::new(&[("CLAUDE.md", text)], &[]);
        assert_eq!(without.codes(), []);
    }

    #[test]
    fn long_files_and_empty_sections_are_named() {
        let long = format!("## A\n{}## Empty\n\n## B\nb\n", "line\n".repeat(200));
        let fake = Fake::new(&[("CLAUDE.md", &long)], &[]);
        assert_eq!(
            fake.codes(),
            [
                (
                    "long-file",
                    None,
                    "has 205 lines; Claude Code's guidance is under 200 lines for one CLAUDE.md"
                        .to_owned()
                ),
                (
                    "empty-section",
                    Some(202),
                    "has a heading and nothing under it".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_title_above_the_sections_is_not_an_empty_section() {
        let fake = Fake::new(
            &[(
                "CLAUDE.md",
                "# Project notes\n\n## Setup\nRun it.\n\n## Style\nTabs.\n\n## Empty\n",
            )],
            &[],
        );
        assert_eq!(
            fake.codes(),
            [(
                "empty-section",
                Some(9),
                "has a heading and nothing under it".to_owned()
            )]
        );
    }

    #[test]
    fn a_skill_without_a_description_or_with_too_much_is_named() {
        let long = format!(
            "---\ndescription: {}\n---\nbody\n",
            "word ".repeat(400).trim_end()
        );
        let huge = format!("---\ndescription: ok\n---\n{}", "step\n".repeat(600));
        let fake = Fake::new(
            &[
                (".claude/skills/bare/SKILL.md", "# Bare\nNo front matter.\n"),
                (".claude/skills/long/SKILL.md", &long),
                (".claude/skills/huge/SKILL.md", &huge),
            ],
            &[],
        );
        assert_eq!(
            fake.codes(),
            [
                (
                    "skill-description",
                    None,
                    "has no description in its front matter, which is what Claude Code goes by to decide when to use it".to_owned()
                ),
                (
                    "long-file",
                    None,
                    "has 603 lines; Claude Code's guidance is under 500 lines for one SKILL.md".to_owned()
                ),
                (
                    "skill-description",
                    None,
                    "has a description of 1999 characters; Claude Code lists the first 1536".to_owned()
                ),
            ]
        );
    }
}
