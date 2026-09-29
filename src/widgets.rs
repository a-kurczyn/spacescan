//! Small UI pieces shared by the panels: flat icons, sortable table
//! headers, the details grid, folder stats and the folder picker.

use super::*;

/// Opens the desktop's native folder chooser (XDG portal — the Plasma dialog
/// on KDE, which lists every drive, mount point and folder) on a background
/// thread so the UI keeps repainting while it's open. The result (None if
/// cancelled) arrives on the returned channel.
pub(crate) fn pick_folder_async(start_dir: Option<PathBuf>) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new().set_title(tr("DIALOG_PICK_FOLDER_TITLE"));
        if let Some(dir) = start_dir {
            dialog = dialog.set_directory(dir);
        }
        let _ = tx.send(dialog.pick_folder());
    });
    rx
}

/// Size / file count / permissions of the folder being viewed.
pub(crate) fn folder_stats_ui(ui: &mut egui::Ui, n: &Node) {
    // One line per stat: never wrap (the corner overlay's area is narrow).
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    ui.label(trf("STATS_SIZE", &[&human_size(n.size)]));
    ui.label(trf("STATS_FILES", &[&format_count(n.file_count)]));
    ui.label(trf("STATS_PERMS", &[&format_perms(n.mode)]));
}


/// Draws one clickable, sortable column header. Clicking the currently
/// active column flips its direction; clicking a different column switches
/// to it with a sensible default direction (descending for numeric columns,
/// so "biggest first" without an extra click — ascending for the name
/// column, so alphabetical order reads naturally).
pub(crate) fn sortable_header(ui: &mut egui::Ui, label: &str, column: SortColumn, state: &mut SortState) -> egui::Response {
    let is_active = state.column == column;
    // The direction marker is a hand-drawn chevron rather than a ▲/▼
    // glyph, which egui's bundled font doesn't render cleanly. The button
    // is laid out by hand (empty label + painted text) so there's room for
    // the chevron after the label; inactive columns reserve the same room
    // so headers don't shift width when the sort column changes.
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::TextStyle::Button.resolve(ui.style()),
        Color32::PLACEHOLDER,
    );
    let pad = ui.spacing().button_padding;
    let chevron_w = galley.size().y * 0.55;
    let gap = pad.x * 0.8;
    let size = Vec2::new(pad.x * 2.0 + galley.size().x + gap + chevron_w, (galley.size().y + pad.y * 2.0).max(ui.spacing().interact_size.y));
    let resp = ui.add(egui::Button::new("").min_size(size));
    // The label is painted, not the button's own text: name it for screen
    // readers, with the sort state when this is the sorted column.
    let name = match (is_active, state.ascending) {
        (false, _) => label.to_string(),
        (true, true) => trf("A11Y_SORTED_ASC", &[label]),
        (true, false) => trf("A11Y_SORTED_DESC", &[label]),
    };
    name_for_screen_readers(&resp, &name, Some(is_active));
    let color = ui.style().interact(&resp).text_color();
    let text_pos = Pos2::new(resp.rect.left() + pad.x, resp.rect.center().y - galley.size().y / 2.0);
    // Headers read as bold, like the previous `.strong()` label: overdraw
    // the text half a pixel to the right.
    ui.painter().galley(text_pos, galley.clone(), color);
    ui.painter().galley(text_pos + Vec2::new(0.5, 0.0), galley, color);
    if is_active {
        let cx = resp.rect.right() - pad.x - chevron_w / 2.0;
        draw_chevron(ui.painter(), Pos2::new(cx, resp.rect.center().y), chevron_w / 2.0, state.ascending, color);
    }
    if resp.clicked() {
        if is_active {
            state.ascending = !state.ascending;
        } else {
            state.column = column;
            state.ascending = column.default_ascending();
        }
    }
    resp
}

/// Draws the hub's two-line label — folder name, then size occupied —
/// wrapped to fit inside the hub circle so a long folder name folds onto
/// multiple lines instead of spilling out past the circle's edge.
/// Flat, single-color vector icons drawn with the painter — deliberately
/// not Unicode/emoji glyphs (📄/📁), since those render in full color via
/// the system's emoji font on most Linux setups, clashing with the app's
/// flat, theme-matched look. `color` should track the current theme's text
/// color so the icon stays flat and readable in both light and dark modes.
pub(crate) fn draw_file_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    let fold = rect.width() * 0.35;
    let body = vec![
        rect.left_top(),
        Pos2::new(rect.right() - fold, rect.top()),
        Pos2::new(rect.right(), rect.top() + fold),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    painter.add(egui::Shape::closed_line(body, stroke));
    // Folded corner.
    painter.line_segment(
        [Pos2::new(rect.right() - fold, rect.top()), Pos2::new(rect.right() - fold, rect.top() + fold)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(rect.right() - fold, rect.top() + fold), Pos2::new(rect.right(), rect.top() + fold)],
        stroke,
    );
    // A couple of text lines, to read unambiguously as a document.
    let lx0 = rect.left() + rect.width() * 0.2;
    let lx1 = rect.right() - rect.width() * 0.2;
    for frac in [0.55, 0.72] {
        let y = rect.top() + rect.height() * frac;
        painter.line_segment([Pos2::new(lx0, y), Pos2::new(lx1, y)], stroke);
    }
}

pub(crate) fn draw_folder_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    let tab_h = rect.height() * 0.22;
    let tab_w = rect.width() * 0.45;
    let body_top = rect.top() + tab_h;
    let tab = vec![
        rect.left_top(),
        Pos2::new(rect.left() + tab_w, rect.top()),
        Pos2::new(rect.left() + tab_w + tab_h * 0.6, body_top),
        Pos2::new(rect.left(), body_top),
    ];
    painter.add(egui::Shape::closed_line(tab, stroke));
    let body = egui::Rect::from_min_max(Pos2::new(rect.left(), body_top), rect.right_bottom());
    painter.rect_stroke(body, egui::CornerRadius::from(1u8), stroke, egui::StrokeKind::Outside);
}

/// Table/grid icon for the Summary view toggle — a bordered rect with a
/// header divider and two column dividers, in the same flat single-color
/// style as the file/folder icons above (chosen over the previous 📊 emoji,
/// which rendered in full color via the system emoji font).
pub(crate) fn draw_table_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    painter.rect_stroke(rect, egui::CornerRadius::from(1u8), stroke, egui::StrokeKind::Outside);
    let header_y = rect.top() + rect.height() * 0.32;
    painter.line_segment([Pos2::new(rect.left(), header_y), Pos2::new(rect.right(), header_y)], stroke);
    let col1_x = rect.left() + rect.width() * 0.38;
    let col2_x = rect.left() + rect.width() * 0.69;
    painter.line_segment([Pos2::new(col1_x, header_y), Pos2::new(col1_x, rect.bottom())], stroke);
    painter.line_segment([Pos2::new(col2_x, header_y), Pos2::new(col2_x, rect.bottom())], stroke);
}

/// Sunburst icon for the Chart view button: a ring around a hub, split
/// into a few slices.
pub(crate) fn draw_chart_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0;
    let hub = r * 0.4;
    painter.circle_stroke(c, r, stroke);
    painter.circle_stroke(c, hub, stroke);
    for deg in [0.0f32, 130.0, 230.0] {
        let d = Vec2::angled((deg - 90.0).to_radians());
        painter.line_segment([c + d * hub, c + d * r], stroke);
    }
}

/// Funnel icon for the Filters toggle — same flat single-color style,
/// chosen over the previous ▽ glyph.
pub(crate) fn draw_filter_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    let stem_half = rect.width() * 0.12;
    let cx = rect.center().x;
    let neck_y = rect.top() + rect.height() * 0.55;
    let points = vec![
        rect.left_top(),
        rect.right_top(),
        Pos2::new(cx + stem_half, neck_y),
        Pos2::new(cx + stem_half, rect.bottom()),
        Pos2::new(cx - stem_half, rect.bottom()),
        Pos2::new(cx - stem_half, neck_y),
    ];
    painter.add(egui::Shape::closed_line(points, stroke));
}

/// A "^" / "v" chevron centered on `center`, `half_w` wide on each side —
/// the sort-direction marker for table headers and the chart-order button.
pub(crate) fn draw_chevron(painter: &egui::Painter, center: Pos2, half_w: f32, up: bool, color: Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    let half_h = half_w * 0.55;
    let (tip, arms) = if up { (-half_h, half_h) } else { (half_h, -half_h) };
    painter.add(egui::Shape::line(
        vec![
            Pos2::new(center.x - half_w, center.y + arms),
            Pos2::new(center.x, center.y + tip),
            Pos2::new(center.x + half_w, center.y + arms),
        ],
        stroke,
    ));
}

/// Chart-order icon: a single glyph ("9" for size, "A" for name) with a
/// down chevron beside it — compact enough for a square toolbar button,
/// unlike the previous "9-1" / "A-Z" text labels.
pub(crate) fn draw_sort_order_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32, glyph: &str) {
    // The toolbar icon rect is small; let the pair spill slightly into the
    // button's padding so both parts stay legible.
    let rect = rect.expand2(Vec2::new(rect.width() * 0.2, 0.0));
    let font = egui::FontId::proportional(rect.height() * 1.05);
    let glyph_center = Pos2::new(rect.left() + rect.width() * 0.28, rect.center().y);
    painter.text(glyph_center, egui::Align2::CENTER_CENTER, glyph, font, color);
    let chevron_center = Pos2::new(rect.left() + rect.width() * 0.8, rect.center().y + rect.height() * 0.05);
    draw_chevron(painter, chevron_center, rect.width() * 0.17, false, color);
}

/// Two panes, side by side (`beside`) or stacked — the toggle for where the
/// Summary view's extension table goes.
pub(crate) fn draw_layout_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32, beside: bool) {
    let stroke = egui::Stroke::new(1.3, color);
    painter.rect_stroke(rect, egui::CornerRadius::from(1u8), stroke, egui::StrokeKind::Outside);
    let c = rect.center();
    let divider = if beside {
        [Pos2::new(c.x, rect.top()), Pos2::new(c.x, rect.bottom())]
    } else {
        [Pos2::new(rect.left(), c.y), Pos2::new(rect.right(), c.y)]
    };
    painter.line_segment(divider, stroke);
}

/// A toolbar button whose face is a hand-drawn flat icon (via `draw`)
/// instead of text/emoji — an empty-label `Button` for correct
/// hit-testing/hover/selected styling, with the icon painted over it
/// afterward using the same resolved color the button would have used for
/// text in that state (idle/hovered/selected), so it blends in exactly
/// like a normal labeled button would.
/// `name` is both its tooltip and its name for screen readers (the drawn
/// icon gives it no text of its own).
pub(crate) fn icon_toolbar_button(
    ui: &mut egui::Ui,
    selected: bool,
    enabled: bool,
    name: &str,
    draw: impl FnOnce(&egui::Painter, egui::Rect, Color32),
) -> egui::Response {
    let size = ui.spacing().interact_size.y;
    let resp = ui.add_enabled(enabled, egui::Button::new("").selected(selected).min_size(Vec2::splat(size)));
    let color = ui.style().interact_selectable(&resp, selected).text_color();
    let icon_rect = resp.rect.shrink(resp.rect.width() * 0.24);
    draw(ui.painter(), icon_rect, color);
    name_for_screen_readers(&resp, name, Some(selected));
    resp.on_hover_text(name)
}

/// `.named(x)` on a glyph button (🔍, ⟳, ⚙ …): `x` becomes its tooltip and
/// its name for screen readers, which would otherwise read the glyph.
pub(crate) trait Named {
    fn named(self, name: &str) -> Self;
}

impl Named for egui::Response {
    fn named(self, name: &str) -> Self {
        name_for_screen_readers(&self, name, None);
        self.on_hover_text(name)
    }
}

/// Gives a button drawn without text (a glyph or painted icon) a name for
/// screen readers and UI automation; `selected` for toggle buttons.
pub(crate) fn name_for_screen_readers(resp: &egui::Response, name: &str, selected: Option<bool>) {
    let enabled = resp.enabled();
    resp.widget_info(|| match selected {
        Some(sel) => egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, sel, name),
        None => egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name),
    });
}

/// Size / counts / permissions / times / owner / type of one item, as a
/// two-column grid — shared by the chart's hover tooltip and the Summary
/// table's details panel.
pub(crate) fn details_grid(
    ui: &mut egui::Ui,
    id: egui::Id,
    h: &HoverInfo,
    mime: Option<&str>,
    user_cache: &mut HashMap<u32, String>,
    group_cache: &mut HashMap<u32, String>,
) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label(if h.is_free { tr("HOVER_AVAILABLE") } else { tr("HOVER_SIZE") });
            ui.label(human_size(h.size));
            ui.end_row();

            // Always 1 for a real file, always 0 for free space — neither is
            // informative, so only show it for folders and the aggregate
            // "other" bucket.
            if h.is_dir {
                ui.label(tr("HOVER_FILES"));
                ui.label(format_count(h.file_count));
                ui.end_row();
            }

            if let Some(m) = h.mode {
                ui.label(tr("HOVER_PERMS"));
                ui.label(format_perms(m));
                ui.end_row();
                // A symbolic link: say so, and where it points (its target
                // isn't part of any total).
                if m & 0o170000 == 0o120000 {
                    ui.label(tr("HOVER_LINK"));
                    ui.label(match std::fs::read_link(&h.path) {
                        Ok(target) => trf("HOVER_LINK_TARGET", &[&show_path(&target)]),
                        Err(_) => tr("HOVER_LINK_UNREADABLE"),
                    });
                    ui.end_row();
                }
            }
            if let Some(mt) = h.mtime {
                ui.label(tr("HOVER_MODIFIED"));
                ui.label(format_epoch(mt));
                ui.end_row();
            }
            if let Some(ct) = h.ctime {
                ui.label(tr("HOVER_CHANGED"));
                ui.label(format_epoch(ct));
                ui.end_row();
            }
            if let (Some(uid), Some(gid)) = (h.uid, h.gid) {
                ui.label(tr("HOVER_OWNER"));
                ui.label(format_owner(uid, gid, user_cache, group_cache));
                ui.end_row();
            }
            if let Some(mime) = mime {
                ui.label(tr("HOVER_TYPE"));
                ui.label(mime);
                ui.end_row();
            }
        });
}

/// `text` cut to fit `width` by replacing its middle with "…", so both the
/// start and the end (where error messages put the reason) stay visible.
pub(crate) fn elide_middle(ui: &egui::Ui, text: &str, font: &egui::FontId, width: f32) -> String {
    let fits = |s: &str| ui.fonts_mut(|f| f.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x) <= width;
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // Keep this many characters, split 40/60 between start and end (the
    // end carries the reason); binary search for the most that fit.
    let cut = |keep: usize| -> String {
        let head = keep * 2 / 5;
        let tail = keep - head;
        let mut s: String = chars[..head].iter().collect();
        s.push('…');
        s.extend(&chars[chars.len() - tail..]);
        s
    };
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if fits(&cut(mid)) { lo = mid } else { hi = mid - 1 }
    }
    cut(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painted_buttons_have_accessible_names() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut state = SortState { column: SortColumn::Size, ascending: false };
        let mut run = || {
            ctx.run_ui(Default::default(), |ui| {
                sortable_header(ui, "Size", SortColumn::Size, &mut state);
                icon_toolbar_button(ui, true, true, "Chart view", draw_chart_icon);
                let _ = ui.button("⟳").named("Rescan");
            })
        };
        let _ = run();
        let out = run();
        let update = out.platform_output.accesskit_update.expect("accesskit output");
        let labels: Vec<String> = update.nodes.iter().filter_map(|(_, n)| n.label().map(str::to_string)).collect();
        eprintln!("{labels:?}");
        assert!(labels.iter().any(|l| l.contains("Size") && l.contains("descending")));
        assert!(labels.iter().any(|l| l == "Chart view"));
        assert!(labels.iter().any(|l| l == "Rescan"));
    }
}
