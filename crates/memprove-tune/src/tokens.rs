//! A rough count of the tokens a text costs, without a tokenizer.

/// An estimate of the tokens in `text`: a quarter of its ASCII bytes plus
/// one and a half per other character, each rounded up. `ab` reports what
/// the agent itself measured; this number is only for sizing and sorting.
pub(crate) fn estimate(text: &str) -> u64 {
    let ascii = text.bytes().filter(u8::is_ascii).count() as u64;
    let other = text
        .chars()
        .filter(|character| !character.is_ascii())
        .count() as u64;
    ascii.div_ceil(4) + (other * 3).div_ceil(2)
}

/// `text` as Claude Code puts an instruction file into context: without
/// the block-level HTML comments outside code blocks.
pub(crate) fn without_comments(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut in_comment = false;
    for line in crate::markdown::lines(text) {
        if in_comment {
            in_comment = !line.text.contains("-->");
            continue;
        }
        if !line.in_code && line.text.trim_start().starts_with("<!--") {
            in_comment = !line.text.contains("-->");
            continue;
        }
        kept.push_str(line.text);
        kept.push('\n');
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_ascii_bytes_or_two_thirds_of_another_character_per_token() {
        assert_eq!(estimate(""), 0);
        assert_eq!(estimate("abcd"), 1);
        assert_eq!(estimate("abcde"), 2);
        assert_eq!(estimate("测试"), 3);
        assert_eq!(estimate("测试 ok"), 4);
    }

    #[test]
    fn block_comments_outside_code_are_not_counted() {
        let text = "# A\n<!-- maintainer\nnotes -->\nkept <!-- inline stays -->\n```\n<!-- in code -->\n```\n<!-- one line -->\nend\n";
        assert_eq!(
            without_comments(text),
            "# A\nkept <!-- inline stays -->\n```\n<!-- in code -->\n```\nend\n"
        );
    }
}
