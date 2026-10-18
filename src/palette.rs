//! Colors a category can be given: a list of 32 to pick from, or any
//! "#RRGGBB". Also how close two colors look, with normal color vision and
//! with each common kind of color blindness, to warn about colors that are
//! hard to tell apart.

use super::*;

/// The colors to pick from: eight hues in four shades, as (id, color on a
/// dark background, color on a light one). Every color stands out from its
/// background by at least 3:1 (the WCAG minimum for graphics); shade 4
/// stands out most.
pub(crate) const COLOR_LIST: [(&str, u32, u32); 32] = [
    ("red-1", 0xe9504d, 0xe24947),
    ("red-2", 0xff726b, 0xc92f33),
    ("red-3", 0xffa098, 0xb00a1d),
    ("red-4", 0xfec8c3, 0x8a0013),
    ("orange-1", 0xd36d02, 0xc96805),
    ("orange-2", 0xf68001, 0xac5701),
    ("orange-3", 0xffa666, 0x8f4802),
    ("orange-4", 0xffcba9, 0x6e3602),
    ("yellow-1", 0xa58a07, 0x9e8406),
    ("yellow-2", 0xc1a20b, 0x867003),
    ("yellow-3", 0xdfbb02, 0x6f5c01),
    ("yellow-4", 0xfdd506, 0x564700),
    ("green-1", 0x1fa836, 0x079f2b),
    ("green-2", 0x43c251, 0x038a24),
    ("green-3", 0x61dc6b, 0x02721c),
    ("green-4", 0x7cf784, 0x005813),
    ("teal-1", 0x04a19b, 0x079a94),
    ("teal-2", 0x03bdb6, 0x05837e),
    ("teal-3", 0x02d9d1, 0x046c68),
    ("teal-4", 0x16f6ed, 0x035350),
    ("blue-1", 0x048ff4, 0x0589e9),
    ("blue-2", 0x52a9fe, 0x0574c7),
    ("blue-3", 0x89c3fe, 0x0060a6),
    ("blue-4", 0xbadbfe, 0x024981),
    ("purple-1", 0xa06aec, 0x9964e5),
    ("purple-2", 0xb688ff, 0x854ece),
    ("purple-3", 0xc9acff, 0x7137b6),
    ("purple-4", 0xdecefe, 0x5b199c),
    ("pink-1", 0xdb509c, 0xd44996),
    ("pink-2", 0xf86ab5, 0xbc3181),
    ("pink-3", 0xff98ca, 0xa4126c),
    ("pink-4", 0xfec4de, 0x820154),
];

/// How many shades each hue of `COLOR_LIST` has (the picker's columns).
pub(crate) const SHADES: usize = 4;

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
        assert_eq!(COLOR_LIST.len() % SHADES, 0);
    }

    #[test]
    fn colors_are_read_and_written() {
        assert_eq!(CatColor::parse("blue-3"), Some(CatColor::Listed(22)));
        let c = CatColor::parse("#1A2b3C").unwrap();
        assert_eq!(c, CatColor::Custom(Color32::from_rgb(0x1a, 0x2b, 0x3c)));
        assert_eq!(c.to_text(), "#1a2b3c");
        assert_eq!(CatColor::parse(" 1a2b3c "), Some(c));
        assert_eq!(CatColor::Listed(22).to_text(), "blue-3");
        for bad in ["", "#12345", "#1234567", "#12345g", "blue-9", "Blue-3"] {
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
