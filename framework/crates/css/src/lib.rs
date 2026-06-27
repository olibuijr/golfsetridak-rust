//! AkurAI-Framework atomic/utility CSS engine — pure `std`, zero dependencies.
//!
//! Tailwind-style atomic CSS, but the utility grammar is ours, so the output is
//! bounded and deterministic. The engine [`scan`]s HTML/template source for the
//! class tokens actually used, then [`generate`]s **only** the CSS for the
//! utilities among them — unknown classes (hand-written component classes) are
//! ignored.
//!
//! ```
//! use akurai_css::build;
//!
//! let html = r#"<div class="flex p-2 text-center unknown-thing">hi</div>"#;
//! let css = build(&[html]);
//! assert_eq!(
//!     css,
//!     ".flex{display:flex}.p-2{padding:8px}.text-center{text-align:center}"
//! );
//! ```
//!
//! Utility grammar (see [`utilities`]):
//! - **spacing** (4px scale): `p|m|px|py|pt|pr|pb|pl|mx|my|gap-{n}` → e.g.
//!   `p-2` ⇒ `padding:8px`, `gap-4` ⇒ `gap:16px`.
//! - **display/flex**: `flex`, `block`, `hidden`, `items-center`,
//!   `justify-center`, `flex-col`.
//! - **text**: `text-center`, `font-bold`, `text-sm`, `text-lg`.
//! - **color (CSS vars)**: `text-accent`, `bg-accent`, `text-muted`.

#![forbid(unsafe_code)]

pub mod generate;
pub mod scan;
pub mod theme;
pub mod utilities;

/// Scan every source for used class tokens and return a minimal stylesheet
/// containing only the recognised utility classes, in first-seen order.
///
/// Sources are scanned left-to-right; a class is emitted once, at the position
/// of its first occurrence across all sources. Unknown classes are omitted.
pub fn build(sources: &[&str]) -> String {
    let mut classes: Vec<String> = Vec::new();
    for source in sources {
        for class in scan::classes(source) {
            if !classes.iter().any(|c| c == &class) {
                classes.push(class);
            }
        }
    }
    generate::stylesheet(&classes)
}
