//! Triple-quoted strings lower to ordinary, borrowed `as_string` calls and concatenation.
use super::*;

pub(super) fn parse(
    raw: &str,
    offset: usize,
    line: usize,
    column: usize,
) -> Result<Expr, FosterError> {
    // Delimiter-only boundary lines are layout, not text. Interior whitespace is exact.
    let leading = raw
        .find('\n')
        .filter(|end| raw[..*end].chars().all(|c| matches!(c, ' ' | '\t' | '\r')))
        .map_or(0, |end| end + 1);
    let end = raw
        .rfind('\n')
        .filter(|end| {
            raw[*end + 1..]
                .chars()
                .all(|c| matches!(c, ' ' | '\t' | '\r'))
        })
        .map_or(raw.len(), |end| {
            if end > 0 && raw.as_bytes()[end - 1] == b'\r' {
                end - 1
            } else {
                end
            }
        });
    let end = end.max(leading);
    let mut chars = raw[leading..end].char_indices().peekable();
    let mut text = String::new();
    let mut parts = Vec::new();
    let error = |message: &str, index: usize| {
        let prefix = &raw[..index];
        let lines = prefix.bytes().filter(|b| *b == b'\n').count();
        let col = prefix
            .rsplit_once('\n')
            .map_or(column + prefix.chars().count(), |(_, tail)| {
                tail.chars().count() + 1
            });
        FosterError::new(message, line + lines, col)
    };
    while let Some((index, c)) = chars.next() {
        match c {
            '\\' => {
                let Some((_, escaped)) = chars.next() else {
                    return Err(error("unterminated escape", leading + index));
                };
                text.push(match escaped {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '"' => '"',
                    '\\' => '\\',
                    _ => {
                        return Err(error(
                            "unknown escape in triple-quoted string",
                            leading + index,
                        ));
                    }
                });
            }
            '%' => {
                if chars.peek().is_some_and(|(_, c)| *c == '%') {
                    chars.next();
                    text.push('%');
                    continue;
                }
                let name_start = leading + index + 1;
                if chars.peek().is_some_and(|(_, c)| *c == '{') {
                    chars.next();
                    let expression_start = name_start + 1;
                    let origin = error("", expression_start);
                    let mut tokens = crate::lexer::lex_interpolation(&raw[expression_start..end])
                        .map_err(|mut diagnostic| {
                        if diagnostic.line == 1 {
                            diagnostic.column += origin.column - 1;
                        }
                        diagnostic.line += origin.line - 1;
                        diagnostic
                    })?;
                    let expression_end = expression_start + tokens.last().unwrap().range.start;
                    for token in &mut tokens {
                        token.range.start += offset + expression_start;
                        token.range.end += offset + expression_start;
                        if token.line == 1 {
                            token.column += origin.column - 1;
                        }
                        token.line += origin.line - 1;
                    }
                    let mut parser = Parser::new(tokens);
                    parser.newlines();
                    let value = parser.delimited_expression()?;
                    parser.newlines();
                    parser.expect(&TokenKind::Eof, "expected one expression in substitution")?;
                    while chars
                        .peek()
                        .is_some_and(|(position, _)| leading + *position < expression_end + 2)
                    {
                        chars.next();
                    }
                    parts.push(Expr::String(std::mem::take(&mut text)));
                    parts.push(convert(
                        value,
                        offset + expression_start..offset + expression_end,
                    ));
                    continue;
                }
                let mut name = String::new();
                while chars
                    .peek()
                    .is_some_and(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '?')
                {
                    name.push(chars.next().unwrap().1);
                }
                if !name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
                    || !chars.next().is_some_and(|(_, c)| c == '%')
                {
                    return Err(error(
                        "expected %name% or %{expression}% substitution; use %% for a literal percent",
                        leading + index,
                    ));
                }
                // Let the ordinary lexer reject keywords and non-identifier spellings.
                let tokens = crate::lexer::lex(&name)?;
                if !matches!(
                    tokens.as_slice(),
                    [
                        Token {
                            kind: TokenKind::Ident(_),
                            ..
                        },
                        Token {
                            kind: TokenKind::Eof,
                            ..
                        }
                    ]
                ) || name == "_"
                {
                    return Err(error("substitution requires a variable name", name_start));
                }
                parts.push(Expr::String(std::mem::take(&mut text)));
                let span = offset + name_start..offset + name_start + name.len();
                let value = Expr::Spanned {
                    expression: Box::new(Expr::Name(name)),
                    span: span.clone(),
                };
                parts.push(convert(value, span));
            }
            '\r' if chars.peek().is_some_and(|(_, c)| *c == '\n') => {}
            _ => text.push(c),
        }
    }
    parts.push(Expr::String(text));
    Ok(parts
        .into_iter()
        .reduce(|left, right| Expr::Binary {
            left: Box::new(left),
            operator: BinaryOp::Add,
            right: Box::new(right),
        })
        .unwrap())
}

fn convert(value: Expr, span: std::ops::Range<usize>) -> Expr {
    Expr::Spanned {
        expression: Box::new(Expr::Call {
            callee: Box::new(Expr::Member {
                object: Box::new(value),
                name: "as_string".into(),
            }),
            arguments: Vec::new(),
        }),
        span,
    }
}
