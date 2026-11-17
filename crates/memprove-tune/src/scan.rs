//! `memprove-tune scan`: what the repository's agent configuration is made of, what
//! each unit costs, and what looks wrong without running anything.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use tune_engine::{json::Out, task};

use crate::{
    error::Error,
    files,
    ledger::{self, Entry, Status},
    report,
    smells::{self, Repo, Smell},
    units::{self, FileInfo, Inventory, Load, Unit},
};

/// The schema of `memprove-tune scan --json`.
pub(crate) const SCAN_SCHEMA: &str = "tune.scan.v1";

/// What a scan found.
pub(crate) struct Scan {
    /// The units and the files they are in.
    pub(crate) inventory: Inventory,
    /// The findings.
    pub(crate) smells: Vec<Smell>,
    /// What `memprove-tune prune` wrote down for the units, by unit, and whether
    /// each still holds for today's configuration and task files.
    pub(crate) verdicts: Vec<(Entry, Status)>,
}

/// The verdicts in the ledger of the repository at `root`, each with
/// whether it still holds.
fn verdicts(
    root: &Path,
    config: &files::FileMap,
    inventory: &Inventory,
) -> Result<Vec<(Entry, Status)>, Error> {
    let today = files::digest(config);
    // Entries of one run name the same task files: read them once.
    let mut digests: BTreeMap<Vec<String>, Option<String>> = BTreeMap::new();
    let mut entries = ledger::read(root)?;
    entries.sort_by(|a, b| a.unit.cmp(&b.unit));
    Ok(entries
        .into_iter()
        .map(|entry| {
            let tasks = digests
                .entry(entry.task_paths.clone())
                .or_insert_with(|| {
                    let paths: Vec<PathBuf> = entry.task_paths.iter().map(PathBuf::from).collect();
                    task::load_suite(root, &paths)
                        .ok()
                        .map(|tasks| ledger::tasks_digest(root, &tasks))
                })
                .clone();
            let status = ledger::status(&entry, inventory, &today, tasks.as_deref());
            (entry, status)
        })
        .collect())
}

/// Scans the configuration in the working tree of the repository at
/// `root`.
pub(crate) fn scan(root: &Path) -> Result<Scan, Error> {
    let config = files::read_config(root)?;
    let read = |path: &str| {
        let file = root.join(path);
        file.is_file()
            .then(|| std::fs::read_to_string(file).ok())
            .flatten()
    };
    let exists = |path: &str| root.join(path).exists();
    let inventory = units::inventory(&config, &read);
    let smells = smells::smells(
        &config,
        &inventory,
        &Repo {
            exists: &exists,
            read: &read,
        },
    );
    let verdicts = verdicts(root, &config, &inventory)?;
    Ok(Scan {
        inventory,
        smells,
        verdicts,
    })
}

/// `count` with a comma between each three digits.
pub(crate) fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `count` of a thing, with the thing in the plural when it should be.
pub(crate) fn counted(count: usize, thing: &str) -> String {
    if count == 1 {
        format!("1 {thing}")
    } else {
        format!("{count} {thing}s")
    }
}

/// When a file loads, as the end of a sentence.
fn loads(load: &Load) -> String {
    match load {
        Load::Always => "loaded in every session".to_owned(),
        Load::OnDemand(when) => format!("loaded {when}"),
        Load::NotRead(why) => format!("not read: {why}"),
    }
}

fn file_heading(info: &FileInfo, units: &[&Unit]) -> String {
    if info.lines == 0 && units.is_empty() {
        let target = info.link_to.as_ref().map_or_else(
            || "a file outside the configuration".to_owned(),
            Clone::clone,
        );
        return format!("{} (link to {target}, {})", info.path, loads(&info.load));
    }
    let listed: u64 = units
        .iter()
        .filter(|unit| unit.kind == units::Kind::Skill)
        .map(|unit| unit.always_tokens)
        .sum();
    let description = if listed > 0 {
        format!(
            "; its description costs about {} tokens in every session",
            grouped(listed)
        )
    } else {
        String::new()
    };
    format!(
        "{} ({}, {}{description})",
        info.path,
        counted(info.lines, "line"),
        loads(&info.load)
    )
}

/// An advice as the reports print it.
fn advice_name(advice: &str) -> &'static str {
    match advice {
        "keep" => "KEEP",
        "removable" => "REMOVABLE",
        _ => "NOT ENOUGH EVIDENCE",
    }
}

/// The verdicts `memprove-tune prune` wrote down, one a line; nothing when there
/// are none.
fn verdict_lines(verdicts: &[(Entry, Status)]) -> String {
    if verdicts.is_empty() {
        return String::new();
    }
    let width = verdicts
        .iter()
        .map(|(entry, _)| entry.unit.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = format!("\nVerdicts ({}): {}\n", ledger::LEDGER_PATH, verdicts.len());
    for (entry, status) in verdicts {
        let holds = if !status.stale.is_empty() {
            format!("stale: {}", status.stale.join("; "))
        } else if let Some(now) = &status.now {
            format!("current; the same text is now {now}")
        } else if status.rest_changed {
            "current; other units have changed since".to_owned()
        } else {
            "current".to_owned()
        };
        let _ = writeln!(
            out,
            "  {:<width$}  {:<19}  {:>9}  {holds}",
            entry.unit,
            advice_name(&entry.advice),
            counted(usize::try_from(entry.pairs()).unwrap_or(usize::MAX), "pair"),
        );
    }
    if verdicts.iter().any(|(_, status)| !status.stale.is_empty()) {
        out.push_str(
            "  A stale verdict says nothing about today's files; `memprove-tune prune` tests them again.\n",
        );
    }
    out
}

/// The scan as text for a person.
pub(crate) fn text(scan: &Scan) -> String {
    let inventory = &scan.inventory;
    if inventory.files.is_empty() {
        let mut out = "No agent configuration found: no CLAUDE.md or AGENTS.md, and nothing under .claude/rules or .claude/skills.\n".to_owned();
        out.push_str(&verdict_lines(&scan.verdicts));
        return out;
    }
    let always: u64 = inventory.units.iter().map(|unit| unit.always_tokens).sum();
    let all: u64 = inventory.units.iter().map(|unit| unit.tokens).sum();
    let mut out = format!(
        "{}, {}. About {} tokens load in every Claude Code session; {} in all.\n",
        counted(inventory.files.len(), "configuration file"),
        counted(inventory.units.len(), "unit"),
        grouped(always),
        grouped(all),
    );
    let width = inventory
        .units
        .iter()
        .map(|unit| unit.id.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    for info in &inventory.files {
        let units: Vec<&Unit> = inventory
            .units
            .iter()
            .filter(|unit| unit.file == info.path)
            .collect();
        let _ = write!(out, "\n{}\n", file_heading(info, &units));
        for unit in units {
            let lines = format!("{}-{}", unit.first_line, unit.last_line);
            let _ = writeln!(
                out,
                "  {:<width$}  lines {lines:<9}  {:>7} tokens",
                unit.id,
                grouped(unit.tokens),
            );
        }
    }
    let _ = write!(out, "\nFindings: {}\n", scan.smells.len());
    for smell in &scan.smells {
        let place = smell.line.map_or_else(
            || smell.file.clone(),
            |line| format!("{}:{line}", smell.file),
        );
        let _ = writeln!(out, "  {place} [{}] {}", smell.code, smell.message);
    }
    out.push_str(&verdict_lines(&scan.verdicts));
    out.push_str(
        "\nToken counts are estimates. Nothing was run and no model was called.\nTo test whether a unit earns its place: memprove-tune ab --drop <UNIT>, or memprove-tune prune for all of them\n",
    );
    out
}

fn load_members(load: &Load) -> [(&'static str, Out); 3] {
    let (name, when, why) = match load {
        Load::Always => ("always", None, None),
        Load::OnDemand(when) => ("on_demand", Some(when.as_str()), None),
        Load::NotRead(why) => ("not_read", None, Some(why.as_str())),
    };
    [
        ("load", Out::str(name)),
        ("when", Out::opt(when, Out::str)),
        ("why", Out::opt(why, Out::str)),
    ]
}

fn line_number(line: usize) -> Out {
    Out::uint(u64::try_from(line).unwrap_or(u64::MAX))
}

/// The verdicts as the JSON document lists them.
fn verdicts_json(verdicts: &[(Entry, Status)]) -> Out {
    Out::Array(
        verdicts
            .iter()
            .map(|(entry, status)| {
                Out::Object(vec![
                    ("unit", Out::str(&entry.unit)),
                    ("advice", Out::str(&entry.advice)),
                    (
                        "status",
                        Out::str(if status.stale.is_empty() {
                            "current"
                        } else {
                            "stale"
                        }),
                    ),
                    ("reasons", Out::strs(&status.stale)),
                    ("rest_changed", Out::bool(status.rest_changed)),
                    ("unit_now", Out::opt(status.now.as_deref(), Out::str)),
                    ("pairs", Out::uint(entry.pairs())),
                    ("only_baseline", Out::uint(entry.only_baseline)),
                    ("only_variant", Out::uint(entry.only_variant)),
                    ("margin", report::fraction(entry.margin)),
                    ("agent", Out::str(&entry.agent)),
                    (
                        "agent_version",
                        Out::opt(entry.agent_version.as_deref(), Out::str),
                    ),
                    ("experiment", Out::str(&entry.experiment)),
                ])
            })
            .collect(),
    )
}

/// The scan as one JSON document.
pub(crate) fn json(scan: &Scan) -> String {
    let inventory = &scan.inventory;
    let files = inventory
        .files
        .iter()
        .map(|info| {
            let mut members = vec![
                ("path", Out::str(&info.path)),
                ("lines", line_number(info.lines)),
                ("link_to", Out::opt(info.link_to.as_deref(), Out::str)),
            ];
            members.extend(load_members(&info.load));
            Out::Object(members)
        })
        .collect();
    let units = inventory
        .units
        .iter()
        .map(|unit| {
            let mut members = vec![
                ("id", Out::str(&unit.id)),
                ("kind", Out::str(unit.kind.name())),
                ("file", Out::str(&unit.file)),
                ("title", Out::str(&unit.title)),
                ("first_line", line_number(unit.first_line)),
                ("last_line", line_number(unit.last_line)),
                ("sha256", Out::str(&unit.sha256)),
                ("tokens", Out::uint(unit.tokens)),
                ("always_tokens", Out::uint(unit.always_tokens)),
            ];
            members.extend(load_members(&unit.load));
            members.push(("imports", Out::strs(&unit.imports)));
            Out::Object(members)
        })
        .collect();
    let findings = scan
        .smells
        .iter()
        .map(|smell| {
            Out::Object(vec![
                ("code", Out::str(smell.code)),
                ("file", Out::str(&smell.file)),
                ("line", Out::opt(smell.line, line_number)),
                ("unit", Out::opt(smell.unit.as_deref(), Out::str)),
                ("message", Out::str(&smell.message)),
            ])
        })
        .collect();
    let totals = Out::Object(vec![
        ("files", line_number(inventory.files.len())),
        ("units", line_number(inventory.units.len())),
        (
            "tokens",
            Out::uint(inventory.units.iter().map(|unit| unit.tokens).sum()),
        ),
        (
            "always_tokens",
            Out::uint(inventory.units.iter().map(|unit| unit.always_tokens).sum()),
        ),
        ("findings", line_number(scan.smells.len())),
    ]);
    let mut document = Out::Object(vec![
        ("schema", Out::str(SCAN_SCHEMA)),
        ("files", Out::Array(files)),
        ("units", Out::Array(units)),
        ("findings", Out::Array(findings)),
        ("verdicts", verdicts_json(&scan.verdicts)),
        ("totals", totals),
    ])
    .render();
    document.push('\n');
    document
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
