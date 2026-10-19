//! Colors a category can be given: a list of 16 to pick from, or any
//! "#RRGGBB". Also how close two colors look, with normal color vision and
//! with each common kind of color blindness, to warn about colors that are
//! hard to tell apart.

use super::*;

/// The colors to pick from, by hue, as (id, color on a dark background,
/// color on a light one). Every color stands out from its background by
/// at least 3:1 (the WCAG minimum for graphics), and every two stay easy
/// to tell apart, on both backgrounds, with normal color vision and with
/// each common kind of color blindness.
pub(crate) const COLOR_LIST: [(&str, u32, u32); 16] = [
    ("pink", 0xffaabb, 0x81394b),
    ("crimson", 0xcc2244, 0xcc2244),
    ("orange", 0xee6600, 0xe75f00),
    ("amber", 0xeeaa00, 0x9e5e00),
    ("lime", 0x99dd66, 0x1e5c00),
    ("sage", 0x779977, 0x547554),
    ("green", 0x00aa44, 0x00a03a),
    ("cyan", 0x00ccff, 0x0076a6),
    ("steel", 0xbbccee, 0x43506d),
    ("blue", 0x3355ee, 0x1300a8),
    ("periwinkle", 0x9999ee, 0x8382d5),
    ("violet", 0x8866dd, 0x6c47bb),
    ("orchid", 0xcc66ff, 0xc15af3),
    ("purple", 0xaa11cc, 0x6a008a),
    ("magenta", 0xdd0099, 0xdd0099),
    ("rose", 0xcc7799, 0xc46f91),
];

/// How many colors of `COLOR_LIST` the picker shows per row.
pub(crate) const PICKER_COLUMNS: usize = 4;

/// A color picked for a category.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CatColor {
    /// An entry of `COLOR_LIST`, which has a version for each theme.
    Listed(usize),
    /// A color typed as "#RRGGBB", the same in both themes.
    Custom(Color32),
}

impl CatColor {
    /// Reads a color as categories.json stores it: a `COLOR_LIST` id, or
    /// "#RRGGBB" (the "#" is optional, any case).
    pub(crate) fn parse(text: &str) -> Option<CatColor> {
        let text = text.trim();
        if let Some(i) = COLOR_LIST.iter().position(|(id, _, _)| *id == text) {
            return Some(CatColor::Listed(i));
        }
        let hex = text.strip_prefix('#').unwrap_or(text);
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        Some(CatColor::Custom(rgb(v)))
    }

    /// The color as categories.json stores it.
    pub(crate) fn to_text(self) -> String {
        match self {
            CatColor::Listed(i) => COLOR_LIST[i].0.to_string(),
            CatColor::Custom(c) => hex_text(c),
        }
    }

    /// The color on a dark or light background.
    pub(crate) fn on(self, dark: bool) -> Color32 {
        match self {
            CatColor::Listed(i) => {
                let (_, d, l) = COLOR_LIST[i];
                rgb(if dark { d } else { l })
            }
            CatColor::Custom(c) => c,
        }
    }
}

/// "#rrggbb".
pub(crate) fn hex_text(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

pub(crate) fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn linear(v: u8) -> f32 {
    let v = f32::from(v) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Contrast ratio between two colors, from 1 (none) to 21, as in WCAG.
pub(crate) fn contrast(a: Color32, b: Color32) -> f32 {
    let lum = |c: Color32| 0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b());
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// A way of seeing colors.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Vision {
    Normal,
    /// Red-blind.
    Protan,
    /// Green-blind.
    Deutan,
    /// Blue-blind.
    Tritan,
}

impl Vision {
    pub(crate) const COLOR_BLIND: [Vision; 3] = [Vision::Protan, Vision::Deutan, Vision::Tritan];

    /// How linear RGB looks with this kind of color blindness, at full
    /// strength (Machado, Oliveira and Fernandes, 2009).
    fn matrix(self) -> Option<[[f32; 3]; 3]> {
        match self {
            Vision::Normal => None,
            Vision::Protan => Some([
                [0.152286, 1.052583, -0.204868],
                [0.114503, 0.786281, 0.099216],
                [-0.003882, -0.048116, 1.051998],
            ]),
            Vision::Deutan => Some([
                [0.367322, 0.860646, -0.227968],
                [0.280085, 0.672501, 0.047413],
                [-0.011820, 0.042940, 0.968881],
            ]),
            Vision::Tritan => Some([
                [1.255528, -0.076749, -0.178779],
                [-0.078411, 0.930809, 0.147602],
                [0.004733, 0.691367, 0.303900],
            ]),
        }
    }

    /// Below this distance (see `distance`) two colors are hard to tell
    /// apart. The color-blind palette's closest pair stays above the
    /// color-blind limit, and the usual palette's above the normal one.
    fn limit(self) -> f32 {
        match self {
            Vision::Normal => 6.0,
            _ => 3.5,
        }
    }
}

/// The color in Oklab (lightness and two color axes, where distances match
/// how different colors look), as seen with `vision`.
fn oklab(c: Color32, vision: Vision) -> [f32; 3] {
    let mut rgb = [linear(c.r()), linear(c.g()), linear(c.b())];
    if let Some(m) = vision.matrix() {
        rgb = m.map(|row| (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]).clamp(0.0, 1.0));
    }
    let [r, g, b] = rgb;
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// How different two colors look with `vision`, in hundredths of Oklab.
pub(crate) fn distance(a: Color32, b: Color32, vision: Vision) -> f32 {
    let (x, y) = (oklab(a, vision), oklab(b, vision));
    100.0 * ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2)).sqrt()
}

/// The first way of seeing (normal vision, then each kind of color
/// blindness if `color_blind`) in which `a` and `b` are hard to tell apart.
pub(crate) fn too_close(a: Color32, b: Color32, color_blind: bool) -> Option<Vision> {
    let mut visions = vec![Vision::Normal];
    if color_blind {
        visions.extend(Vision::COLOR_BLIND);
    }
    visions.into_iter().find(|&v| distance(a, b, v) < v.limit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Backgrounds of egui's and KDE Breeze's dark and light themes.
    const DARK_BACKGROUNDS: [u32; 2] = [0x1b1b1b, 0x141618];
    const LIGHT_BACKGROUNDS: [u32; 3] = [0xf8f8f8, 0xffffff, 0xeff0f1];

    #[test]
    fn listed_colors_stand_out_from_both_themes() {
        for (id, dark, light) in COLOR_LIST {
            for bg in DARK_BACKGROUNDS {
                assert!(contrast(rgb(dark), rgb(bg)) >= 3.0, "{id} on {bg:06x}");
            }
            for bg in LIGHT_BACKGROUNDS {
                assert!(contrast(rgb(light), rgb(bg)) >= 3.0, "{id} on {bg:06x}");
            }
        }
        let ids: HashSet<&str> = COLOR_LIST.iter().map(|c| c.0).collect();
        assert_eq!(ids.len(), COLOR_LIST.len());
        assert_eq!(COLOR_LIST.len() % PICKER_COLUMNS, 0);
    }

    #[test]
    fn listed_colors_are_easy_to_tell_apart() {
        for dark in [true, false] {
            let other = CategoryModel::other_color(dark);
            let colors: Vec<(&str, Color32)> = (0..COLOR_LIST.len())
                .map(|i| (COLOR_LIST[i].0, CatColor::Listed(i).on(dark)))
                .collect();
            for (i, &(id, color)) in colors.iter().enumerate() {
                assert_eq!(too_close(color, other, true), None, "{id} vs other");
                for &(id2, color2) in &colors[i + 1..] {
                    assert_eq!(
                        too_close(color, color2, true),
                        None,
                        "{id} vs {id2} (dark {dark})"
                    );
                }
            }
        }
    }

    #[test]
    fn colors_are_read_and_written() {
        assert_eq!(CatColor::parse("blue"), Some(CatColor::Listed(9)));
        let c = CatColor::parse("#1A2b3C").unwrap();
        assert_eq!(c, CatColor::Custom(Color32::from_rgb(0x1a, 0x2b, 0x3c)));
        assert_eq!(c.to_text(), "#1a2b3c");
        assert_eq!(CatColor::parse(" 1a2b3c "), Some(c));
        assert_eq!(CatColor::Listed(9).to_text(), "blue");
        for bad in ["", "#12345", "#1234567", "#12345g", "blue-3", "Blue"] {
            assert_eq!(CatColor::parse(bad), None, "{bad}");
        }
        assert_eq!(c.on(true), c.on(false));
        assert_ne!(CatColor::Listed(0).on(true), CatColor::Listed(0).on(false));
    }

    /// The built-in palettes raise no warnings; near-equal colors and pairs
    /// that only color blindness confuses do.
    #[test]
    fn closeness_warnings() {
        for safe in [false, true] {
            for dark in [false, true] {
                let m = CategoryModel::defaults();
                let colors: Vec<Color32> = m.all().map(|c| m.color_in(c, dark, safe)).collect();
                for (i, a) in colors.iter().enumerate() {
                    for b in &colors[i + 1..] {
                        assert_eq!(too_close(*a, *b, safe), None, "{a:?} {b:?} safe={safe}");
                    }
                }
            }
        }
        let red = rgb(0xd62728);
        assert_eq!(too_close(red, rgb(0xd8292a), false), Some(Vision::Normal));
        // Red and green, told apart only with normal color vision.
        let green = rgb(0x2ca02c);
        let red_like = rgb(0xb5651d);
        assert_eq!(too_close(green, red_like, false), None);
        assert!(too_close(green, red_like, true).is_some());
    }
}
