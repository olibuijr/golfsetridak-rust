//! End-to-end: scan a small HTML snippet and emit exactly the used utilities.

use akurai_css::build;

#[test]
fn build_emits_exactly_used_rules() {
    let html = r#"
        <main class="flex flex-col gap-4">
          <h1 class="text-lg font-bold text-center">Title</h1>
          <p class="p-2 text-muted component-prose">Body</p>
          <button class="px-4 py-2 bg-accent">Go</button>
        </main>
    "#;

    let css = build(&[html]);

    // Every used utility present, in first-seen order; nothing else.
    let expected = concat!(
        ".flex{display:flex}",
        ".flex-col{flex-direction:column}",
        ".gap-4{gap:16px}",
        ".text-lg{font-size:1.125rem}",
        ".font-bold{font-weight:700}",
        ".text-center{text-align:center}",
        ".p-2{padding:8px}",
        ".text-muted{color:var(--muted)}",
        ".px-4{padding-left:16px;padding-right:16px}",
        ".py-2{padding-top:8px;padding-bottom:8px}",
        ".bg-accent{background-color:var(--accent)}",
    );
    assert_eq!(css, expected);

    // Hand-written component class is omitted.
    assert!(!css.contains("component-prose"));
}

#[test]
fn build_dedupes_across_sources() {
    let a = r#"<div class="p-2 flex">"#;
    let b = r#"<div class="flex p-2 m-1">"#;
    // `flex`/`p-2` first seen in `a`; `m-1` only in `b`, appended last.
    assert_eq!(
        build(&[a, b]),
        ".p-2{padding:8px}.flex{display:flex}.m-1{margin:4px}"
    );
}
