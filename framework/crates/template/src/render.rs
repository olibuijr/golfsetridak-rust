//! Render an AST against a JSON [`Value`] context into an HTML string.

use crate::ast::{Node, Path};
use crate::error::Error;
use akurai_json::Value;
use std::collections::HashMap;

/// Carries the registered templates (for `include`) through the recursion.
pub(crate) struct Renderer<'a> {
    pub templates: &'a HashMap<String, Vec<Node>>,
}

/// A loop variable binding. The newest scope wins on name collisions.
type Scope = Vec<(String, Value)>;

impl Renderer<'_> {
    pub fn render(&self, nodes: &[Node], root: &Value, out: &mut String) -> Result<(), Error> {
        let mut scopes: Scope = Vec::new();
        self.render_nodes(nodes, root, &mut scopes, out)
    }

    fn render_nodes(
        &self,
        nodes: &[Node],
        root: &Value,
        scopes: &mut Scope,
        out: &mut String,
    ) -> Result<(), Error> {
        for node in nodes {
            match node {
                Node::Text(t) => out.push_str(t),
                Node::Var { path, escape } => {
                    if let Some(v) = resolve(path, scopes, root) {
                        let s = stringify(&v);
                        if *escape {
                            push_escaped(&s, out);
                        } else {
                            out.push_str(&s);
                        }
                    }
                }
                Node::If {
                    cond,
                    body,
                    else_body,
                } => {
                    let truthy = resolve(cond, scopes, root)
                        .as_ref()
                        .map(is_truthy)
                        .unwrap_or(false);
                    let branch = if truthy { body } else { else_body };
                    self.render_nodes(branch, root, scopes, out)?;
                }
                Node::For { var, path, body } => {
                    if let Some(Value::Array(items)) = resolve(path, scopes, root) {
                        for item in items {
                            scopes.push((var.clone(), item));
                            self.render_nodes(body, root, scopes, out)?;
                            scopes.pop();
                        }
                    }
                }
                Node::Include(name) => {
                    let included = self
                        .templates
                        .get(name)
                        .ok_or_else(|| Error::new(format!("unknown template '{name}'")))?;
                    // Includes share the root context and current loop scopes.
                    self.render_nodes(included, root, scopes, out)?;
                }
                Node::Translate { key, args, escape } => {
                    // Look up __messages.<key> from the root context.
                    // __messages is an Object of key → translated-string pairs,
                    // injected by the framework from the active locale catalog.
                    let translated = root
                        .get("__messages")
                        .and_then(|msgs| msgs.get(key.as_str()))
                        .and_then(|v| v.as_str())
                        .map(|template| {
                            if args.is_empty() {
                                template.to_string()
                            } else {
                                // Replace {placeholder} with resolved context values.
                                let mut text = template.to_string();
                                for (name, path) in args {
                                    if let Some(val) = resolve(path, scopes, root) {
                                        text =
                                            text.replace(&format!("{{{name}}}"), &stringify(&val));
                                    }
                                }
                                text
                            }
                        })
                        // Fall back to the key itself — never panic, never empty.
                        .unwrap_or_else(|| key.clone());

                    if *escape {
                        push_escaped(&translated, out);
                    } else {
                        out.push_str(&translated);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Resolve a dotted path: first segment from the loop scopes (newest first),
/// else from the root object; remaining segments index into objects.
fn resolve(path: &Path, scopes: &Scope, root: &Value) -> Option<Value> {
    let first = path.first()?;
    let mut cur = scopes
        .iter()
        .rev()
        .find(|(name, _)| name == first)
        .map(|(_, v)| v.clone())
        .or_else(|| root.get(first).cloned())?;
    for seg in &path[1..] {
        cur = cur.get(seg)?.clone();
    }
    Some(cur)
}

/// How a value appears when interpolated.
fn stringify(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        // Arrays/objects fall back to JSON — usually a template bug, but visible.
        other => other.to_json(),
    }
}

/// Template truthiness: null/false/0/empty are falsy.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(n) => *n != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Escape the five HTML-significant characters.
fn push_escaped(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}
