//! Tokenize a template string and build its [`Node`] tree.
//!
//! Markers: `{{ }}` / `{{{ }}}` (interpolation), `{% %}` (tags), `{# #}`
//! (comments). Everything else is literal text.

use crate::ast::{Node, Path};
use crate::error::Error;

/// Parse a template source into its AST.
pub fn parse(source: &str) -> Result<Vec<Node>, Error> {
    let tokens = tokenize(source)?;
    let mut p = TokenStream { tokens, pos: 0 };
    let (nodes, term) = p.block()?;
    match term {
        Term::Eof => Ok(nodes),
        Term::Else => Err(Error::new("'else' without matching 'if'")),
        Term::EndIf => Err(Error::new("'endif' without matching 'if'")),
        Term::EndFor => Err(Error::new("'endfor' without matching 'for'")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Text(String),
    Var {
        path: Path,
        escape: bool,
    },
    Translate {
        key: String,
        args: Vec<(String, Path)>,
        escape: bool,
    },
    If(Path),
    Else,
    EndIf,
    For {
        var: String,
        path: Path,
    },
    EndFor,
    Include(String),
}

fn tokenize(source: &str) -> Result<Vec<Token>, Error> {
    let mut tokens = Vec::new();
    let mut rest = source;

    while !rest.is_empty() {
        // Find the earliest opening marker.
        let next = ["{{", "{%", "{#"]
            .iter()
            .filter_map(|m| rest.find(m).map(|i| (i, *m)))
            .min_by_key(|(i, _)| *i);

        let (idx, marker) = match next {
            Some(found) => found,
            None => {
                tokens.push(Token::Text(rest.to_string()));
                break;
            }
        };

        if idx > 0 {
            tokens.push(Token::Text(rest[..idx].to_string()));
        }
        rest = &rest[idx..];

        match marker {
            "{{" => {
                let triple = rest.starts_with("{{{");
                let (open, close): (usize, &str) = if triple { (3, "}}}") } else { (2, "}}") };
                let end = rest
                    .find(close)
                    .ok_or_else(|| Error::new("unclosed '{{'"))?;
                let inner = rest[open..end].trim();
                let escape = !triple;

                // Detect the `t "key" …` translate helper.
                // The distinguishing mark is the quoted string immediately after `t `.
                let tok = if inner.starts_with("t \"") {
                    let (key, args) = parse_translate(inner)?;
                    Token::Translate { key, args, escape }
                } else {
                    Token::Var {
                        path: parse_path(inner),
                        escape,
                    }
                };
                tokens.push(tok);
                rest = &rest[end + close.len()..];
            }
            "{%" => {
                let end = rest.find("%}").ok_or_else(|| Error::new("unclosed '{%'"))?;
                tokens.push(parse_tag(rest[2..end].trim())?);
                rest = &rest[end + 2..];
            }
            "{#" => {
                let end = rest.find("#}").ok_or_else(|| Error::new("unclosed '{#'"))?;
                rest = &rest[end + 2..];
            }
            _ => unreachable!(),
        }
    }

    Ok(tokens)
}

/// Parse the content of a `{{ t "key" … }}` expression.
///
/// `inner` is already trimmed and starts with `t "`.
/// Returns `(key, args)` where `args` is a list of `(name, path)` pairs.
fn parse_translate(inner: &str) -> Result<(String, Vec<(String, Path)>), Error> {
    // Skip `t ` — exactly two characters.
    let rest = &inner[2..]; // `"key" …` or `"key"`

    // Parse the quoted key.
    if !rest.starts_with('"') {
        return Err(Error::new(
            "t helper: key must be a quoted string, e.g. {{ t \"my.key\" }}",
        ));
    }
    let rest = &rest[1..]; // skip opening quote
    let key_end = rest
        .find('"')
        .ok_or_else(|| Error::new("t helper: unclosed quote in key"))?;
    let key = rest[..key_end].to_string();
    let rest = rest[key_end + 1..].trim_start();

    // Parse optional `name=dotted.path` argument pairs.
    let mut args: Vec<(String, Path)> = Vec::new();
    for pair in rest.split_whitespace() {
        let eq = pair.find('=').ok_or_else(|| {
            Error::new(format!(
                "t helper: '{pair}' is not a valid name=path argument"
            ))
        })?;
        let name = pair[..eq].to_string();
        let path_str = &pair[eq + 1..];
        if path_str.is_empty() {
            return Err(Error::new(format!(
                "t helper: argument '{name}=' has an empty path"
            )));
        }
        args.push((name, parse_path(path_str)));
    }

    Ok((key, args))
}

fn parse_tag(inner: &str) -> Result<Token, Error> {
    if inner == "else" {
        return Ok(Token::Else);
    }
    if inner == "endif" {
        return Ok(Token::EndIf);
    }
    if inner == "endfor" {
        return Ok(Token::EndFor);
    }
    if let Some(cond) = inner.strip_prefix("if ") {
        return Ok(Token::If(parse_path(cond.trim())));
    }
    if let Some(rest) = inner.strip_prefix("for ") {
        // `for <var> in <path>`
        let mut parts = rest.split_whitespace();
        let var = parts
            .next()
            .ok_or_else(|| Error::new("'for' needs a variable"))?;
        match parts.next() {
            Some("in") => {}
            _ => return Err(Error::new("'for' must be 'for <var> in <path>'")),
        }
        let path = parts
            .next()
            .ok_or_else(|| Error::new("'for' needs an iterable"))?;
        return Ok(Token::For {
            var: var.to_string(),
            path: parse_path(path),
        });
    }
    if let Some(rest) = inner.strip_prefix("include ") {
        let name = rest.trim().trim_matches('"');
        return Ok(Token::Include(name.to_string()));
    }
    Err(Error::new(format!("unknown tag '{{% {inner} %}}'")))
}

fn parse_path(s: &str) -> Path {
    s.split('.')
        .map(str::trim)
        .filter(|seg| !seg.is_empty())
        .map(String::from)
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum Term {
    Eof,
    Else,
    EndIf,
    EndFor,
}

struct TokenStream {
    tokens: Vec<Token>,
    pos: usize,
}

impl TokenStream {
    /// Parse a sequence of nodes until a block terminator or EOF.
    fn block(&mut self) -> Result<(Vec<Node>, Term), Error> {
        let mut nodes = Vec::new();
        loop {
            let token = match self.tokens.get(self.pos) {
                None => return Ok((nodes, Term::Eof)),
                Some(t) => t.clone(),
            };
            self.pos += 1;

            match token {
                Token::Text(t) => nodes.push(Node::Text(t)),
                Token::Var { path, escape } => nodes.push(Node::Var { path, escape }),
                Token::Translate { key, args, escape } => {
                    nodes.push(Node::Translate { key, args, escape })
                }
                Token::Include(name) => nodes.push(Node::Include(name)),
                Token::Else => return Ok((nodes, Term::Else)),
                Token::EndIf => return Ok((nodes, Term::EndIf)),
                Token::EndFor => return Ok((nodes, Term::EndFor)),
                Token::If(cond) => {
                    let (body, term) = self.block()?;
                    let else_body = match term {
                        Term::EndIf => Vec::new(),
                        Term::Else => {
                            let (eb, t2) = self.block()?;
                            if t2 != Term::EndIf {
                                return Err(Error::new("unclosed 'if' (expected 'endif')"));
                            }
                            eb
                        }
                        _ => return Err(Error::new("unclosed 'if' (expected 'endif')")),
                    };
                    nodes.push(Node::If {
                        cond,
                        body,
                        else_body,
                    });
                }
                Token::For { var, path } => {
                    let (body, term) = self.block()?;
                    if term != Term::EndFor {
                        return Err(Error::new("unclosed 'for' (expected 'endfor')"));
                    }
                    nodes.push(Node::For { var, path, body });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_and_vars() {
        let ast = parse("Hi {{ name }}!").unwrap();
        assert_eq!(
            ast,
            vec![
                Node::Text("Hi ".into()),
                Node::Var {
                    path: vec!["name".into()],
                    escape: true
                },
                Node::Text("!".into()),
            ]
        );
    }

    #[test]
    fn triple_brace_is_raw() {
        let ast = parse("{{{ html }}}").unwrap();
        assert_eq!(
            ast,
            vec![Node::Var {
                path: vec!["html".into()],
                escape: false
            }]
        );
    }

    #[test]
    fn parses_if_else() {
        let ast = parse("{% if ok %}Y{% else %}N{% endif %}").unwrap();
        assert_eq!(
            ast,
            vec![Node::If {
                cond: vec!["ok".into()],
                body: vec![Node::Text("Y".into())],
                else_body: vec![Node::Text("N".into())],
            }]
        );
    }

    #[test]
    fn parses_for() {
        let ast = parse("{% for x in items %}{{ x }}{% endfor %}").unwrap();
        assert_eq!(
            ast,
            vec![Node::For {
                var: "x".into(),
                path: vec!["items".into()],
                body: vec![Node::Var {
                    path: vec!["x".into()],
                    escape: true
                }],
            }]
        );
    }

    #[test]
    fn comments_are_dropped() {
        assert_eq!(
            parse("a{# hidden #}b").unwrap(),
            vec![Node::Text("a".into()), Node::Text("b".into())]
        );
    }

    #[test]
    fn dotted_path() {
        let ast = parse("{{ user.name }}").unwrap();
        assert_eq!(
            ast,
            vec![Node::Var {
                path: vec!["user".into(), "name".into()],
                escape: true
            }]
        );
    }

    #[test]
    fn rejects_unbalanced() {
        assert!(parse("{% if a %}x").is_err());
        assert!(parse("{% endfor %}").is_err());
        assert!(parse("{{ a").is_err());
        assert!(parse("{% bogus %}").is_err());
    }

    // ── t helper parse tests ──────────────────────────────────────────────────

    #[test]
    fn parses_t_helper_no_args() {
        let ast = parse(r#"{{ t "hello" }}"#).unwrap();
        assert_eq!(
            ast,
            vec![Node::Translate {
                key: "hello".into(),
                args: vec![],
                escape: true,
            }]
        );
    }

    #[test]
    fn parses_t_helper_with_arg() {
        let ast = parse(r#"{{ t "greeting" name=user.name }}"#).unwrap();
        assert_eq!(
            ast,
            vec![Node::Translate {
                key: "greeting".into(),
                args: vec![("name".into(), vec!["user".into(), "name".into()])],
                escape: true,
            }]
        );
    }

    #[test]
    fn parses_t_helper_with_multiple_args() {
        let ast = parse(r#"{{ t "msg" a=x.y b=z }}"#).unwrap();
        assert_eq!(
            ast,
            vec![Node::Translate {
                key: "msg".into(),
                args: vec![
                    ("a".into(), vec!["x".into(), "y".into()]),
                    ("b".into(), vec!["z".into()]),
                ],
                escape: true,
            }]
        );
    }

    #[test]
    fn parses_t_helper_triple_brace_raw() {
        let ast = parse(r#"{{{ t "raw.key" }}}"#).unwrap();
        assert_eq!(
            ast,
            vec![Node::Translate {
                key: "raw.key".into(),
                args: vec![],
                escape: false,
            }]
        );
    }

    #[test]
    fn var_named_t_without_quotes_is_still_a_var() {
        // `{{ t }}` — no quotes — must resolve path ["t"], not a translate.
        let ast = parse("{{ t }}").unwrap();
        assert_eq!(
            ast,
            vec![Node::Var {
                path: vec!["t".into()],
                escape: true,
            }]
        );
    }

    #[test]
    fn t_helper_bad_syntax_is_error() {
        assert!(parse(r#"{{ t "unclosed }}"#).is_err());
        assert!(parse(r#"{{ t "key" badarg }}"#).is_err());
        assert!(parse(r#"{{ t "key" name= }}"#).is_err());
    }
}
