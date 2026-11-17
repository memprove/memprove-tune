//! A variant: today's configuration with something changed. Dropping a
//! unit, replacing or adding a file, or leaving the configuration out.

use std::path::Path;

use crate::{
    error::Error,
    files::{Entry, FileMap, is_config_path},
    units::{Inventory, Kind},
};

/// One change to the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// Without the unit of this id.
    Drop(String),
    /// With the configuration file at `path` holding `bytes` instead.
    Replace {
        /// The configuration file, from the repository root.
        path: String,
        /// Its new content.
        bytes: Vec<u8>,
    },
    /// With a new configuration file at `path` holding `bytes`.
    Add {
        /// The configuration file, from the repository root.
        path: String,
        /// Its content.
        bytes: Vec<u8>,
    },
    /// Without any configuration.
    Bare,
}

/// A named set of changes: one arm of an experiment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Variant {
    /// What the report calls it.
    pub(crate) label: String,
    /// The changes, in the order given.
    pub(crate) changes: Vec<Change>,
}

impl Change {
    /// The change `text` spells: `drop=UNIT`, `replace=PATH=FILE`,
    /// `add=PATH=FILE` or `bare`. `FILE` is read now, relative to `cwd`.
    pub(crate) fn parse(text: &str, cwd: &Path) -> Result<Self, Error> {
        let wrong = || {
            Error::Usage(format!(
                "`{text}` is not a change: write drop=UNIT, replace=PATH=FILE, add=PATH=FILE or bare"
            ))
        };
        if text == "bare" {
            return Ok(Self::Bare);
        }
        let (kind, value) = text.split_once('=').ok_or_else(wrong)?;
        if value.is_empty() {
            return Err(wrong());
        }
        match kind {
            "drop" => Ok(Self::Drop(value.to_owned())),
            "replace" | "add" => {
                let (path, file) = value
                    .split_once('=')
                    .filter(|(path, file)| !path.is_empty() && !file.is_empty())
                    .ok_or_else(wrong)?;
                let source = cwd.join(file);
                let bytes = std::fs::read(&source).map_err(|error| Error::io(&source, error))?;
                let path = path.to_owned();
                Ok(if kind == "add" {
                    Self::Add { path, bytes }
                } else {
                    Self::Replace { path, bytes }
                })
            }
            _ => Err(wrong()),
        }
    }

    /// The change in a few words, for a variant nobody named.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Drop(unit) => format!("without {unit}"),
            Self::Replace { path, .. } => format!("with {path} replaced"),
            Self::Add { path, .. } => format!("with {path} added"),
            Self::Bare => "without any configuration".to_owned(),
        }
    }
}

impl Variant {
    /// The variant `NAME:CHANGE[,CHANGE...]` spells.
    pub(crate) fn parse(spec: &str, cwd: &Path) -> Result<Self, Error> {
        let (label, changes) = spec
            .split_once(':')
            .filter(|(label, changes)| !label.is_empty() && !changes.is_empty())
            .ok_or_else(|| {
                Error::Usage(format!(
                    "`{spec}` is not a variant: write NAME:CHANGE[,CHANGE...]"
                ))
            })?;
        Ok(Self {
            label: label.to_owned(),
            changes: changes
                .split(',')
                .map(|change| Change::parse(change, cwd))
                .collect::<Result<_, _>>()?,
        })
    }

    /// A variant of one change, named after it.
    pub(crate) fn of(change: Change) -> Self {
        Self {
            label: change.label(),
            changes: vec![change],
        }
    }
}

/// `text` without its lines `first` to `last` (counted from 1).
fn without_lines(text: &str, first: usize, last: usize) -> String {
    text.split_inclusive('\n')
        .enumerate()
        .filter(|(index, _)| index + 1 < first || index + 1 > last)
        .map(|(_, line)| line)
        .collect()
}

/// `base` with `changes` made. `inventory` is that of `base`.
pub(crate) fn changed(
    base: &FileMap,
    inventory: &Inventory,
    changes: &[Change],
) -> Result<FileMap, Error> {
    let emptied = changes.contains(&Change::Bare);
    let mut files = if emptied {
        FileMap::new()
    } else {
        base.clone()
    };
    // The sections to cut, last line first, so that the lines of one cut
    // are not moved by another.
    let mut cuts: Vec<(&str, usize, usize)> = Vec::new();
    for change in changes {
        let Change::Drop(id) = change else {
            continue;
        };
        if emptied {
            return Err(Error::Usage(format!(
                "`bare` leaves no configuration, so there is no `{id}` to drop"
            )));
        }
        let unit = inventory
            .units
            .iter()
            .find(|unit| unit.id == *id)
            .ok_or_else(|| {
                Error::Usage(format!(
                    "the configuration has no unit `{id}`; `tune scan` lists the units"
                ))
            })?;
        match unit.kind {
            Kind::Section => cuts.push((&unit.file, unit.first_line, unit.last_line)),
            Kind::File | Kind::Rule => {
                files.remove(&unit.file);
            }
            Kind::Skill => {
                let inside = format!("{}/", unit.id);
                files.retain(|path, _| !path.starts_with(&inside));
            }
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    for (file, first, last) in cuts.into_iter().rev() {
        let Some(Entry::File { bytes, executable }) = files.get(file) else {
            continue;
        };
        let left = without_lines(&String::from_utf8_lossy(bytes), first, last);
        if left.trim().is_empty() {
            files.remove(file);
        } else {
            let entry = Entry::File {
                bytes: left.into_bytes(),
                executable: *executable,
            };
            files.insert(file.to_owned(), entry);
        }
    }
    for change in changes {
        match change {
            Change::Replace { path, bytes } => {
                if !files.contains_key(path) {
                    return Err(Error::Usage(format!(
                        "there is no configuration file `{path}` to replace; add=PATH=FILE adds one"
                    )));
                }
                files.insert(path.clone(), plain(bytes));
            }
            Change::Add { path, bytes } => {
                if !is_config_path(path) {
                    return Err(Error::Usage(format!(
                        "`{path}` is not a configuration file: those are CLAUDE.md, AGENTS.md and the files under .claude/rules and .claude/skills"
                    )));
                }
                if files.contains_key(path) {
                    return Err(Error::Usage(format!(
                        "the configuration file `{path}` exists; replace=PATH=FILE replaces it"
                    )));
                }
                files.insert(path.clone(), plain(bytes));
            }
            Change::Drop(_) | Change::Bare => {}
        }
    }
    Ok(files)
}

fn plain(bytes: &[u8]) -> Entry {
    Entry::File {
        bytes: bytes.to_vec(),
        executable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::inventory;

    fn map(files: &[(&str, &str)]) -> FileMap {
        files
            .iter()
            .map(|(path, text)| ((*path).to_owned(), Entry::text(text)))
            .collect()
    }

    fn after(files: &[(&str, &str)], changes: &[Change]) -> Result<FileMap, Error> {
        let base = map(files);
        let found = inventory(&base, &|_| None);
        changed(&base, &found, changes)
    }

    fn drop(unit: &str) -> Change {
        Change::Drop(unit.to_owned())
    }

    const CLAUDE: &str = "Intro.\n\n## Setup\nRun it.\n\n## Style\nTabs.\n\n## Tests\nRun them.\n";

    #[test]
    fn dropping_sections_removes_their_lines_and_nothing_else() {
        let one = after(&[("CLAUDE.md", CLAUDE)], &[drop("CLAUDE.md#style")]).expect("drop");
        assert_eq!(
            one["CLAUDE.md"].as_text(),
            Some("Intro.\n\n## Setup\nRun it.\n\n## Tests\nRun them.\n")
        );
        let two = after(
            &[("CLAUDE.md", CLAUDE)],
            &[drop("CLAUDE.md#setup"), drop("CLAUDE.md#tests")],
        )
        .expect("drop");
        assert_eq!(
            two["CLAUDE.md"].as_text(),
            Some("Intro.\n\n## Style\nTabs.\n\n")
        );
    }

    #[test]
    fn dropping_all_of_a_file_a_rule_or_a_skill_removes_its_files() {
        let files = [
            ("CLAUDE.md", "## A\na\n## B\nb\n"),
            ("AGENTS.md", "Plain.\n"),
            (".claude/rules/api.md", "# API\nJSON.\n"),
            (
                ".claude/skills/deploy/SKILL.md",
                "---\ndescription: d\n---\nGo.\n",
            ),
            (".claude/skills/deploy/run.sh", "echo\n"),
            (
                ".claude/skills/other/SKILL.md",
                "---\ndescription: o\n---\nGo.\n",
            ),
        ];
        let left = after(
            &files,
            &[
                drop("CLAUDE.md#a"),
                drop("CLAUDE.md#b"),
                drop("AGENTS.md"),
                drop(".claude/rules/api.md"),
                drop(".claude/skills/deploy"),
            ],
        )
        .expect("drop");
        let paths: Vec<&str> = left.keys().map(String::as_str).collect();
        assert_eq!(paths, [".claude/skills/other/SKILL.md"]);
    }

    #[test]
    fn an_unknown_unit_is_a_usage_error_that_says_where_to_look() {
        let error =
            after(&[("CLAUDE.md", CLAUDE)], &[drop("CLAUDE.md#nope")]).expect_err("unknown");
        assert!(
            matches!(&error, Error::Usage(detail) if detail.contains("CLAUDE.md#nope") && detail.contains("tune scan")),
            "{error}"
        );
    }

    #[test]
    fn replace_add_and_bare() {
        let files = [("CLAUDE.md", CLAUDE), (".claude/rules/api.md", "old\n")];
        let replaced = after(
            &files,
            &[
                Change::Replace {
                    path: ".claude/rules/api.md".to_owned(),
                    bytes: b"new\n".to_vec(),
                },
                Change::Add {
                    path: ".claude/rules/db.md".to_owned(),
                    bytes: b"added\n".to_vec(),
                },
            ],
        )
        .expect("changes");
        assert_eq!(replaced[".claude/rules/api.md"].as_text(), Some("new\n"));
        assert_eq!(replaced[".claude/rules/db.md"].as_text(), Some("added\n"));
        assert_eq!(replaced["CLAUDE.md"].as_text(), Some(CLAUDE));

        let bare = after(
            &files,
            &[
                Change::Bare,
                Change::Add {
                    path: "CLAUDE.md".to_owned(),
                    bytes: b"only this\n".to_vec(),
                },
            ],
        )
        .expect("bare");
        assert_eq!(bare.len(), 1);
        assert_eq!(bare["CLAUDE.md"].as_text(), Some("only this\n"));
    }

    #[test]
    fn changes_that_cannot_be_made_are_usage_errors() {
        let files = [("CLAUDE.md", CLAUDE)];
        let bytes = Vec::new();
        for change in [
            Change::Replace {
                path: ".claude/rules/missing.md".to_owned(),
                bytes: bytes.clone(),
            },
            Change::Add {
                path: "CLAUDE.md".to_owned(),
                bytes: bytes.clone(),
            },
            Change::Add {
                path: "src/main.rs".to_owned(),
                bytes: bytes.clone(),
            },
        ] {
            let error = after(&files, std::slice::from_ref(&change)).expect_err("refused");
            assert!(matches!(error, Error::Usage(_)), "{change:?}");
        }
        let error = after(&files, &[Change::Bare, drop("CLAUDE.md#setup")]).expect_err("refused");
        assert!(matches!(error, Error::Usage(_)));
    }

    #[test]
    fn changes_and_variants_are_parsed_from_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("new.md"), "new\n").expect("write");
        assert_eq!(
            Change::parse("drop=CLAUDE.md#setup", dir.path()).expect("drop"),
            drop("CLAUDE.md#setup")
        );
        assert_eq!(
            Change::parse("bare", dir.path()).expect("bare"),
            Change::Bare
        );
        assert_eq!(
            Change::parse("replace=CLAUDE.md=new.md", dir.path()).expect("replace"),
            Change::Replace {
                path: "CLAUDE.md".to_owned(),
                bytes: b"new\n".to_vec()
            }
        );
        let variant = Variant::parse(
            "short:drop=CLAUDE.md#a,add=.claude/rules/x.md=new.md",
            dir.path(),
        )
        .expect("variant");
        assert_eq!(variant.label, "short");
        assert_eq!(variant.changes.len(), 2);
        for wrong in [
            "",
            "nameonly",
            ":bare",
            "x:",
            "x:frob=1",
            "x:replace=CLAUDE.md",
        ] {
            assert!(
                matches!(Variant::parse(wrong, dir.path()), Err(Error::Usage(_))),
                "{wrong:?}"
            );
        }
        assert!(matches!(
            Change::parse("add=CLAUDE.md=missing.md", dir.path()),
            Err(Error::Io { .. })
        ));
        assert_eq!(drop("CLAUDE.md#a").label(), "without CLAUDE.md#a");
        assert_eq!(Change::Bare.label(), "without any configuration");
    }
}
