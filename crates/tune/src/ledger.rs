//! The verdicts a repository keeps: `.tune/verdicts.json`, one entry per
//! unit that was tested, with what the entry is bound to. A verdict is
//! stale once the unit or the tasks it was measured on have changed.

use std::{fs, path::Path};

use sha2::{Digest, Sha256};
use tune_engine::{
    Task, TaskRef,
    json::Out,
    strict_json::{JsonValue, parse_strict},
};

use crate::{error::Error, units::Inventory};

/// The schema of the ledger.
pub(crate) const LEDGER_SCHEMA: &str = "tune.verdicts.v1";

/// Where the ledger is, from the repository root.
pub(crate) const LEDGER_PATH: &str = ".tune/verdicts.json";

/// What was found about one unit, and what the finding is bound to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    /// The unit's id.
    pub(crate) unit: String,
    /// The SHA-256 of the unit's content when it was tested.
    pub(crate) sha256: String,
    /// `keep`, `removable` or `inconclusive`.
    pub(crate) advice: String,
    /// Pairs where the agent passed with the unit and without it.
    pub(crate) both_pass: u64,
    /// Pairs where it failed with the unit and without it.
    pub(crate) both_fail: u64,
    /// Pairs where it passed only with the unit.
    pub(crate) only_baseline: u64,
    /// Pairs where it passed only without the unit.
    pub(crate) only_variant: u64,
    /// The margin of "no worse" the advice was given at.
    pub(crate) margin: f64,
    /// The digest of the whole configuration the unit was tested in.
    pub(crate) config: String,
    /// The digest of the task files.
    pub(crate) tasks: String,
    /// Where the task files were, as the command was given them.
    pub(crate) task_paths: Vec<String>,
    /// The agent, as the report names it.
    pub(crate) agent: String,
    /// The agent's version, when it reports one.
    pub(crate) agent_version: Option<String>,
    /// The experiment that holds the trials.
    pub(crate) experiment: String,
}

impl Entry {
    /// All pairs.
    pub(crate) const fn pairs(&self) -> u64 {
        self.both_pass + self.both_fail + self.only_baseline + self.only_variant
    }
}

/// Whether an entry still says something about today's repository.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Status {
    /// Why the verdict no longer holds; empty when it does.
    pub(crate) stale: Vec<&'static str>,
    /// The unit and the tasks are the same, but other units have changed.
    pub(crate) rest_changed: bool,
    /// What the unit is called today, when that is not what the entry
    /// calls it: the same text under another name.
    pub(crate) now: Option<String>,
}

/// A name for the content of the task files of the repository at `root`:
/// the same files give the same name, in any order.
pub(crate) fn tasks_digest(root: &Path, tasks: &[(Task, TaskRef)]) -> String {
    let mut files: Vec<(&str, Vec<u8>, Vec<u8>)> = tasks
        .iter()
        .map(|(task, _)| {
            let read = |path: &Path| fs::read(root.join(path)).unwrap_or_default();
            (
                task.id.as_str(),
                read(&task.source),
                task.check_patch.as_deref().map(read).unwrap_or_default(),
            )
        })
        .collect();
    files.sort_unstable();
    let mut hash = Sha256::new();
    let mut field = |bytes: &[u8]| {
        // Lengths make the encoding unambiguous.
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    };
    field(b"tune.tasks.v1");
    for (id, task, patch) in &files {
        field(id.as_bytes());
        field(task);
        field(patch);
    }
    tune_engine::hex(&hash.finalize())
}

/// The entry `value` spells; the error names what is missing.
fn entry_of(value: &JsonValue) -> Result<Entry, String> {
    let text = |name: &str| {
        value
            .get(name)
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("an entry has no text `{name}`"))
    };
    let count = |name: &str| {
        value
            .get(name)
            .and_then(JsonValue::as_i64)
            .and_then(|count| u64::try_from(count).ok())
            .ok_or_else(|| format!("an entry has no count `{name}`"))
    };
    let advice = text("advice")?;
    if !["keep", "removable", "inconclusive"].contains(&advice.as_str()) {
        return Err(format!("`{advice}` is not an advice"));
    }
    let margin = match value.get("margin") {
        Some(JsonValue::Number(written)) => written.parse::<f64>().ok(),
        _ => None,
    }
    .ok_or_else(|| "an entry has no number `margin`".to_owned())?;
    let task_paths = value
        .get("task_paths")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "an entry has no list `task_paths`".to_owned())?
        .iter()
        .filter_map(JsonValue::as_str)
        .map(str::to_owned)
        .collect();
    Ok(Entry {
        unit: text("unit")?,
        sha256: text("sha256")?,
        advice,
        both_pass: count("both_pass")?,
        both_fail: count("both_fail")?,
        only_baseline: count("only_baseline")?,
        only_variant: count("only_variant")?,
        margin,
        config: text("config")?,
        tasks: text("tasks")?,
        task_paths,
        agent: text("agent")?,
        agent_version: value
            .get("agent_version")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        experiment: text("experiment")?,
    })
}

/// The ledger of the repository at `root`; empty when there is none.
pub(crate) fn read(root: &Path) -> Result<Vec<Entry>, Error> {
    let file = root.join(LEDGER_PATH);
    if !file.exists() {
        return Ok(Vec::new());
    }
    let written = fs::read_to_string(&file).map_err(|error| Error::io(&file, error))?;
    let wrong = |detail: String| {
        Error::Invalid(format!(
            "{LEDGER_PATH} cannot be read: {detail}. Fix the file or delete it; `tune prune` writes it again."
        ))
    };
    let value = parse_strict(&written).map_err(|error| wrong(error.to_string()))?;
    if value.get("schema").and_then(JsonValue::as_str) != Some(LEDGER_SCHEMA) {
        return Err(wrong(format!("its schema is not {LEDGER_SCHEMA}")));
    }
    value
        .get("verdicts")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| wrong("it has no list `verdicts`".to_owned()))?
        .iter()
        .map(|entry| entry_of(entry).map_err(&wrong))
        .collect()
}

/// The ledger as the file holds it, by unit.
pub(crate) fn render(entries: &[Entry]) -> String {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.unit.cmp(&b.unit));
    let verdicts = sorted
        .into_iter()
        .map(|entry| {
            Out::Object(vec![
                ("unit", Out::str(&entry.unit)),
                ("sha256", Out::str(&entry.sha256)),
                ("advice", Out::str(&entry.advice)),
                ("pairs", Out::uint(entry.pairs())),
                ("both_pass", Out::uint(entry.both_pass)),
                ("both_fail", Out::uint(entry.both_fail)),
                ("only_baseline", Out::uint(entry.only_baseline)),
                ("only_variant", Out::uint(entry.only_variant)),
                ("margin", Out::Raw(format!("{:.4}", entry.margin))),
                ("config", Out::str(&entry.config)),
                ("tasks", Out::str(&entry.tasks)),
                ("task_paths", Out::strs(&entry.task_paths)),
                ("agent", Out::str(&entry.agent)),
                (
                    "agent_version",
                    Out::opt(entry.agent_version.as_deref(), Out::str),
                ),
                ("experiment", Out::str(&entry.experiment)),
            ])
        })
        .collect();
    Out::Object(vec![
        ("schema", Out::str(LEDGER_SCHEMA)),
        ("verdicts", Out::Array(verdicts)),
    ])
    .render()
}

/// `old` with the entries of `new` in place of those for the same unit,
/// and without the entries of units that `today` no longer has.
pub(crate) fn merged(old: Vec<Entry>, new: &[Entry], today: &Inventory) -> Vec<Entry> {
    let mut entries: Vec<Entry> = old
        .into_iter()
        .filter(|entry| today.units.iter().any(|unit| unit.id == entry.unit))
        .filter(|entry| !new.iter().any(|newer| newer.unit == entry.unit))
        .chain(new.iter().cloned())
        .collect();
    entries.sort_by(|a, b| a.unit.cmp(&b.unit));
    entries
}

/// Writes the ledger of the repository at `root`.
pub(crate) fn write(root: &Path, entries: &[Entry]) -> Result<(), Error> {
    let file = root.join(LEDGER_PATH);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(|error| Error::io(dir, error))?;
    }
    // Written beside the file and moved over it, so that a reader never
    // sees half of it.
    let draft = file.with_extension("json.tmp");
    fs::write(&draft, render(entries)).map_err(|error| Error::io(&draft, error))?;
    fs::rename(&draft, &file).map_err(|error| Error::io(&file, error))
}

/// Whether `entry` holds for the configuration `today` with the digest
/// `config`, and for task files whose digest is `tasks` (`None` when they
/// cannot be read).
pub(crate) fn status(
    entry: &Entry,
    today: &Inventory,
    config: &str,
    tasks: Option<&str>,
) -> Status {
    let mut stale = Vec::new();
    let mut now = None;
    match today.units.iter().find(|unit| unit.id == entry.unit) {
        Some(unit) if unit.sha256 != entry.sha256 => stale.push("the unit changed"),
        Some(_) => {}
        None => {
            // Removing its neighbours can rename a unit: a file left with
            // one section is one unit, named by its path. The verdict is
            // about the text, so it follows the text when exactly one unit
            // holds it.
            let mut same = today
                .units
                .iter()
                .filter(|unit| unit.sha256 == entry.sha256);
            match (same.next(), same.next()) {
                (Some(unit), None) => now = Some(unit.id.clone()),
                _ => stale.push("the unit is gone"),
            }
        }
    }
    match tasks {
        None => stale.push("the task files cannot be read"),
        Some(digest) if digest != entry.tasks => stale.push("the task files changed"),
        Some(_) => {}
    }
    Status {
        rest_changed: stale.is_empty() && config != entry.config,
        stale,
        now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        files::{
            Entry as FileEntry, FileMap,
            testing::{commit, repo, write as put},
        },
        units,
    };

    fn entry(unit: &str, advice: &str) -> Entry {
        Entry {
            unit: unit.to_owned(),
            sha256: "aa".repeat(32),
            advice: advice.to_owned(),
            both_pass: 28,
            both_fail: 2,
            only_baseline: 0,
            only_variant: 0,
            margin: 0.1,
            config: "cc".repeat(32),
            tasks: "dd".repeat(32),
            task_paths: vec!["evals".to_owned()],
            agent: "claude-code".to_owned(),
            agent_version: Some("2.1.0 (Claude Code)".to_owned()),
            experiment: "0123456789ab".to_owned(),
        }
    }

    fn inventory(claude: &str) -> Inventory {
        let files = FileMap::from([("CLAUDE.md".to_owned(), FileEntry::text(claude))]);
        units::inventory(&files, &|_| None)
    }

    const CLAUDE: &str = "## Useful\nWrite ok.\n\n## Useless\nBe kind.\n";

    #[test]
    fn the_file_is_strict_json_by_unit_and_reads_back_the_same() {
        let mut second = entry("CLAUDE.md#useful", "keep");
        second.agent_version = None;
        second.margin = 0.125;
        let entries = vec![entry("CLAUDE.md#useless", "removable"), second];
        let written = render(&entries);
        assert!(written.ends_with("}\n"), "{written}");
        let value = parse_strict(&written).expect("strict JSON");
        assert_eq!(
            value.get("schema").and_then(JsonValue::as_str),
            Some(LEDGER_SCHEMA)
        );
        let first = written.find("CLAUDE.md#useful").expect("useful");
        let last = written.find("CLAUDE.md#useless").expect("useless");
        assert!(first < last, "not by unit:\n{written}");
        assert!(written.contains("\"pairs\": 30"), "{written}");

        let repo = repo();
        write(repo.path(), &entries).expect("write");
        let back = read(repo.path()).expect("read");
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].unit, "CLAUDE.md#useful");
        assert_eq!(back[0].agent_version, None);
        assert!((back[0].margin - 0.125).abs() < 1e-9);
        assert_eq!(back[1], entries[0]);
        assert_eq!(
            fs::read_to_string(repo.path().join(LEDGER_PATH)).expect("file"),
            written
        );
        // Written again, the file is the same byte for byte.
        write(repo.path(), &back).expect("write");
        assert_eq!(
            fs::read_to_string(repo.path().join(LEDGER_PATH)).expect("file"),
            written
        );
    }

    #[test]
    fn no_file_is_an_empty_ledger_and_a_broken_one_is_an_error() {
        let repo = repo();
        assert_eq!(read(repo.path()).expect("read"), Vec::new());
        put(repo.path(), LEDGER_PATH, "{\"schema\": \"other\"}");
        let wrong = read(repo.path()).expect_err("wrong schema").to_string();
        assert!(wrong.contains(LEDGER_PATH), "{wrong}");
        put(repo.path(), LEDGER_PATH, "not json");
        let broken = read(repo.path()).expect_err("not JSON").to_string();
        assert!(broken.contains(LEDGER_PATH), "{broken}");
    }

    #[test]
    fn new_entries_replace_old_ones_and_units_that_are_gone_are_dropped() {
        let today = inventory(CLAUDE);
        let mut newer = entry("CLAUDE.md#useless", "removable");
        newer.both_pass = 58;
        let merged = merged(
            vec![
                entry("CLAUDE.md#useless", "inconclusive"),
                entry("CLAUDE.md#useful", "keep"),
                entry("CLAUDE.md#gone", "keep"),
            ],
            std::slice::from_ref(&newer),
            &today,
        );
        let units: Vec<&str> = merged.iter().map(|entry| entry.unit.as_str()).collect();
        assert_eq!(units, ["CLAUDE.md#useful", "CLAUDE.md#useless"]);
        assert_eq!(merged[1], newer);
    }

    #[test]
    fn a_verdict_is_stale_when_its_unit_or_its_tasks_changed() {
        let today = inventory(CLAUDE);
        let unit = today
            .units
            .iter()
            .find(|unit| unit.id == "CLAUDE.md#useless")
            .expect("unit");
        let mut current = entry("CLAUDE.md#useless", "removable");
        current.sha256.clone_from(&unit.sha256);
        let (config, tasks) = (current.config.clone(), current.tasks.clone());

        assert_eq!(
            status(&current, &today, &config, Some(&tasks)),
            Status::default()
        );
        // Another unit changed: the verdict stands, with a note.
        let rest = status(&current, &today, "another digest", Some(&tasks));
        assert!(rest.stale.is_empty() && rest.rest_changed, "{rest:?}");

        let edited = inventory("## Useful\nWrite ok.\n\n## Useless\nBe very kind.\n");
        assert_eq!(
            status(&current, &edited, "another digest", Some(&tasks)).stale,
            ["the unit changed"]
        );
        let gone = inventory("## Useful\nWrite ok.\n\n## Other\nx\n");
        assert_eq!(
            status(&current, &gone, "another digest", Some(&tasks)).stale,
            ["the unit is gone"]
        );
        assert_eq!(
            status(&current, &today, &config, Some("other tasks")).stale,
            ["the task files changed"]
        );
        assert_eq!(
            status(&current, &edited, &config, None).stale,
            ["the unit changed", "the task files cannot be read"]
        );
    }

    #[test]
    fn a_unit_whose_text_is_the_same_under_a_new_name_keeps_its_verdict() {
        let before = inventory(CLAUDE);
        let unit = before
            .units
            .iter()
            .find(|unit| unit.id == "CLAUDE.md#useful")
            .expect("unit");
        let mut kept = entry("CLAUDE.md#useful", "keep");
        kept.sha256.clone_from(&unit.sha256);
        let tasks = kept.tasks.clone();

        // The other section is removed: the file is one unit now, named by
        // its path, and its text is the text that was tested.
        let after = inventory("## Useful\nWrite ok.\n\n");
        assert_eq!(after.units[0].id, "CLAUDE.md");
        let status = status(&kept, &after, "another digest", Some(&tasks));
        assert!(status.stale.is_empty(), "{status:?}");
        assert_eq!(status.now.as_deref(), Some("CLAUDE.md"));
        assert!(status.rest_changed);

        // Two units with that text: there is no telling which it is.
        let twice = inventory("## A\nWrite ok.\n\n## A\nWrite ok.\n\n");
        let mut copy = entry("CLAUDE.md#useful", "keep");
        copy.sha256.clone_from(&twice.units[0].sha256);
        assert_eq!(twice.units[0].sha256, twice.units[1].sha256);
        let status = super::status(&copy, &twice, "another digest", Some(&tasks));
        assert_eq!(status.stale, ["the unit is gone"]);
        assert_eq!(status.now, None);
    }

    #[test]
    fn the_digest_of_the_tasks_names_their_files_in_any_order() {
        let repo = repo();
        let task = |name: &str, prompt: &str| {
            put(
                repo.path(),
                &format!("evals/{name}.json"),
                &format!(
                    r#"{{"schema": "tune.task.v1", "prompt": "{prompt}", "check": {{"argv": ["true"]}}}}"#
                ),
            );
        };
        task("a", "one");
        task("b", "two");
        commit(repo.path(), "tasks");
        let load = |paths: &[&str]| {
            let paths: Vec<std::path::PathBuf> = paths.iter().map(Into::into).collect();
            tune_engine::task::load_suite(repo.path(), &paths).expect("tasks")
        };
        let both = tasks_digest(repo.path(), &load(&["evals"]));
        assert_eq!(both.len(), 64);
        assert_eq!(
            both,
            tasks_digest(repo.path(), &load(&["evals/b.json", "evals/a.json"]))
        );
        assert_ne!(both, tasks_digest(repo.path(), &load(&["evals/a.json"])));
        task("b", "two, reworded");
        assert_ne!(both, tasks_digest(repo.path(), &load(&["evals"])));
    }
}
