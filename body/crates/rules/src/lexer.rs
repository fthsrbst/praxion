//! The line-based lexer.
//!
//! Tokens carry one-based line and column positions. `#` starts a comment that
//! runs to the end of the line. Strings may contain `\"`, `\\`, `\n`, `\t`,
//! `\r` and `\u{...}` escapes. Every line ends with a [`TokenKind::Newline`]
//! token so the parser can stay line-oriented, which is what "indentation does
//! not matter" means here.

use crate::ast::{Pos, Span};
use crate::error::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Ident(String),
    Int(i64),
    Str(String),
    /// `$name`.
    Dollar(String),
    /// `#name`: an item or block tag. `#` followed by anything else starts a
    /// comment.
    Tag(String),
    Arrow,
    LParen,
    RParen,
    Comma,
    Colon,
    Assign,
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
    Newline,
    Eof,
}

impl TokenKind {
    /// A short description for error messages.
    pub fn describe(&self) -> String {
        match self {
            Self::Ident(s) => format!("`{s}`"),
            Self::Int(n) => format!("`{n}`"),
            Self::Str(s) => format!("string {:?}", s),
            Self::Dollar(s) => format!("`${s}`"),
            Self::Tag(s) => format!("`#{s}`"),
            Self::Arrow => "`->`".into(),
            Self::LParen => "`(`".into(),
            Self::RParen => "`)`".into(),
            Self::Comma => "`,`".into(),
            Self::Colon => "`:`".into(),
            Self::Assign => "`=`".into(),
            Self::Lt => "`<`".into(),
            Self::Le => "`<=`".into(),
            Self::Eq => "`==`".into(),
            Self::Ne => "`!=`".into(),
            Self::Ge => "`>=`".into(),
            Self::Gt => "`>`".into(),
            Self::Newline => "end of line".into(),
            Self::Eof => "end of input".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Lexes one source file. Stops at the first lexical error.
pub fn lex(source: &str) -> Result<Vec<Token>, Error> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1u32;
    let mut col = 1u32;

    // Positions are one-based; `advance` moves one character.
    fn advance(i: &mut usize, col: &mut u32) {
        *i += 1;
        *col += 1;
    }

    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\r' => advance(&mut i, &mut col),
            '\n' => {
                let pos = Pos::new(line, col);
                out.push(Token {
                    kind: TokenKind::Newline,
                    span: Span::empty(pos),
                });
                i += 1;
                line += 1;
                col = 1;
            }
            '#' => {
                let start = Pos::new(line, col);
                advance(&mut i, &mut col);
                if i < chars.len() && is_ident_start(chars[i]) {
                    let name = read_ident(&chars, &mut i, &mut col);
                    out.push(Token {
                        kind: TokenKind::Tag(name),
                        span: Span::new(start, Pos::new(line, col)),
                    });
                } else {
                    while i < chars.len() && chars[i] != '\n' {
                        advance(&mut i, &mut col);
                    }
                }
            }
            '"' => {
                let start = Pos::new(line, col);
                advance(&mut i, &mut col);
                let mut value = String::new();
                loop {
                    if i >= chars.len() || chars[i] == '\n' {
                        return Err(Error::at("unterminated string", start));
                    }
                    let ch = chars[i];
                    if ch == '"' {
                        advance(&mut i, &mut col);
                        break;
                    }
                    if ch != '\\' {
                        value.push(ch);
                        advance(&mut i, &mut col);
                        continue;
                    }
                    // Escape sequence.
                    let esc_pos = Pos::new(line, col);
                    advance(&mut i, &mut col);
                    if i >= chars.len() {
                        return Err(Error::at("unterminated string", start));
                    }
                    let esc = chars[i];
                    match esc {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        'n' => value.push('\n'),
                        't' => value.push('\t'),
                        'r' => value.push('\r'),
                        'u' => {
                            advance(&mut i, &mut col);
                            if i >= chars.len() || chars[i] != '{' {
                                return Err(Error::at("expected `{` after `\\u`", esc_pos));
                            }
                            advance(&mut i, &mut col);
                            let mut hex = String::new();
                            while i < chars.len() && chars[i] != '}' {
                                hex.push(chars[i]);
                                advance(&mut i, &mut col);
                            }
                            if i >= chars.len() {
                                return Err(Error::at("unterminated `\\u{...}` escape", esc_pos));
                            }
                            advance(&mut i, &mut col); // `}`
                            let code = u32::from_str_radix(&hex, 16).map_err(|_| {
                                Error::at("invalid `\\u{...}` escape", esc_pos)
                            })?;
                            let decoded = char::from_u32(code)
                                .ok_or_else(|| Error::at("invalid `\\u{...}` escape", esc_pos))?;
                            value.push(decoded);
                            continue;
                        }
                        other => {
                            return Err(Error::at(format!("unknown escape `\\{other}`"), esc_pos));
                        }
                    }
                    advance(&mut i, &mut col);
                }
                let end = Pos::new(line, col);
                out.push(Token {
                    kind: TokenKind::Str(value),
                    span: Span::new(start, end),
                });
            }
            '$' => {
                let start = Pos::new(line, col);
                advance(&mut i, &mut col);
                let name = read_ident(&chars, &mut i, &mut col);
                if name.is_empty() {
                    return Err(Error::at("expected a parameter name after `$`", start));
                }
                out.push(Token {
                    kind: TokenKind::Dollar(name),
                    span: Span::new(start, Pos::new(line, col)),
                });
            }
            '(' | ')' | ',' | ':' | '=' | '<' | '>' | '!' | '-' => {
                let start = Pos::new(line, col);
                advance(&mut i, &mut col);
                let next = chars.get(i).copied();
                let kind = match (c, next) {
                    ('-', Some('>')) => {
                        advance(&mut i, &mut col);
                        TokenKind::Arrow
                    }
                    ('<', Some('=')) => {
                        advance(&mut i, &mut col);
                        TokenKind::Le
                    }
                    ('>', Some('=')) => {
                        advance(&mut i, &mut col);
                        TokenKind::Ge
                    }
                    ('=', Some('=')) => {
                        advance(&mut i, &mut col);
                        TokenKind::Eq
                    }
                    ('!', Some('=')) => {
                        advance(&mut i, &mut col);
                        TokenKind::Ne
                    }
                    ('!', _) => {
                        return Err(Error::at("unexpected `!`; comparison is `!=`", start));
                    }
                    ('-', Some(d)) if d.is_ascii_digit() => {
                        // A negative integer literal.
                        let mut text = String::from("-");
                        read_digits(&chars, &mut i, &mut col, &mut text);
                        let n = text
                            .parse::<i64>()
                            .map_err(|_| Error::at("integer literal out of range", start))?;
                        out.push(Token {
                            kind: TokenKind::Int(n),
                            span: Span::new(start, Pos::new(line, col)),
                        });
                        continue;
                    }
                    ('-', _) => {
                        return Err(Error::at("unexpected `-`; only `->` and negative integers are allowed", start));
                    }
                    ('(', _) => TokenKind::LParen,
                    (')', _) => TokenKind::RParen,
                    (',', _) => TokenKind::Comma,
                    (':', _) => TokenKind::Colon,
                    ('=', _) => TokenKind::Assign,
                    ('<', _) => TokenKind::Lt,
                    ('>', _) => TokenKind::Gt,
                    _ => unreachable!(),
                };
                out.push(Token {
                    kind,
                    span: Span::new(start, Pos::new(line, col)),
                });
            }
            c if c.is_ascii_digit() => {
                let start = Pos::new(line, col);
                let mut text = String::new();
                read_digits(&chars, &mut i, &mut col, &mut text);
                let n = text
                    .parse::<i64>()
                    .map_err(|_| Error::at("integer literal out of range", start))?;
                out.push(Token {
                    kind: TokenKind::Int(n),
                    span: Span::new(start, Pos::new(line, col)),
                });
            }
            c if is_ident_start(c) => {
                let start = Pos::new(line, col);
                let name = read_ident(&chars, &mut i, &mut col);
                out.push(Token {
                    kind: TokenKind::Ident(name),
                    span: Span::new(start, Pos::new(line, col)),
                });
            }
            other => {
                return Err(Error::at(
                    format!("unexpected character `{other}`"),
                    Pos::new(line, col),
                ));
            }
        }
    }

    out.push(Token {
        kind: TokenKind::Eof,
        span: Span::empty(Pos::new(line, col)),
    });
    Ok(out)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn read_ident(chars: &[char], i: &mut usize, col: &mut u32) -> String {
    let mut out = String::new();
    while *i < chars.len() && is_ident_continue(chars[*i]) {
        out.push(chars[*i]);
        *i += 1;
        *col += 1;
    }
    out
}

fn read_digits(chars: &[char], i: &mut usize, col: &mut u32, out: &mut String) {
    while *i < chars.len() && chars[*i].is_ascii_digit() {
        out.push(chars[*i]);
        *i += 1;
        *col += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        lex(source)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| *k != TokenKind::Newline && *k != TokenKind::Eof)
            .collect()
    }

    #[test]
    fn lexes_a_rule() {
        assert_eq!(
            kinds("count(x) >= 3 -> done\n"),
            vec![
                TokenKind::Ident("count".into()),
                TokenKind::LParen,
                TokenKind::Ident("x".into()),
                TokenKind::RParen,
                TokenKind::Ge,
                TokenKind::Int(3),
                TokenKind::Arrow,
                TokenKind::Ident("done".into()),
            ]
        );
    }

    #[test]
    fn comments_and_dollar() {
        assert_eq!(
            kinds("true -> mine(stone) # a comment\n"),
            vec![
                TokenKind::Ident("true".into()),
                TokenKind::Arrow,
                TokenKind::Ident("mine".into()),
                TokenKind::LParen,
                TokenKind::Ident("stone".into()),
                TokenKind::RParen,
            ]
        );
        assert_eq!(
            kinds("$n >= -2"),
            vec![TokenKind::Dollar("n".into()), TokenKind::Ge, TokenKind::Int(-2)]
        );
    }

    #[test]
    fn string_escapes() {
        assert_eq!(
            kinds(r#"doc "a\"b\n""#),
            vec![
                TokenKind::Ident("doc".into()),
                TokenKind::Str("a\"b\n".into())
            ]
        );
    }

    #[test]
    fn error_positions() {
        let err = lex("true -> done\n? ").unwrap_err();
        assert_eq!((err.line, err.col), (2, 1));
        let err = lex("x = \"oops").unwrap_err();
        assert_eq!(err.message, "unterminated string");
        assert_eq!((err.line, err.col), (1, 5));
    }
}
