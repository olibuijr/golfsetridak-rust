//! The template AST: what a parsed template is made of.

/// A dotted lookup path into the context, e.g. `user.name` → `["user","name"]`.
pub type Path = Vec<String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Literal text, emitted verbatim.
    Text(String),
    /// `{{ path }}` (HTML-escaped) or `{{{ path }}}` (raw, `escape = false`).
    Var { path: Path, escape: bool },
    /// `{% if path %} body {% else %} else_body {% endif %}`.
    If {
        cond: Path,
        body: Vec<Node>,
        else_body: Vec<Node>,
    },
    /// `{% for var in path %} body {% endfor %}` over an array.
    For {
        var: String,
        path: Path,
        body: Vec<Node>,
    },
    /// `{% include "name" %}` — renders another registered template inline.
    Include(String),
    /// `{{ t "key" }}` or `{{ t "key" name=path … }}` — i18n translation lookup.
    ///
    /// The renderer reads `__messages.<key>` from the root context, replaces
    /// `{name}` placeholders by resolving each `(name, path)` arg, then emits
    /// the result (HTML-escaped when `escape = true`).  If the key is absent
    /// the key text itself is emitted.
    Translate {
        key: String,
        /// Named arguments for `{placeholder}` interpolation.  Each element is
        /// `(placeholder_name, context_path)`.
        args: Vec<(String, Path)>,
        /// `true` for `{{ t … }}`, `false` for `{{{ t … }}}`.
        escape: bool,
    },
}
