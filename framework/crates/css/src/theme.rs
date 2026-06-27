//! Named multi-theme generator — pure `std`, zero dependencies.
//!
//! Emits one `themes.css` holding every theme as a `:root[data-theme="…"]`
//! token block over the framework's existing semantic tokens (`--bg`, `--fg`,
//! `--accent`, `--ok`, `--danger`, …). Components never change: they read the
//! same token names, the active `data-theme` swaps the values.
//!
//! Themes are [base16](https://github.com/tinted-theming/schemes) colour
//! schemes — 16 perceptual slots (`base00`–`base0F`). The mapping below is
//! **polarity-agnostic**: a base16 *light* scheme inverts the `base00`→`base07`
//! ramp itself, so the same slot→token mapping produces a correct light theme
//! with no special-casing. That is what lets one rule cover dark and light.
//!
//! Slot → token (base16 convention):
//! `base00` background · `base01` recessed · `base02` panel · `base03` raised /
//! border / dim · `base04` muted text · `base05` text · `base08` red/danger ·
//! `base0A` yellow/warn · `base0B` green/ok · `base0C` cyan/info ·
//! `base0D` blue/accent · `base0E` magenta/accent-2.
//!
//! The first registered scheme (`akurai`) is also emitted as the bare `:root`
//! default so a page with no `data-theme` set still themes correctly.

/// A parsed base16 scheme: display name, light/dark polarity, and the 16 colours
/// (`palette[0]` = `base00` … `palette[15]` = `base0F`), each a CSS hex string
/// including the leading `#`.
#[derive(Clone)]
pub struct Scheme {
    pub name: String,
    pub variant: Variant,
    pub palette: [String; 16],
}

/// Theme polarity. Drives `color-scheme:` (so form controls/scrollbars match)
/// and the default shadow depth.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Dark,
    Light,
}

impl Variant {
    fn css(self) -> &'static str {
        match self {
            Variant::Dark => "dark",
            Variant::Light => "light",
        }
    }
}

/// One registry row: stable `slug` (the `data-theme` value and switcher key),
/// human `family` for grouping in the picker, and the raw vendored scheme text.
struct Entry {
    slug: &'static str,
    family: &'static str,
    raw: &'static str,
}

/// The shipped themes, in picker order. The first row is the default `:root`.
/// Adding a theme is data-only: vendor a base16 scheme and add a line here.
const SCHEMES: &[Entry] = &[
    Entry {
        slug: "akurai",
        family: "AkurAI",
        raw: include_str!("schemes/akurai.yaml"),
    },
    Entry {
        slug: "akurai-light",
        family: "AkurAI",
        raw: include_str!("schemes/akurai-light.yaml"),
    },
    Entry {
        slug: "claude-code",
        family: "Claude Code",
        raw: include_str!("schemes/claude-code.yaml"),
    },
    Entry {
        slug: "claude-code-light",
        family: "Claude Code",
        raw: include_str!("schemes/claude-code-light.yaml"),
    },
    Entry {
        slug: "nord",
        family: "Nord",
        raw: include_str!("schemes/nord.yaml"),
    },
    Entry {
        slug: "nord-light",
        family: "Nord",
        raw: include_str!("schemes/nord-light.yaml"),
    },
    Entry {
        slug: "catppuccin-mocha",
        family: "Catppuccin",
        raw: include_str!("schemes/catppuccin-mocha.yaml"),
    },
    Entry {
        slug: "catppuccin-latte",
        family: "Catppuccin",
        raw: include_str!("schemes/catppuccin-latte.yaml"),
    },
    Entry {
        slug: "solarized-dark",
        family: "Solarized",
        raw: include_str!("schemes/solarized-dark.yaml"),
    },
    Entry {
        slug: "solarized-light",
        family: "Solarized",
        raw: include_str!("schemes/solarized-light.yaml"),
    },
    Entry {
        slug: "gruvbox-dark",
        family: "Gruvbox",
        raw: include_str!("schemes/gruvbox-dark-medium.yaml"),
    },
    Entry {
        slug: "gruvbox-light",
        family: "Gruvbox",
        raw: include_str!("schemes/gruvbox-light-medium.yaml"),
    },
    Entry {
        slug: "tokyo-night",
        family: "Tokyo Night",
        raw: include_str!("schemes/tokyo-night-dark.yaml"),
    },
    Entry {
        slug: "tokyo-night-light",
        family: "Tokyo Night",
        raw: include_str!("schemes/tokyo-night-light.yaml"),
    },
    Entry {
        slug: "rose-pine",
        family: "Rosé Pine",
        raw: include_str!("schemes/rose-pine.yaml"),
    },
    Entry {
        slug: "rose-pine-dawn",
        family: "Rosé Pine",
        raw: include_str!("schemes/rose-pine-dawn.yaml"),
    },
    Entry {
        slug: "dracula",
        family: "Dracula",
        raw: include_str!("schemes/dracula.yaml"),
    },
];

/// Public metadata for one theme — everything the front-end switcher needs.
pub struct ThemeMeta {
    pub slug: &'static str,
    pub family: &'static str,
    pub label: String,
    pub variant: Variant,
}

/// The theme registry as structured metadata, in picker order.
pub fn registry() -> Vec<ThemeMeta> {
    SCHEMES
        .iter()
        .map(|e| {
            let s = parse(e.raw);
            ThemeMeta {
                slug: e.slug,
                family: e.family,
                label: s.name,
                variant: s.variant,
            }
        })
        .collect()
}

/// The registry as JSON, served at `/themes.json` so the switcher is data-driven
/// and never drifts from the generated CSS. Shape:
/// `[{"slug","family","label","variant"}, …]`.
pub fn registry_json() -> String {
    let mut out = String::from("[");
    for (i, m) in registry().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            r#"{{"slug":"{}","family":"{}","label":"{}","variant":"{}"}}"#,
            esc(m.slug),
            esc(m.family),
            esc(&m.label),
            m.variant.css()
        ));
    }
    out.push(']');
    out
}

/// Render the full multi-theme stylesheet. The first scheme is emitted as the
/// bare `:root` default; every scheme (including the first) is also emitted as
/// `:root[data-theme="slug"]` so an explicit selection always wins.
pub fn themes_css() -> String {
    let mut out = String::new();
    out.push_str(
        "/* AkurAI-Framework themes — generated by akurai-css::theme. Do not edit by hand. */\n",
    );
    for (i, e) in SCHEMES.iter().enumerate() {
        let s = parse(e.raw);
        if i == 0 {
            out.push_str(":root{");
            out.push_str(&vars(&s));
            out.push_str("}\n");
        }
        out.push_str(&format!(":root[data-theme=\"{}\"]{{", e.slug));
        out.push_str(&vars(&s));
        out.push_str("}\n");
    }
    out
}

/// Map a parsed scheme's 16 slots to the framework's semantic token set.
fn vars(s: &Scheme) -> String {
    let p = &s.palette; // p[0]=base00 … p[15]=base0F
    let shadow = match s.variant {
        Variant::Dark => "0 24px 60px -28px rgba(0,0,0,.55)",
        Variant::Light => "0 18px 45px -24px rgba(20,22,30,.16)",
    };
    format!(
        "--bg:{bg};--bg-2:{bg2};--panel:{panel};--panel-2:{panel2};\
         --border:{border};--border-2:{border2};\
         --fg:{fg};--muted:{muted};--dim:{dim};\
         --accent:{accent};--accent-2:{accent2};\
         --ok:{ok};--warn:{warn};--info:{info};--danger:{danger};\
         --shadow:{shadow};color-scheme:{scheme};",
        bg = p[0],       // base00
        bg2 = p[1],      // base01
        panel = p[2],    // base02
        panel2 = p[3],   // base03
        border = p[3],   // base03
        border2 = p[4],  // base04
        fg = p[5],       // base05
        muted = p[4],    // base04
        dim = p[3],      // base03
        accent = p[13],  // base0D
        accent2 = p[14], // base0E
        ok = p[11],      // base0B
        warn = p[10],    // base0A
        info = p[12],    // base0C
        danger = p[8],   // base08
        shadow = shadow,
        scheme = s.variant.css(),
    )
}

/// Parse a flat base16 scheme document. Tolerant by design: it reads only the
/// `name:`, `variant:`, and `base00`–`base0F` lines, ignores everything else
/// (`system`, `author`, blank lines, `# comments` trailing a value), and never
/// panics on a malformed line. Unknown/missing slots stay `#000000`.
pub fn parse(text: &str) -> Scheme {
    let mut name = String::new();
    let mut variant = Variant::Dark;
    let mut palette: [String; 16] = Default::default();
    for slot in palette.iter_mut() {
        *slot = "#000000".to_string();
    }

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            name = unquote(rest);
        } else if let Some(rest) = line.strip_prefix("variant:") {
            variant = match unquote(rest).to_ascii_lowercase().as_str() {
                "light" => Variant::Light,
                _ => Variant::Dark,
            };
        } else if let Some(rest) = line.strip_prefix("base") {
            // `00: "#2E3440"  # comment`  →  index 0, value #2E3440
            let Some((idx_str, val_str)) = rest.split_once(':') else {
                continue;
            };
            let Some(idx) = slot_index(idx_str.trim()) else {
                continue;
            };
            let hex = unquote(val_str);
            if !hex.is_empty() {
                palette[idx] = normalize_hex(&hex);
            }
        }
    }

    Scheme {
        name,
        variant,
        palette,
    }
}

/// Two-hex-digit base16 slot id (`"00"`..="0F", case-insensitive) → array index.
fn slot_index(id: &str) -> Option<usize> {
    if id.len() != 2 {
        return None;
    }
    usize::from_str_radix(id, 16).ok().filter(|&n| n < 16)
}

/// Take the first double-quoted token on the line, else the first bare token,
/// stripping a trailing `# comment`. Returns the value without surrounding
/// quotes or whitespace.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if let Some(start) = s.find('"') {
        if let Some(end) = s[start + 1..].find('"') {
            return s[start + 1..start + 1 + end].trim().to_string();
        }
    }
    // No quotes: take up to a comment marker.
    s.split('#')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_string()
}

/// Ensure a leading `#` and lowercase the hex so output is uniform.
fn normalize_hex(h: &str) -> String {
    let h = h.trim();
    let body = h.strip_prefix('#').unwrap_or(h);
    format!("#{}", body.to_ascii_lowercase())
}

/// Minimal JSON string escaping for the registry payload.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_scheme() {
        let s = parse(super::SCHEMES[0].raw);
        assert_eq!(s.name, "AkurAI");
        assert!(matches!(s.variant, Variant::Dark));
        assert_eq!(s.palette[0], "#060912"); // base00 (deep-navy, drive-derived)
        assert_eq!(s.palette[13], "#5b8cff"); // base0D / accent
    }

    #[test]
    fn parses_value_with_trailing_comment() {
        // Dracula's vendored file has `base00: "#282a36"  # Default Background`.
        let dracula = parse(include_str!("schemes/dracula.yaml"));
        assert_eq!(dracula.palette[0], "#282a36");
        assert_eq!(dracula.palette[13], "#bd93f9"); // accent stays clean of comment
    }

    #[test]
    fn light_variant_detected() {
        let latte = parse(include_str!("schemes/catppuccin-latte.yaml"));
        assert!(matches!(latte.variant, Variant::Light));
    }

    #[test]
    fn default_root_and_every_theme_block_present() {
        let css = themes_css();
        assert!(css.contains(":root{--bg:#060912"), "default :root missing");
        // Every registered slug has a data-theme block.
        for e in SCHEMES {
            let needle = format!(":root[data-theme=\"{}\"]{{", e.slug);
            assert!(css.contains(&needle), "missing block for {}", e.slug);
        }
        // Semantic mapping reaches the accent slot.
        assert!(
            css.contains("--accent:#81a1c1"),
            "Nord accent (base0D) not mapped"
        );
    }

    #[test]
    fn color_scheme_tracks_variant() {
        let css = themes_css();
        assert!(css.contains("data-theme=\"claude-code-light\"]{"));
        // The light block must declare color-scheme:light.
        let block = css
            .split("data-theme=\"claude-code-light\"]{")
            .nth(1)
            .unwrap();
        let block = block.split('}').next().unwrap();
        assert!(
            block.contains("color-scheme:light"),
            "light theme not marked"
        );
    }

    #[test]
    fn registry_json_is_wellformed() {
        let json = registry_json();
        assert!(json.starts_with('['));
        assert!(json.ends_with(']'));
        assert!(json.contains(r#""slug":"nord""#));
        assert!(json.contains(r#""variant":"light""#));
        assert!(json.contains(r#""family":"Claude Code""#));
    }

    #[test]
    fn every_theme_maps_all_required_tokens() {
        for e in SCHEMES {
            let s = parse(e.raw);
            let v = vars(&s);
            for tok in [
                "--bg:",
                "--fg:",
                "--accent:",
                "--ok:",
                "--warn:",
                "--danger:",
                "--panel:",
                "--border:",
                "--muted:",
                "color-scheme:",
            ] {
                assert!(v.contains(tok), "{} missing {}", e.slug, tok);
            }
        }
    }
}
