//! The utility grammar: a class name → CSS declaration body.
//!
//! [`declaration`] returns the declaration block (e.g. `"padding:8px"`) for a
//! recognised utility class, or `None` for anything we don't define (which the
//! generator then omits). Two layers: a fixed table of keyword utilities and a
//! parametric spacing parser on a 4px scale.

/// Pixels per spacing step. `p-2` ⇒ `2 * 4 = 8px`.
const SPACING_STEP_PX: u32 = 4;

/// Return the CSS declaration body for `class`, or `None` if it is not a known
/// utility.
///
/// The body has no surrounding braces and no trailing semicolon, e.g.
/// `declaration("p-2") == Some("padding:8px")`.
pub fn declaration(class: &str) -> Option<String> {
    if let Some(body) = keyword(class) {
        return Some(body.to_string());
    }
    spacing(class)
}

/// Fixed (non-parametric) utilities.
fn keyword(class: &str) -> Option<&'static str> {
    let body = match class {
        // display / flex
        "flex" => "display:flex",
        "block" => "display:block",
        "hidden" => "display:none",
        "flex-col" => "flex-direction:column",
        "items-center" => "align-items:center",
        "justify-center" => "justify-content:center",
        // text
        "text-center" => "text-align:center",
        "font-bold" => "font-weight:700",
        "text-sm" => "font-size:0.875rem",
        "text-lg" => "font-size:1.125rem",
        // color (driven by CSS variables)
        "text-accent" => "color:var(--accent)",
        "text-muted" => "color:var(--muted)",
        "bg-accent" => "background-color:var(--accent)",
        _ => return None,
    };
    Some(body)
}

/// Parametric spacing utilities on a 4px scale: `{prefix}-{n}`.
///
/// `prefix` selects one or more CSS properties; `n` is a non-negative integer
/// multiplied by [`SPACING_STEP_PX`].
fn spacing(class: &str) -> Option<String> {
    let (prefix, n) = class.rsplit_once('-')?;
    let n: u32 = parse_scale(n)?;
    let px = n * SPACING_STEP_PX;

    let props: &[&str] = match prefix {
        "p" => &["padding"],
        "m" => &["margin"],
        "px" => &["padding-left", "padding-right"],
        "py" => &["padding-top", "padding-bottom"],
        "pt" => &["padding-top"],
        "pr" => &["padding-right"],
        "pb" => &["padding-bottom"],
        "pl" => &["padding-left"],
        "mx" => &["margin-left", "margin-right"],
        "my" => &["margin-top", "margin-bottom"],
        "gap" => &["gap"],
        _ => return None,
    };

    let body = props
        .iter()
        .map(|p| format!("{p}:{px}px"))
        .collect::<Vec<_>>()
        .join(";");
    Some(body)
}

/// Parse a spacing scale index: a plain non-negative integer with no sign,
/// leading zeros, or other cruft.
fn parse_scale(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacing_scale() {
        assert_eq!(declaration("p-2").as_deref(), Some("padding:8px"));
        assert_eq!(declaration("p-0").as_deref(), Some("padding:0px"));
        assert_eq!(declaration("m-4").as_deref(), Some("margin:16px"));
        assert_eq!(declaration("gap-1").as_deref(), Some("gap:4px"));
    }

    #[test]
    fn axis_spacing_emits_two_properties() {
        assert_eq!(
            declaration("px-2").as_deref(),
            Some("padding-left:8px;padding-right:8px")
        );
        assert_eq!(
            declaration("py-3").as_deref(),
            Some("padding-top:12px;padding-bottom:12px")
        );
    }

    #[test]
    fn keyword_utilities() {
        assert_eq!(declaration("flex").as_deref(), Some("display:flex"));
        assert_eq!(declaration("hidden").as_deref(), Some("display:none"));
        assert_eq!(declaration("font-bold").as_deref(), Some("font-weight:700"));
        assert_eq!(
            declaration("text-accent").as_deref(),
            Some("color:var(--accent)")
        );
    }

    #[test]
    fn unknown_classes_are_none() {
        assert_eq!(declaration("btn-primary"), None);
        assert_eq!(declaration("p-"), None);
        assert_eq!(declaration("p-x"), None);
        assert_eq!(declaration("p--1"), None);
        assert_eq!(declaration("p-01"), None);
        assert_eq!(declaration("zzz-2"), None);
    }
}
