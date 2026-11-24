use std::fmt::{self, Display, Formatter};

use crate::render::markdown::{code_block, details};
use crate::render::{config, list, search, session, shell, status};
use crate::text::{char_len, plural};
use crate::transcript::{ToolCall, ToolResult};

const MAX_TOOL_RESULT_CHARS: usize = 4000;

const CLAUDEGREP_OMITTED: &str = "*(claudegrep output omitted: it repeats other sessions)*";

pub struct DeepCall<'a>(pub &'a ToolCall);

impl Display for DeepCall<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let call = self.0;
        let input = serde_json::to_string_pretty(&call.input).unwrap_or_default();
        writeln!(f, "**🔧 {}**\n\n{}", call.name, code_block(&input, "json"))?;
        let Some(result) = &call.result else {
            return Ok(());
        };
        if runs_claudegrep(call) || is_claudegrep_output(&result.text) {
            return writeln!(f, "\n{CLAUDEGREP_OMITTED}");
        }
        let label = if result.is_error { "❌ Result (error)" } else { "Result" };
        writeln!(f, "\n{}", details(label, code_block(&body(result), "")))
    }
}

fn body(result: &ToolResult) -> String {
    let text = result.text.trim();
    let length = char_len(text);
    let mut body = if length > MAX_TOOL_RESULT_CHARS {
        let kept: String = text.chars().take(MAX_TOOL_RESULT_CHARS).collect();
        let cut = length - MAX_TOOL_RESULT_CHARS;
        format!("{kept}\n… [truncated {cut} {}]", plural(cut, "char", "chars"))
    } else {
        text.to_owned()
    };
    if result.images > 0 {
        let separator = if body.is_empty() { "" } else { "\n" };
        let images = result.images;
        body = format!("{body}{separator}[{images} {} omitted]", plural(images, "image", "images"));
    }
    body
}

fn runs_claudegrep(call: &ToolCall) -> bool {
    let command = call.input.get("command").and_then(serde_json::Value::as_str).unwrap_or_default();
    call.name.to_lowercase().contains("claudegrep") || shell::programs(command).iter().any(|program| program.name == "claudegrep")
}

fn is_claudegrep_output(text: &str) -> bool {
    text.lines().any(|line| {
        [search::HEADING, list::HEADING, status::HEADING, config::HEADING].iter().any(|heading| line.starts_with(heading))
            || (line.starts_with(session::DATE) && line.contains(session::SESSION))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn call(name: &str, input: Value, result: Option<ToolResult>) -> ToolCall {
        ToolCall { name: name.to_owned(), input, result }
    }

    fn result(text: &str, is_error: bool, images: usize) -> ToolResult {
        ToolResult { text: text.to_owned(), is_error, images }
    }

    fn bash(command: &str) -> ToolCall {
        call("Bash", json!({ "command": command }), None)
    }

    #[test]
    fn detects_claudegrep_invocations() {
        assert!(runs_claudegrep(&bash("claudegrep --all \"ocr\"")));
        assert!(runs_claudegrep(&bash("cd x && ~/.local/bin/claudegrep list | head")));
        assert!(runs_claudegrep(&bash("echo \"$(sudo -u x claudegrep list)\"")));
        assert!(runs_claudegrep(&call("mcp__claudegrep__search", json!({}), None)));
        assert!(runs_claudegrep(&call("mcp__ClaudeGrep__read", json!({}), None)));
        assert!(!runs_claudegrep(&bash("cd /home/dev/claudegrep && cargo test")));
        assert!(!runs_claudegrep(&bash("grep claudegrep Cargo.toml")));
        assert!(!runs_claudegrep(&call("Read", json!({ "file_path": "/x/claudegrep" }), None)));
    }

    #[test]
    fn detects_claudegrep_output() {
        assert!(is_claudegrep_output("Finished\n# Search: ocr\n\n*in all projects*"));
        assert!(is_claudegrep_output("# Sessions in all projects\n"));
        assert!(is_claudegrep_output("# claudegrep status\n"));
        assert!(is_claudegrep_output("# claudegrep config\n"));
        assert!(is_claudegrep_output("# Title\n\n**Date:** 2026-10-14 10:58 · **Session:** `26e5b481` · **Turns:** 4"));
        assert!(!is_claudegrep_output("# Search results\nplain markdown"));
        assert!(!is_claudegrep_output("**Date:** 2026-10-14"));
    }

    #[test]
    fn a_call_shows_its_input_and_result() {
        let shown = DeepCall(&call("Read", json!({ "file_path": "/p/a.rs" }), Some(result("fn a() {}\n", false, 0))));
        insta::assert_snapshot!(shown.to_string(), @r#"
        **🔧 Read**

        ```json
        {
          "file_path": "/p/a.rs"
        }
        ```

        <details>
        <summary>Result</summary>

        ```
        fn a() {}
        ```

        </details>
        "#);
    }

    #[test]
    fn a_call_without_a_result_shows_only_its_input() {
        assert_eq!(DeepCall(&call("Glob", json!({}), None)).to_string(), "**🔧 Glob**\n\n```json\n{}\n```\n");
    }

    #[test]
    fn errors_are_labelled() {
        let shown = DeepCall(&call("Bash", json!({ "command": "false" }), Some(result("exit 1", true, 0)))).to_string();
        assert!(shown.contains("<summary>❌ Result (error)</summary>"), "{shown}");
    }

    #[test]
    fn claudegrep_output_is_omitted() {
        let omitted = |call: ToolCall| DeepCall(&call).to_string().ends_with(&format!("```\n\n{CLAUDEGREP_OMITTED}\n"));
        let found = || Some(result("1 match", false, 0));
        assert!(omitted(call("Bash", json!({ "command": "claudegrep search x" }), found())));
        assert!(omitted(call("mcp__claudegrep__search", json!({ "query": "x" }), found())));
        assert!(omitted(call("Bash", json!({ "command": "./run.sh" }), Some(result("# Search: x\n", false, 0)))));
        assert!(!omitted(call("Bash", json!({ "command": "./run.sh" }), found())));
    }

    #[test]
    fn results_are_cut_after_the_limit() {
        let at_limit = "x".repeat(MAX_TOOL_RESULT_CHARS);
        assert_eq!(body(&result(&at_limit, false, 0)), at_limit);
        let past = format!("{at_limit}ä");
        assert_eq!(body(&result(&past, false, 0)), format!("{at_limit}\n… [truncated 1 char]"));
    }

    #[test]
    fn results_are_trimmed_before_counting() {
        let padded = format!("  {}  \n", "x".repeat(MAX_TOOL_RESULT_CHARS));
        assert_eq!(body(&result(&padded, false, 0)), "x".repeat(MAX_TOOL_RESULT_CHARS));
    }

    #[test]
    fn images_are_counted_after_the_text() {
        assert_eq!(body(&result("screenshot taken", false, 2)), "screenshot taken\n[2 images omitted]");
        assert_eq!(body(&result("", false, 1)), "[1 image omitted]");
        assert_eq!(body(&result("  ", false, 0)), "");
    }
}
