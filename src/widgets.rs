//! Small UI pieces shared by the panels: flat icons, sortable table
//! headers, the details grid, folder stats and the folder picker.

use super::*;

/// Opens the desktop's folder chooser (XDG portal) on a background thread,
/// so the UI keeps running while it's open. The chosen folder, or None if
/// cancelled, arrives on the returned channel.
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
    // One line per stat, never wrapped.
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    ui.label(trf("STATS_SIZE", &[&human_size(n.size)]));
    ui.label(trf("STATS_FILES", &[&format_count(n.file_count)]));
    ui.label(trf("STATS_PERMS", &[&format_perms(n.mode)]));
}

/// A clickable column header. Clicking the sorted column reverses it;
/// clicking another sorts by it, largest first (A–Z for names).
pub(crate) fn sortable_header(
    ui: &mut egui::Ui,
    label: &str,
    column: SortColumn,
    state: &mut SortState,
) -> egui::Response {
    let is_active = state.column == column;
    // Label and direction chevron are painted over an empty button. Every
    // header reserves room for the chevron, so widths don't jump when the
    // sort column changes.
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::TextStyle::Button.resolve(ui.style()),
        Color32::PLACEHOLDER,
    );
    let pad = ui.spacing().button_padding;
    let chevron_w = galley.size().y * 0.55;
    let gap = pad.x * 0.8;
    let size = Vec2::new(
        pad.x * 2.0 + galley.size().x + gap + chevron_w,
        (galley.size().y + pad.y * 2.0).max(ui.spacing().interact_size.y),
    );
    let resp = ui.add(egui::Button::new("").min_size(size));
    // The name screen readers announce, with the sort direction if sorted.
    let name = match (is_active, state.ascending) {
        (false, _) => label.to_string(),
        (true, true) => trf("A11Y_SORTED_ASC", &[label]),
        (true, false) => trf("A11Y_SORTED_DESC", &[label]),
    };
    name_for_screen_readers(&resp, &name, Some(is_active));
    let color = ui.style().interact(&resp).text_color();
    let text_pos = Pos2::new(
        resp.rect.left() + pad.x,
        resp.rect.center().y - galley.size().y / 2.0,
    );
    // Bold look: the text drawn twice, half a pixel apart.
    ui.painter().galley(text_pos, galley.clone(), color);
    ui.painter()
        .galley(text_pos + Vec2::new(0.5, 0.0), galley, color);
    if is_active {
        let cx = resp.rect.right() - pad.x - chevron_w / 2.0;
        draw_chevron(
            ui.painter(),
            Pos2::new(cx, resp.rect.center().y),
            chevron_w / 2.0,
            state.ascending,
            color,
        );
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

/// File icon. Icons are drawn as flat single-color shapes in `color`
/// (normally the text color), since emoji glyphs render in full color.
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
        [
            Pos2::new(rect.right() - fold, rect.top()),
            Pos2::new(rect.right() - fold, rect.top() + fold),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(rect.right() - fold, rect.top() + fold),
            Pos2::new(rect.right(), rect.top() + fold),
        ],
        stroke,
    );
    // Text lines.
    let lx0 = rect.left() + rect.width() * 0.2;
    let lx1 = rect.right() - rect.width() * 0.2;
    for frac in [0.55, 0.72] {
        let y = rect.top() + rect.height() * frac;
        painter.line_segment([Pos2::new(lx0, y), Pos2::new(lx1, y)], stroke);
    }
}

/// Scroll bars that stay on screen beside the content: wide enough to
/// grab, with a clearly visible track and a handle in the text color.
pub(crate) fn steady_scroll_style() -> egui::style::ScrollStyle {
    egui::style::ScrollStyle {
        bar_width: 10.0,
        handle_min_length: 40.0,
        foreground_color: true,
        dormant_background_opacity: 0.8,
        active_background_opacity: 0.9,
        interact_background_opacity: 1.0,
        dormant_handle_opacity: 0.5,
        active_handle_opacity: 0.7,
        interact_handle_opacity: 0.9,
        ..egui::style::ScrollStyle::solid()
    }
}

/// A function that paints an icon into a rectangle in a color.
pub(crate) type DrawIcon = fn(&egui::Painter, egui::Rect, Color32);

/// Categories icon: a narrow bar split into three uneven segments, the top
/// one filled, like a small category bar.
pub(crate) fn draw_category_bar_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    let w = rect.width() * 0.45;
    let bar = egui::Rect::from_center_size(rect.center(), Vec2::new(w, rect.height()));
    let split1 = bar.top() + bar.height() * 0.45;
    let split2 = bar.top() + bar.height() * 0.75;
    painter.rect_filled(
        egui::Rect::from_min_max(bar.left_top(), Pos2::new(bar.right(), split1)),
        egui::CornerRadius::from(1u8),
        color,
    );
    painter.rect_stroke(
        bar,
        egui::CornerRadius::from(1u8),
        stroke,
        egui::StrokeKind::Inside,
    );
    painter.line_segment(
        [
            Pos2::new(bar.left(), split2),
            Pos2::new(bar.right(), split2),
        ],
        stroke,
    );
}

/// Extensions icon: a file with a round tag on its corner.
pub(crate) fn draw_extension_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let file = egui::Rect::from_min_max(rect.left_top(), rect.right_bottom() - rect.size() * 0.15);
    draw_file_icon(painter, file, color);
    painter.circle_filled(
        rect.right_bottom() - rect.size() * 0.12,
        rect.width() * 0.2,
        color,
    );
}

/// Folder contents icon: a top line with two indented lines hanging from
/// it, like a folder tree.
pub(crate) fn draw_tree_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    let x = |f: f32| rect.left() + rect.width() * f;
    let y = |f: f32| rect.top() + rect.height() * f;
    painter.line_segment(
        [Pos2::new(x(0.1), y(0.2)), Pos2::new(x(0.9), y(0.2))],
        stroke,
    );
    // The trunk, from under the top line's start to the last branch.
    painter.line_segment(
        [Pos2::new(x(0.25), y(0.2)), Pos2::new(x(0.25), y(0.8))],
        stroke,
    );
    for f in [0.5, 0.8] {
        painter.line_segment([Pos2::new(x(0.25), y(f)), Pos2::new(x(0.45), y(f))], stroke);
        painter.line_segment([Pos2::new(x(0.55), y(f)), Pos2::new(x(0.9), y(f))], stroke);
    }
}

/// Flat list icon: four even lines.
pub(crate) fn draw_flat_list_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    for f in [0.2, 0.4, 0.6, 0.8] {
        let y = rect.top() + rect.height() * f;
        painter.line_segment(
            [
                Pos2::new(rect.left() + rect.width() * 0.1, y),
                Pos2::new(rect.right() - rect.width() * 0.1, y),
            ],
            stroke,
        );
    }
}

/// Folder icon.
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
    painter.rect_stroke(
        body,
        egui::CornerRadius::from(1u8),
        stroke,
        egui::StrokeKind::Outside,
    );
}

/// Table icon for the Summary view button.
pub(crate) fn draw_table_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::from(1u8),
        stroke,
        egui::StrokeKind::Outside,
    );
    let header_y = rect.top() + rect.height() * 0.32;
    painter.line_segment(
        [
            Pos2::new(rect.left(), header_y),
            Pos2::new(rect.right(), header_y),
        ],
        stroke,
    );
    let col1_x = rect.left() + rect.width() * 0.38;
    let col2_x = rect.left() + rect.width() * 0.69;
    painter.line_segment(
        [
            Pos2::new(col1_x, header_y),
            Pos2::new(col1_x, rect.bottom()),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(col2_x, header_y),
            Pos2::new(col2_x, rect.bottom()),
        ],
        stroke,
    );
}

/// Sunburst icon for the Chart view button.
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

/// Funnel icon for the Filters button.
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

/// An up or down chevron centered on `center`, `half_w` wide on each side.
pub(crate) fn draw_chevron(
    painter: &egui::Painter,
    center: Pos2,
    half_w: f32,
    up: bool,
    color: Color32,
) {
    let stroke = egui::Stroke::new(1.5, color);
    let half_h = half_w * 0.55;
    let (tip, arms) = if up {
        (-half_h, half_h)
    } else {
        (half_h, -half_h)
    };
    painter.add(egui::Shape::line(
        vec![
            Pos2::new(center.x - half_w, center.y + arms),
            Pos2::new(center.x, center.y + tip),
            Pos2::new(center.x + half_w, center.y + arms),
        ],
        stroke,
    ));
}

/// Chart-order icon: "9" (by size) or "A" (by name) with a down chevron.
pub(crate) fn draw_sort_order_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    color: Color32,
    glyph: &str,
) {
    // Slightly wider than the icon area, into the button's padding.
    let rect = rect.expand2(Vec2::new(rect.width() * 0.2, 0.0));
    let font = egui::FontId::proportional(rect.height() * 1.05);
    let glyph_center = Pos2::new(rect.left() + rect.width() * 0.28, rect.center().y);
    painter.text(
        glyph_center,
        egui::Align2::CENTER_CENTER,
        glyph,
        font,
        color,
    );
    let chevron_center = Pos2::new(
        rect.left() + rect.width() * 0.8,
        rect.center().y + rect.height() * 0.05,
    );
    draw_chevron(painter, chevron_center, rect.width() * 0.17, false, color);
}

/// What a square button is, for screen readers.
#[derive(Clone, Copy)]
pub(crate) enum ButtonRole {
    /// A one-shot action; `lit` highlights it (e.g. the current scan target).
    Action { lit: bool },
    /// An on/off control, announced as a toggle.
    Toggle { on: bool },
}

/// A toolbar toggle showing an icon painted by `draw`, in the color the
/// button would give its text. `name` is its tooltip and its name for
/// screen readers.
pub(crate) fn icon_toolbar_button(
    ui: &mut egui::Ui,
    selected: bool,
    enabled: bool,
    name: &str,
    draw: impl FnOnce(&egui::Painter, egui::Rect, Color32),
) -> egui::Response {
    square_button(ui, ButtonRole::Toggle { on: selected }, enabled, name, draw)
}

/// A square button of `tool_button_size` with an icon painted by `draw`.
fn square_button(
    ui: &mut egui::Ui,
    role: ButtonRole,
    enabled: bool,
    name: &str,
    draw: impl FnOnce(&egui::Painter, egui::Rect, Color32),
) -> egui::Response {
    let (lit, toggle_state) = match role {
        ButtonRole::Action { lit } => (lit, None),
        ButtonRole::Toggle { on } => (on, Some(on)),
    };
    // Only a toggle has a selected state (screen readers announce it as a
    // toggle button); a lit action is highlighted by hand instead.
    let mut button = egui::Button::new("").min_size(tool_button_size(ui));
    if let Some(on) = toggle_state {
        button = button.selected(on);
    }
    let resp = ui.add_enabled(enabled, button);
    if lit && toggle_state.is_none() {
        let fill = ui.visuals().selection.bg_fill;
        let radius = ui.visuals().widgets.active.corner_radius;
        ui.painter().rect_filled(resp.rect, radius, fill);
    }
    let color = ui.style().interact_selectable(&resp, lit).text_color();
    let icon_rect = resp.rect.shrink(resp.rect.width() * 0.24);
    draw(ui.painter(), icon_rect, color);
    name_for_screen_readers(&resp, name, toggle_state);
    resp.on_hover_text(name)
}

/// The size of every toolbar and panel button: a square as tall as a
/// text button, so glyph and painted icons line up.
pub(crate) fn tool_button_size(ui: &egui::Ui) -> Vec2 {
    Vec2::splat(
        ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y,
    )
}

/// A toolbar button showing `glyph` (🔍, ⚙, …) centered in the same square
/// as the painted icons. `name` is its tooltip and its name for screen
/// readers.
pub(crate) fn glyph_toolbar_button(
    ui: &mut egui::Ui,
    role: ButtonRole,
    enabled: bool,
    glyph: &str,
    name: &str,
) -> egui::Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    square_button(ui, role, enabled, name, |painter, rect, color| {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            font,
            color,
        );
    })
}

/// `.named(x)` on a glyph button (🔍, ⟳, ⚙ …): `x` becomes its tooltip and
/// its name for screen readers.
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

/// Size, counts, permissions, times, owner and type of one item as a
/// two-column grid (chart tooltip and table details panel).
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
            ui.label(if h.is_free {
                tr("HOVER_AVAILABLE")
            } else {
                tr("HOVER_SIZE")
            });
            ui.label(human_size(h.size));
            ui.end_row();

            // File count: shown for folders and "other" only.
            if h.is_dir {
                ui.label(tr("HOVER_FILES"));
                ui.label(format_count(h.file_count));
                ui.end_row();
            }

            if let Some(m) = h.mode {
                // Symbolic link: where it points (not counted in sizes).
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
            // Folders and "other": the newest and oldest file inside, by the
            // Changed time the slice colors use.
            if let Some((newest, oldest)) = h.age_range {
                for (key, t) in [("HOVER_NEWEST", newest), ("HOVER_OLDEST", oldest)] {
                    if t != NO_TIME {
                        ui.label(tr(key)).on_hover_text(tr("HOVER_AGE_RANGE_HINT"));
                        ui.label(format_epoch(t));
                        ui.end_row();
                    }
                }
            }
            if let Some(m) = h.mode {
                ui.label(tr("HOVER_PERMS"));
                ui.label(format_perms(m));
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
    // Cut very long texts first; no line fits more than a few hundred
    // characters.
    let text = &shorten_middle(text, 600);
    let fits = |s: &str| {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE)
                .size()
                .x
        }) <= width
    };
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // Binary search for how many characters fit, kept 40% from the start
    // and 60% from the end.
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
        let mid = (lo + hi).div_ceil(2);
        if fits(&cut(mid)) {
            lo = mid
        } else {
            hi = mid - 1
        }
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
        let mut state = SortState {
            column: SortColumn::Size,
            ascending: false,
        };
        let mut run = || {
            ctx.run_ui(Default::default(), |ui| {
                sortable_header(ui, "Size", SortColumn::Size, &mut state);
                icon_toolbar_button(ui, true, true, "Chart view", draw_chart_icon);
                glyph_toolbar_button(ui, ButtonRole::Action { lit: true }, true, "⟳", "Rescan");
            })
        };
        let _ = run();
        let out = run();
        let update = out
            .platform_output
            .accesskit_update
            .expect("accesskit output");
        let labels: Vec<String> = update
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().map(str::to_string))
            .collect();
        eprintln!("{labels:?}");
        assert!(
            labels
                .iter()
                .any(|l| l.contains("Size") && l.contains("descending"))
        );
        assert!(labels.iter().any(|l| l == "Chart view"));
        assert!(labels.iter().any(|l| l == "Rescan"));
        // Toggles report their state; actions (even lit ones) don't.
        let node = |label: &str| {
            update
                .nodes
                .iter()
                .find(|(_, n)| n.label() == Some(label))
                .map(|(_, n)| n.clone())
                .unwrap()
        };
        assert!(node("Chart view").toggled().is_some());
        assert!(node("Rescan").toggled().is_none());
    }
}
