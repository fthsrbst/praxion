//! The parser: one line is one construct; indentation is ignored.
//!
//! ```text
//! file       := skill*
//! skill      := 'skill' IDENT '(' params? ')' NEWLINE body
//! params     := param (',' param)*
//! param      := IDENT ':' TYPE ('=' literal)?
//! body       := ('doc' STRING NEWLINE)? 'rules:' NEWLINE rule*
//! rule       := expr '->' action ('until' expr)? ('max_ticks' INT)?
//! action     := 'done' | 'fail' '(' IDENT ')' | 'do' IDENT '(' args? ')' | IDENT '(' args? ')'
//! expr       := or
//! or         := and ('or' and)*
//! and        := not ('and' not)*
//! not        := 'not' not | cmp
//! cmp        := primary (cmpop primary)?
//! primary    := 'true' | 'false' | INT | '$' IDENT | IDENT '(' args? ')' | '(' expr ')'
//! ```
//!
//! A rule belongs to the preceding `skill` line. `minecraft:` may prefix an
//! identifier; it is removed, so `minecraft:stone` and `stone` are the same
//! name.

use crate::ast::{
    Action, ActionKind, ArgValue, CmpOp, Expr, ExprKind, Param, Pos, Rule, Skill, Span, ValueType,
};
use crate::error::Error;
use crate::lexer::{Token, TokenKind, lex};
use crate::normalize_id;

struct Line {
    tokens: Vec<Token>,
    /// Where the line ends, used when a token is missing at the end.
    end: Pos,
}

struct Cursor<'a> {
    line: &'a Line,
    idx: usize,
    last_end: Pos,
}

impl<'a> Cursor<'a> {
    fn new(line: &'a Line) -> Self {
        Self {
            line,
            idx: 0,
            last_end: line.end,
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.line.tokens.get(self.idx)
    }

    fn peek_kind(&self) -> Option<&TokenKind> {
        self.peek().map(|t| &t.kind)
    }

    fn peek2(&self) -> Option<&Token> {
        self.line.tokens.get(self.idx + 1)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.line.tokens.get(self.idx).cloned();
        if let Some(token) = &token {
            self.idx += 1;
            self.last_end = token.span.end;
        }
        token
    }

    fn expect_end(&self) -> Result<(), Error> {
        match self.peek() {
            None => Ok(()),
            Some(token) => Err(Error::at(
                format!("unexpected {} after the rule", token.kind.describe()),
                token.span.start,
            )),
        }
    }

    fn expect_kind(
        &mut self,
        want: &TokenKind,
        _what: &str,
        message: impl FnOnce() -> String,
    ) -> Result<Token, Error> {
        match self.peek() {
            Some(token) if same_kind(&token.kind, want) => {
                Ok(self.next().expect("peeked token exists"))
            }
            Some(token) => Err(Error::at(message(), token.span.start)),
            None => Err(Error::at(message(), self.line.end)),
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<Token, Error> {
        match self.peek() {
            Some(token) if matches!(token.kind, TokenKind::Ident(_)) => {
                Ok(self.next().expect("peeked token exists"))
            }
            Some(token) => Err(Error::at(
                format!("expected {what}, found {}", token.kind.describe()),
                token.span.start,
            )),
            None => Err(Error::at(format!("expected {what}"), self.line.end)),
        }
    }

    fn eat_kind(&mut self, want: &TokenKind) -> Option<Token> {
        match self.peek() {
            Some(token) if same_kind(&token.kind, want) => self.next(),
            _ => None,
        }
    }

    fn eat_ident(&mut self, name: &str) -> Option<Token> {
        match self.peek() {
            Some(token) if matches!(&token.kind, TokenKind::Ident(s) if s == name) => self.next(),
            _ => None,
        }
    }
}

fn same_kind(have: &TokenKind, want: &TokenKind) -> bool {
    std::mem::discriminant(have) == std::mem::discriminant(want)
}

fn ident_text(token: &Token) -> String {
    match &token.kind {
        TokenKind::Ident(s) => s.clone(),
        _ => unreachable!("caller checked the token kind"),
    }
}

/// Parses a whole source file. Stops at the first syntax error.
pub fn parse(source: &str) -> Result<Vec<Skill>, Error> {
    let tokens = lex(source)?;
    let lines = group_lines(tokens);
    let mut skills = Vec::new();
    let mut current: Option<Builder> = None;

    for line in &lines {
        let starts_skill = matches!(
            line.tokens.first().map(|t| &t.kind),
            Some(TokenKind::Ident(s)) if s == "skill"
        );
        if starts_skill {
            if let Some(builder) = current.take() {
                skills.push(builder.finish()?);
            }
            current = Some(parse_skill_header(line)?);
        } else {
            let Some(builder) = current.as_mut() else {
                return Err(Error::at(
                    "a rule or declaration outside a skill",
                    line.tokens[0].span.start,
                ));
            };
            parse_body_line(builder, line)?;
        }
    }
    if let Some(builder) = current {
        skills.push(builder.finish()?);
    }
    Ok(skills)
}

struct Builder {
    name: String,
    name_span: Span,
    params: Vec<Param>,
    doc: Option<String>,
    rules: Vec<Rule>,
    saw_rules: bool,
}

impl Builder {
    fn finish(self) -> Result<Skill, Error> {
        if !self.saw_rules {
            return Err(Error::at(
                format!("skill `{}` has no `rules:` section", self.name),
                self.name_span.start,
            ));
        }
        if self.rules.is_empty() {
            return Err(Error::at(
                format!("skill `{}` has no rules", self.name),
                self.name_span.start,
            ));
        }
        Ok(Skill {
            name: self.name,
            params: self.params,
            doc: self.doc,
            rules: self.rules,
            span: self.name_span,
            file: None,
        })
    }
}

fn group_lines(tokens: Vec<Token>) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut current: Vec<Token> = Vec::new();
    for token in tokens {
        match token.kind {
            TokenKind::Newline => {
                if !current.is_empty() {
                    lines.push(Line {
                        tokens: std::mem::take(&mut current),
                        end: token.span.start,
                    });
                }
            }
            TokenKind::Eof => {
                if !current.is_empty() {
                    lines.push(Line {
                        tokens: std::mem::take(&mut current),
                        end: token.span.start,
                    });
                }
            }
            _ => current.push(token),
        }
    }
    lines
}

fn parse_skill_header(line: &Line) -> Result<Builder, Error> {
    let mut c = Cursor::new(line);
    c.expect_ident("`skill`")?;
    let name_token = c.expect_ident("a skill name")?;
    let name = ident_text(&name_token);
    c.expect_kind(&TokenKind::LParen, "`(`", || {
        format!("expected `(` after the skill name `{name}`")
    })?;

    let mut params = Vec::new();
    if c.peek_kind() != Some(&TokenKind::RParen) {
        loop {
            let param_token = c.expect_ident("a parameter name")?;
            let param_name = ident_text(&param_token);
            c.expect_kind(&TokenKind::Colon, "`:`", || {
                format!("expected `:` after the parameter name `{param_name}`")
            })?;
            let type_token = c.expect_ident("a parameter type")?;
            let type_name = ident_text(&type_token);
            let ty = ValueType::parse(&type_name).ok_or_else(|| {
                Error::at(
                    format!(
                        "unknown type `{type_name}`; expected int, item, block, entity_kind or bool"
                    ),
                    type_token.span.start,
                )
            })?;
            let default = if c.eat_kind(&TokenKind::Assign).is_some() {
                Some(parse_literal(&mut c, ty)?)
            } else {
                None
            };
            params.push(Param {
                name: param_name,
                ty,
                default,
                span: Span::new(param_token.span.start, type_token.span.end),
            });
            if c.eat_kind(&TokenKind::Comma).is_some() {
                continue;
            }
            break;
        }
    }
    c.expect_kind(&TokenKind::RParen, "`)`", || {
        "expected `)` after the parameter list".into()
    })?;
    c.expect_end()?;
    Ok(Builder {
        name,
        name_span: name_token.span,
        params,
        doc: None,
        rules: Vec::new(),
        saw_rules: false,
    })
}

fn parse_literal(c: &mut Cursor<'_>, ty: ValueType) -> Result<ArgValue, Error> {
    match ty {
        ValueType::Int => {
            let token = c.expect_kind(&TokenKind::Int(0), "an int literal", || {
                "expected an int default".into()
            })?;
            match token.kind {
                TokenKind::Int(n) => Ok(ArgValue::Int(n)),
                _ => unreachable!(),
            }
        }
        ValueType::Bool => {
            let token = c.expect_ident("`true` or `false`")?;
            match ident_text(&token).as_str() {
                "true" => Ok(ArgValue::Bool(true)),
                "false" => Ok(ArgValue::Bool(false)),
                other => Err(Error::at(
                    format!("expected `true` or `false`, found `{other}`"),
                    token.span.start,
                )),
            }
        }
        ValueType::Item | ValueType::Block | ValueType::EntityKind => {
            let Some(token) = c.next() else {
                return Err(Error::at("expected an identifier", c.line.end));
            };
            let name = match &token.kind {
                TokenKind::Ident(_) => parse_name(c, token.clone())?,
                TokenKind::Tag(tag) => format!("#{tag}"),
                other => {
                    return Err(Error::at(
                        format!("expected an identifier, found {}", other.describe()),
                        token.span.start,
                    ));
                }
            };
            Ok(match ty {
                ValueType::Item => ArgValue::Item(name),
                ValueType::Block => ArgValue::Block(name),
                ValueType::EntityKind => ArgValue::EntityKind(name),
                _ => unreachable!(),
            })
        }
    }
}

fn parse_body_line(builder: &mut Builder, line: &Line) -> Result<(), Error> {
    let first = &line.tokens[0];
    let is_doc = matches!(&first.kind, TokenKind::Ident(s) if s == "doc");
    let is_rules = matches!(&first.kind, TokenKind::Ident(s) if s == "rules")
        && matches!(line.tokens.get(1).map(|t| &t.kind), Some(TokenKind::Colon));

    if is_doc {
        if builder.saw_rules {
            return Err(Error::at(
                "`doc` must come before `rules:`",
                first.span.start,
            ));
        }
        if builder.doc.is_some() {
            return Err(Error::at("duplicate `doc`", first.span.start));
        }
        let mut c = Cursor::new(line);
        c.expect_ident("`doc`")?;
        let token = c.expect_kind(&TokenKind::Str(String::new()), "a string", || {
            "expected a string after `doc`".into()
        })?;
        let text = match token.kind {
            TokenKind::Str(s) => s,
            _ => unreachable!(),
        };
        c.expect_end()?;
        builder.doc = Some(text);
        return Ok(());
    }

    if is_rules {
        if builder.saw_rules {
            return Err(Error::at("duplicate `rules:` section", first.span.start));
        }
        let mut c = Cursor::new(line);
        c.expect_ident("`rules`")?;
        c.expect_kind(&TokenKind::Colon, "`:`", || "expected `:` after `rules`".into())?;
        c.expect_end()?;
        builder.saw_rules = true;
        return Ok(());
    }

    if !builder.saw_rules {
        return Err(Error::at(
            "rule before `rules:`",
            first.span.start,
        ));
    }
    builder.rules.push(parse_rule(line)?);
    Ok(())
}

fn parse_rule(line: &Line) -> Result<Rule, Error> {
    let mut c = Cursor::new(line);
    let start = line.tokens[0].span.start;
    let guard = parse_or(&mut c)?;
    let found = c
        .peek()
        .map(|t| t.kind.describe())
        .unwrap_or_else(|| "end of line".into());
    c.expect_kind(&TokenKind::Arrow, "`->`", || {
        format!("expected `->` after the guard, found {found}")
    })?;
    let action = parse_action(&mut c)?;

    let mut until = None;
    let mut max_ticks = None;
    if c.eat_ident("until").is_some() {
        until = Some(parse_or(&mut c)?);
    }
    if c.eat_ident("max_ticks").is_some() {
        let value = c.expect_kind(&TokenKind::Int(0), "an int", || {
            "expected an int after `max_ticks`".into()
        })?;
        let n = match value.kind {
            TokenKind::Int(n) => n,
            _ => unreachable!(),
        };
        if n <= 0 || n > u32::MAX as i64 {
            return Err(Error::at(
                "`max_ticks` must be a positive int",
                value.span.start,
            ));
        }
        max_ticks = Some(n as u32);
    }
    c.expect_end()?;
    Ok(Rule {
        guard,
        action,
        until,
        max_ticks,
        span: Span::new(start, c.last_end),
    })
}

fn parse_or(c: &mut Cursor<'_>) -> Result<Expr, Error> {
    let mut left = parse_and(c)?;
    while c.eat_ident("or").is_some() {
        let right = parse_and(c)?;
        let span = Span::new(left.span.start, right.span.end);
        left = Expr::new(ExprKind::Or(Box::new(left), Box::new(right)), span);
    }
    Ok(left)
}

fn parse_and(c: &mut Cursor<'_>) -> Result<Expr, Error> {
    let mut left = parse_not(c)?;
    while c.eat_ident("and").is_some() {
        let right = parse_not(c)?;
        let span = Span::new(left.span.start, right.span.end);
        left = Expr::new(ExprKind::And(Box::new(left), Box::new(right)), span);
    }
    Ok(left)
}

fn parse_not(c: &mut Cursor<'_>) -> Result<Expr, Error> {
    if let Some(token) = c.eat_ident("not") {
        let inner = parse_not(c)?;
        let span = Span::new(token.span.start, inner.span.end);
        return Ok(Expr::new(ExprKind::Not(Box::new(inner)), span));
    }
    parse_cmp(c)
}

fn parse_cmp(c: &mut Cursor<'_>) -> Result<Expr, Error> {
    let left = parse_primary(c)?;
    let op = match c.peek_kind() {
        Some(TokenKind::Lt) => Some(CmpOp::Lt),
        Some(TokenKind::Le) => Some(CmpOp::Le),
        Some(TokenKind::Eq) => Some(CmpOp::Eq),
        Some(TokenKind::Ne) => Some(CmpOp::Ne),
        Some(TokenKind::Ge) => Some(CmpOp::Ge),
        Some(TokenKind::Gt) => Some(CmpOp::Gt),
        _ => None,
    };
    if let Some(op) = op {
        c.next();
        let right = parse_primary(c)?;
        let span = Span::new(left.span.start, right.span.end);
        return Ok(Expr::new(
            ExprKind::Cmp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            },
            span,
        ));
    }
    Ok(left)
}

fn parse_primary(c: &mut Cursor<'_>) -> Result<Expr, Error> {
    let Some(token) = c.next() else {
        return Err(Error::at("expected a guard expression", c.line.end));
    };
    match &token.kind {
        TokenKind::Int(n) => Ok(Expr::new(ExprKind::Int(*n), token.span)),
        TokenKind::Dollar(name) => Ok(Expr::new(ExprKind::Param(name.clone()), token.span)),
        TokenKind::Tag(name) => Ok(Expr::new(
            ExprKind::Name(format!("#{name}")),
            token.span,
        )),
        TokenKind::LParen => {
            let inner = parse_or(c)?;
            c.expect_kind(&TokenKind::RParen, "`)`", || {
                "expected `)` to close the group".into()
            })?;
            Ok(inner)
        }
        TokenKind::Ident(name) => {
            let name = name.clone();
            match name.as_str() {
                "true" => Ok(Expr::new(ExprKind::Bool(true), token.span)),
                "false" => Ok(Expr::new(ExprKind::Bool(false), token.span)),
                _ => {
                    if c.peek_kind() == Some(&TokenKind::LParen) {
                        c.next();
                        let args = parse_args(c)?;
                        let close = c.expect_kind(&TokenKind::RParen, "`)`", || {
                            format!("expected `)` after the arguments of `{name}`")
                        })?;
                        Ok(Expr::new(
                            ExprKind::Call {
                                name: normalize_id(&name),
                                args,
                            },
                            Span::new(token.span.start, close.span.end),
                        ))
                    } else {
                        let name = parse_name(c, token.clone())?;
                        Ok(Expr::new(ExprKind::Name(name), token.span))
                    }
                }
            }
        }
        other => Err(Error::at(
            format!("expected a guard expression, found {}", other.describe()),
            token.span.start,
        )),
    }
}

fn parse_args(c: &mut Cursor<'_>) -> Result<Vec<Expr>, Error> {
    let mut args = Vec::new();
    if c.peek_kind() == Some(&TokenKind::RParen) {
        return Ok(args);
    }
    loop {
        args.push(parse_arg(c)?);
        if c.eat_kind(&TokenKind::Comma).is_some() {
            continue;
        }
        break;
    }
    Ok(args)
}

fn parse_arg(c: &mut Cursor<'_>) -> Result<Expr, Error> {
    let Some(token) = c.next() else {
        return Err(Error::at("expected an argument", c.line.end));
    };
    match &token.kind {
        TokenKind::Int(n) => Ok(Expr::new(ExprKind::Int(*n), token.span)),
        TokenKind::Dollar(name) => Ok(Expr::new(ExprKind::Param(name.clone()), token.span)),
        TokenKind::Tag(name) => Ok(Expr::new(
            ExprKind::Name(format!("#{name}")),
            token.span,
        )),
        TokenKind::Ident(name) => {
            let name = name.clone();
            match name.as_str() {
                "true" => Ok(Expr::new(ExprKind::Bool(true), token.span)),
                "false" => Ok(Expr::new(ExprKind::Bool(false), token.span)),
                _ => {
                    if c.peek_kind() == Some(&TokenKind::LParen) {
                        c.next();
                        let args = parse_args(c)?;
                        let close = c.expect_kind(&TokenKind::RParen, "`)`", || {
                            format!("expected `)` after the arguments of `{name}`")
                        })?;
                        Ok(Expr::new(
                            ExprKind::Call {
                                name: normalize_id(&name),
                                args,
                            },
                            Span::new(token.span.start, close.span.end),
                        ))
                    } else {
                        let name = parse_name(c, token.clone())?;
                        Ok(Expr::new(ExprKind::Name(name), token.span))
                    }
                }
            }
        }
        other => Err(Error::at(
            format!(
                "expected an argument (int literal, identifier or $parameter), found {}",
                other.describe()
            ),
            token.span.start,
        )),
    }
}

/// Reads an identifier, merging an adjacent `namespace:path` pair.
fn parse_name(c: &mut Cursor<'_>, token: Token) -> Result<String, Error> {
    let mut name = ident_text(&token);
    let adjacent = match (c.peek(), c.peek2()) {
        (Some(colon), Some(next)) => {
            matches!(colon.kind, TokenKind::Colon)
                && colon.span.start.line == token.span.end.line
                && colon.span.start.col == token.span.end.col
                && next.span.start.line == colon.span.end.line
                && next.span.start.col == colon.span.end.col
                && matches!(next.kind, TokenKind::Ident(_))
        }
        _ => false,
    };
    if adjacent {
        c.next();
        let next = c.next().expect("peeked token exists");
        name = format!("{name}:{}", ident_text(&next));
    }
    Ok(normalize_id(&name))
}

fn parse_action(c: &mut Cursor<'_>) -> Result<Action, Error> {
    let Some(token) = c.next() else {
        return Err(Error::at("expected an action", c.line.end));
    };
    let TokenKind::Ident(name) = token.kind.clone() else {
        return Err(Error::at(
            format!("expected an action, found {}", token.kind.describe()),
            token.span.start,
        ));
    };
    match name.as_str() {
        "done" => Ok(Action::new(ActionKind::Done, token.span)),
        "fail" => {
            c.expect_kind(&TokenKind::LParen, "`(`", || "expected `(` after `fail`".into())?;
            let kind = c.expect_ident("a failure kind")?;
            let close = c.expect_kind(&TokenKind::RParen, "`)`", || {
                "expected `)` after the failure kind".into()
            })?;
            Ok(Action::new(
                ActionKind::Fail(ident_text(&kind)),
                Span::new(token.span.start, close.span.end),
            ))
        }
        "do" => {
            let skill = c.expect_ident("a skill name after `do`")?;
            c.expect_kind(&TokenKind::LParen, "`(`", || {
                "expected `(` after the skill name".into()
            })?;
            let args = parse_args(c)?;
            let close = c.expect_kind(&TokenKind::RParen, "`)`", || {
                "expected `)` after the skill arguments".into()
            })?;
            Ok(Action::new(
                ActionKind::Do {
                    skill: ident_text(&skill),
                    args,
                },
                Span::new(token.span.start, close.span.end),
            ))
        }
        _ => {
            c.expect_kind(&TokenKind::LParen, "`(`", || {
                format!("expected `(` after the primitive name `{name}`")
            })?;
            let args = parse_args(c)?;
            let close = c.expect_kind(&TokenKind::RParen, "`)`", || {
                "expected `)` after the arguments".into()
            })?;
            Ok(Action::new(
                ActionKind::Primitive {
                    name: normalize_id(&name),
                    args,
                },
                Span::new(token.span.start, close.span.end),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_construct() {
        let source = r#"
# a comment
skill demo(a: int = 3, b: item, c: block = minecraft:stone, d: entity_kind, e: bool = true)
  doc "hello \"world\""
  rules:
    true -> done
"#;
        let skills = parse(source).unwrap();
        assert_eq!(skills.len(), 1);
        let skill = &skills[0];
        assert_eq!(skill.name, "demo");
        assert_eq!(skill.params.len(), 5);
        assert_eq!(skill.params[0].default, Some(ArgValue::Int(3)));
        assert_eq!(skill.params[2].default, Some(ArgValue::Block("stone".into())));
        assert_eq!(skill.params[4].default, Some(ArgValue::Bool(true)));
        assert_eq!(skill.doc.as_deref(), Some("hello \"world\""));
        assert_eq!(skill.rules.len(), 1);
    }

    #[test]
    fn parses_full_rule() {
        let source = r#"
skill s()
  rules:
    count(cobblestone) >= 3 and (near(table, 4) or not has(x)) -> mine(stone) until count(cobblestone) < 3 max_ticks 40
    true -> do other(1, $n, stone)
"#;
        // `$n` is unknown here; parsing does not check it.
        let skills = parse(source).unwrap();
        let rules = &skills[0].rules;
        assert!(rules[0].until.is_some());
        assert_eq!(rules[0].max_ticks, Some(40));
        assert!(matches!(rules[1].action.kind, ActionKind::Do { .. }));
    }

    #[test]
    fn indentation_does_not_matter() {
        let a = parse("skill s()\nrules:\ntrue -> done\n").unwrap();
        let b = parse("  skill s()\n      rules:\n  true -> done\n").unwrap();
        assert_eq!(a, b);
    }
}
