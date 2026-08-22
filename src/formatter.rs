use crate::error::FosterError;

const INDENT: &str = "    ";

/// Formats Foster source while preserving comments and literal contents.
///
/// Formatting validates the complete source before producing output. It normalizes line endings,
/// indentation, trailing whitespace, and the final newline. Delimiter indentation is derived from
/// braces, parentheses, and brackets outside comments and literals.
pub fn format(source: &str) -> Result<String, FosterError> {
    crate::parse(source)?;

    let source = source.replace("\r\n", "\n").replace('\r', "\n");
    let mut state = ScanState::default();
    let mut delimiter_indents = Vec::new();
    let mut output = Vec::new();
    let mut previous_blank = false;
    let mut type_continuation = false;

    for raw_line in source.lines() {
        let content = raw_line.trim();
        if content.is_empty() {
            if !output.is_empty() && !previous_blank {
                output.push(String::new());
            }
            previous_blank = true;
            continue;
        }

        previous_blank = false;
        if begins_declaration(content) && !content.starts_with('|') && !content.starts_with('&') {
            type_continuation = content.starts_with("type ") && content.ends_with('=');
        }

        let leading_closers = count_leading_closers(content, &state);
        let retained = delimiter_indents.len().saturating_sub(leading_closers);
        let mut indent = if leading_closers > 0 {
            delimiter_indents
                .get(retained)
                .copied()
                .unwrap_or(1usize)
                .saturating_sub(1)
        } else {
            delimiter_indents.last().copied().unwrap_or(0)
        };
        if type_continuation && matches!(content.as_bytes().first(), Some(b'|' | b'&')) {
            indent = indent.max(1);
        }

        output.push(format!("{}{}", INDENT.repeat(indent), content));
        scan_line(content, indent, &mut state, &mut delimiter_indents);

        if type_continuation
            && delimiter_indents.is_empty()
            && !content.starts_with("type ")
            && !content.starts_with('|')
            && !content.starts_with('&')
        {
            type_continuation = false;
        }
    }

    while output.last().is_some_and(String::is_empty) {
        output.pop();
    }
    Ok(format!("{}\n", output.join("\n")))
}

fn begins_declaration(line: &str) -> bool {
    let line = line.strip_prefix("pub ").unwrap_or(line);
    ["import ", "const ", "type ", "func ", "test "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

#[derive(Default)]
struct ScanState {
    block_comment_depth: usize,
    string: Option<char>,
    escaped: bool,
}

fn count_leading_closers(line: &str, state: &ScanState) -> usize {
    if state.block_comment_depth != 0 || state.string.is_some() {
        return 0;
    }
    line.chars()
        .take_while(|character| matches!(character, '}' | ')' | ']'))
        .count()
}

fn scan_line(
    line: &str,
    line_indent: usize,
    state: &mut ScanState,
    delimiter_indents: &mut Vec<usize>,
) {
    let characters = line.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();

        if state.block_comment_depth > 0 {
            if character == '/' && next == Some('*') {
                state.block_comment_depth += 1;
                index += 2;
                continue;
            }
            if character == '*' && next == Some('/') {
                state.block_comment_depth -= 1;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }

        if let Some(quote) = state.string {
            if state.escaped {
                state.escaped = false;
            } else if character == '\\' {
                state.escaped = true;
            } else if character == quote {
                state.string = None;
            }
            index += 1;
            continue;
        }

        if character == '/' && next == Some('/') {
            break;
        }
        if character == '/' && next == Some('*') {
            state.block_comment_depth = 1;
            index += 2;
            continue;
        }
        if matches!(character, '"' | '\'') {
            state.string = Some(character);
            index += 1;
            continue;
        }
        match character {
            '{' | '(' | '[' => delimiter_indents.push(line_indent + 1),
            '}' | ')' | ']' => {
                delimiter_indents.pop();
            }
            _ => {}
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::format;

    #[test]
    fn formats_indentation_and_preserves_comments_and_literals() {
        let source = "// { retained\r\nfunc main() -> String {  \r\nvalue = \"}\"\r\nbranch {\r\ntrue -> value\r\n_ -> \"no\"\r\n}\r\n}\r\n";
        assert_eq!(
            format(source).unwrap(),
            "// { retained\nfunc main() -> String {\n    value = \"}\"\n    branch {\n        true -> value\n        _ -> \"no\"\n    }\n}\n"
        );
    }

    #[test]
    fn formats_multiline_type_composition() {
        let source = "type Foo =\n| Bar\n| What\n& SomeContract\n& {\npub func describe(self) -> String\n}\n";
        assert_eq!(
            format(source).unwrap(),
            "type Foo =\n    | Bar\n    | What\n    & SomeContract\n    & {\n        pub func describe(self) -> String\n    }\n"
        );
    }

    #[test]
    fn rejects_invalid_source() {
        assert!(format("func main( {\n").is_err());
    }

    #[test]
    fn formats_test_declarations() {
        assert_eq!(
            format("test \"works\" {\nprintln()\n}\n").unwrap(),
            "test \"works\" {\n    println()\n}\n"
        );
    }
}
