//! AkurAI-Framework template engine — pure `std` (+ internal `akurai-json`).
//!
//! A small server-side HTML templating language that renders against a JSON
//! [`Value`] context — the framework's SSR/"dynamic content" layer.
//!
//! ```
//! use akurai_template::Engine;
//! use akurai_json::Value;
//!
//! let mut engine = Engine::new();
//! engine.register("hi", "Hello, {{ name }}!").unwrap();
//! let ctx = Value::Object(vec![("name".into(), Value::Str("Óli".into()))]);
//! assert_eq!(engine.render("hi", &ctx).unwrap(), "Hello, Óli!");
//! ```
//!
//! ## Syntax
//!
//! | Syntax | Meaning |
//! |--------|---------|
//! | `{{ x }}` | HTML-escaped variable |
//! | `{{{ x }}}` | Raw (unescaped) variable |
//! | `{% if x %}…{% else %}…{% endif %}` | Conditional |
//! | `{% for v in xs %}…{% endfor %}` | Loop over array |
//! | `{% include "name" %}` | Inline another template |
//! | `{# comment #}` | Comment (stripped) |
//! | `{{ t "key" }}` | i18n translation lookup |
//! | `{{ t "key" name=user.name }}` | Translation with placeholder substitution |
//!
//! Paths are dotted (`user.name`); loop variables shadow root keys.
//!
//! ## i18n — the `t` helper
//!
//! `{{ t "message.key" }}` looks up `__messages["message.key"]` in the render
//! context and emits the translated string (HTML-escaped).  The caller is
//! responsible for populating `__locale` (the active locale code, a `Str`) and
//! `__messages` (an `Object` of key → string) in the context before calling
//! [`Engine::render`].
//!
//! Fallback: if the key is not found in `__messages`, the key itself is emitted
//! — always safe, never panics.
//!
//! Named placeholders (`{name}`) in the translated string are filled by
//! resolving the matching `name=path` argument from the context:
//!
//! ```text
//! {{ t "greeting" name=user.name }}
//! ```
//!
//! See `site/content/docs/i18n.md` for the full catalog format and wiring guide.

#![forbid(unsafe_code)]

mod ast;
mod error;
mod parse;
mod render;

pub use ast::Node;
pub use error::Error;

use akurai_json::Value;
use render::Renderer;
use std::collections::HashMap;

/// Holds parsed, named templates and renders them. Names let templates
/// `{% include %}` one another (e.g. a shared header/footer).
#[derive(Default)]
pub struct Engine {
    templates: HashMap<String, Vec<Node>>,
}

impl Engine {
    pub fn new() -> Engine {
        Engine::default()
    }

    /// Parse `source` and store it under `name`. Returns a parse error without
    /// mutating the engine.
    pub fn register(&mut self, name: &str, source: &str) -> Result<(), Error> {
        let nodes = parse::parse(source)?;
        self.templates.insert(name.to_string(), nodes);
        Ok(())
    }

    /// Render a registered template against `context`.
    pub fn render(&self, name: &str, context: &Value) -> Result<String, Error> {
        let nodes = self
            .templates
            .get(name)
            .ok_or_else(|| Error::new(format!("unknown template '{name}'")))?;
        let mut out = String::new();
        Renderer {
            templates: &self.templates,
        }
        .render(nodes, context, &mut out)?;
        Ok(out)
    }

    /// Render a one-off template string with no registration (no includes).
    pub fn render_str(source: &str, context: &Value) -> Result<String, Error> {
        let mut engine = Engine::new();
        engine.register("__inline__", source)?;
        engine.render("__inline__", context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(pairs: Vec<(&str, Value)>) -> Value {
        Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    #[test]
    fn interpolates_and_escapes() {
        let ctx = obj(vec![("x", Value::Str("<b>&\"hi\"".into()))]);
        assert_eq!(
            Engine::render_str("[{{ x }}]", &ctx).unwrap(),
            "[&lt;b&gt;&amp;&quot;hi&quot;]"
        );
    }

    #[test]
    fn raw_does_not_escape() {
        let ctx = obj(vec![("x", Value::Str("<b>".into()))]);
        assert_eq!(Engine::render_str("{{{ x }}}", &ctx).unwrap(), "<b>");
    }

    #[test]
    fn if_else_branches() {
        let t = "{% if on %}ON{% else %}OFF{% endif %}";
        assert_eq!(
            Engine::render_str(t, &obj(vec![("on", Value::Bool(true))])).unwrap(),
            "ON"
        );
        assert_eq!(
            Engine::render_str(t, &obj(vec![("on", Value::Bool(false))])).unwrap(),
            "OFF"
        );
        // missing key is falsy
        assert_eq!(Engine::render_str(t, &obj(vec![])).unwrap(), "OFF");
    }

    #[test]
    fn loops_over_array_with_fields() {
        let ctx = obj(vec![(
            "items",
            Value::Array(vec![
                obj(vec![("name", Value::Str("a".into()))]),
                obj(vec![("name", Value::Str("b".into()))]),
            ]),
        )]);
        let out = Engine::render_str(
            "{% for it in items %}<li>{{ it.name }}</li>{% endfor %}",
            &ctx,
        )
        .unwrap();
        assert_eq!(out, "<li>a</li><li>b</li>");
    }

    #[test]
    fn includes_share_context() {
        let mut engine = Engine::new();
        engine.register("head", "<h1>{{ title }}</h1>").unwrap();
        engine
            .register("page", "{% include \"head\" %}<p>body</p>")
            .unwrap();
        let ctx = obj(vec![("title", Value::Str("Hi".into()))]);
        assert_eq!(
            engine.render("page", &ctx).unwrap(),
            "<h1>Hi</h1><p>body</p>"
        );
    }

    #[test]
    fn nested_loop_and_conditional() {
        let ctx = obj(vec![(
            "rows",
            Value::Array(vec![
                obj(vec![("v", Value::Int(1)), ("hot", Value::Bool(true))]),
                obj(vec![("v", Value::Int(2)), ("hot", Value::Bool(false))]),
            ]),
        )]);
        let t = "{% for r in rows %}{{ r.v }}{% if r.hot %}!{% endif %} {% endfor %}";
        assert_eq!(Engine::render_str(t, &ctx).unwrap(), "1! 2 ");
    }

    #[test]
    fn unknown_template_errors() {
        let engine = Engine::new();
        assert!(engine.render("nope", &Value::Null).is_err());
    }

    // ── i18n `t` helper render tests ─────────────────────────────────────────

    fn ctx_with_messages(locale: &str, pairs: Vec<(&str, &str)>) -> Value {
        let msgs = Value::Object(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), Value::Str(v.to_string())))
                .collect(),
        );
        obj(vec![
            ("__locale", Value::Str(locale.to_string())),
            ("__messages", msgs),
        ])
    }

    #[test]
    fn t_helper_translates_key() {
        let ctx = ctx_with_messages("is", vec![("hello", "Halló")]);
        assert_eq!(
            Engine::render_str(r#"{{ t "hello" }}"#, &ctx).unwrap(),
            "Halló"
        );
    }

    #[test]
    fn t_helper_missing_key_falls_back_to_key() {
        let ctx = ctx_with_messages("en", vec![]);
        assert_eq!(
            Engine::render_str(r#"{{ t "app.title" }}"#, &ctx).unwrap(),
            "app.title"
        );
    }

    #[test]
    fn t_helper_missing_messages_falls_back_to_key() {
        // No __messages at all in the context.
        assert_eq!(
            Engine::render_str(r#"{{ t "hello" }}"#, &obj(vec![])).unwrap(),
            "hello"
        );
    }

    #[test]
    fn t_helper_html_escapes_translation() {
        let ctx = ctx_with_messages("en", vec![("xss", "<script>alert(1)</script>")]);
        assert_eq!(
            Engine::render_str(r#"{{ t "xss" }}"#, &ctx).unwrap(),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
    }

    #[test]
    fn t_helper_raw_does_not_escape() {
        let ctx = ctx_with_messages("en", vec![("html", "<b>bold</b>")]);
        assert_eq!(
            Engine::render_str(r#"{{{ t "html" }}}"#, &ctx).unwrap(),
            "<b>bold</b>"
        );
    }

    #[test]
    fn t_helper_interpolates_arg_from_context() {
        let pairs = vec![
            ("__locale", Value::Str("en".into())),
            (
                "__messages",
                Value::Object(vec![(
                    "greeting".into(),
                    Value::Str("Hello, {name}!".into()),
                )]),
            ),
            (
                "user",
                Value::Object(vec![("name".into(), Value::Str("Óli".into()))]),
            ),
        ];
        let ctx = Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
        assert_eq!(
            Engine::render_str(r#"{{ t "greeting" name=user.name }}"#, &ctx).unwrap(),
            "Hello, Óli!"
        );
    }

    #[test]
    fn t_helper_unknown_placeholder_left_verbatim() {
        let ctx = ctx_with_messages("en", vec![("msg", "Hello, {name}!")]);
        // arg not supplied → placeholder stays as-is
        assert_eq!(
            Engine::render_str(r#"{{ t "msg" }}"#, &ctx).unwrap(),
            "Hello, {name}!"
        );
    }

    #[test]
    fn templates_without_t_are_byte_identical() {
        // Verify backward compatibility: templates not using `t` render identically.
        let ctx = obj(vec![
            ("title", Value::Str("AkurAI".into())),
            ("count", Value::Int(3)),
        ]);
        let src = "<h1>{{ title }}</h1><p>{{ count }} items</p>";
        assert_eq!(
            Engine::render_str(src, &ctx).unwrap(),
            "<h1>AkurAI</h1><p>3 items</p>"
        );
    }
}
