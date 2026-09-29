//! Colors from the desktop's (KDE) color scheme, plus structural styling.

use super::*;

// ---------------- Theming ----------------
//
// Colors are never hardcoded: they're read from the user's actual desktop
// color scheme (KDE Plasma's kdeglobals) so the app matches whatever theme
// and accent color the user picked in System Settings, light or dark,
// rather than imposing a fixed palette. Only *structural* polish (corner
// rounding, spacing) is applied on top — that's theme-agnostic by nature.

pub(crate) struct KdeColors {
    pub(crate) window_bg: Color32,
    pub(crate) view_bg: Color32,
    pub(crate) text: Color32,
    pub(crate) accent: Color32,
    pub(crate) button_bg: Color32,
}

pub(crate) fn parse_rgb(s: &str) -> Option<Color32> {
    let mut parts = s.trim().split(',');
    let r: u8 = parts.next()?.trim().parse().ok()?;
    let g: u8 = parts.next()?.trim().parse().ok()?;
    let b: u8 = parts.next()?.trim().parse().ok()?;
    Some(Color32::from_rgb(r, g, b))
}

pub(crate) fn read_kde_colors() -> Option<KdeColors> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".config/kdeglobals");
    let content = std::fs::read_to_string(path).ok()?;

    let mut section = String::new();
    let (mut window_bg, mut view_bg, mut text, mut accent, mut button_bg) =
        (None, None, None, None, None);

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
            ("[Colors:Button]", "BackgroundNormal") => button_bg = parse_rgb(value),
            _ => {}
        }
    }

    let window_bg = window_bg?;
    Some(KdeColors {
        view_bg: view_bg.unwrap_or(window_bg),
        text: text.unwrap_or(Color32::WHITE),
        accent: accent?,
        button_bg: button_bg.unwrap_or(window_bg),
        window_bg,
    })
}

/// Structural-only style refinements — corner rounding and spacing — that
/// read as "designed" regardless of which color scheme is active.
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

pub(crate) fn apply_theme(ctx: &egui::Context) {
    apply_structural_style(ctx);
    let Some(kde) = read_kde_colors() else {
        return; // not on KDE (or couldn't read it): keep egui's own default
    };
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(kde.text);
    visuals.panel_fill = kde.view_bg;
    visuals.window_fill = kde.window_bg;
    // Used as the empty "trough" for progress bars, sliders, text-edit
    // backgrounds, etc. Needs to read as visibly *lighter* than the panel
    // behind it on a dark theme — darker (as `gamma_multiply(0.85)` gave)
    // was nearly invisible, leaving no visible container/boundary for
    // things like the scan progress bar to fill up against.
    visuals.extreme_bg_color = gamma_lighten(kde.view_bg, 0.55);
    visuals.faint_bg_color = kde.window_bg.gamma_multiply(1.1);
    visuals.hyperlink_color = kde.accent;
    visuals.selection.bg_fill = kde.accent;
    visuals.selection.stroke.color = kde.text;
    visuals.widgets.inactive.bg_fill = kde.button_bg;
    visuals.widgets.inactive.weak_bg_fill = kde.button_bg;
    visuals.widgets.hovered.bg_fill = kde.button_bg.gamma_multiply(1.25);
    visuals.widgets.active.bg_fill = kde.accent;
    visuals.widgets.noninteractive.bg_fill = kde.window_bg;
    ctx.set_visuals(visuals);
}

/// egui's built-in fonts have no Chinese/Japanese/Korean glyphs, so such
/// names drew as boxes. This adds a system font that has them (found via
/// fontconfig) as the last fallback. The compact Droid Sans Fallback (~4 MB)
/// is preferred over the full CJK collections (30 MB+, all kept in memory).
pub(crate) fn add_cjk_fallback_font(ctx: &egui::Context) {
    let find = |pattern: &str| -> Option<PathBuf> {
        let out = std::process::Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
        let path = PathBuf::from(String::from_utf8(out.stdout).ok()?);
        path.is_file().then_some(path)
    };
    let font = find("Droid Sans Fallback")
        .filter(|p| p.to_string_lossy().contains("DroidSansFallback"))
        .or_else(|| find("sans-serif:lang=ja"));
    let Some(bytes) = font.and_then(|p| std::fs::read(p).ok()) else { return };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("cjk_fallback".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("cjk_fallback".into());
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_names_have_glyphs() {
        let ctx = egui::Context::default();
        add_cjk_fallback_font(&ctx);
        let _ = ctx.run_ui(Default::default(), |_| {});
        let font = egui::FontId::proportional(14.0);
        let has = |c: char| ctx.fonts_mut(|f| f.has_glyph(&font, c));
        assert!(has('ü'));
        // Only meaningful where a CJK font is installed (as on the dev box).
        if std::process::Command::new("fc-list").arg(":lang=ja").output().is_ok_and(|o| !o.stdout.is_empty()) {
            assert!(has('日') && has('本') && has('語'));
        }
    }
}
