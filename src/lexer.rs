use crate::error::FosterError;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub column: usize,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    Integer(i64),
    Float(f64),
    String(String),
    CodePoint(String),
    Symbol(String),
    DocComment(String),
    ModuleDocComment(String),
    Func,
    Test,
    Const,
    Let,
    Pub,
    Type,
    Enum,
    Import,
    As,
    Return,
    Assert,
    Loop,
    Break,
    Continue,
    If,
    Branch,
    Else,
    Remote,
    Await,
    Try,
    True,
    False,
    Copy,
    Move,
    Ref,
    Group,
    Read,
    Mut,
    Reshape,
    Consume,
    Suspend,
    Intrinsic,
    Pipe,
    Ampersand,
    Caret,
    Tilde,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    DoubleColon,
    Dot,
    Arrow,
    Equal,
    EqualEqual,
    Bang,
    BangEqual,
    Plus,
    Minus,
    Star,
    Slash,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Newline,
    Eof,
}

pub fn lex(source: &str) -> Result<Vec<Token>, FosterError> {
    Lexer::new(source).lex_all()
}

struct Lexer<'a> {
    chars: Vec<char>,
    index: usize,
    line: usize,
    column: usize,
    byte_index: usize,
    _source: &'a str,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().collect(),
            index: 0,
            line: 1,
            column: 1,
            byte_index: 0,
            _source: source,
        }
    }

    fn lex_all(mut self) -> Result<Vec<Token>, FosterError> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            let (line, column, offset) = (self.line, self.column, self.byte_index);
            match c {
                ' ' | '\t' | '\r' => {
                    self.advance();
                }
                '\n' => {
                    self.advance();
                    out.push(Token {
                        kind: TokenKind::Newline,
                        line,
                        column,
                        range: offset..self.byte_index,
                    });
                }
                '/' if self.peek_next() == Some('/') && self.peek_n(2) == Some('!') => {
                    out.push(self.doc_line(true));
                }
                '/' if self.peek_next() == Some('/') && self.peek_n(2) == Some('/') => {
                    out.push(self.doc_line(false));
                }
                '/' if self.peek_next() == Some('/') => {
                    while !matches!(self.peek(), None | Some('\n')) {
                        self.advance();
                    }
                }
                '/' if self.peek_next() == Some('*') => {
                    if let Some(comment) = self.block_comment()? {
                        out.push(comment);
                    }
                }
                '0'..='9' => out.push(self.number()?),
                '"' => out.push(self.string()?),
                '\'' => out.push(self.code_point()?),
                ':' if self.peek_next().is_some_and(is_ident_start) => out.push(self.symbol()),
                c if is_ident_start(c) => out.push(self.identifier()),
                '(' => {
                    self.advance();
                    out.push(tok(
                        TokenKind::LParen,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                ')' => {
                    self.advance();
                    out.push(tok(
                        TokenKind::RParen,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                '{' => {
                    self.advance();
                    out.push(tok(
                        TokenKind::LBrace,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                '}' => {
                    self.advance();
                    out.push(tok(
                        TokenKind::RBrace,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                '[' => {
                    self.advance();
                    out.push(tok(
                        TokenKind::LBracket,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                '|' => {
                    self.advance();
                    out.push(tok(TokenKind::Pipe, line, column, offset..self.byte_index));
                }
                '&' => {
                    self.advance();
                    out.push(tok(
                        TokenKind::Ampersand,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                '^' => {
                    self.advance();
                    out.push(tok(TokenKind::Caret, line, column, offset..self.byte_index));
                }
                '~' => {
                    self.advance();
                    out.push(tok(TokenKind::Tilde, line, column, offset..self.byte_index));
                }
                ']' => {
                    self.advance();
                    out.push(tok(
                        TokenKind::RBracket,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                ',' => {
                    self.advance();
                    out.push(tok(TokenKind::Comma, line, column, offset..self.byte_index));
                }
                ':' if self.peek_next() == Some(':') => {
                    self.advance();
                    self.advance();
                    out.push(tok(
                        TokenKind::DoubleColon,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                ':' => {
                    self.advance();
                    out.push(tok(TokenKind::Colon, line, column, offset..self.byte_index));
                }
                '.' => {
                    self.advance();
                    out.push(tok(TokenKind::Dot, line, column, offset..self.byte_index));
                }
                '+' => {
                    self.advance();
                    out.push(tok(TokenKind::Plus, line, column, offset..self.byte_index));
                }
                '*' => {
                    self.advance();
                    out.push(tok(TokenKind::Star, line, column, offset..self.byte_index));
                }
                '/' => {
                    self.advance();
                    out.push(tok(TokenKind::Slash, line, column, offset..self.byte_index));
                }
                '-' if self.peek_next() == Some('>') => {
                    self.advance();
                    self.advance();
                    out.push(tok(TokenKind::Arrow, line, column, offset..self.byte_index));
                }
                '-' => {
                    self.advance();
                    out.push(tok(TokenKind::Minus, line, column, offset..self.byte_index));
                }
                '=' if self.peek_next() == Some('=') => {
                    self.advance();
                    self.advance();
                    out.push(tok(
                        TokenKind::EqualEqual,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                '=' => {
                    self.advance();
                    out.push(tok(TokenKind::Equal, line, column, offset..self.byte_index));
                }
                '!' if self.peek_next() == Some('=') => {
                    self.advance();
                    self.advance();
                    out.push(tok(
                        TokenKind::BangEqual,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                '!' => {
                    self.advance();
                    out.push(tok(TokenKind::Bang, line, column, offset..self.byte_index));
                }
                '<' if self.peek_next() == Some('=') => {
                    self.advance();
                    self.advance();
                    out.push(tok(
                        TokenKind::LessEqual,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                '<' => {
                    self.advance();
                    out.push(tok(TokenKind::Less, line, column, offset..self.byte_index));
                }
                '>' if self.peek_next() == Some('=') => {
                    self.advance();
                    self.advance();
                    out.push(tok(
                        TokenKind::GreaterEqual,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                '>' => {
                    self.advance();
                    out.push(tok(
                        TokenKind::Greater,
                        line,
                        column,
                        offset..self.byte_index,
                    ));
                }
                _ => {
                    return Err(FosterError::new(
                        format!("unexpected character `{c}`"),
                        line,
                        column,
                    ));
                }
            }
        }
        out.push(tok(
            TokenKind::Eof,
            self.line,
            self.column,
            self.byte_index..self.byte_index,
        ));
        Ok(out)
    }

    fn number(&mut self) -> Result<Token, FosterError> {
        let (line, column, offset) = (self.line, self.column, self.byte_index);
        let start = self.index;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.advance();
        }
        let mut is_float = false;
        if self.peek() == Some('.') && self.peek_next().is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            self.advance();
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.advance();
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            is_float = true;
            self.advance();
            if matches!(self.peek(), Some('+' | '-')) {
                self.advance();
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(FosterError::new("expected exponent digits", line, column));
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.advance();
            }
        }
        let text: String = self.chars[start..self.index].iter().collect();
        let kind = if is_float {
            let value = text
                .parse()
                .map_err(|_| FosterError::new("invalid float literal", line, column))?;
            TokenKind::Float(value)
        } else {
            let value = text
                .parse()
                .map_err(|_| FosterError::new("integer is out of range", line, column))?;
            TokenKind::Integer(value)
        };
        Ok(Token {
            kind,
            line,
            column,
            range: offset..self.byte_index,
        })
    }

    fn string(&mut self) -> Result<Token, FosterError> {
        let (line, column, offset) = (self.line, self.column, self.byte_index);
        self.advance();
        let mut value = String::new();
        while let Some(c) = self.peek() {
            if c == '"' {
                self.advance();
                return Ok(Token {
                    kind: TokenKind::String(value),
                    line,
                    column,
                    range: offset..self.byte_index,
                });
            }
            if c == '\n' {
                return Err(FosterError::new("unterminated string", line, column));
            }
            if c == '\\' {
                self.advance();
                let escaped = self
                    .advance()
                    .ok_or_else(|| FosterError::new("unterminated escape", line, column))?;
                value.push(match escaped {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '"' => '"',
                    '\\' => '\\',
                    other => {
                        return Err(FosterError::new(
                            format!("unknown escape `\\{other}`"),
                            self.line,
                            self.column,
                        ));
                    }
                });
            } else {
                value.push(c);
                self.advance();
            }
        }
        Err(FosterError::new("unterminated string", line, column))
    }

    fn code_point(&mut self) -> Result<Token, FosterError> {
        let (line, column, offset) = (self.line, self.column, self.byte_index);
        self.advance();
        let value = if self.peek() == Some('\\') {
            self.advance();
            let escaped = self
                .advance()
                .ok_or_else(|| FosterError::new("unterminated code-point escape", line, column))?;
            match escaped {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '\'' => '\'',
                '"' => '"',
                '\\' => '\\',
                other => {
                    return Err(FosterError::new(
                        format!("unknown escape `\\{other}`"),
                        line,
                        column,
                    ));
                }
            }
        } else {
            self.advance()
                .ok_or_else(|| FosterError::new("unterminated code-point literal", line, column))?
        };
        if self.peek() != Some('\'') {
            return Err(FosterError::new(
                "code-point literal must contain exactly one Unicode scalar value",
                line,
                column,
            ));
        }
        self.advance();
        Ok(Token {
            kind: TokenKind::CodePoint(value.to_string()),
            line,
            column,
            range: offset..self.byte_index,
        })
    }

    fn symbol(&mut self) -> Token {
        let (line, column, offset) = (self.line, self.column, self.byte_index);
        self.advance();
        let name = self.take_identifier();
        Token {
            kind: TokenKind::Symbol(name),
            line,
            column,
            range: offset..self.byte_index,
        }
    }

    fn doc_line(&mut self, module: bool) -> Token {
        let (line, column, offset) = (self.line, self.column, self.byte_index);
        self.advance();
        self.advance();
        self.advance();
        if self.peek() == Some(' ') {
            self.advance();
        }
        let mut value = String::new();
        while !matches!(self.peek(), None | Some('\n')) {
            value.push(self.advance().expect("peeked comment character"));
        }
        Token {
            kind: if module {
                TokenKind::ModuleDocComment(value.trim_end().to_owned())
            } else {
                TokenKind::DocComment(value.trim_end().to_owned())
            },
            line,
            column,
            range: offset..self.byte_index,
        }
    }

    fn block_comment(&mut self) -> Result<Option<Token>, FosterError> {
        let (line, column, offset) = (self.line, self.column, self.byte_index);
        self.advance();
        self.advance();
        let documentation = self.peek() == Some('*');
        let mut depth = 1;
        let mut value = String::new();
        while self.peek().is_some() {
            if self.peek() == Some('/') && self.peek_next() == Some('*') {
                depth += 1;
                self.advance();
                self.advance();
            } else if self.peek() == Some('*') && self.peek_next() == Some('/') {
                self.advance();
                self.advance();
                depth -= 1;
                if depth == 0 {
                    return Ok(documentation.then(|| Token {
                        kind: TokenKind::DocComment(normalize_doc_block(&value)),
                        line,
                        column,
                        range: offset..self.byte_index,
                    }));
                }
            } else {
                let character = self.advance().expect("comment has a character");
                if documentation {
                    value.push(character);
                }
            }
        }
        Err(FosterError::new("unterminated block comment", line, column))
    }

    fn identifier(&mut self) -> Token {
        let (line, column, offset) = (self.line, self.column, self.byte_index);
        let name = self.take_identifier();
        let kind = match name.as_str() {
            "func" | "function" => TokenKind::Func,
            "test" => TokenKind::Test,
            "const" => TokenKind::Const,
            "let" => TokenKind::Let,
            "pub" => TokenKind::Pub,
            "type" => TokenKind::Type,
            "enum" => TokenKind::Enum,
            "import" => TokenKind::Import,
            "as" => TokenKind::As,
            "return" => TokenKind::Return,
            "assert" => TokenKind::Assert,
            "loop" => TokenKind::Loop,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "if" => TokenKind::If,
            "branch" => TokenKind::Branch,
            "else" => TokenKind::Else,
            "remote" => TokenKind::Remote,
            "await" => TokenKind::Await,
            "try" => TokenKind::Try,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "copy" => TokenKind::Copy,
            "move" => TokenKind::Move,
            "ref" => TokenKind::Ref,
            "group" => TokenKind::Group,
            "read" => TokenKind::Read,
            "mut" => TokenKind::Mut,
            "reshape" => TokenKind::Reshape,
            "consume" => TokenKind::Consume,
            "suspend" => TokenKind::Suspend,
            "intrinsic" => TokenKind::Intrinsic,
            _ => TokenKind::Ident(name),
        };
        Token {
            kind,
            line,
            column,
            range: offset..self.byte_index,
        }
    }

    fn take_identifier(&mut self) -> String {
        let start = self.index;
        self.advance();
        while self.peek().is_some_and(is_ident_continue) {
            self.advance();
        }
        self.chars[start..self.index].iter().collect()
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }
    fn peek_next(&self) -> Option<char> {
        self.chars.get(self.index + 1).copied()
    }
    fn peek_n(&self, offset: usize) -> Option<char> {
        self.chars.get(self.index + offset).copied()
    }
    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.index += 1;
        self.byte_index += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(c)
    }
}

fn normalize_doc_block(value: &str) -> String {
    let lines = value
        .lines()
        .map(|line| {
            line.trim_start()
                .strip_prefix('*')
                .unwrap_or(line.trim_start())
                .trim_start()
                .trim_end()
        })
        .collect::<Vec<_>>();
    let start = lines.iter().position(|line| !line.is_empty()).unwrap_or(0);
    let end = lines
        .iter()
        .rposition(|line| !line.is_empty())
        .map_or(start, |index| index + 1);
    lines[start..end].join("\n")
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}
fn is_ident_continue(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit() || c == '?'
}
fn tok(kind: TokenKind, line: usize, column: usize, range: Range<usize>) -> Token {
    Token {
        kind,
        line,
        column,
        range,
    }
}
