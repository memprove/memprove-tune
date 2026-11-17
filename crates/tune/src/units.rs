//! The units of a repository's agent configuration: the pieces an
//! experiment can take away one at a time. An instruction file gives one
//! unit per section, a rule file is one unit, a skill directory is one
//! unit.

use sha2::{Digest, Sha256};

use crate::{
    files::{Entry, FileMap, INSTRUCTION_FILES},
    markdown, tokens,
};

/// What kind of piece a unit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A section of an instruction file.
    Section,
    /// An instruction file that does not split into sections.
    File,
    /// A file under `.claude/rules/`.
    Rule,
    /// A directory under `.claude/skills/` with a `SKILL.md`.
    Skill,
}

impl Kind {
    /// The name in reports.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::File => "file",
            Self::Rule => "rule",
            Self::Skill => "skill",
        }
    }
}

/// When Claude Code puts a unit into the agent's context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Load {
    /// At the start of every session.
    Always,
    /// Later, on the condition these words name.
    OnDemand(String),
    /// Never, for the reason these words give.
    NotRead(String),
}

/// One unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    /// Its name on the command line: `CLAUDE.md#testing`,
    /// `.claude/rules/style.md`, `.claude/skills/deploy`.
    pub(crate) id: String,
    /// What kind of piece it is.
    pub(crate) kind: Kind,
    /// The file it is in; for a skill, its `SKILL.md`.
    pub(crate) file: String,
    /// The heading, the file name, or the skill's name.
    pub(crate) title: String,
    /// Its first line in `file`, counted from 1.
    pub(crate) first_line: usize,
    /// Its last line in `file`.
    pub(crate) last_line: usize,
    /// SHA-256 of its content, hex: of its lines, or of every file of a
    /// skill.
    pub(crate) sha256: String,
    /// The estimated tokens of all of it, with the files it imports.
    pub(crate) tokens: u64,
    /// The part of `tokens` that is in context from the start of every
    /// session.
    pub(crate) always_tokens: u64,
    /// When it is loaded.
    pub(crate) load: Load,
    /// The repository files its lines import with `@path`.
    pub(crate) imports: Vec<String>,
}

/// One file of the configuration that holds units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileInfo {
    /// Its path.
    pub(crate) path: String,
    /// Its lines; 0 for a link.
    pub(crate) lines: usize,
    /// The configuration file it is a link to, when it is one.
    pub(crate) link_to: Option<String>,
    /// When it is loaded.
    pub(crate) load: Load,
}

/// The units of a configuration and the files they are in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Inventory {
    /// Instruction files first, then rules, then skills; in each, by path.
    pub(crate) units: Vec<Unit>,
    /// The instruction and rule files and each skill's `SKILL.md`, in the
    /// same order.
    pub(crate) files: Vec<FileInfo>,
}

/// The units of `files`. `read` gives the text of another file of the
/// repository (for the files an instruction file imports), or `None`.
pub(crate) fn inventory(files: &FileMap, read: &dyn Fn(&str) -> Option<String>) -> Inventory {
    let mut found = Inventory::default();
    instruction_units(files, read, &mut found);
    rule_units(files, &mut found);
    skill_units(files, &mut found);
    found
}

/// The directory of `path` (empty at the root) and its file name.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// Why Claude Code does not read an `AGENTS.md` by itself.
const AGENTS_NOT_READ: &str =
    "Claude Code reads CLAUDE.md, not AGENTS.md, and no CLAUDE.md imports it";

/// When an instruction file loads, going by its place alone.
fn load_by_place(path: &str) -> Load {
    let (dir, name) = split(path);
    if name != "CLAUDE.md" {
        return Load::NotRead(AGENTS_NOT_READ.to_owned());
    }
    // `.claude/CLAUDE.md` stands for the directory that holds `.claude`.
    let owner = dir
        .strip_suffix(".claude")
        .map_or(dir, |owner| owner.trim_end_matches('/'));
    if owner.is_empty() {
        Load::Always
    } else {
        Load::OnDemand(format!("when a file under {owner}/ is read"))
    }
}

const fn rank(load: &Load) -> u8 {
    match load {
        Load::Always => 2,
        Load::OnDemand(_) => 1,
        Load::NotRead(_) => 0,
    }
}

/// The `@path` imports of the prose lines `lines` of a file in `dir`: the
/// paths that name a file of the configuration or of the repository.
fn imports_of(
    lines: &[markdown::Line<'_>],
    dir: &str,
    files: &FileMap,
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut imports: Vec<String> = Vec::new();
    for line in lines
        .iter()
        .filter(|line| !line.in_code && !line.in_front_matter)
    {
        // An import inside inline code is not one.
        let mut prose = line.text.to_owned();
        for span in markdown::code_spans(line.text) {
            prose = prose.replace(span, "");
        }
        for word in prose.split_whitespace() {
            let Some(target) = word.strip_prefix('@') else {
                continue;
            };
            let target =
                target.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']);
            let Some(path) = resolve(dir, target) else {
                continue;
            };
            let exists = files.contains_key(&path) || read(&path).is_some();
            if exists && !imports.contains(&path) {
                imports.push(path);
            }
        }
    }
    imports
}

/// When each instruction file of `paths` loads: by its place, or sooner
/// when a file that loads sooner imports it or is a link to it.
fn instruction_loads(
    paths: &[&String],
    files: &FileMap,
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, Load)> {
    // Which instruction file brings in which other: imports and links.
    let mut brings: Vec<(String, String)> = Vec::new();
    for path in paths {
        let dir = split(path).0;
        match &files[*path] {
            Entry::Link(target) => {
                if let Some(target) =
                    resolve(dir, target).filter(|target| files.contains_key(target))
                {
                    brings.push(((*path).clone(), target));
                }
            }
            entry @ Entry::File { .. } => {
                let Some(text) = entry.as_text() else {
                    continue;
                };
                for import in imports_of(&markdown::lines(text), dir, files, read) {
                    if paths.contains(&&import) {
                        brings.push(((*path).clone(), import));
                    }
                }
            }
        }
    }
    let mut loads: Vec<(String, Load)> = paths
        .iter()
        .map(|path| ((*path).clone(), load_by_place(path)))
        .collect();
    // Imports nest at most four deep.
    for _ in 0..4 {
        for (from, to) in &brings {
            let Some(source) = loads
                .iter()
                .find(|(path, _)| path == from)
                .map(|(_, load)| load.clone())
            else {
                continue;
            };
            if let Some((_, load)) = loads.iter_mut().find(|(path, _)| path == to)
                && rank(&source) > rank(load)
            {
                *load = source;
            }
        }
    }
    loads
}

/// The units of the instruction file `path`, whose text is `text`, and the
/// number of its lines.
fn section_units(
    path: &str,
    text: &str,
    load: &Load,
    files: &FileMap,
    read: &dyn Fn(&str) -> Option<String>,
    found: &mut Inventory,
) -> usize {
    let dir = split(path).0;
    let lines = markdown::lines(text);
    let sections = markdown::sections(text);
    let whole = sections.len() == 1 && sections[0].level == 0;
    let mut ids: Vec<String> = Vec::new();
    for section in &sections {
        let id = if whole {
            path.to_owned()
        } else {
            let slug = if section.level == 0 {
                "_top".to_owned()
            } else {
                markdown::slug(&section.title)
            };
            let mut id = format!("{path}#{slug}");
            let mut repeat = 2;
            while ids.contains(&id) {
                id = format!("{path}#{slug}-{repeat}");
                repeat += 1;
            }
            id
        };
        ids.push(id.clone());
        let body: String = lines[section.start..section.end]
            .iter()
            .flat_map(|line| [line.text, "\n"])
            .collect();
        let imports = imports_of(&lines[section.start..section.end], dir, files, read);
        let imported: u64 = imports
            .iter()
            .filter(|import| !files.contains_key(*import))
            .filter_map(|import| read(import))
            .map(|text| tokens::estimate(&text))
            .sum();
        let tokens = tokens::estimate(&tokens::without_comments(&body)) + imported;
        found.units.push(Unit {
            id,
            kind: if whole { Kind::File } else { Kind::Section },
            file: path.to_owned(),
            title: if whole {
                split(path).1.to_owned()
            } else {
                section.title.clone()
            },
            first_line: section.start + 1,
            last_line: section.end,
            sha256: tune_engine::hex(&Sha256::digest(body.as_bytes())),
            tokens,
            always_tokens: if *load == Load::Always { tokens } else { 0 },
            load: load.clone(),
            imports,
        });
    }
    lines.len()
}

fn instruction_units(
    files: &FileMap,
    read: &dyn Fn(&str) -> Option<String>,
    found: &mut Inventory,
) {
    let mut paths: Vec<&String> = files
        .keys()
        .filter(|path| INSTRUCTION_FILES.contains(&split(path).1))
        .collect();
    paths.sort_by_key(|path| {
        let place = match path.as_str() {
            "CLAUDE.md" => 0,
            ".claude/CLAUDE.md" => 1,
            _ => 2,
        };
        (place, (*path).clone())
    });

    for (path, load) in instruction_loads(&paths, files, read) {
        let dir = split(&path).0;
        let entry = &files[&path];
        if let Entry::Link(target) = entry {
            found.files.push(FileInfo {
                link_to: resolve(dir, target).filter(|target| files.contains_key(target)),
                path,
                lines: 0,
                load,
            });
            continue;
        }
        let Some(text) = entry.as_text() else {
            continue;
        };
        let lines = section_units(&path, text, &load, files, read, found);
        found.files.push(FileInfo {
            path,
            lines,
            link_to: None,
            load,
        });
    }
}

/// The part of `path` up to and including the directory after
/// `.claude/<kind>/`, and the directory that holds `.claude` (empty at the
/// root).
fn claude_dir<'a>(path: &'a str, kind: &str) -> Option<(&'a str, &'a str)> {
    let marker = format!(".claude/{kind}/");
    let at = if path.starts_with(&marker) {
        0
    } else {
        path.find(&format!("/{marker}"))? + 1
    };
    let owner = path[..at].trim_end_matches('/');
    let rest = &path[at + marker.len()..];
    let name_end = rest.find('/').unwrap_or(rest.len());
    Some((&path[..at + marker.len() + name_end], owner))
}

fn rule_units(files: &FileMap, found: &mut Inventory) {
    for (path, entry) in files {
        let Some((_, owner)) = claude_dir(path, "rules") else {
            continue;
        };
        let name = split(path).1;
        let markdown = std::path::Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
        if !markdown || INSTRUCTION_FILES.contains(&name) {
            continue;
        }
        let Some(text) = entry.as_text() else {
            continue;
        };
        let paths = markdown::front_matter(text)
            .into_iter()
            .find(|(key, value)| key == "paths" && !value.is_empty())
            .map(|(_, value)| value);
        let load = match (paths, owner) {
            (Some(paths), _) => Load::OnDemand(format!("when a file matching {paths} is read")),
            (None, "") => Load::Always,
            (None, owner) => Load::OnDemand(format!("when a file under {owner}/ is read")),
        };
        let lines = markdown::lines(text);
        let title = markdown::sections(text)
            .into_iter()
            .find(|section| section.level > 0)
            .map_or_else(|| name.to_owned(), |section| section.title);
        let tokens = tokens::estimate(&tokens::without_comments(text));
        found.units.push(Unit {
            id: path.clone(),
            kind: Kind::Rule,
            file: path.clone(),
            title,
            first_line: 1,
            last_line: lines.len(),
            sha256: tune_engine::hex(&Sha256::digest(text.as_bytes())),
            tokens,
            always_tokens: if load == Load::Always { tokens } else { 0 },
            load: load.clone(),
            imports: Vec::new(),
        });
        found.files.push(FileInfo {
            path: path.clone(),
            lines: lines.len(),
            link_to: None,
            load,
        });
    }
}

/// The longest skill description Claude Code lists, in characters.
const SKILL_LISTING_CHARS: usize = 1536;

fn skill_units(files: &FileMap, found: &mut Inventory) {
    let mut dirs: Vec<(&str, &str)> = files
        .keys()
        .filter_map(|path| claude_dir(path, "skills"))
        .collect();
    dirs.dedup();
    for (dir, owner) in dirs {
        let file = format!("{dir}/SKILL.md");
        let Some(text) = files.get(&file).and_then(Entry::as_text) else {
            continue;
        };
        let front = markdown::front_matter(text);
        let value = |key: &str| {
            front
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
                .filter(|value| !value.is_empty())
        };
        let title = value("name").map_or_else(|| split(dir).1.to_owned(), str::to_owned);
        let listed = owner.is_empty() && value("disable-model-invocation") != Some("true");
        let always_tokens = if listed {
            let description: String = [value("description"), value("when_to_use")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(SKILL_LISTING_CHARS)
                .collect();
            tokens::estimate(&format!("{title}: {description}"))
        } else {
            0
        };
        // Every file of the skill, by its path inside the skill.
        let inside: FileMap = files
            .iter()
            .filter_map(|(path, entry)| {
                let rest = path.strip_prefix(dir)?.strip_prefix('/')?;
                Some((rest.to_owned(), entry.clone()))
            })
            .collect();
        let lines = markdown::lines(text).len();
        found.units.push(Unit {
            id: dir.to_owned(),
            kind: Kind::Skill,
            file: file.clone(),
            title,
            first_line: 1,
            last_line: lines,
            sha256: crate::files::digest(&inside),
            tokens: tokens::estimate(text),
            always_tokens,
            load: Load::OnDemand("when the skill is invoked".to_owned()),
            imports: Vec::new(),
        });
        found.files.push(FileInfo {
            path: file,
            lines,
            link_to: None,
            load: Load::OnDemand("when the skill is invoked".to_owned()),
        });
    }
}

/// `dir` joined with `target` and normalized, when the result stays
/// inside the repository.
pub(crate) fn resolve(dir: &str, target: &str) -> Option<String> {
    if target.is_empty() || target.starts_with(['/', '~']) {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(files: &[(&str, &str)]) -> FileMap {
        files
            .iter()
            .map(|(path, text)| ((*path).to_owned(), Entry::text(text)))
            .collect()
    }

    fn none(_: &str) -> Option<String> {
        None
    }

    fn ids(inventory: &Inventory) -> Vec<&str> {
        inventory
            .units
            .iter()
            .map(|unit| unit.id.as_str())
            .collect()
    }

    fn unit<'a>(inventory: &'a Inventory, id: &str) -> &'a Unit {
        inventory
            .units
            .iter()
            .find(|unit| unit.id == id)
            .unwrap_or_else(|| panic!("no unit {id} in {:?}", ids(inventory)))
    }

    const CLAUDE: &str = "Read this first.\n\n## Commands\n\nRun `make test`.\n\n## Style\n\nTabs.\n\n## Commands\n\nAgain.\n";

    #[test]
    fn an_instruction_file_gives_one_unit_per_section() {
        let found = inventory(&map(&[("CLAUDE.md", CLAUDE)]), &none);
        assert_eq!(
            ids(&found),
            [
                "CLAUDE.md#_top",
                "CLAUDE.md#commands",
                "CLAUDE.md#style",
                "CLAUDE.md#commands-2"
            ]
        );
        let commands = unit(&found, "CLAUDE.md#commands");
        assert_eq!(commands.kind, Kind::Section);
        assert_eq!(commands.title, "Commands");
        assert_eq!((commands.first_line, commands.last_line), (3, 6));
        assert_eq!(commands.load, Load::Always);
        assert_eq!(commands.tokens, commands.always_tokens);
        assert_eq!(
            commands.tokens,
            tokens::estimate("## Commands\n\nRun `make test`.\n\n")
        );
        assert_eq!(commands.sha256.len(), 64);
        assert_ne!(commands.sha256, unit(&found, "CLAUDE.md#commands-2").sha256);
        assert_eq!(found.files.len(), 1);
        assert_eq!(found.files[0].lines, 13);
    }

    #[test]
    fn a_file_without_sections_is_one_unit_named_by_its_path() {
        let found = inventory(&map(&[("CLAUDE.md", "Use tabs.\nRun the tests.\n")]), &none);
        assert_eq!(ids(&found), ["CLAUDE.md"]);
        let whole = unit(&found, "CLAUDE.md");
        assert_eq!(whole.kind, Kind::File);
        assert_eq!((whole.first_line, whole.last_line), (1, 2));
    }

    #[test]
    fn where_a_file_is_decides_when_it_loads() {
        let found = inventory(
            &map(&[
                ("CLAUDE.md", "root\n"),
                (".claude/CLAUDE.md", "also root\n"),
                ("web/CLAUDE.md", "nested\n"),
                ("AGENTS.md", "for other agents\n"),
            ]),
            &none,
        );
        assert_eq!(unit(&found, "CLAUDE.md").load, Load::Always);
        assert_eq!(unit(&found, ".claude/CLAUDE.md").load, Load::Always);
        let nested = unit(&found, "web/CLAUDE.md");
        assert_eq!(
            nested.load,
            Load::OnDemand("when a file under web/ is read".to_owned())
        );
        assert_eq!(nested.always_tokens, 0);
        assert!(nested.tokens > 0);
        let agents = unit(&found, "AGENTS.md");
        assert!(matches!(&agents.load, Load::NotRead(why) if why.contains("AGENTS.md")));
        assert_eq!(agents.always_tokens, 0);
        // Instruction files come first: the root CLAUDE.md, then the others.
        assert_eq!(
            ids(&found),
            [
                "CLAUDE.md",
                ".claude/CLAUDE.md",
                "AGENTS.md",
                "web/CLAUDE.md"
            ]
        );
    }

    #[test]
    fn an_imported_agents_file_loads_with_the_file_that_imports_it() {
        let found = inventory(
            &map(&[
                ("CLAUDE.md", "@AGENTS.md\n\nUse plan mode.\n"),
                ("AGENTS.md", "shared rules\n"),
            ]),
            &none,
        );
        assert_eq!(unit(&found, "AGENTS.md").load, Load::Always);
        assert_eq!(unit(&found, "CLAUDE.md").imports, ["AGENTS.md"]);
        // The imported instruction file is counted once, as its own unit.
        assert_eq!(
            unit(&found, "CLAUDE.md").tokens,
            tokens::estimate("@AGENTS.md\n\nUse plan mode.\n")
        );
    }

    #[test]
    fn an_imported_file_counts_toward_the_unit_that_imports_it() {
        let read = |path: &str| (path == "docs/git.md").then(|| "x".repeat(400));
        let found = inventory(
            &map(&[(
                "CLAUDE.md",
                "## Git\nSee @docs/git.md, ask @alice, mail a@b.c.\n```\n@docs/git.md\n```\n## Other\ntext\n",
            )]),
            &read,
        );
        let git = unit(&found, "CLAUDE.md#git");
        assert_eq!(git.imports, ["docs/git.md"]);
        assert_eq!(
            git.tokens,
            tokens::estimate(
                "## Git\nSee @docs/git.md, ask @alice, mail a@b.c.\n```\n@docs/git.md\n```\n"
            ) + 100
        );
        assert!(unit(&found, "CLAUDE.md#other").imports.is_empty());
    }

    #[test]
    fn a_link_to_another_instruction_file_loads_its_target() {
        let mut files = map(&[("AGENTS.md", "## A\na\n## B\nb\n")]);
        files.insert("CLAUDE.md".to_owned(), Entry::Link("AGENTS.md".to_owned()));
        let found = inventory(&files, &none);
        assert_eq!(ids(&found), ["AGENTS.md#a", "AGENTS.md#b"]);
        assert_eq!(unit(&found, "AGENTS.md#a").load, Load::Always);
        let link = found
            .files
            .iter()
            .find(|file| file.path == "CLAUDE.md")
            .expect("the link is listed");
        assert_eq!(link.link_to.as_deref(), Some("AGENTS.md"));
    }

    #[test]
    fn a_rule_is_one_unit_and_paths_make_it_load_on_demand() {
        let found = inventory(
            &map(&[
                (".claude/rules/style.md", "# Style\n## A\n## B\n"),
                (
                    ".claude/rules/api/rest.md",
                    "---\npaths:\n  - \"src/api/**/*.ts\"\n---\n# API\nValidate input.\n",
                ),
                (".claude/rules/notes.txt", "not a rule\n"),
            ]),
            &none,
        );
        assert_eq!(
            ids(&found),
            [".claude/rules/api/rest.md", ".claude/rules/style.md"]
        );
        let style = unit(&found, ".claude/rules/style.md");
        assert_eq!(style.kind, Kind::Rule);
        assert_eq!(style.load, Load::Always);
        assert_eq!((style.first_line, style.last_line), (1, 3));
        let rest = unit(&found, ".claude/rules/api/rest.md");
        assert_eq!(
            rest.load,
            Load::OnDemand("when a file matching src/api/**/*.ts is read".to_owned())
        );
        assert_eq!(rest.always_tokens, 0);
    }

    #[test]
    fn a_skill_is_one_unit_whose_description_is_always_loaded() {
        let skill = "---\nname: ship\ndescription: Deploy the app to production\n---\n# Steps\n1. Build.\n2. Push.\n";
        let files = map(&[
            (".claude/skills/deploy/SKILL.md", skill),
            (".claude/skills/deploy/scripts/run.sh", "#!/bin/sh\n"),
            (".claude/skills/orphan/notes.md", "no SKILL.md here\n"),
            (
                ".claude/skills/manual/SKILL.md",
                "---\ndescription: Only by hand\ndisable-model-invocation: true\n---\nbody\n",
            ),
        ]);
        let found = inventory(&files, &none);
        assert_eq!(
            ids(&found),
            [".claude/skills/deploy", ".claude/skills/manual"]
        );
        let deploy = unit(&found, ".claude/skills/deploy");
        assert_eq!(deploy.kind, Kind::Skill);
        assert_eq!(deploy.title, "ship");
        assert_eq!(deploy.file, ".claude/skills/deploy/SKILL.md");
        assert_eq!(deploy.tokens, tokens::estimate(skill));
        assert_eq!(
            deploy.always_tokens,
            tokens::estimate("ship: Deploy the app to production")
        );
        assert_eq!(
            deploy.load,
            Load::OnDemand("when the skill is invoked".to_owned())
        );
        let manual = unit(&found, ".claude/skills/manual");
        assert_eq!(manual.title, "manual");
        assert_eq!(manual.always_tokens, 0);

        // Another file of the skill changes the unit's hash.
        let mut edited = files.clone();
        edited.insert(
            ".claude/skills/deploy/scripts/run.sh".to_owned(),
            Entry::text("#!/bin/sh\necho changed\n"),
        );
        assert_ne!(
            unit(&inventory(&edited, &none), ".claude/skills/deploy").sha256,
            deploy.sha256
        );
    }

    #[test]
    fn paths_resolve_inside_the_repository_only() {
        assert_eq!(resolve("", "AGENTS.md").as_deref(), Some("AGENTS.md"));
        assert_eq!(
            resolve("web", "./docs/a.md").as_deref(),
            Some("web/docs/a.md")
        );
        assert_eq!(
            resolve("web/app", "../../README.md").as_deref(),
            Some("README.md")
        );
        assert_eq!(resolve("web", "../../etc/passwd"), None);
        assert_eq!(resolve("", "/etc/passwd"), None);
        assert_eq!(resolve("", "~/notes.md"), None);
        assert_eq!(resolve("", ""), None);
    }
}
