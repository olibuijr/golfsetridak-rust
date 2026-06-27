//! Shop helpers shared by the storefront, admin, and checkout layers.
//!
//! Products and product categories now live in the framework collections DB,
//! so the former B+tree-backed `ShopStore` data layer has been retired. This
//! module retains only the price-formatting and slug helpers that the HTTP
//! layer still depends on.

/// Build a URL-safe ASCII slug from `text`, transliterating Icelandic letters.
pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in text.trim().to_lowercase().chars() {
        let repl = match ch {
            'a'..='z' | '0'..='9' => Some(ch.to_string()),
            'á' | 'à' | 'â' | 'ä' | 'å' => Some("a".into()),
            'é' | 'è' | 'ê' | 'ë' => Some("e".into()),
            'í' | 'ì' | 'î' | 'ï' => Some("i".into()),
            'ó' | 'ò' | 'ô' => Some("o".into()),
            'ö' => Some("o".into()),
            'ú' | 'ù' | 'û' | 'ü' => Some("u".into()),
            'ý' | 'ÿ' => Some("y".into()),
            'æ' => Some("ae".into()),
            'þ' => Some("th".into()),
            'ð' => Some("d".into()),
            _ => None,
        };
        if let Some(s) = repl {
            if !s.is_empty() {
                out.push_str(&s);
                last_dash = false;
            }
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Format an ISK amount with `.` thousands separators, e.g. `1.234 kr`.
pub fn format_isk(amount: i64) -> String {
    let neg = amount < 0;
    let digits = amount.unsigned_abs().to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(ch);
    }
    format!("{}{} kr", if neg { "-" } else { "" }, out)
}
