//! Look and feel: colors from the KDE color scheme when there is one,
//! corner rounding and spacing, and fallback fonts for CJK names.

use super::*;

/// The colors spacescan takes from KDE's color scheme.
pub(crate) struct KdeColors {
    pub(crate) window_bg: Color32,
    pub(crate) view_bg: Color32,
    pub(crate) text: Color32,
    pub(crate) accent: Color32,
    /// Text on the selection color.
    pub(crate) accent_text: Option<Color32>,
    pub(crate) button_bg: Color32,
}

/// "r,g,b" as a color.
pub(crate) fn parse_rgb(s: &str) -> Option<Color32> {
    let mut parts = s.trim().split(',');
    let r: u8 = parts.next()?.trim().parse().ok()?;
    let g: u8 = parts.next()?.trim().parse().ok()?;
    let b: u8 = parts.next()?.trim().parse().ok()?;
    Some(Color32::from_rgb(r, g, b))
}

/// The colors from ~/.config/kdeglobals, if it's there.
pub(crate) fn read_kde_colors() -> Option<KdeColors> {
    parse_kde_colors(&std::fs::read_to_string(home_dir().join(".config/kdeglobals")).ok()?)
}

/// The colors in a KDE color scheme file (kdeglobals or *.colors).
pub(crate) fn parse_kde_colors(content: &str) -> Option<KdeColors> {
    let mut section = String::new();
    let (mut window_bg, mut view_bg, mut text, mut accent, mut accent_text, mut button_bg) =
        (None, None, None, None, None, None);

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line.to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (section.as_str(), key) {
            ("[Colors:Window]", "BackgroundNormal") => window_bg = parse_rgb(value),
            ("[Colors:Window]", "ForegroundNormal") => text = parse_rgb(value),
            ("[Colors:View]", "BackgroundNormal") => view_bg = parse_rgb(value),
            ("[Colors:Selection]", "BackgroundNormal") => accent = parse_rgb(value),
            ("[Colors:Selection]", "ForegroundNormal") => accent_text = parse_rgb(value),
            ("[Colors:Button]", "BackgroundNormal") => button_bg = parse_rgb(value),
            _ => {}
        }
    }

    let window_bg = window_bg?;
    let view_bg = view_bg.unwrap_or(window_bg);
    let default_text = if is_light(view_bg) {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    Some(KdeColors {
        view_bg,
        text: text.unwrap_or(default_text),
        accent: accent?,
        accent_text,
        button_bg: button_bg.unwrap_or(window_bg),
        window_bg,
    })
}

/// True for a light color (relative luminance above one half).
pub(crate) fn is_light(c: Color32) -> bool {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b()) > 0.5
}

/// Corner rounding and spacing, the same for every color scheme.
pub(crate) fn apply_structural_style(ctx: &egui::Context) {
    let radius = egui::CornerRadius::from(6u8);
    ctx.all_styles_mut(|style| {
        style.visuals.window_corner_radius = radius;
        style.visuals.menu_corner_radius = radius;
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.open.corner_radius = radius;
        style.spacing.item_spacing = egui::Vec2::new(8.0, 8.0);
        style.spacing.button_padding = egui::Vec2::new(10.0, 5.0);
        style.spacing.window_margin = egui::Margin::same(10);
    });
}

/// Applies the style, and KDE's colors when available (else egui's own).
pub(crate) fn apply_theme(ctx: &egui::Context) {
    apply_structural_style(ctx);
    if let Some(kde) = read_kde_colors() {
        ctx.set_visuals(kde_visuals(&kde));
    }
}

/// egui's look in KDE's colors: its light or dark style (whichever matches
/// the scheme's background), recolored.
pub(crate) fn kde_visuals(kde: &KdeColors) -> egui::Visuals {
    let light = is_light(kde.view_bg);
    let mut visuals = if light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    visuals.override_text_color = Some(kde.text);
    visuals.panel_fill = kde.view_bg;
    visuals.window_fill = kde.window_bg;
    // The empty part of progress bars, sliders and text fields stands out
    // from the panel: darker on a light scheme, lighter on a dark one.
    visuals.extreme_bg_color = if light {
        kde.view_bg.gamma_multiply(0.92)
    } else {
        gamma_lighten(kde.view_bg, 0.55)
    };
    let shade = |c: Color32, amount: f32| {
        if light {
            c.gamma_multiply(1.0 - amount)
        } else {
            c.gamma_multiply(1.0 + amount)
        }
    };
    visuals.faint_bg_color = shade(kde.window_bg, 0.1);
    visuals.hyperlink_color = kde.accent;
    visuals.selection.bg_fill = kde.accent;
    visuals.selection.stroke.color = kde.accent_text.unwrap_or(kde.text);
    visuals.widgets.inactive.bg_fill = kde.button_bg;
    visuals.widgets.inactive.weak_bg_fill = kde.button_bg;
    visuals.widgets.hovered.bg_fill = shade(kde.button_bg, 0.25);
    visuals.widgets.hovered.weak_bg_fill = shade(kde.button_bg, 0.15);
    visuals.widgets.active.bg_fill = kde.accent;
    visuals.widgets.noninteractive.bg_fill = kde.window_bg;
    visuals
}

/// A system font file (and face index, for collections) matching the
/// fontconfig `pattern`, or None.
fn system_font(pattern: &str) -> Option<(PathBuf, u32)> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}|%{index}", pattern])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let (file, index) = text.split_once('|')?;
    let file = PathBuf::from(file);
    // For variable fonts fontconfig puts a named-instance number in the high
    // 16 bits; the face within the collection is the low 16.
    let index = index.trim().parse::<u32>().unwrap_or(0) & 0xFFFF;
    file.is_file().then_some((file, index))
}

/// Adds system fonts with Chinese and Japanese glyphs (which egui's fonts
/// lack) as fallbacks, and a Korean one when `korean`. Korean fonts are
/// large (30 MB+ in memory), so they're loaded only once a scan finds a
/// Korean name.
pub(crate) fn install_fallback_fonts(ctx: &egui::Context, korean: bool) {
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |id: &str, font: Option<(PathBuf, u32)>| {
        let Some((path, index)) = font else { return };
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let mut data = egui::FontData::from_owned(bytes);
        data.index = index;
        fonts.font_data.insert(id.into(), std::sync::Arc::new(data));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(id.into());
        }
    };
    let cjk = system_font("Droid Sans Fallback")
        .filter(|(p, _)| p.to_string_lossy().contains("DroidSansFallback"))
        .or_else(|| system_font("sans-serif:lang=ja"));
    add("cjk_fallback", cjk);
    if korean {
        add("korean_fallback", system_font("sans-serif:lang=ko"));
    }
    ctx.set_fonts(fonts);
}

/// True for Korean script (Hangul syllables and jamo).
pub(crate) fn is_hangul(c: char) -> bool {
    matches!(c, '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}' | '\u{A960}'..='\u{A97F}' | '\u{AC00}'..='\u{D7FF}')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contrast ratio (1 to 21) between two colors, as in WCAG.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let lin = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    const BREEZE_LIGHT: &str = "[Colors:Button]\nBackgroundNormal=252,252,252\n[Colors:Selection]\n\
        BackgroundNormal=61,174,233\nForegroundNormal=255,255,255\n[Colors:View]\nBackgroundNormal=255,255,255\n\
        [Colors:Window]\nBackgroundNormal=239,240,241\nForegroundNormal=35,38,41\n";
    const BREEZE_DARK: &str = "[Colors:Button]\nBackgroundNormal=41,44,48\n[Colors:Selection]\n\
        BackgroundNormal=61,174,233\nForegroundNormal=252,252,252\n[Colors:View]\nBackgroundNormal=20,22,24\n\
        [Colors:Window]\nBackgroundNormal=32,35,38\nForegroundNormal=252,252,252\n";

    /// Light and dark schemes both give readable text, strong text and
    /// selected text.
    #[test]
    fn kde_schemes_stay_readable() {
        for (scheme, light) in [(BREEZE_LIGHT, true), (BREEZE_DARK, false)] {
            let v = kde_visuals(&parse_kde_colors(scheme).unwrap());
            assert_eq!(v.dark_mode, !light);
            assert!(contrast(v.text_color(), v.panel_fill) > 7.0);
            assert!(
                contrast(v.strong_text_color(), v.panel_fill) > 7.0,
                "strong text, light={light}"
            );
            assert!(
                contrast(v.weak_text_color(), v.panel_fill) > 2.5,
                "weak text, light={light}"
            );
            assert!(contrast(v.selection.stroke.color, v.selection.bg_fill) > 2.0);
        }
    }

    #[test]
    fn cjk_names_have_glyphs() {
        let ctx = egui::Context::default();
        install_fallback_fonts(&ctx, false);
        let _ = ctx.run_ui(Default::default(), |_| {});
        let font = egui::FontId::proportional(14.0);
        let has = |c: char| ctx.fonts_mut(|f| f.has_glyph(&font, c));
        assert!(has('ü') && has('Ж') && has('é'));
        // Only checked where a CJK font is installed.
        if std::process::Command::new("fc-list")
            .arg(":lang=ja")
            .output()
            .is_ok_and(|o| !o.stdout.is_empty())
        {
            assert!(has('日') && has('本') && has('語'));
        }
        if std::process::Command::new("fc-list")
            .arg(":lang=ko")
            .output()
            .is_ok_and(|o| !o.stdout.is_empty())
        {
            assert!(!has('한'));
            install_fallback_fonts(&ctx, true);
            let _ = ctx.run_ui(Default::default(), |_| {});
            assert!(has('한') && has('국') && has('어'));
            assert!(has('日'));
        }
    }
}
