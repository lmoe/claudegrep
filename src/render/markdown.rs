use std::fmt::{self, Display, Formatter, Write};

const LISTED: usize = 6;

pub struct Details<'a, B> {
    summary: &'a str,
    body: B,
}

pub const fn details<B: Display>(summary: &str, body: B) -> Details<'_, B> {
    Details { summary, body }
}

impl<B: Display> Display for Details<'_, B> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "<details>\n<summary>{}</summary>\n\n{}\n\n</details>", self.summary, self.body)
    }
}

/// A fenced code block, its fence longer than any run of backticks inside it.
pub struct CodeBlock<'a> {
    content: &'a str,
    language: &'a str,
}

pub const fn code_block<'a>(content: &'a str, language: &'a str) -> CodeBlock<'a> {
    CodeBlock { content, language }
}

impl Display for CodeBlock<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let longest_run = self.content.split(|c| c != '`').map(str::len).max().unwrap_or(0);
        let fence = Fence(longest_run.max(2) + 1);
        write!(f, "{fence}{}\n{}\n{fence}", self.language, self.content)
    }
}

struct Fence(usize);

impl Display for Fence {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        (0..self.0).try_for_each(|_| f.write_char('`'))
    }
}

pub struct CodeList<'a, T>(pub &'a [T]);

impl<T: Display> Display for CodeList<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (index, item) in self.0.iter().take(LISTED).enumerate() {
            let separator = if index == 0 { "" } else { ", " };
            write!(f, "{separator}`{item}`")?;
        }
        if self.0.len() > LISTED {
            write!(f, " and {} more", self.0.len() - LISTED)?;
        }
        Ok(())
    }
}

pub struct Quote<'a>(pub &'a str);

impl Display for Quote<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (index, line) in self.0.lines().enumerate() {
            if index > 0 {
                f.write_char('\n')?;
            }
            if line.is_empty() { f.write_char('>') } else { write!(f, "> {line}") }?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_fences_outgrow_the_longest_backtick_run() {
        assert_eq!(code_block("plain", "json").to_string(), "```json\nplain\n```");
        assert_eq!(code_block("a `` b", "").to_string(), "```\na `` b\n```");
        assert_eq!(code_block("a ``` b", "").to_string(), "````\na ``` b\n````");
        assert_eq!(code_block("```rust\n```````\n```", "md").to_string(), "````````md\n```rust\n```````\n```\n````````");
    }

    #[test]
    fn code_blocks_keep_empty_content() {
        assert_eq!(code_block("", "").to_string(), "```\n\n```");
    }

    #[test]
    fn quotes_mark_empty_lines_without_a_trailing_space() {
        assert_eq!(Quote("one\n\ntwo\n").to_string(), "> one\n>\n> two");
        assert_eq!(Quote("  indented").to_string(), ">   indented");
        assert_eq!(Quote("").to_string(), "");
    }

    #[test]
    fn details_wrap_any_body() {
        assert_eq!(
            details("Result", code_block("ok", "")).to_string(),
            "<details>\n<summary>Result</summary>\n\n```\nok\n```\n\n</details>"
        );
    }

    #[test]
    fn code_lists_name_the_rest_after_six() {
        assert_eq!(CodeList::<&str>(&[]).to_string(), "");
        assert_eq!(CodeList(&["a", "b"]).to_string(), "`a`, `b`");
        assert_eq!(CodeList(&["1", "2", "3", "4", "5", "6"]).to_string(), "`1`, `2`, `3`, `4`, `5`, `6`");
        assert_eq!(CodeList(&["1", "2", "3", "4", "5", "6", "7", "8"]).to_string(), "`1`, `2`, `3`, `4`, `5`, `6` and 2 more");
    }
}
