//! Stylesheet generation: used class tokens → compact CSS.
//!
//! Each recognised utility becomes one compact rule `.class{declaration}`;
//! unknown classes are skipped. Rules are emitted in the order the classes are
//! given, with no whitespace between them, for stable, minimal output.

use crate::utilities;

/// Build a compact stylesheet for `classes`, keeping only recognised utilities
/// and preserving input order.
pub fn stylesheet(classes: &[String]) -> String {
    let mut out = String::new();
    for class in classes {
        if let Some(body) = utilities::declaration(class) {
            out.push('.');
            out.push_str(class);
            out.push('{');
            out.push_str(&body);
            out.push('}');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn emits_only_known_utilities_in_order() {
        let css = stylesheet(&owned(&["flex", "btn", "p-2"]));
        assert_eq!(css, ".flex{display:flex}.p-2{padding:8px}");
    }

    #[test]
    fn empty_input_yields_empty_string() {
        assert_eq!(stylesheet(&[]), "");
    }

    #[test]
    fn all_unknown_yields_empty_string() {
        assert_eq!(stylesheet(&owned(&["card", "btn-primary"])), "");
    }
}
