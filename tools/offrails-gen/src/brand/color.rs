//! Colors for the brand: hex in and out, OKLCH scales (the way Tailwind 4 builds its palettes),
//! and WCAG 2.2 contrast. Standard library only.

/// An sRGB color, each channel 0.0–1.0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

/// OKLCH: lightness 0–1, chroma ~0–0.4, hue in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

pub const WHITE: Rgb = Rgb {
    r: 1.0,
    g: 1.0,
    b: 1.0,
};

/// The shades of every palette, Tailwind's names.
pub const SHADES: [u16; 11] = [50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950];

impl Rgb {
    /// `#rgb` or `#rrggbb` (case-insensitive). `None` for anything else.
    pub fn parse(hex: &str) -> Option<Rgb> {
        let h = hex.trim().strip_prefix('#')?;
        let full: String = match h.len() {
            3 => h.chars().flat_map(|c| [c, c]).collect(),
            6 => h.to_owned(),
            _ => return None,
        };
        let byte = |i: usize| u8::from_str_radix(full.get(i..i + 2)?, 16).ok();
        Some(Rgb {
            r: f64::from(byte(0)?) / 255.0,
            g: f64::from(byte(2)?) / 255.0,
            b: f64::from(byte(4)?) / 255.0,
        })
    }

    /// Lowercase `#rrggbb`.
    pub fn hex(self) -> String {
        let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", c(self.r), c(self.g), c(self.b))
    }

    /// WCAG relative luminance.
    pub fn luminance(self) -> f64 {
        let lin = |c: f64| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    pub fn oklch(self) -> Oklch {
        let lin = |c: f64| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (lin(self.r), lin(self.g), lin(self.b));
        let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
        let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
        let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
        let ok_l = 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s;
        let a = 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s;
        let bb = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s;
        let c = (a * a + bb * bb).sqrt();
        let mut h = bb.atan2(a).to_degrees();
        if h < 0.0 {
            h += 360.0;
        }
        Oklch { l: ok_l, c, h }
    }
}

impl Oklch {
    /// Linear sRGB, unclamped (out of gamut when a channel leaves 0–1).
    fn linear(self) -> (f64, f64, f64) {
        let (a, b) = (
            self.c * self.h.to_radians().cos(),
            self.c * self.h.to_radians().sin(),
        );
        let l = (self.l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
        let m = (self.l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
        let s = (self.l - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
        (
            4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
            -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
            -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s,
        )
    }

    fn in_gamut(self) -> bool {
        let (r, g, b) = self.linear();
        [r, g, b].iter().all(|v| (-1e-4..=1.0 + 1e-4).contains(v))
    }

    /// The sRGB color, chroma reduced (hue and lightness kept) until it fits the gamut.
    pub fn rgb(self) -> Rgb {
        let mut c = self;
        c.l = c.l.clamp(0.0, 1.0);
        if !c.in_gamut() {
            let (mut lo, mut hi) = (0.0, c.c);
            for _ in 0..30 {
                let mid = (lo + hi) / 2.0;
                if (Oklch { c: mid, ..c }).in_gamut() {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            c.c = lo;
        }
        let (r, g, b) = c.linear();
        let gamma = |v: f64| {
            let v = v.clamp(0.0, 1.0);
            if v <= 0.003_130_8 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            }
        };
        Rgb {
            r: gamma(r),
            g: gamma(g),
            b: gamma(b),
        }
    }
}

/// WCAG contrast ratio, 1.0–21.0.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (x, y) = (a.luminance(), b.luminance());
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// Lightness of each shade in Tailwind 4's chromatic palettes (indigo), and chroma relative to 600.
const SCALE_L: [f64; 11] = [
    0.962, 0.930, 0.870, 0.785, 0.673, 0.585, 0.511, 0.457, 0.398, 0.359, 0.257,
];
const SCALE_C: [f64; 11] = [
    0.07, 0.12, 0.22, 0.36, 0.63, 0.89, 1.0, 0.90, 0.76, 0.55, 0.34,
];

/// A 50–950 scale whose 600 is exactly `base` (the brand color on buttons), the other shades at
/// Tailwind's lightness steps with the base's hue, kept in order around the base.
pub fn scale(base: Rgb) -> [Rgb; 11] {
    let b = base.oklch();
    let mut out = [base; 11];
    for (i, slot) in out.iter_mut().enumerate() {
        if i == 6 {
            continue;
        }
        let steps = i as f64 - 6.0;
        // Tailwind's step, moved by how far the base sits from Tailwind's 600, less so toward
        // the ends (50 and 950 stay near Tailwind's), then kept in order around the base.
        let weight = if i < 6 {
            i as f64 / 6.0
        } else {
            1.0 - (i as f64 - 6.0) / 4.0
        };
        let target = SCALE_L[i] + (b.l - SCALE_L[6]).clamp(-0.06, 0.06) * weight;
        let l = if i < 6 {
            target.max(b.l - 0.035 * steps).min(0.985)
        } else {
            target.min(b.l - 0.035 * steps).max(0.12)
        };
        *slot = Oklch {
            l,
            c: b.c * SCALE_C[i],
            h: b.h,
        }
        .rgb();
    }
    out
}

/// Gray lightness steps (Tailwind 4's `gray`), for a neutral tinted with a hex color's hue.
const NEUTRAL_L: [f64; 11] = [
    0.985, 0.967, 0.928, 0.872, 0.707, 0.551, 0.446, 0.373, 0.278, 0.21, 0.13,
];

/// A neutral scale tinted with `tint`'s hue (chroma capped, so it stays a gray).
pub fn neutral_from(tint: Rgb) -> [Rgb; 11] {
    let t = tint.oklch();
    let c = t.c.min(0.03);
    let ramp = [0.2, 0.3, 0.4, 0.5, 0.7, 0.9, 1.0, 1.0, 1.0, 1.0, 1.0];
    let mut out = [WHITE; 11];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = Oklch {
            l: NEUTRAL_L[i],
            c: c * ramp[i],
            h: t.h,
        }
        .rgb();
    }
    out
}

/// Tailwind's named grays (50–950), the values its utilities have used since v3.
pub fn named_neutral(name: &str) -> Option<[&'static str; 11]> {
    Some(match name {
        "gray" => [
            "#f9fafb", "#f3f4f6", "#e5e7eb", "#d1d5db", "#9ca3af", "#6b7280", "#4b5563", "#374151",
            "#1f2937", "#111827", "#030712",
        ],
        "slate" => [
            "#f8fafc", "#f1f5f9", "#e2e8f0", "#cbd5e1", "#94a3b8", "#64748b", "#475569", "#334155",
            "#1e293b", "#0f172a", "#020617",
        ],
        "zinc" => [
            "#fafafa", "#f4f4f5", "#e4e4e7", "#d4d4d8", "#a1a1aa", "#71717a", "#52525b", "#3f3f46",
            "#27272a", "#18181b", "#09090b",
        ],
        "neutral" => [
            "#fafafa", "#f5f5f5", "#e5e5e5", "#d4d4d4", "#a3a3a3", "#737373", "#525252", "#404040",
            "#262626", "#171717", "#0a0a0a",
        ],
        "stone" => [
            "#fafaf9", "#f5f5f4", "#e7e5e4", "#d6d3d1", "#a8a29e", "#78716c", "#57534e", "#44403c",
            "#292524", "#1c1917", "#0c0a09",
        ],
        _ => return None,
    })
}

/// The nearest color to `color` (same hue and chroma, lightness moved) that reaches `ratio`
/// against `other`, moving lightness in the direction that raises contrast. `None` if none does.
pub fn nearest_passing(color: Rgb, other: Rgb, ratio: f64) -> Option<Rgb> {
    let start = color.oklch();
    let darker = other.luminance() > 0.18;
    let mut best = None;
    for step in 1..=100 {
        let d = f64::from(step) * 0.01;
        let l = if darker { start.l - d } else { start.l + d };
        if !(0.0..=1.0).contains(&l) {
            break;
        }
        let c = (Oklch { l, ..start }).rgb();
        if contrast(c, other) >= ratio {
            best = Some(c);
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(h: &str) -> Rgb {
        Rgb::parse(h).unwrap()
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        assert_eq!(rgb("#4F46E5").hex(), "#4f46e5");
        assert_eq!(rgb("#fff").hex(), "#ffffff");
        for bad in ["4f46e5", "#4f46e", "#ggg", "", "#12345678"] {
            assert!(Rgb::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn contrast_matches_wcag_reference_values() {
        assert!((contrast(WHITE, rgb("#000")) - 21.0).abs() < 1e-9);
        assert!((contrast(rgb("#777"), WHITE) - 4.48).abs() < 0.01);
        assert!((contrast(rgb("#4f46e5"), WHITE) - 6.29).abs() < 0.01);
        assert_eq!(contrast(rgb("#123456"), rgb("#123456")), 1.0);
    }

    #[test]
    fn oklch_round_trips_in_gamut_colors() {
        for h in [
            "#4f46e5", "#0891b2", "#16a34a", "#f59e0b", "#111827", "#ffffff",
        ] {
            assert_eq!(rgb(h).oklch().rgb().hex(), h, "{h}");
        }
    }

    #[test]
    fn a_scale_keeps_the_base_at_600_and_runs_light_to_dark() {
        let s = scale(rgb("#4f46e5"));
        assert_eq!(s[6].hex(), "#4f46e5");
        for w in s.windows(2) {
            assert!(
                w[0].luminance() > w[1].luminance(),
                "{} then {}",
                w[0].hex(),
                w[1].hex()
            );
        }
        // Close to Tailwind's own indigo at the ends.
        assert!(contrast(s[0], rgb("#eef2ff")) < 1.1);
        assert!(contrast(s[10], rgb("#1e1b4b")) < 1.3);
        // A very light brand color still gets an ordered scale.
        let y = scale(rgb("#fde047"));
        assert_eq!(y[6].hex(), "#fde047");
        for w in y.windows(2) {
            // Pinned at the top of the range, neighbors may tie (their chroma differs a little).
            assert!(
                w[0].luminance() >= w[1].luminance() - 0.01,
                "{} then {}",
                w[0].hex(),
                w[1].hex()
            );
        }
    }

    #[test]
    fn neutrals_are_named_or_tinted() {
        assert_eq!(named_neutral("slate").unwrap()[10], "#020617");
        assert!(named_neutral("purple").is_none());
        let n = neutral_from(rgb("#4f46e5"));
        assert!(n[0].luminance() > 0.9 && n[10].luminance() < 0.02);
        assert!(n.iter().all(|c| c.oklch().c < 0.035));
    }

    #[test]
    fn the_nearest_passing_color_passes() {
        let yellow = rgb("#facc15");
        assert!(contrast(yellow, WHITE) < 4.5);
        let fixed = nearest_passing(yellow, WHITE, 4.5).unwrap();
        assert!(contrast(fixed, WHITE) >= 4.5);
        assert!(fixed.luminance() < yellow.luminance());
        let dark_bg = rgb("#030712");
        let fixed = nearest_passing(rgb("#312e81"), dark_bg, 4.5).unwrap();
        assert!(contrast(fixed, dark_bg) >= 4.5);
    }
}
