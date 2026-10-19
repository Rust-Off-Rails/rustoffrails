//! `cargo offrails brand`: one file, `brand.toml`, rebrands the app (guides/views.md#brand).
//!
//! It renders, from the app root:
//!   assets/brand.css        the Tailwind theme: primary, accent and neutral scales (50–950), the
//!                           on-primary text color, fonts and radius; semantic tokens for light and
//!                           dark (imported by assets/app.css)
//!   src/brand.rs            the name, tagline and colors for Rust: config::APP_NAME, the
//!                           theme-color meta, and the email layout's inline colors
//!   static/favicon.svg      the mark (text or the logo SVG) on a primary tile
//!   assets/brand/og.svg     the social card's source
//!   static/favicon.ico, apple-touch-icon.png, icon-512.png, og.png
//!                           rasterized with the installed Chrome or rsvg-convert (raster.rs);
//!                           without either they are left as they are, and it says so
//! and, when the mailer part is installed, points the email header at `crate::brand::email`.
//!
//! Every text/background pair the components use is checked against WCAG 2.2 AA (4.5:1 for text)
//! in light and dark; a failing pair refuses with the pair, the ratio and a passing color.
//! `--check` renders in memory and fails if a generated file differs (verify --full runs it).

pub mod color;
mod raster;
pub mod toml;

use std::fs;
use std::path::Path;

use color::{Rgb, SHADES, contrast};

use crate::util::{self, Fail, Result};

pub struct Options {
    /// Compare instead of write: exit 1 when a generated file is stale.
    pub check: bool,
    /// Rasterize the PNGs (needs Chrome or rsvg-convert).
    pub images: bool,
    /// Write brand.toml from src/config.rs when there is none.
    pub init: bool,
    /// Fewer lines (`new` runs it).
    pub quiet: bool,
}

/// The template's brand.toml, with `{name}` and `{tagline}`. A test keeps
/// template/app/brand.toml identical to it.
pub const DEFAULT_TOML: &str = r##"# brand.toml: the app's look, in one file. After editing it, run `cargo offrails brand`: it renders
# assets/brand.css (the Tailwind theme the components use, light and dark), src/brand.rs (the name
# and colors for Rust and email), static/favicon.svg and the icons, and the social card
# (assets/brand/og.svg, static/og.png). It refuses colors that fail WCAG AA contrast, and says
# which pair and what would pass. GUIDE: .offrails/guides/views.md#brand

name = "{name}"
tagline = "{tagline}"

[colors]
primary = "#4f46e5"   # buttons, links, focus rings, the favicon tile (exactly this on buttons)
accent = "#0d9488"    # badges, highlights, the social card's bar
neutral = "gray"      # surfaces, text and borders: gray | slate | zinc | neutral | stone, or a hex to tint with
# on_primary = "#ffffff"   # text on primary buttons; default: white or near-black, whichever passes

[fonts]
# CSS font stacks. Self-host web fonts in static/fonts/ (the CSP blocks font CDNs).
sans = "ui-sans-serif, system-ui, sans-serif, 'Apple Color Emoji', 'Segoe UI Emoji'"
heading = ""          # "" = the sans stack
mono = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"

[shape]
radius = "0.375rem"   # controls and badges; cards and dialogs use larger steps of it. "0" = square

[logo]
mark = ""             # the favicon's text mark, 1-2 characters ("" = the name's first letter)
svg = ""              # or a square SVG file (e.g. "assets/brand/logo.svg"), drawn on the tile instead
tile = true           # draw the mark on a primary rounded tile (false: the SVG as it is)
"##;

#[derive(Debug, Clone)]
pub enum Neutral {
    Named(String),
    Tint(Rgb),
}

#[derive(Debug, Clone)]
pub struct Brand {
    pub name: String,
    pub tagline: String,
    pub primary: Rgb,
    pub accent: Rgb,
    pub neutral: Neutral,
    pub on_primary: Option<Rgb>,
    pub font_sans: String,
    pub font_heading: String,
    pub font_mono: String,
    pub radius: String,
    pub mark: String,
    pub logo_svg: String,
    pub tile: bool,
}

/// The scales and derived colors every output uses.
pub struct Palette {
    pub primary: [Rgb; 11],
    pub accent: [Rgb; 11],
    pub neutral: [Rgb; 11],
    pub on_primary: Rgb,
    pub on_accent: Rgb,
    pub primary_hover: Rgb,
}

/// The shade at `s` (50…950) of a scale.
fn at(scale: &[Rgb; 11], s: u16) -> Rgb {
    scale[SHADES.iter().position(|x| *x == s).unwrap_or(6)]
}

impl Brand {
    pub fn parse(text: &str) -> std::result::Result<Brand, String> {
        let t = toml::parse(text)?;
        for key in t.keys() {
            let known = [
                "name",
                "tagline",
                "colors.primary",
                "colors.accent",
                "colors.neutral",
                "colors.on_primary",
                "fonts.sans",
                "fonts.heading",
                "fonts.mono",
                "shape.radius",
                "logo.mark",
                "logo.svg",
                "logo.tile",
            ];
            if !known.contains(&key.as_str()) {
                return Err(format!(
                    "unknown key {key} (brand.toml keys: {})",
                    known.join(", ")
                ));
            }
        }
        let s = |k: &str, default: &str| -> std::result::Result<String, String> {
            match t.get(k) {
                None => Ok(default.to_owned()),
                Some(toml::Value::Str(v)) => Ok(v.trim().to_owned()),
                Some(_) => Err(format!("{k} must be a \"string\"")),
            }
        };
        let hex = |k: &str, default: &str| -> std::result::Result<Rgb, String> {
            let v = s(k, default)?;
            Rgb::parse(&v).ok_or_else(|| format!("{k} = {v:?} is not a #rrggbb color"))
        };
        let name = s("name", "")?;
        if name.is_empty()
            || name
                .chars()
                .any(|c| matches!(c, '"' | '\\' | '{' | '}' | '<' | '>' | '\n' | '\r'))
        {
            return Err(format!(
                "name = {name:?}: plain text, not empty, no quotes, backslashes, braces or angle brackets"
            ));
        }
        let tagline = s("tagline", "")?;
        if tagline.contains(['\n', '\r']) || tagline.contains("{{") || tagline.contains("{%") {
            return Err("tagline: one line of plain text".into());
        }
        let neutral_raw = s("colors.neutral", "gray")?;
        let neutral = if let Some(c) = Rgb::parse(&neutral_raw) {
            Neutral::Tint(c)
        } else if color::named_neutral(&neutral_raw).is_some() {
            Neutral::Named(neutral_raw)
        } else {
            return Err(format!(
                "colors.neutral = {neutral_raw:?}: gray, slate, zinc, neutral, stone, or a #rrggbb tint"
            ));
        };
        let on_primary = match s("colors.on_primary", "")? {
            v if v.is_empty() => None,
            v => Some(
                Rgb::parse(&v)
                    .ok_or_else(|| format!("colors.on_primary = {v:?} is not a #rrggbb color"))?,
            ),
        };
        let font = |k: &str, default: &str| -> std::result::Result<String, String> {
            let v = s(k, default)?;
            if v.contains([';', '{', '}', '<', '>', '\\']) {
                return Err(format!(
                    "{k}: a CSS font stack (no ; {{ }} < > or backslashes)"
                ));
            }
            Ok(v)
        };
        let font_sans = font(
            "fonts.sans",
            "ui-sans-serif, system-ui, sans-serif, 'Apple Color Emoji', 'Segoe UI Emoji'",
        )?;
        let font_sans = if font_sans.is_empty() {
            "ui-sans-serif, system-ui, sans-serif".to_owned()
        } else {
            font_sans
        };
        let radius = s("shape.radius", "0.375rem")?;
        if radius_rem(&radius).is_none() {
            return Err(format!(
                "shape.radius = {radius:?}: a length like 0.375rem, 6px or 0"
            ));
        }
        let mark = s("logo.mark", "")?;
        let mark = if mark.is_empty() {
            initial(&name)
        } else {
            mark
        };
        if mark.chars().count() > 2 {
            return Err(format!(
                "logo.mark = {mark:?}: one or two characters (use logo.svg for a drawn logo)"
            ));
        }
        let tile = match t.get("logo.tile") {
            None => true,
            Some(toml::Value::Bool(b)) => *b,
            Some(_) => return Err("logo.tile must be true or false".into()),
        };
        Ok(Brand {
            name,
            tagline,
            primary: hex("colors.primary", "#4f46e5")?,
            accent: hex("colors.accent", "#0d9488")?,
            neutral,
            on_primary,
            font_heading: font("fonts.heading", "")?,
            font_mono: font(
                "fonts.mono",
                "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
            )?,
            font_sans,
            radius,
            mark,
            logo_svg: s("logo.svg", "")?,
            tile,
        })
    }

    pub fn palette(&self) -> Palette {
        let primary = color::scale(self.primary);
        let accent = color::scale(self.accent);
        let neutral = match &self.neutral {
            Neutral::Named(n) => {
                let hexes = color::named_neutral(n).unwrap_or_else(|| {
                    color::named_neutral("gray").expect("gray is a named neutral")
                });
                hexes.map(|h| Rgb::parse(h).expect("the named neutrals are valid hex"))
            }
            Neutral::Tint(c) => color::neutral_from(*c),
        };
        // Accessible by construction: the shades components put text in reach AA on the
        // surfaces they sit on, whatever the base color (a yellow brand's 700 is darkened).
        let primary = readable(primary, &neutral);
        let accent = readable(accent, &neutral);
        let dark_text = neutral[10];
        let best_on = |bg: Rgb| {
            if contrast(color::WHITE, bg) >= 4.5
                || contrast(color::WHITE, bg) >= contrast(dark_text, bg)
            {
                color::WHITE
            } else {
                dark_text
            }
        };
        let on_primary = self.on_primary.unwrap_or_else(|| best_on(primary[6]));
        // Hover moves away from the text color: darker under light text, lighter under dark.
        let primary_hover = if on_primary.luminance() > 0.5 {
            primary[7]
        } else {
            primary[5]
        };
        Palette {
            on_accent: best_on(accent[6]),
            primary,
            accent,
            neutral,
            on_primary,
            primary_hover,
        }
    }

    fn font_heading_or_sans(&self) -> &str {
        if self.font_heading.is_empty() {
            &self.font_sans
        } else {
            &self.font_heading
        }
    }
}

/// Text shades moved until they pass on their surfaces: 700 (links) on the lightest card,
/// 800 on 100 (badges), 300 (dark links) on the darkest card, 200 on 950 (dark badges); then
/// the scale is kept in order. 600, the brand's own color, never moves.
fn readable(mut sc: [Rgb; 11], neutral: &[Rgb; 11]) -> [Rgb; 11] {
    let fit = |c: Rgb, bg: Rgb| {
        if contrast(c, bg) >= AA + 0.1 {
            c
        } else {
            color::nearest_passing(c, bg, AA + 0.1).unwrap_or(c)
        }
    };
    sc[7] = fit(fit(sc[7], color::WHITE), neutral[0]);
    sc[3] = fit(sc[3], neutral[9]);
    // Darker shades stay at least as dark as the one before; lighter ones at least as light.
    for i in 8..11 {
        if sc[i].luminance() > sc[i - 1].luminance() {
            sc[i] = sc[i - 1];
        }
    }
    for i in (0..3).rev() {
        if sc[i].luminance() < sc[i + 1].luminance() {
            sc[i] = sc[i + 1];
        }
    }
    sc[8] = fit(sc[8], sc[1]);
    sc[2] = fit(sc[2], sc[10]);
    sc
}

/// The first letter or digit of the name, uppercased (`"my app"` → `"M"`).
fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "A".into())
}

/// `0.375rem` → 0.375, `6px` → 0.375, `0` → 0.
fn radius_rem(r: &str) -> Option<f64> {
    if r == "0" {
        return Some(0.0);
    }
    let (num, unit) = r
        .find(|c: char| c.is_ascii_alphabetic())
        .map(|i| r.split_at(i))?;
    let n: f64 = num.parse().ok()?;
    if !(0.0..=4.0 * 16.0).contains(&n) {
        return None;
    }
    match unit {
        "rem" | "em" => Some(n),
        "px" => Some(n / 16.0),
        _ => None,
    }
}

/// One text/background pair the components use.
pub struct Pair {
    pub scheme: &'static str,
    pub what: &'static str,
    pub fg: Rgb,
    pub bg: Rgb,
    /// Which brand.toml color to change when it fails.
    pub knob: &'static str,
}

pub fn pairs(p: &Palette) -> Vec<Pair> {
    let n = |s| at(&p.neutral, s);
    let pr = |s| at(&p.primary, s);
    let ac = |s| at(&p.accent, s);
    let w = color::WHITE;
    vec![
        Pair {
            scheme: "light",
            what: "body text",
            fg: n(900),
            bg: w,
            knob: "colors.neutral",
        },
        Pair {
            scheme: "light",
            what: "muted text",
            fg: n(500),
            bg: w,
            knob: "colors.neutral",
        },
        Pair {
            scheme: "light",
            what: "muted text on a card",
            fg: n(600),
            bg: n(100),
            knob: "colors.neutral",
        },
        Pair {
            scheme: "light",
            what: "links",
            fg: pr(700),
            bg: w,
            knob: "colors.primary",
        },
        Pair {
            scheme: "light",
            what: "links on a card",
            fg: pr(700),
            bg: n(50),
            knob: "colors.primary",
        },
        Pair {
            scheme: "light",
            what: "primary badge",
            fg: pr(800),
            bg: pr(100),
            knob: "colors.primary",
        },
        Pair {
            scheme: "light",
            what: "accent badge",
            fg: ac(800),
            bg: ac(100),
            knob: "colors.accent",
        },
        Pair {
            scheme: "dark",
            what: "body text",
            fg: n(100),
            bg: n(950),
            knob: "colors.neutral",
        },
        Pair {
            scheme: "dark",
            what: "muted text",
            fg: n(400),
            bg: n(950),
            knob: "colors.neutral",
        },
        Pair {
            scheme: "dark",
            what: "muted text on a card",
            fg: n(400),
            bg: n(900),
            knob: "colors.neutral",
        },
        Pair {
            scheme: "dark",
            what: "links",
            fg: pr(300),
            bg: n(950),
            knob: "colors.primary",
        },
        Pair {
            scheme: "dark",
            what: "links on a card",
            fg: pr(300),
            bg: n(900),
            knob: "colors.primary",
        },
        Pair {
            scheme: "dark",
            what: "primary badge",
            fg: pr(200),
            bg: pr(950),
            knob: "colors.primary",
        },
        Pair {
            scheme: "dark",
            what: "accent badge",
            fg: ac(200),
            bg: ac(950),
            knob: "colors.accent",
        },
        Pair {
            scheme: "both",
            what: "primary button text",
            fg: p.on_primary,
            bg: pr(600),
            knob: "colors.primary",
        },
        Pair {
            scheme: "both",
            what: "primary button text, hovered",
            fg: p.on_primary,
            bg: p.primary_hover,
            knob: "colors.primary",
        },
    ]
}

/// WCAG 2.2 AA for normal text.
pub const AA: f64 = 4.5;

/// The failing pairs, each as one line with the fix.
pub fn contrast_failures(b: &Brand, p: &Palette) -> Vec<String> {
    let mut out = Vec::new();
    for pair in pairs(p) {
        let r = contrast(pair.fg, pair.bg);
        if r >= AA {
            continue;
        }
        let fix = if pair.what.starts_with("primary button") {
            if b.on_primary.is_some() {
                let passing = color::nearest_passing(b.primary, p.on_primary, AA)
                    .map(|c| format!(", or primary = \"{}\"", c.hex()))
                    .unwrap_or_default();
                format!(
                    "remove colors.on_primary (the brand then picks white or near-black){passing}"
                )
            } else {
                match color::nearest_passing(pair.bg, pair.fg, AA) {
                    Some(c) => format!("colors.primary = \"{}\"", c.hex()),
                    None => "choose a darker or lighter colors.primary".into(),
                }
            }
        } else if pair.knob == "colors.neutral" {
            "use a named neutral (gray, slate, zinc, neutral, stone) or a less saturated tint"
                .into()
        } else {
            let base = if pair.knob == "colors.accent" {
                b.accent
            } else {
                b.primary
            };
            match color::nearest_passing(base, pair.bg, AA) {
                Some(c) => format!(
                    "{} = \"{}\" (a darker or lighter base moves every shade)",
                    pair.knob,
                    c.hex()
                ),
                None => format!("change {}", pair.knob),
            }
        };
        out.push(format!(
            "{} {}: {} on {} is {:.2}:1, WCAG AA needs {AA}:1. Fix: {fix}",
            pair.scheme,
            pair.what,
            pair.fg.hex(),
            pair.bg.hex(),
            r
        ));
    }
    out
}

// ---- outputs ------------------------------------------------------------------------------------

const GENERATED: &str = "Generated by `cargo offrails brand` from brand.toml. Don't edit: change brand.toml and re-run.";

pub fn brand_css(b: &Brand, p: &Palette) -> String {
    let mut s = format!(
        "/* assets/brand.css: {GENERATED}\n   assets/app.css imports it. GUIDE: .offrails/guides/views.md#brand */\n@theme {{\n"
    );
    let scale = |s: &mut String, name: &str, sc: &[Rgb; 11]| {
        for (shade, c) in SHADES.iter().zip(sc) {
            s.push_str(&format!("  --color-{name}-{shade}: {};\n", c.hex()));
        }
    };
    s.push_str("  /* primary: buttons, links, focus. 600 is brand.toml's color exactly. */\n");
    scale(&mut s, "primary", &p.primary);
    s.push_str(&format!(
        "  --color-primary-hover: {};\n  --color-on-primary: {};\n",
        p.primary_hover.hex(),
        p.on_primary.hex()
    ));
    s.push_str("  /* accent: badges, highlights. */\n");
    scale(&mut s, "accent", &p.accent);
    s.push_str(&format!("  --color-on-accent: {};\n", p.on_accent.hex()));
    let neutral_name = match &b.neutral {
        Neutral::Named(n) => n.clone(),
        Neutral::Tint(c) => format!("tinted {}", c.hex()),
    };
    s.push_str(&format!(
        "  /* gray-* in templates is the brand's neutral ({neutral_name}). */\n"
    ));
    scale(&mut s, "gray", &p.neutral);
    s.push_str(
        "  /* indigo-* (the default palette parts and older templates use) is the primary. */\n",
    );
    scale(&mut s, "indigo", &p.primary);
    s.push_str(&format!(
        "  --font-sans: {};\n  --font-heading: {};\n  --font-mono: {};\n",
        b.font_sans,
        b.font_heading_or_sans(),
        b.font_mono
    ));
    let r = &b.radius;
    if r == "0" {
        s.push_str("  --radius-sm: 0;\n  --radius-md: 0;\n  --radius-lg: 0;\n  --radius-xl: 0;\n");
    } else {
        s.push_str(&format!(
            "  --radius-sm: calc({r} * 0.667);\n  --radius-md: {r};\n  --radius-lg: calc({r} * 1.333);\n  --radius-xl: calc({r} * 2);\n"
        ));
    }
    s.push_str("}\n\n");
    let n = |sh| at(&p.neutral, sh).hex();
    let pr = |sh| at(&p.primary, sh).hex();
    let light = format!(
        "  --brand-bg: #ffffff;\n  --brand-surface: {};\n  --brand-text: {};\n  --brand-muted: {};\n  --brand-border: {};\n  --brand-link: {};\n  --brand-focus: {};\n",
        n(50),
        n(900),
        n(600),
        n(200),
        pr(700),
        pr(500)
    );
    let dark = format!(
        "  --brand-bg: {};\n  --brand-surface: {};\n  --brand-text: {};\n  --brand-muted: {};\n  --brand-border: {};\n  --brand-link: {};\n  --brand-focus: {};\n",
        n(950),
        n(900),
        n(100),
        n(400),
        n(800),
        pr(300),
        pr(400)
    );
    s.push_str("/* Semantic tokens for CSS that isn't a utility (the contrast check proves these pairs). */\n");
    s.push_str(&format!(":root {{\n{light}}}\n"));
    s.push_str(&format!(":root[data-theme=\"dark\"] {{\n{dark}}}\n"));
    s.push_str(&format!(
        "@media (prefers-color-scheme: dark) {{\n  :root:not([data-theme=\"light\"]) {{\n{}  }}\n}}\n",
        dark.lines().map(|l| format!("  {l}\n")).collect::<String>()
    ));
    s
}

pub fn brand_rs(b: &Brand, p: &Palette) -> String {
    let n = |sh| at(&p.neutral, sh).hex();
    let pr = |sh| at(&p.primary, sh).hex();
    format!(
        r##"//! {GENERATED}
//! The brand for Rust code: the app's name (`config::APP_NAME`), the theme-color meta tag, and the
//! email layout's inline colors (mail clients ignore the app's CSS). GUIDE: .offrails/guides/views.md#brand

{name}
{tagline}
/// `colors.primary`: buttons, links, the favicon tile, `<meta name="theme-color">`.
pub const PRIMARY: &str = "{primary}";
pub const ON_PRIMARY: &str = "{on_primary}";
pub const ACCENT: &str = "{accent}";
pub const THEME_COLOR: &str = PRIMARY;
{sans}

/// Inline colors for `templates/mailers/layout.html`: light first, then the dark-mode block's.
pub mod email {{
    pub const HEADER_BG: &str = "{header_bg}";
    pub const HEADER_TEXT: &str = "{on_primary}";
    pub const BG: &str = "{bg}";
    pub const CARD: &str = "#ffffff";
    pub const BORDER: &str = "{border}";
    pub const TEXT: &str = "{text}";
    pub const MUTED: &str = "{muted}";
    pub const LINK: &str = "{link}";
    pub const BUTTON_BG: &str = "{header_bg}";
    pub const BUTTON_TEXT: &str = "{on_primary}";
    pub const DARK_BG: &str = "{dark_bg}";
    pub const DARK_CARD: &str = "{dark_card}";
    pub const DARK_BORDER: &str = "{dark_border}";
    pub const DARK_TEXT: &str = "{dark_text}";
    pub const DARK_MUTED: &str = "{dark_muted}";
    pub const DARK_LINK: &str = "{dark_link}";
}}
"##,
        name = const_str("NAME", &b.name),
        tagline = const_str("TAGLINE", &b.tagline),
        primary = pr(600),
        on_primary = p.on_primary.hex(),
        accent = at(&p.accent, 600).hex(),
        sans = const_str("FONT_SANS", &b.font_sans),
        header_bg = pr(600),
        bg = n(100),
        border = n(200),
        text = n(900),
        muted = n(500),
        link = pr(700),
        dark_bg = n(950),
        dark_card = n(900),
        dark_border = n(700),
        dark_text = n(200),
        dark_muted = n(400),
        dark_link = pr(300),
    )
}

/// `pub const NAME: &str = "…";`, wrapped the way rustfmt wraps it (100 columns), so `cargo fmt`
/// leaves the generated file alone and `brand --check` stays green after it.
fn const_str(name: &str, value: &str) -> String {
    let lit = format!("{value:?}");
    let one = format!("pub const {name}: &str = {lit};");
    if one.chars().count() <= 100 || lit.chars().count() + 5 > 100 {
        one
    } else {
        format!("pub const {name}: &str =\n    {lit};")
    }
}

/// XML text and attribute escaping.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A logo file's `viewBox` and inner markup.
fn split_svg(svg: &str) -> std::result::Result<(String, String), String> {
    let start = svg.find("<svg").ok_or("no <svg> element")?;
    let open_end = svg[start..].find('>').ok_or("unterminated <svg> tag")? + start;
    let close = svg.rfind("</svg>").ok_or("no </svg>")?;
    if close < open_end {
        return Err("an empty <svg/>".into());
    }
    let tag = &svg[start..open_end];
    let attr = |name: &str| -> Option<String> {
        let at = tag.find(&format!("{name}=\""))? + name.len() + 2;
        let end = tag[at..].find('"')? + at;
        Some(tag[at..end].to_owned())
    };
    let view_box = attr("viewBox").unwrap_or_else(|| {
        let num = |a: &str| {
            attr(a)
                .and_then(|v| v.trim_end_matches("px").parse::<f64>().ok())
                .unwrap_or(64.0)
        };
        format!("0 0 {} {}", num("width"), num("height"))
    });
    Ok((view_box, svg[open_end + 1..close].trim().to_owned()))
}

/// The favicon's inner markup in a 64×64 box (the tile and the mark).
fn mark_markup(b: &Brand, p: &Palette, logo: Option<&(String, String)>) -> String {
    let rx = (radius_rem(&b.radius).unwrap_or(0.375) * 37.3)
        .round()
        .min(32.0);
    let tile = if b.tile || logo.is_none() {
        format!(
            "<rect width=\"64\" height=\"64\" rx=\"{rx}\" fill=\"{}\"/>",
            at(&p.primary, 600).hex()
        )
    } else {
        String::new()
    };
    match logo {
        Some((vb, inner)) if b.tile => format!(
            "{tile}<svg x=\"12\" y=\"12\" width=\"40\" height=\"40\" viewBox=\"{vb}\">{inner}</svg>"
        ),
        Some((vb, inner)) => {
            format!("<svg width=\"64\" height=\"64\" viewBox=\"{vb}\">{inner}</svg>")
        }
        None => {
            let two = b.mark.chars().count() > 1;
            let (size, y) = if two { (28, 42) } else { (38, 44) };
            format!(
                "{tile}<text x=\"32\" y=\"{y}\" text-anchor=\"middle\" font-family=\"{}\" font-size=\"{size}\" font-weight=\"800\" fill=\"{}\">{}</text>",
                esc(b.font_heading_or_sans()),
                p.on_primary.hex(),
                esc(&b.mark)
            )
        }
    }
}

pub fn favicon_svg(b: &Brand, p: &Palette, logo: Option<&(String, String)>) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">{}</svg>\n",
        mark_markup(b, p, logo)
    )
}

/// Up to two lines of about `width` characters; the rest becomes an ellipsis.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(l) if l.chars().count() + 1 + word.chars().count() <= width => {
                l.push(' ');
                l.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    if lines.len() > 2 {
        lines.truncate(2);
        lines[1].push('…');
    }
    lines
}

pub fn og_svg(b: &Brand, p: &Palette, logo: Option<&(String, String)>) -> String {
    let n = |sh| at(&p.neutral, sh).hex();
    let name_size = match b.name.chars().count() {
        0..=16 => 88,
        17..=24 => 72,
        _ => 56,
    };
    let font = esc(b.font_heading_or_sans());
    let mut tag = String::new();
    for (i, line) in wrap(&b.tagline, 46).iter().enumerate() {
        tag.push_str(&format!(
            "<text x=\"96\" y=\"{}\" font-family=\"{}\" font-size=\"38\" fill=\"{}\">{}</text>",
            452 + i * 52,
            esc(&b.font_sans),
            n(300),
            esc(line)
        ));
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1200\" height=\"630\" viewBox=\"0 0 1200 630\">\
<rect width=\"1200\" height=\"630\" fill=\"{bg}\"/>\
<svg x=\"96\" y=\"132\" width=\"112\" height=\"112\" viewBox=\"0 0 64 64\">{mark}</svg>\
<text x=\"96\" y=\"372\" font-family=\"{font}\" font-size=\"{name_size}\" font-weight=\"800\" fill=\"#ffffff\">{name}</text>\
{tag}<rect y=\"616\" width=\"1200\" height=\"14\" fill=\"{bar}\"/></svg>\n",
        bg = n(950),
        mark = mark_markup(b, p, logo),
        name = esc(&b.name),
        bar = at(&p.accent, 500).hex(),
    )
}

/// The mailer part's stock header row, and what brand points it at.
const MAIL_HEADER_STOCK: &str = "<td class=\"mail-text\" style=\"padding:0 4px 16px;font:600 16px/1.4 -apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif;color:#111827;\">{{ crate::config::APP_NAME }}</td>";
const MAIL_HEADER_BRANDED: &str = "<td style=\"padding:12px 16px;background:{{ crate::brand::email::HEADER_BG }};color:{{ crate::brand::email::HEADER_TEXT }};border-radius:8px;font:600 16px/1.4 -apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif;\">{{ crate::config::APP_NAME }}</td>\n          </tr>\n          <tr>\n            <td style=\"height:12px;line-height:12px;font-size:0;\">&nbsp;</td>";

/// The mailer layout with its header on the brand's colors; `None` when it already is (or was
/// changed by hand, so there is nothing to recognize).
pub fn branded_mail_layout(layout: &str) -> Option<String> {
    (layout.contains(MAIL_HEADER_STOCK) && !layout.contains("crate::brand::email"))
        .then(|| layout.replacen(MAIL_HEADER_STOCK, MAIL_HEADER_BRANDED, 1))
}

/// FNV-1a, for the images stamp.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

// ---- the command --------------------------------------------------------------------------------

/// The app's display name from `src/config.rs` (`pub const APP_NAME: &str = "…";`), for --init.
fn config_app_name(app: &Path) -> Option<String> {
    let text = fs::read_to_string(app.join("src").join("config.rs")).ok()?;
    let line = text.lines().find(|l| l.contains("const APP_NAME"))?;
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_owned())
}

/// The default description from `src/views/mod.rs` (`DEFAULT_DESCRIPTION`), for --init.
fn views_description(app: &Path) -> Option<String> {
    let text = fs::read_to_string(app.join("src").join("views").join("mod.rs")).ok()?;
    let rest = &text[text.find("DEFAULT_DESCRIPTION")?..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(rest[start..end].to_owned())
}

pub fn default_toml(name: &str, tagline: &str) -> String {
    DEFAULT_TOML
        .replace("{name}", name)
        .replace("{tagline}", tagline)
}

pub fn run(app: &Path, o: &Options) -> Result<()> {
    let toml_path = app.join("brand.toml");
    if !toml_path.is_file() {
        if !o.init {
            return Err(Fail::new(
                "no brand.toml here: `cargo offrails brand --init` writes one (name from src/config.rs), then edit it and run `cargo offrails brand`",
            ));
        }
        let name = config_app_name(app).unwrap_or_else(|| "My App".into());
        // The tagline is a phrase (the home page title is "<name>: <tagline>"): no closing period.
        let tagline = views_description(app)
            .map(|d| d.trim().trim_end_matches('.').to_owned())
            .unwrap_or_default();
        util::write_text(&toml_path, &default_toml(&name, &tagline))?;
        println!("  write brand.toml (name {name:?}; edit it, then run cargo offrails brand)");
    }
    let text = util::read_text(&toml_path)?;
    let brand = Brand::parse(&text).map_err(|e| Fail::new(format!("brand.toml: {e}")))?;
    let palette = brand.palette();
    let failures = contrast_failures(&brand, &palette);
    if !failures.is_empty() {
        return Err(Fail::new(format!(
            "brand.toml: {} color pair(s) fail WCAG AA contrast (nothing written):\n  {}",
            failures.len(),
            failures.join("\n  ")
        )));
    }
    let logo = if brand.logo_svg.is_empty() {
        None
    } else {
        let path = app.join(&brand.logo_svg);
        let svg = util::read_text(&path)?;
        Some(split_svg(&svg).map_err(|e| Fail::new(format!("logo.svg {}: {e}", brand.logo_svg)))?)
    };
    let favicon = favicon_svg(&brand, &palette, logo.as_ref());
    let og = og_svg(&brand, &palette, logo.as_ref());
    let mut outputs: Vec<(&str, String)> = vec![
        ("assets/brand.css", brand_css(&brand, &palette)),
        ("src/brand.rs", brand_rs(&brand, &palette)),
        ("static/favicon.svg", favicon.clone()),
        ("assets/brand/og.svg", og.clone()),
    ];
    let mail = app.join("templates").join("mailers").join("layout.html");
    if let Ok(layout) = fs::read_to_string(&mail)
        && let Some(branded) = branded_mail_layout(&layout)
    {
        outputs.push(("templates/mailers/layout.html", branded));
    }
    let stamp_path = "assets/brand/images.stamp";
    let stamp = format!("{:016x}\n", fnv(format!("{favicon}{og}").as_bytes()));

    if o.check {
        let stale: Vec<&str> = outputs
            .iter()
            .filter(|(p, text)| fs::read_to_string(util::rel(app, p)).ok().as_deref() != Some(text))
            .map(|(p, _)| *p)
            .collect();
        if fs::read_to_string(util::rel(app, stamp_path))
            .ok()
            .as_deref()
            != Some(stamp.as_str())
        {
            println!(
                "note: the PNG icons and static/og.png were not rendered from the current brand (run `cargo offrails brand` where Chrome or rsvg-convert is installed)"
            );
        }
        if stale.is_empty() {
            println!(
                "brand: brand.toml is current ({} contrast pairs pass WCAG AA)",
                pairs(&palette).len()
            );
            return Ok(());
        }
        return Err(Fail::new(format!(
            "stale: {} differ from what brand.toml renders (brand.toml or a part changed since the last render). Run `cargo offrails brand`.",
            stale.join(", ")
        )));
    }

    println!(
        "brand: {} (primary {}, accent {}, neutral {}; {} contrast pairs pass WCAG AA)",
        brand.name,
        brand.primary.hex(),
        brand.accent.hex(),
        match &brand.neutral {
            Neutral::Named(n) => n.clone(),
            Neutral::Tint(c) => c.hex(),
        },
        pairs(&palette).len()
    );
    let mut written = Vec::new();
    for (p, text) in &outputs {
        let path = util::rel(app, p);
        if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            util::write_text(&path, text)?;
            written.push(*p);
        }
    }
    if !o.quiet || !written.is_empty() {
        println!(
            "  write {}",
            if written.is_empty() {
                "nothing (already current)".to_owned()
            } else {
                written.join(", ")
            }
        );
    }
    if !o.images {
        return Ok(());
    }
    if fs::read_to_string(util::rel(app, stamp_path))
        .ok()
        .as_deref()
        == Some(stamp.as_str())
    {
        if !o.quiet {
            println!(
                "  images current (static/favicon.ico, apple-touch-icon.png, icon-512.png, og.png)"
            );
        }
        return Ok(());
    }
    let Some(renderer) = raster::find() else {
        println!(
            "  note: no Chrome, Chromium, Edge or rsvg-convert: the PNGs (favicon.ico, apple-touch-icon.png, icon-512.png, og.png) keep their old look; the SVG favicon is current. Re-run `cargo offrails brand` where one is installed (CHROME_PATH names a browser)."
        );
        return Ok(());
    };
    let work = std::env::temp_dir().join(format!("offrails-brand-{}", std::process::id()));
    util::ctx(fs::create_dir_all(&work), "creating", &work)?;
    let result = render_images(app, &renderer, &work);
    let _ = fs::remove_dir_all(&work);
    match result {
        Ok(()) => {
            util::write_text(&util::rel(app, stamp_path), &stamp)?;
            println!(
                "  render static/favicon.ico, apple-touch-icon.png, icon-512.png, og.png ({})",
                renderer.name()
            );
            Ok(())
        }
        Err(e) => {
            println!("  note: the PNGs were not rendered ({e}); the SVGs are current");
            Ok(())
        }
    }
}

fn render_images(app: &Path, r: &raster::Renderer, work: &Path) -> std::result::Result<(), String> {
    let favicon = util::rel(app, "static/favicon.svg");
    let og = util::rel(app, "assets/brand/og.svg");
    let ico_png = work.join("favicon-48.png");
    raster::render(r, &favicon, 48, 48, &ico_png, work)?;
    let tmp = |n: &str| work.join(n);
    raster::render(r, &favicon, 180, 180, &tmp("apple.png"), work)?;
    raster::render(r, &favicon, 512, 512, &tmp("icon-512.png"), work)?;
    raster::render(r, &og, 1200, 630, &tmp("og.png"), work)?;
    let read = |p: &Path| fs::read(p).map_err(|e| e.to_string());
    let png48 = read(&ico_png)?;
    for (p, w) in [
        (&ico_png, 48),
        (&tmp("apple.png"), 180),
        (&tmp("icon-512.png"), 512),
    ] {
        match raster::png_size(&read(p)?) {
            Some((x, y)) if x == w && y == w => {}
            got => return Err(format!("{} came out {got:?}, not {w}×{w}", p.display())),
        }
    }
    let write = |rel: &str, bytes: &[u8]| {
        fs::write(util::rel(app, rel), bytes).map_err(|e| format!("writing {rel}: {e}"))
    };
    write("static/favicon.ico", &raster::ico(&png48, 48))?;
    write("static/apple-touch-icon.png", &read(&tmp("apple.png"))?)?;
    write("static/icon-512.png", &read(&tmp("icon-512.png"))?)?;
    write("static/og.png", &read(&tmp("og.png"))?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template_brand() -> Brand {
        Brand::parse(&default_toml("Offrails App", "A new Rust off Rails app")).unwrap()
    }

    #[test]
    fn the_template_brand_toml_is_the_default() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../template/app/brand.toml");
        let text = fs::read_to_string(file).unwrap().replace("\r\n", "\n");
        assert_eq!(
            text,
            default_toml("Offrails App", "A new Rust off Rails app")
        );
    }

    #[test]
    fn the_template_outputs_are_current() {
        // template/app ships what `cargo offrails brand` renders from its brand.toml.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../template/app");
        let b = template_brand();
        let p = b.palette();
        for (file, want) in [
            ("assets/brand.css", brand_css(&b, &p)),
            ("src/brand.rs", brand_rs(&b, &p)),
            ("static/favicon.svg", favicon_svg(&b, &p, None)),
            ("assets/brand/og.svg", og_svg(&b, &p, None)),
        ] {
            let got = fs::read_to_string(root.join(file))
                .unwrap()
                .replace("\r\n", "\n");
            assert_eq!(
                got, want,
                "{file}: run `cargo offrails brand` in template/app"
            );
        }
    }

    #[test]
    fn the_default_brand_passes_aa_everywhere() {
        let b = template_brand();
        let p = b.palette();
        assert_eq!(contrast_failures(&b, &p), Vec::<String>::new());
        assert_eq!(p.on_primary, color::WHITE);
        assert_eq!(at(&p.primary, 600).hex(), "#4f46e5");
        assert_eq!(at(&p.neutral, 950).hex(), "#030712");
        for n in ["slate", "zinc", "neutral", "stone"] {
            let t =
                default_toml("X", "").replace("neutral = \"gray\"", &format!("neutral = \"{n}\""));
            let b = Brand::parse(&t).unwrap();
            assert!(contrast_failures(&b, &b.palette()).is_empty(), "{n}");
        }
    }

    #[test]
    fn a_light_primary_gets_dark_button_text() {
        let t = default_toml("X", "").replace("#4f46e5", "#facc15");
        let b = Brand::parse(&t).unwrap();
        let p = b.palette();
        assert_ne!(p.on_primary, color::WHITE);
        assert!(contrast(p.on_primary, at(&p.primary, 600)) >= AA);
        assert!(contrast(p.on_primary, p.primary_hover) >= AA);
        assert!(
            contrast_failures(&b, &p).is_empty(),
            "{:?}",
            contrast_failures(&b, &p)
        );
    }

    #[test]
    fn a_failing_pair_refuses_with_the_pair_the_ratio_and_a_fix() {
        let t = default_toml("X", "")
            .replace("#4f46e5", "#facc15")
            .replace("# on_primary = \"#ffffff\"", "on_primary = \"#ffffff\"");
        let b = Brand::parse(&t).unwrap();
        let f = contrast_failures(&b, &b.palette());
        assert!(!f.is_empty());
        assert!(
            f[0].contains("primary button text: #ffffff on #facc15 is 1.53:1"),
            "{}",
            f[0]
        );
        assert!(f[0].contains("remove colors.on_primary"), "{}", f[0]);
        assert!(f[0].contains("or primary = \"#"), "{}", f[0]);
    }

    #[test]
    fn bad_values_are_refused_with_their_key() {
        let base = default_toml("X", "");
        for (from, to, want) in [
            (
                "primary = \"#4f46e5\"",
                "primary = \"blue\"",
                "colors.primary",
            ),
            (
                "neutral = \"gray\"",
                "neutral = \"purple\"",
                "colors.neutral",
            ),
            ("radius = \"0.375rem\"", "radius = \"big\"", "shape.radius"),
            ("mark = \"\"", "mark = \"XYZ\"", "logo.mark"),
            ("[shape]", "[shape]\ncorner = 1", "unknown key shape.corner"),
            ("mono = \"", "mono = \"x; } body { color: red", "fonts.mono"),
        ] {
            assert!(base.contains(from), "{from}");
            let e = Brand::parse(&base.replace(from, to)).unwrap_err();
            assert!(e.contains(want), "{to}: {e}");
        }
        assert!(
            Brand::parse(&base.replace("name = \"X\"", "name = \"a\\\"b\""))
                .unwrap_err()
                .contains("name")
        );
    }

    #[test]
    fn the_css_carries_every_scale_fonts_radius_and_both_schemes() {
        let b = template_brand();
        let css = brand_css(&b, &b.palette());
        for name in ["primary", "accent", "gray", "indigo"] {
            for s in SHADES {
                assert!(
                    css.contains(&format!("--color-{name}-{s}: #")),
                    "{name}-{s}"
                );
            }
        }
        assert!(css.contains("--color-primary-600: #4f46e5;"));
        assert!(css.contains("--color-on-primary: #ffffff;"));
        assert!(css.contains("--radius-md: 0.375rem;"));
        assert!(css.contains("--font-heading: ui-sans-serif"));
        assert!(css.contains(":root[data-theme=\"dark\"]"));
        assert!(css.contains(":root:not([data-theme=\"light\"])"));
        let square = Brand::parse(&default_toml("X", "").replace("\"0.375rem\"", "\"0\"")).unwrap();
        assert!(brand_css(&square, &square.palette()).contains("--radius-md: 0;"));
    }

    #[test]
    fn the_rust_module_escapes_and_carries_the_email_colors() {
        let b = Brand::parse(&default_toml("Book Shelf", "Books \\\"you\\\" read.")).unwrap();
        let rs = brand_rs(&b, &b.palette());
        assert!(rs.contains("pub const NAME: &str = \"Book Shelf\";"));
        assert!(rs.contains("pub const TAGLINE: &str = \"Books \\\"you\\\" read.\";"));
        assert!(rs.contains("pub const HEADER_BG: &str = \"#4f46e5\";"));
        assert!(rs.contains("pub const DARK_LINK: &str = \"#"));
    }

    #[test]
    fn the_favicon_is_a_text_mark_or_the_logo() {
        let b = template_brand();
        let p = b.palette();
        let f = favicon_svg(&b, &p, None);
        assert!(
            f.contains("fill=\"#4f46e5\"") && f.contains(">O</text>"),
            "{f}"
        );
        let logo = split_svg("<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><path d=\"M2 2h20v20z\"/></svg>").unwrap();
        assert_eq!(logo.0, "0 0 24 24");
        let f = favicon_svg(&b, &p, Some(&logo));
        assert!(f.contains("<svg x=\"12\" y=\"12\" width=\"40\" height=\"40\" viewBox=\"0 0 24 24\"><path d=\"M2 2h20v20z\"/></svg>"), "{f}");
        assert!(
            split_svg("<svg width=\"32\" height=\"16\"><g/></svg>")
                .unwrap()
                .0
                == "0 0 32 16"
        );
        assert!(split_svg("<div/>").is_err());
    }

    #[test]
    fn the_social_card_wraps_and_escapes() {
        let b = Brand::parse(&default_toml(
            "R&D",
            "A very long tagline that keeps going well past one line of the social card and then some more words",
        ))
        .unwrap();
        let og = og_svg(&b, &b.palette(), None);
        assert!(og.contains(">R&amp;D</text>"));
        assert!(og.contains("y=\"452\"") && og.contains("y=\"504\"") && !og.contains("y=\"556\""));
        assert!(og.contains('…'));
        assert!(og.starts_with(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1200\" height=\"630\""
        ));
    }

    #[test]
    fn the_mail_header_is_pointed_at_the_brand_once() {
        let layout = format!("<tr>\n            {MAIL_HEADER_STOCK}\n          </tr>");
        let branded = branded_mail_layout(&layout).unwrap();
        assert!(branded.contains("crate::brand::email::HEADER_BG"));
        assert!(branded_mail_layout(&branded).is_none(), "idempotent");
        assert!(branded_mail_layout("<td>custom</td>").is_none());
    }

    #[test]
    fn the_mailer_part_still_has_the_stock_header() {
        // If the mailer part's layout changes, MAIL_HEADER_STOCK must follow it.
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../template/parts/mailer/files/templates/mailers/layout.html");
        let text = fs::read_to_string(file).unwrap();
        assert!(text.contains(MAIL_HEADER_STOCK));
    }

    #[test]
    fn long_constants_wrap_like_rustfmt() {
        assert_eq!(
            const_str("NAME", "Shelf"),
            "pub const NAME: &str = \"Shelf\";"
        );
        let long = "x".repeat(80);
        assert_eq!(
            const_str("NAME", &long),
            format!("pub const NAME: &str =\n    \"{long}\";")
        );
        let huge = "x".repeat(120);
        assert_eq!(
            const_str("NAME", &huge),
            format!("pub const NAME: &str = \"{huge}\";")
        );
    }

    #[test]
    fn radius_lengths() {
        assert_eq!(radius_rem("0"), Some(0.0));
        assert_eq!(radius_rem("6px"), Some(0.375));
        assert_eq!(radius_rem("0.5rem"), Some(0.5));
        assert_eq!(radius_rem("1vw"), None);
        assert_eq!(radius_rem("rem"), None);
    }
}
