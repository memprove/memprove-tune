//! The little Markdown the scan needs: which lines are code, where the
//! sections of an instruction file begin, what a front matter block says,
//! and the inline code and link targets of a line. Not a Markdown parser.

/// One line of a file and where it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Line<'a> {
    /// The line without its line ending.
    pub(crate) text: &'a str,
    /// Inside a fenced code block, or one of its fence lines.
    pub(crate) in_code: bool,
    /// Inside the front matter block at the top of the file, or one of its
    /// `---` lines.
    pub(crate) in_front_matter: bool,
}

/// The lines of `text`, each marked as code, front matter or prose.
pub(crate) fn lines(text: &str) -> Vec<Line<'_>> {
    let mut marked = Vec::new();
    // The fence that opened the current code block: its mark and length.
    let mut fence: Option<(u8, usize)> = None;
    let mut in_front_matter = false;
    for (index, line) in text.lines().enumerate() {
        if index == 0 && line.trim_end() == "---" && closes_front_matter(text) {
            in_front_matter = true;
            marked.push(Line {
                text: line,
                in_code: false,
                in_front_matter: true,
            });
            continue;
        }
        if in_front_matter {
            in_front_matter = line.trim_end() != "---";
            marked.push(Line {
                text: line,
                in_code: false,
                in_front_matter: true,
            });
            continue;
        }
        let in_code = match (fence, fence_of(line)) {
            (Some((mark, length)), Some((other, run, rest))) => {
                if other == mark && run >= length && rest.trim().is_empty() {
                    fence = None;
                }
                true
            }
            (Some(_), None) => true,
            (None, Some((mark, run, _))) => {
                fence = Some((mark, run));
                true
            }
            (None, None) => false,
        };
        marked.push(Line {
            text: line,
            in_code,
            in_front_matter: false,
        });
    }
    marked
}

/// Whether the `---` on the first line of `text` is closed by a later one.
fn closes_front_matter(text: &str) -> bool {
    text.lines().skip(1).any(|line| line.trim_end() == "---")
}

/// The fence a line opens or closes: its mark, the length of the run and
/// what follows it.
fn fence_of(line: &str) -> Option<(u8, usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let mark = *rest.as_bytes().first()?;
    if mark != b'`' && mark != b'~' {
        return None;
    }
    let run = rest.bytes().take_while(|byte| *byte == mark).count();
    (run >= 3).then(|| (mark, run, &rest[run..]))
}

/// The level and text of an ATX heading: one to six `#`, then a space or
/// the end of the line, indented at most three spaces.
fn heading(line: &str) -> Option<(u8, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let level = rest.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    let title = after.trim();
    // A closing run of `#` is not part of the title.
    let title = match title.trim_end_matches('#') {
        closed if closed.len() < title.len() && (closed.is_empty() || closed.ends_with(' ')) => {
            closed.trim_end()
        }
        _ => title,
    };
    Some((u8::try_from(level).ok()?, title.to_owned()))
}

/// A run of lines that stands or falls together: a heading and everything
/// under it, or the text before the first heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
    /// The heading's level, 1 to 6; 0 for the text before the first
    /// heading and for a file that is not split.
    pub(crate) level: u8,
    /// The heading's text; empty for level 0.
    pub(crate) title: String,
    /// The first line, counted from 0.
    pub(crate) start: usize,
    /// One past the last line.
    pub(crate) end: usize,
}

/// The sections `text` splits into: at every heading of the shallowest
/// level that occurs at least twice, and at any shallower heading. Deeper
/// headings stay inside their section. Text before the first such heading
/// is a section of level 0 when it holds more than blank lines. A file
/// where no heading level repeats is one section of level 0.
pub(crate) fn sections(text: &str) -> Vec<Section> {
    let marked = lines(text);
    if marked.is_empty() {
        return Vec::new();
    }
    let headings: Vec<(usize, u8, String)> = marked
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.in_code && !line.in_front_matter)
        .filter_map(|(index, line)| heading(line.text).map(|(level, title)| (index, level, title)))
        .collect();
    let split = (1..=6u8).find(|level| {
        headings
            .iter()
            .filter(|(_, other, _)| other == level)
            .count()
            >= 2
    });
    let whole = || {
        vec![Section {
            level: 0,
            title: String::new(),
            start: 0,
            end: marked.len(),
        }]
    };
    let Some(split) = split else {
        return whole();
    };
    let starts: Vec<&(usize, u8, String)> = headings
        .iter()
        .filter(|(_, level, _)| *level <= split)
        .collect();
    let mut found = Vec::new();
    let first = starts.first().map_or(marked.len(), |(index, _, _)| *index);
    if marked[..first]
        .iter()
        .any(|line| !line.text.trim().is_empty())
    {
        found.push(Section {
            level: 0,
            title: String::new(),
            start: 0,
            end: first,
        });
    }
    for (position, (index, level, title)) in starts.iter().enumerate() {
        let end = starts
            .get(position + 1)
            .map_or(marked.len(), |(next, _, _)| *next);
        found.push(Section {
            level: *level,
            title: title.clone(),
            start: *index,
            end,
        });
    }
    found
}

/// `title` as the part of a unit's name after `#`: lowercase letters and
/// digits (of any script) kept, every run of anything else one `-`.
pub(crate) fn slug(title: &str) -> String {
    let mut slug = String::new();
    for character in title.chars() {
        if character.is_alphanumeric() {
            slug.extend(character.to_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "section".to_owned()
    } else {
        slug.to_owned()
    }
}

/// The top-level `key: value` pairs of the front matter block at the top
/// of `text`; a list value is its items joined with `, `. Empty without a
/// front matter block.
pub(crate) fn front_matter(text: &str) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in lines(text)
        .iter()
        .filter(|line| line.in_front_matter && line.text.trim_end() != "---")
    {
        let text = line.text;
        if let Some(item) = text.trim_start().strip_prefix("- ")
            && text.starts_with([' ', '-'])
        {
            // An item of the list the last key opened.
            if let Some((_, value)) = pairs.last_mut() {
                if !value.is_empty() {
                    value.push_str(", ");
                }
                value.push_str(unquote(item));
            }
            continue;
        }
        if text.starts_with([' ', '\t', '#']) {
            continue;
        }
        let Some((key, value)) = text.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let value = match value
            .strip_prefix('[')
            .and_then(|list| list.strip_suffix(']'))
        {
            Some(list) => list
                .split(',')
                .map(|item| unquote(item.trim()))
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
            None => unquote(value).to_owned(),
        };
        pairs.push((key.trim().to_owned(), value));
    }
    pairs
}

/// `value` without one pair of surrounding quotes.
fn unquote(value: &str) -> &str {
    let value = value.trim();
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// The inline code spans of one line of prose.
pub(crate) fn code_spans(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let run = bytes[at..].iter().take_while(|byte| **byte == b'`').count();
        let open = at + run;
        // The span ends at the next run of the same length.
        let mut scan = open;
        let mut close = None;
        while scan < bytes.len() {
            if bytes[scan] == b'`' {
                let other = bytes[scan..]
                    .iter()
                    .take_while(|byte| **byte == b'`')
                    .count();
                if other == run {
                    close = Some(scan);
                    break;
                }
                scan += other;
            } else {
                scan += 1;
            }
        }
        match close {
            Some(close) => {
                spans.push(line[open..close].trim());
                at = close + run;
            }
            None => at = open,
        }
    }
    spans
}

/// The targets of the inline links (`[text](target)`) of one line of
/// prose, without a title.
pub(crate) fn link_targets(line: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("](") {
        let after = &rest[at + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        let target = after[..end].split_whitespace().next().unwrap_or_default();
        if !target.is_empty() {
            targets.push(target);
        }
        rest = &after[end + 1..];
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> Vec<(u8, String, usize, usize)> {
        sections(text)
            .into_iter()
            .map(|section| (section.level, section.title, section.start, section.end))
            .collect()
    }

    #[test]
    fn splits_at_the_shallowest_heading_level_that_repeats() {
        // The title is shallower than the level that repeats, so it starts
        // a section of its own; `### A1` is deeper and stays inside `A`.
        let text = "# Title\nintro\n## A\na\n### A1\ndeep\n## B\nb\n";
        assert_eq!(
            spans(text),
            [
                (1, "Title".to_owned(), 0, 2),
                (2, "A".to_owned(), 2, 6),
                (2, "B".to_owned(), 6, 8),
            ]
        );
    }

    #[test]
    fn a_heading_inside_a_fenced_block_is_not_a_heading() {
        let text =
            "## A\n```sh\n# not a heading\n## nor this\n```\n## B\n~~~\n## still code\n~~~\n";
        assert_eq!(
            spans(text),
            [(2, "A".to_owned(), 0, 5), (2, "B".to_owned(), 5, 9)]
        );
    }

    #[test]
    fn a_file_where_no_level_repeats_is_one_section() {
        assert_eq!(
            spans("# Only\ntext\n## One\nmore\n"),
            [(0, String::new(), 0, 4)]
        );
        assert_eq!(spans("just text\n"), [(0, String::new(), 0, 1)]);
        assert_eq!(spans(""), []);
    }

    #[test]
    fn a_shallower_heading_also_starts_a_section() {
        assert_eq!(
            spans("## A\na\n# Big\n## B #\n"),
            [
                (2, "A".to_owned(), 0, 2),
                (1, "Big".to_owned(), 2, 3),
                (2, "B".to_owned(), 3, 4),
            ]
        );
    }

    #[test]
    fn blank_text_before_the_first_heading_is_no_section() {
        assert_eq!(
            spans("\n\n# A\n# B\n"),
            [(1, "A".to_owned(), 2, 3), (1, "B".to_owned(), 3, 4)]
        );
    }

    #[test]
    fn front_matter_is_not_split_and_belongs_to_the_top() {
        let text = "---\n# not a heading\nname: x\n---\n# A\n# B\n";
        assert_eq!(
            spans(text),
            [
                (0, String::new(), 0, 4),
                (1, "A".to_owned(), 4, 5),
                (1, "B".to_owned(), 5, 6),
            ]
        );
        let marked = lines(text);
        assert!(marked[1].in_front_matter && marked[3].in_front_matter);
        assert!(!marked[4].in_front_matter);
    }

    #[test]
    fn what_is_not_a_heading() {
        // No space after the marks, seven marks, four spaces of indent.
        assert_eq!(
            spans("#A\n####### B\n    # C\n# D\n# E\n"),
            [
                (0, String::new(), 0, 3),
                (1, "D".to_owned(), 3, 4),
                (1, "E".to_owned(), 4, 5),
            ]
        );
    }

    #[test]
    fn slugs_keep_letters_and_digits_of_any_script() {
        assert_eq!(slug("Testing & CI/CD!"), "testing-ci-cd");
        assert_eq!(slug("  2. Code Style  "), "2-code-style");
        assert_eq!(slug("测试 规则"), "测试-规则");
        assert_eq!(slug("`npm test`"), "npm-test");
        assert_eq!(slug("!!!"), "section");
    }

    #[test]
    fn front_matter_gives_scalars_and_joined_lists() {
        let text = "---\nname: deploy\ndescription: \"Deploy the app: to production\"\npaths:\n  - \"src/**/*.ts\"\n  - lib/**\nglobs: [a, 'b']\ndisable-model-invocation: true\n---\nbody: not this\n";
        assert_eq!(
            front_matter(text),
            [
                ("name".to_owned(), "deploy".to_owned()),
                (
                    "description".to_owned(),
                    "Deploy the app: to production".to_owned()
                ),
                ("paths".to_owned(), "src/**/*.ts, lib/**".to_owned()),
                ("globs".to_owned(), "a, b".to_owned()),
                ("disable-model-invocation".to_owned(), "true".to_owned()),
            ]
        );
        assert_eq!(front_matter("no front matter\n---\nname: x\n---\n"), []);
        assert_eq!(front_matter("---\nname: never closed\n"), []);
    }

    #[test]
    fn code_spans_and_link_targets_of_a_line() {
        assert_eq!(
            code_spans("run `npm test` and `make lint`, not ``a`b``"),
            ["npm test", "make lint", "a`b"]
        );
        assert_eq!(code_spans("an unclosed `span"), Vec::<&str>::new());
        assert_eq!(
            link_targets(
                "see [setup](docs/setup.md \"Setup\") and [site](https://e.com), not [ref][1]"
            ),
            ["docs/setup.md", "https://e.com"]
        );
    }
}
