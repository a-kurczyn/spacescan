//! The window's panels, drawn in this order every frame by `ui` (main.rs):
//! toolbar (top), filters and settings (right, when open), Issues log
//! (bottom), and the main area — the chart, or the Summary view's tables.

use super::*;

const FILTER_PANEL_WIDTH: f32 = 340.0;
const SETTINGS_PANEL_WIDTH: f32 = 320.0;

/// How much the window was widened for each side panel, to give back when
/// it closes.
#[derive(Default)]
pub(crate) struct WindowGrown {
    filters: f32,
    settings: f32,
}

/// Opening a side panel widens the window by the panel's width so the main
/// area keeps its size; closing it gives that width back. A maximized or
/// fullscreen window can't grow, so it's left alone (the panel then takes
/// its room from the main area).
fn resize_for_panel(ctx: &egui::Context, opening: bool, width: f32, grown: &mut f32) {
    // The window's own size (viewport().inner_rect is unknown on Wayland,
    // where windows can't read their position).
    let (size, fixed) = ctx.input(|i| {
        let v = i.viewport();
        (i.viewport_rect().size(), v.maximized == Some(true) || v.fullscreen == Some(true))
    });
    let delta = if opening {
        if fixed {
            return;
        }
        *grown = width;
        width
    } else {
        if fixed || *grown == 0.0 {
            *grown = 0.0;
            return;
        }
        -std::mem::take(grown)
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size + Vec2::new(delta, 0.0)));
}

impl DiskScanApp {
    /// Top bar: starting points, path bar, view toggles.
    pub(crate) fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // `Sides` measures the right-hand content first and gives the
        // left-hand content whatever room remains — unlike a manual
        // "reserve N pixels" guess, the nav buttons can never end up
        // clipped regardless of window width, font, or theme. Both
        // closures below only read pre-cloned local state and report what
        // happened; every actual `self` mutation happens afterward, since
        // Sides::show can't hand out two simultaneous `&mut self` closures.
        let root_arc = self.root.clone();
        let cur_view_idx = self.view_stack.last().unwrap().clone();
        let can_reload = root_arc.is_some();
        let mut path_input = self.path_input.clone();
        let path_input_was_focused = self.path_input_focused;

        enum NavAction {
            None,
            Reload,
        }
        let mut nav_action = NavAction::None;
        let mut settings_toggled = false;
        let settings_open = self.show_settings;
        let mut open_picker = false;
        let mut start_at: Option<PathBuf> = None;
        let picking_folder = self.folder_pick_rx.is_some();
        let summary_on = self.summary_view;
        let mut set_summary: Option<bool> = None;
        let mut empty_bin = false;
        let mut rescan = false;
        // What the user is looking at right now: the scan target while a scan
        // runs (self.root still holds the *previous* result until it's
        // done), otherwise the folder currently zoomed into. Drives both the
        // path bar and which starting-point button reads as selected.
        let current_path: Option<PathBuf> = if self.scanning {
            Some(self.partial_root.path.clone())
        } else {
            root_arc.as_ref().map(|r| get_node(r, &cur_view_idx).path.clone())
        };
        let home = home_dir();
        let mut filters_toggled = false;
        let filter_active = self.filter.is_some();
        let filters_open = self.show_filters;
        let mut crumb_click: Option<PathBuf> = None;
        let mut start_path_edit = false;
        let mut stop_path_edit = false;
        let editing_path = self.path_editing || current_path.is_none();
        let focus_path_edit = std::mem::take(&mut self.path_edit_focus_pending);
        // Clickable segments of the current path: "/", "mnt", "DATA", ...
        let crumbs: Vec<(String, PathBuf)> = current_path
            .as_ref()
            .map(|p| {
                let mut v: Vec<(String, PathBuf)> = p
                    .ancestors()
                    .map(|a| {
                        let label = a.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| show_path(&a));
                        (label, a.to_path_buf())
                    })
                    .collect();
                v.reverse();
                v
            })
            .unwrap_or_default();
        let mut submit: Option<String> = None;
        let mut path_input_focused = path_input_was_focused;

        egui::Panel::top("top").show(ui, |ui| {
            egui::Sides::new().shrink_left().show(
                ui,
                |ui| {
                    // Starting points: Search (any drive/mount/folder via the
                    // system dialog), Root, Home. Root/Home read as selected
                    // while they're the current scan target.
                    if ui
                        .add_enabled(!picking_folder, egui::Button::new("🔍"))
                        .on_hover_text(tr("TOOLBAR_PICK_FOLDER"))
                        .clicked()
                    {
                        open_picker = true;
                    }
                    if ui
                        .add(egui::Button::new("/").selected(current_path.as_deref() == Some(Path::new("/"))))
                        .on_hover_text(tr("TOOLBAR_SCAN_ROOT"))
                        .clicked()
                    {
                        start_at = Some(PathBuf::from("/"));
                    }
                    if ui
                        .add(egui::Button::new("🏠").selected(current_path.as_deref() == Some(home.as_path())))
                        .on_hover_text(trf("TOOLBAR_SCAN_HOME", &[&show_path(&home)]))
                        .clicked()
                    {
                        start_at = Some(home.clone());
                    }
                    if ui
                        .add_enabled(can_reload, egui::Button::new("⟳"))
                        .on_hover_text(tr("TOOLBAR_RESCAN"))
                        .clicked()
                    {
                        rescan = true;
                    }

                    // Keep the path bar synced to navigation (mount switches,
                    // zooming into the chart) as long as the user isn't
                    // currently typing in it — otherwise we'd clobber their
                    // in-progress edit every frame.
                    if !path_input_was_focused {
                        let current = current_path
                            .as_ref()
                            .map(|p| show_path(&p))
                            .unwrap_or_default();
                        if path_input != current {
                            path_input = current;
                        }
                    }

                    if editing_path {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut path_input)
                                .desired_width(ui.available_width())
                                .hint_text(tr("TOOLBAR_PATH_HINT")),
                        );
                        if focus_path_edit {
                            resp.request_focus();
                        }
                        path_input_focused = resp.has_focus();
                        if resp.lost_focus() {
                            // Enter submits; Esc or clicking elsewhere just
                            // goes back to the clickable segments.
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                submit = Some(path_input.clone());
                            }
                            stop_path_edit = true;
                        }
                    } else {
                        // Clickable path segments; the scroll area keeps the
                        // deepest folders visible when the path is long.
                        let edit_w = 28.0;
                        egui::ScrollArea::horizontal()
                            .id_salt("path_crumbs")
                            // Fill the whole width (not just the segments')
                            // so ✏ lands at the far right of the path field.
                            .auto_shrink([false, true])
                            .stick_to_right(true)
                            .max_width((ui.available_width() - edit_w).max(0.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    let last = crumbs.len().saturating_sub(1);
                                    for (i, (label, path)) in crumbs.iter().enumerate() {
                                        if i > 0 && i <= last && !(i == 1 && crumbs[0].0 == "/") {
                                            ui.weak("/");
                                        }
                                        let btn = egui::Button::new(if i == last { egui::RichText::new(label).strong() } else { egui::RichText::new(label) })
                                            .frame(false);
                                        if ui.add(btn).on_hover_text(show_path(&path)).clicked() && i != last {
                                            crumb_click = Some(path.clone());
                                        }
                                    }
                                });
                            });
                        // A plain clickable label, not a Button: even with
                        // .frame(false) a Button still reserves its normal
                        // left/right button_padding around the glyph for
                        // its click target, which is exactly the padding
                        // asked to go away — a Label has none.
                        let pencil = ui.add(egui::Label::new("✏").sense(egui::Sense::click()));
                        if pencil.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr("TOOLBAR_PATH_EDIT_TOOLTIP")).clicked() {
                            start_path_edit = true;
                        }
                    }
                },
                // Settings sit at the far right. (Navigation lives in the
                // cross in the main area's top-right corner — see the end of
                // this function.)
                // Right-to-left layout: each item added sits to the left of
                // the previous one. Far right: filters + settings; then,
                // after a separator, empty trash and the Chart / Table view
                // pair (the active view highlighted).
                |ui| {
                    if ui
                        .add(egui::Button::new("⚙").selected(settings_open))
                        .on_hover_text(tr("SETTINGS_TITLE"))
                        .clicked()
                    {
                        settings_toggled = true;
                    }
                    // Lit while the panel is open, and while a filter is
                    // applied (so a hidden, active filter isn't forgotten).
                    if icon_toolbar_button(ui, filters_open || filter_active, true, draw_filter_icon)
                        .on_hover_text(if filter_active { tr("TOOLBAR_FILTERS_ACTIVE") } else { tr("FILTER_TITLE") })
                        .clicked()
                    {
                        filters_toggled = true;
                    }
                    ui.separator();

                    if ui.button("🗑").on_hover_text(tr("TOOLBAR_EMPTY_TRASH")).clicked() {
                        empty_bin = true;
                    }
                    if icon_toolbar_button(ui, summary_on, true, draw_table_icon)
                        .on_hover_text(tr("TOOLBAR_SUMMARY_VIEW"))
                        .clicked()
                    {
                        set_summary = Some(true);
                    }
                    if icon_toolbar_button(ui, !summary_on, true, draw_chart_icon)
                        .on_hover_text(tr("TOOLBAR_CHART_VIEW"))
                        .clicked()
                    {
                        set_summary = Some(false);
                    }
                    ui.separator();
                },
            );
        });

        self.path_input = path_input;
        self.path_input_focused = path_input_focused;
        if settings_toggled {
            self.show_settings = !self.show_settings;
            let mut grown = self.window_grown.settings;
            resize_for_panel(&ctx, self.show_settings, SETTINGS_PANEL_WIDTH, &mut grown);
            self.window_grown.settings = grown;
        }
        if filters_toggled {
            self.show_filters = !self.show_filters;
            let mut grown = self.window_grown.filters;
            resize_for_panel(&ctx, self.show_filters, FILTER_PANEL_WIDTH, &mut grown);
            self.window_grown.filters = grown;
        }
        if start_path_edit {
            self.path_editing = true;
            self.path_edit_focus_pending = true;
        }
        if stop_path_edit {
            self.path_editing = false;
        }
        if let Some(p) = crumb_click {
            // A folder inside the finished scan: just zoom to it. Anything
            // else (a parent of the scanned folder, or mid-scan): scan it.
            let in_tree = if self.scanning { None } else { self.root.as_ref().and_then(|r| index_path_to(r, &p)) };
            match in_tree {
                Some(idx) => {
                    if self.view_stack.last() != Some(&idx) {
                        self.view_stack.push(idx);
                    }
                }
                None => start_at = Some(p),
            }
        }
        if let Some(on) = set_summary {
            self.summary_view = on;
        }
        if empty_bin {
            self.ask_empty_trash();
        }
        if rescan {
            nav_action = NavAction::Reload;
        }
        if open_picker {
            // Open the dialog at the folder currently shown, if any.
            let start_dir = root_arc.as_ref().map(|r| get_node(r, &cur_view_idx).path.clone());
            self.folder_pick_rx = Some(pick_folder_async(start_dir));
        }
        if let Some(result) = self.folder_pick_rx.as_ref().map(|rx| rx.try_recv()) {
            match result {
                Ok(picked) => {
                    self.folder_pick_rx = None;
                    start_at = start_at.or(picked);
                }
                Err(TryRecvError::Empty) => {
                    // Nothing else wakes the UI when the dialog closes.
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(TryRecvError::Disconnected) => {
                    self.folder_pick_rx = None;
                    self.log_issue(tr("ERR_FOLDER_DIALOG_FAILED"));
                }
            }
        }
        if let Some(p) = start_at {
            self.start_scan(p);
        }
        if let Some(trimmed) = submit.as_deref().map(str::trim) {
            if let Some(scheme_end) = trimmed.find("://") {
                // A URL like smb://host/share is a virtual URI (KIO/GVFS),
                // not a real filesystem path — there's no directory to stat
                // until it's actually mounted. Auto-mounting it ourselves
                // would mean shelling out to `gio mount` and risking a hang
                // waiting on a credentials prompt we have no way to
                // surface, so just say clearly what's needed instead.
                let scheme = &trimmed[..scheme_end];
                self.log_issue(trf("ERR_NETWORK_URL", &[scheme]));
            } else {
                // "~" and "~/…" mean the home folder, as in a shell.
                let p = match trimmed.strip_prefix('~') {
                    Some("") => home_dir(),
                    Some(rest) if rest.starts_with('/') => home_dir().join(&rest[1..]),
                    _ => PathBuf::from(trimmed),
                };
                match std::fs::metadata(&p) {
                    Ok(m) if m.is_dir() => self.start_scan(p),
                    Ok(_) => self.log_issue(trf("ERR_NOT_A_DIRECTORY", &[&show_path(&p)])),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        self.log_issue(trf("ERR_PATH_NOT_FOUND", &[&show_path(&p)]))
                    }
                    Err(e) => self.log_issue(friendly_io_error(&p, &e)),
                }
            }
        }
        match nav_action {
            NavAction::None => {}
            NavAction::Reload => {
                if let Some(root) = &root_arc {
                    let p = get_node(root, &cur_view_idx).path.clone();
                    self.start_scan(p);
                }
            }
        }
    }

    /// Filters panel (right side), while open.
    pub(crate) fn filter_panel_ui(&mut self, ui: &mut egui::Ui) {
        // Filters: narrow what the chart/summary show to matching files.
        // Applied on demand (Enter or Apply) since re-filtering a big scan
        // isn't free; folders are re-totalled from the files that match.
        if self.show_filters {
            egui::Panel::right("filter_panel")
                .resizable(true)
                .default_size(FILTER_PANEL_WIDTH)
                // Wide enough that the label column ("Created"/"Modified",
                // or a longer translation of them) plus two date fields
                // never get squeezed into clipping their "YYYY-MM-DD"-style
                // hint text — that's what was showing up as "YYYY-...".
                .min_size(300.0)
                .max_size(600.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading(tr("FILTER_TITLE"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").on_hover_text(tr("FILTER_CLOSE")).clicked() {
                                self.show_filters = false;
                            }
                        });
                    });
                    ui.separator();

                    let mut submitted = false;
                    // Returns true when Enter was pressed in the field.
                    // add_sized (an exact allocated rect) rather than
                    // desired_width (a hint the Grid negotiates around) —
                    // desired_width let the Grid's own per-column width
                    // inference end up giving the min/from column less
                    // room than the to/max one despite both requesting the
                    // same width, clipping "YYYY-MM-DD" down to "YYYY-...".
                    // An exact size can't be negotiated down that way.
                    // Each field is checked as it's typed: an invalid value
                    // gets a red outline (and its reason on hover), and
                    // Apply stays disabled until it's fixed.
                    let mut invalid_fields = 0;
                    let error_color = ui.visuals().error_fg_color;
                    let mut field = |ui: &mut egui::Ui, value: &mut String, hint: &str, check: &dyn Fn(&str) -> Result<(), String>| -> bool {
                        let size = Vec2::new(130.0, ui.spacing().interact_size.y);
                        let mut r = ui.add_sized(size, egui::TextEdit::singleline(value).hint_text(hint));
                        if value.trim().is_empty() {
                            // empty: no limit
                        } else if let Err(e) = check(value) {
                            invalid_fields += 1;
                            ui.painter().rect_stroke(r.rect, 2.0, egui::Stroke::new(1.5, error_color), egui::StrokeKind::Outside);
                            r = r.on_hover_text(e);
                        }
                        r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    };
                    let size_ok = |s: &str| parse_size(s).map(|_| ());
                    let date_ok = |s: &str| parse_date(s, false).map(|_| ());
                    let f = &mut self.filter_form;

                    ui.label(tr("FILTER_NAME_LABEL"));
                    // The Aa toggle is placed first (fixed size), so the
                    // text field — added after, with the same passive
                    // "fill everything left" INFINITY it always used — only
                    // ever sees whatever room the toggle didn't already
                    // claim. Deriving the field's width from a live
                    // available_width() reading instead (subtracting the
                    // toggle's width by hand) briefly latched onto a much
                    // larger number during the same-frame relayout that
                    // happens when another docked panel opens/closes, and
                    // since this panel has no max_width, it grew to match
                    // and stayed that way.
                    let r = ui
                        .horizontal(|ui| {
                            let case_tip = if f.case_sensitive { tr("FILTER_CASE_SENSITIVE") } else { tr("FILTER_CASE_INSENSITIVE") };
                            if ui
                                .add(egui::Button::new("Aa").selected(f.case_sensitive))
                                .on_hover_text(case_tip)
                                .clicked()
                            {
                                f.case_sensitive = !f.case_sensitive;
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut f.name)
                                    .hint_text(tr("FILTER_NAME_HINT"))
                                    .desired_width(f32::INFINITY),
                            )
                            .on_hover_text(tr("FILTER_NAME_HOVER"))
                        })
                        .inner;
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submitted = true;
                    }
                    ui.add_space(6.0);

                    let grid_enter = egui::Grid::new("filter_grid").num_columns(3).spacing([6.0, 6.0]).show(ui, |ui| {
                        let mut enter = false;
                        ui.label("");
                        ui.weak(tr("FILTER_COL_FROM_MIN"));
                        ui.weak(tr("FILTER_COL_TO_MAX"));
                        ui.end_row();
                        ui.label(tr("FILTER_ROW_SIZE"));
                        enter |= field(ui, &mut f.min_size, &tr("FILTER_HINT_SIZE_MIN"), &size_ok);
                        enter |= field(ui, &mut f.max_size, &tr("FILTER_HINT_SIZE_MAX"), &size_ok);
                        ui.end_row();
                        ui.label(tr("FILTER_ROW_CREATED"));
                        enter |= field(ui, &mut f.min_created, &tr("FILTER_HINT_DATE"), &date_ok);
                        enter |= field(ui, &mut f.max_created, &tr("FILTER_HINT_DATE"), &date_ok);
                        ui.end_row();
                        ui.label(tr("FILTER_ROW_MODIFIED"));
                        enter |= field(ui, &mut f.min_modified, &tr("FILTER_HINT_DATE"), &date_ok);
                        enter |= field(ui, &mut f.max_modified, &tr("FILTER_HINT_DATE"), &date_ok);
                        ui.end_row();
                        enter
                    });
                    submitted |= grid_enter.inner;
                    ui.add_space(6.0);

                    let dirty = self.filter_form != self.filter_applied;
                    let valid = invalid_fields == 0;
                    let mut clear = false;
                    ui.horizontal(|ui| {
                        if ui.add_enabled(dirty && valid, egui::Button::new(tr("FILTER_APPLY"))).clicked() {
                            submitted = true;
                        }
                        if ui.add_enabled(self.filter.is_some() || self.filter_form != FilterForm::default(), egui::Button::new(tr("FILTER_CLEAR"))).clicked() {
                            clear = true;
                        }
                    });
                    if clear {
                        self.filter_form = FilterForm::default();
                        submitted = true;
                    } else if !valid {
                        // Enter in a field with an invalid value applies
                        // nothing (the outline says why).
                        submitted = false;
                    }
                    if submitted {
                        self.apply_filter_form();
                    }

                    if !valid {
                        // Never let an invalid field look applied: say what
                        // the table is really showing.
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            tr(if self.filter.is_some() { "FILTER_INVALID_KEEPS_PREVIOUS" } else { "FILTER_INVALID" }),
                        );
                    } else if let Some(e) = &self.filter_error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    } else if self.filter.is_some() {
                        match (&self.root, &self.full_root) {
                            (Some(r), Some(full)) => {
                                ui.label(trf(
                                    "FILTER_SHOWING",
                                    &[
                                        &format_count(r.file_count),
                                        &format_count(full.file_count),
                                        &human_size(r.size),
                                        &human_size(full.size),
                                    ],
                                ));
                            }
                            _ => {
                                ui.weak(tr("FILTER_APPLIES_ON_FINISH"));
                            }
                        }
                        if self.scanning {
                            ui.weak(tr("FILTER_LIVE_UNFILTERED"));
                        }
                    }
                    ui.add_space(6.0);
                    ui.weak(tr("FILTER_HELP"));
                });
        }
    }

    /// Chart settings panel (right side), while open.
    pub(crate) fn settings_panel_ui(&mut self, ui: &mut egui::Ui) {
        // Chart settings live in a docked panel on the right side of the main
        // window (toggled by the ⚙ button) instead of a floating popup.
        if self.show_settings {
            egui::Panel::right("settings_panel")
                .resizable(true)
                .default_size(SETTINGS_PANEL_WIDTH)
                .min_size(240.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading(tr("SETTINGS_TITLE"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").on_hover_text(tr("SETTINGS_CLOSE")).clicked() {
                                self.show_settings = false;
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let s = &mut self.settings;

                        ui.label(tr("SETTINGS_DEPTH_GROUPING"));
                        ui.add(
                            egui::Slider::new(&mut s.max_render_depth, Settings::DEPTH)
                                .text(tr("SETTINGS_DEPTH_LEVELS")),
                        );
                        ui.add_enabled(
                            !s.unlimited_slices,
                            egui::Slider::new(&mut s.min_segment_angle_deg, Settings::MIN_ANGLE)
                                .text(tr("SETTINGS_MIN_SLICE_ANGLE")),
                        );
                        ui.add_enabled(
                            !s.unlimited_slices,
                            egui::Slider::new(&mut s.max_children_shown, Settings::MAX_CHILDREN)
                                .text(tr("SETTINGS_MAX_SLICES")),
                        );
                        ui.checkbox(&mut s.unlimited_slices, tr("SETTINGS_UNLIMITED_SLICES"))
                            .on_hover_text(tr("SETTINGS_UNLIMITED_SLICES_HOVER"));
                        ui.add(
                            egui::Slider::new(&mut s.hub_radius_frac, Settings::HUB)
                                .text(tr("SETTINGS_HUB_SIZE")),
                        );

                        ui.separator();
                        ui.label(tr("SETTINGS_COLORS"));
                        ui.add(egui::Slider::new(&mut s.ring_sat, Settings::RING_SAT).text(tr("SETTINGS_RING_SAT")));
                        ui.add(
                            egui::Slider::new(&mut s.ring_val_base, Settings::RING_VAL_BASE)
                                .text(tr("SETTINGS_RING_BRIGHT_OUTER")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.ring_val_falloff, Settings::RING_VAL_FALLOFF)
                                .text(tr("SETTINGS_BRIGHT_FALLOFF")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.ring_val_floor, Settings::RING_VAL_FLOOR)
                                .text(tr("SETTINGS_BRIGHT_FLOOR")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.other_sat, Settings::OTHER_SAT)
                                .text(tr("SETTINGS_OTHER_SAT")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.other_val, Settings::OTHER_VAL)
                                .text(tr("SETTINGS_OTHER_BRIGHT")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.free_space_gamma, Settings::FREE_GAMMA)
                                .text(tr("SETTINGS_FREE_GAMMA")),
                        );

                        ui.separator();
                        ui.label(tr("SETTINGS_LINE_RENDERING"));
                        ui.add(
                            egui::Slider::new(&mut s.stroke_width, Settings::STROKE_WIDTH)
                                .text(tr("SETTINGS_BORDER_THICKNESS")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.stroke_alpha, 0..=255)
                                .text(tr("SETTINGS_BORDER_DARKNESS")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.tess_px_per_step, Settings::TESS)
                                .text(tr("SETTINGS_CURVE_SMOOTH")),
                        );

                        ui.separator();
                        ui.label(tr("SETTINGS_LOG"));
                        ui.add(
                            egui::Slider::new(&mut s.max_log_lines, Settings::LOG_LINES)
                                .text(tr("SETTINGS_MAX_LOG_LINES")),
                        );

                        ui.separator();
                        ui.label(tr("SETTINGS_SCANNING"));
                        ui.checkbox(&mut s.apparent_size, tr("SETTINGS_APPARENT_SIZE"))
                            .on_hover_text(tr("SETTINGS_APPARENT_SIZE_HOVER"));
                        ui.add(
                            egui::Slider::new(&mut s.progress_interval_pow2, Settings::PROGRESS_POW2)
                                .custom_formatter(|v, _| format!("{}", 1u64 << (v as u32)))
                                .custom_parser(|s| {
                                    s.parse::<u64>()
                                        .ok()
                                        .map(|v| v.max(1).next_power_of_two().trailing_zeros().min(16) as f64)
                                })
                                .text(tr("SETTINGS_PROGRESS_INTERVAL")),
                        );

                        ui.separator();
                        if ui.button(tr("SETTINGS_DEFAULTS")).clicked() {
                            *s = Settings::default();
                        }

                        ui.separator();
                        ui.label(tr("SETTINGS_LANGUAGE"));
                        let langs = available_languages();
                        let current = current_lang_code();
                        let current_name = langs
                            .iter()
                            .find(|(c, _)| *c == current)
                            .map(|(_, n)| n.clone())
                            .unwrap_or_else(|| current.clone());
                        egui::ComboBox::from_id_salt("lang_combo")
                            .selected_text(current_name)
                            .show_ui(ui, |ui| {
                                for (code, name) in &langs {
                                    if ui.selectable_label(*code == current, name).clicked() {
                                        set_language(code);
                                    }
                                }
                            });
                    });
                });
        }
    }

    /// Bottom bar: Issues log and status.
    pub(crate) fn log_panel_ui(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("log_panel")
            .resizable(true)
            .default_size(110.0)
            .size_range(egui::Rangef::new(28.0, 600.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(if self.log.is_empty() {
                        tr("LOG_ISSUES_NONE")
                    } else {
                        trf("LOG_ISSUES_COUNT", &[&format_count(self.log.len() as u64)])
                    });
                    if !self.log.is_empty() && ui.small_button(tr("LOG_CLEAR")).clicked() {
                        self.log.clear();
                        self.log_truncated = 0;
                    }
                    if !self.status.is_empty() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.small(&self.status);
                        });
                    }
                });
                if !self.log.is_empty() {
                    // One line per issue, newest first. A line too long for
                    // the bar loses its middle ("…") rather than its end,
                    // where the reason is; hovering shows it in full. Only
                    // the visible lines are laid out.
                    let font = egui::TextStyle::Small.resolve(ui.style());
                    let row_h = ui.text_style_height(&egui::TextStyle::Small);
                    let n = self.log.len() + usize::from(self.log_truncated > 0);
                    egui::ScrollArea::vertical().auto_shrink([false, true]).show_rows(ui, row_h, n, |ui, range| {
                        let width = ui.available_width();
                        for i in range {
                            let Some(line) = self.log.len().checked_sub(i + 1).map(|k| &self.log[k]) else {
                                ui.small(trf("LOG_MORE_NOT_SHOWN", &[&format_count(self.log_truncated)]));
                                continue;
                            };
                            let shown = elide_middle(ui, line, &font, width);
                            let r = ui.add(egui::Label::new(egui::RichText::new(&shown).small()).extend());
                            if shown.len() != line.len() {
                                r.on_hover_text(line);
                            }
                        }
                    });
                }
            });
    }

    /// Main area: the chart (live preview while scanning) or the Summary
    /// view. Returns its rectangle, for the overlays drawn over it.
    pub(crate) fn main_area_ui(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let ctx = ui.ctx().clone();
        let central = egui::CentralPanel::default().show(ui, |ui| {
            self.chart_segs.clear();
            if !self.scanning && self.root.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label(tr("CENTRAL_EMPTY_STATE"));
                });
                return;
            }
            if self.scanning {
                self.scan_preview_ui(ui);
                return;
            }

            let root = match &self.root {
                Some(r) => r.clone(),
                None => return,
            };

            if self.summary_view {
                let view_node = get_node(&root, self.view_stack.last().unwrap());
                self.summary_ui(ui, view_node);
                return;
            }

            // Room above the chart for the path line of the top-left info
            // (size/files/perms may overlap the chart's corner).
            ui.add_space(ui.text_style_height(&egui::TextStyle::Body) + 12.0);
            let avail = ui.available_size();

            // Reserve a fixed-height strip below the chart for hover
            // details, full width (so long paths never wrap) — fixed
            // regardless of whether anything is currently hovered, so the
            // chart's own size never jumps when hovering starts/stops.
            let hover_strip_height = ui.text_style_height(&egui::TextStyle::Body) * 3.0 + 12.0;
            let content_height = (avail.y - hover_strip_height).max(50.0);

            let (response, painter) =
                ui.allocate_painter(Vec2::new(avail.x, content_height), egui::Sense::click_and_drag());
            let (center, max_radius) = self.chart_view(&ctx, &response);
            let hub_radius = max_radius * self.settings.hub_radius_frac;
            let ring_thickness = (max_radius - hub_radius) / self.settings.max_render_depth as f32;
            self.chart_ui(ui, &root, &response, &painter, (center, hub_radius, ring_thickness));
        });
        central.response.rect
    }

    /// While scanning: a read-only live preview of the chart.
    fn scan_preview_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Read-only live preview: draw whatever top-level children
        // have streamed in so far, so the sunburst blossoms one
        // petal at a time instead of staying blank until the whole
        // drive finishes. No hover/click/context-menu here — the
        // data is still changing underneath every frame.
        // Room above the chart for the path line of the top-left
        // info (size/files/perms may overlap the chart's corner),
        // same as the finished view, so the chart doesn't jump when
        // the scan completes.
        ui.add_space(ui.text_style_height(&egui::TextStyle::Body) + 12.0);
        let avail = ui.available_size();

        // Reserve the exact same bottom strip as the completed
        // view (see hover_strip_height there) so the chart is
        // sized identically in both states — otherwise the circle
        // visibly jumps/shrinks the instant scanning finishes.
        // The scan-progress readout lives in that strip instead of
        // a fixed-position overlay: a floating box can't overlap
        // the chart if the chart's own drawable area already
        // excludes that space.
        let status_strip_height = ui.text_style_height(&egui::TextStyle::Body) * 3.0 + 12.0;
        let content_height = (avail.y - status_strip_height).max(50.0);

        let (response, painter) =
            ui.allocate_painter(Vec2::new(avail.x, content_height), egui::Sense::drag());
        let side = response.rect.width().min(response.rect.height());
        let (center, max_radius) = self.chart_view(&ctx, &response);
        let hub_radius = max_radius * self.settings.hub_radius_frac;
        let ring_thickness = (max_radius - hub_radius) / self.settings.max_render_depth as f32;

        let bg = ui.visuals().panel_fill;
        let free_color = gamma_lighten(bg, self.settings.free_space_gamma);
        painter.circle_filled(center, hub_radius, bg);
        draw_hub_text(&painter, center, hub_radius, &self.partial_root.name, self.partial_root.size);

        // Include free space once scanning has actually produced
        // some content, not from frame one: statvfs answers
        // instantly, but the first real directory can take a while
        // to show up (e.g. a dormant HDD spinning up) — showing
        // free space alone in the meantime looks like a stalled,
        // near-empty chart. Once content exists, size against total
        // capacity from then on so proportions stay stable for the
        // rest of the blossom animation (no jump at completion).
        let has_content = !self.partial_root.children.is_empty();
        let live_free_bytes = if has_content {
            self.free_space.map(|(_, free)| free).unwrap_or(0)
        } else {
            0
        };
        let live_total_capacity = if has_content {
            self.free_space.map(|(total, _)| total).unwrap_or(0)
        } else {
            0
        };
        let mut segs = Vec::new();
        layout_sunburst(
            &self.partial_root,
            vec![],
            0.0,
            std::f32::consts::TAU,
            0,
            &self.hidden,
            live_free_bytes,
            live_total_capacity,
            &self.settings,
            self.chart_order,
            &mut segs,
        );
        for seg in &segs {
            let r0 = hub_radius + ring_thickness * seg.ring as f32;
            let r1 = r0 + ring_thickness;
            let top_hue = hue_for_branch(*seg.idx_path.first().unwrap_or(&0));
            let color = if seg.is_free { free_color } else { segment_color(seg, top_hue, &self.settings) };
            draw_arc_mesh(&painter, center, r0, r1, seg.start_angle, seg.end_angle, color, &self.settings);
        }

        // Bytes-scanned-so-far vs. known total capacity is a cheap,
        // filesystem-agnostic progress proxy (unlike file count,
        // which has no reliable upfront total — see is_real_mount_point
        // discussion; NTFS in particular fakes its inode totals).
        // It's imperfect (many tiny files vs. one huge file skews
        // it) but it's honest about what it measures and free to
        // compute from data we already track.
        //
        // Any other folder has no such total, so a counting pass
        // (count_entries) runs alongside the scan and progress is
        // items scanned / items counted. Until counting finishes the
        // total is a lower bound, so the fraction is held monotonic
        // rather than letting the bar slide backwards as it grows.
        let used_target = self.free_space.map(|(total, free)| total.saturating_sub(free));
        let (raw, label) = match used_target.filter(|&u| u > 0) {
            Some(u) => (
                self.partial_root.size as f64 / u as f64,
                trf("SCAN_PROGRESS_ITEMS_SCANNED", &[&format_count(self.scanned_count)]),
            ),
            None => {
                use std::sync::atomic::Ordering;
                let (found, counted) = self
                    .entry_count
                    .as_ref()
                    .map_or((0, false), |c| (c.found.load(Ordering::Relaxed), c.done.load(Ordering::Relaxed)));
                let total = found.max(self.scanned_count).max(1);
                let label = if counted {
                    trf("SCAN_PROGRESS_OF_TOTAL", &[&format_count(self.scanned_count), &format_count(total)])
                } else {
                    trf("SCAN_PROGRESS_ITEMS", &[&format_count(self.scanned_count)])
                };
                // Until counting finishes the total is only a lower
                // bound — and on a cold disk the count isn't reliably
                // ahead of the scan, so any fraction from it can
                // wildly overshoot (and the bar never moves back).
                // Hold at 0 meanwhile; the label shows it's working.
                (if counted { self.scanned_count as f64 / total as f64 } else { 0.0 }, label)
            }
        };
        self.progress_shown = self.progress_shown.max(raw.clamp(0.0, 1.0) as f32);
        let fraction = self.progress_shown;

        // Rendered directly into the reserved strip below the
        // chart (ui's cursor sits there now, since the painter
        // above only consumed content_height, not the full avail)
        // — full width, and structurally unable to overlap the
        // chart regardless of how full the drive is.
        ui.add_space(4.0);
        // Match the chart's own bounding width (`side`), not the
        // full panel — the panel is usually wider than the circle
        // (height is normally the limiting dimension), so a
        // full-width bar visually mismatched the chart above it.
        // Centered under the chart via a left inset.
        let left_inset = ((avail.x - side) / 2.0).max(0.0);
        let bar_resp = ui
            .horizontal(|ui| {
                ui.add_space(left_inset);
                ui.add(
                    egui::ProgressBar::new(fraction).desired_width(side),
                )
            })
            .inner;

        // Centered on the bar. (egui's built-in ProgressBar text
        // sits at the left edge instead.)
        let text_color = ui.visuals().selection.stroke.color;
        let galley = ui.painter().layout_no_wrap(label, egui::FontId::default(), text_color);
        ui.painter()
            .galley(bar_resp.rect.center() - galley.size() / 2.0, galley, text_color);
    }

    /// Summary view: the contents table (table.rs) and the by-extension
    /// breakdown.
    fn summary_ui(&mut self, ui: &mut egui::Ui, view_node: &Node) {
        if self.ext_breakdown_for.as_deref() != Some(view_node.path.as_path()) {
            self.ext_breakdown = extension_breakdown(view_node);
            self.ext_breakdown_for = Some(view_node.path.clone());
        }

        let mut ext_rows = self.ext_breakdown.clone();
        let es = self.ext_sort;
        ext_rows.sort_by(|a, b| {
            let ord = match es.column {
                SortColumn::Size => a.1.cmp(&b.1),
                SortColumn::Files => a.2.cmp(&b.2),
                // Extensions have no dates or permissions; those headers aren't
                // offered for this table.
                SortColumn::Modified | SortColumn::Changed | SortColumn::Perms => std::cmp::Ordering::Equal,
                SortColumn::Name => a.0.cmp(&b.0),
            };
            if es.ascending { ord } else { ord.reverse() }
        });

        // Drawn in the main area itself (not floating over it), so a side
        // panel always clips it instead of being drawn over. Both tables
        // scroll on their own. Beside: each gets the full height. Below:
        // the extension breakdown keeps up to about a third of it.
        let avail = ui.available_height();
        let heading_h = ui.text_style_height(&egui::TextStyle::Heading) * 2.0 + 16.0;
        if self.table.ext_beside {
            let ext_w = 300.0;
            let table_w = (ui.available_width() - ext_w - 16.0).max(320.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(Vec2::new(table_w, avail), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_max_width(table_w);
                    self.table_ui(ui, view_node, avail - heading_h);
                });
                ui.separator();
                ui.vertical(|ui| self.ext_table_ui(ui, &ext_rows, avail - heading_h));
            });
        } else {
            let ext_h = (avail * 0.3).clamp(110.0, 260.0);
            self.table_ui(ui, view_node, avail - ext_h - heading_h);
            ui.add_space(8.0);
            ui.separator();
            let rest = ui.available_height() - heading_h / 2.0;
            self.ext_table_ui(ui, &ext_rows, rest);
        }
    }

    /// "By file extension": heading with the layout toggle (below / beside
    /// the contents table), then the table, scrolling past `max_height`.
    fn ext_table_ui(&mut self, ui: &mut egui::Ui, ext_rows: &[(String, u64, u64)], max_height: f32) {
        ui.horizontal(|ui| {
            ui.heading(tr("SUMMARY_BY_EXTENSION"));
            let beside = self.table.ext_beside;
            // The icon previews the layout the button switches to.
            if icon_toolbar_button(ui, false, true, |p, r, c| draw_layout_icon(p, r, c, !beside))
                .on_hover_text(tr(if beside { "EXT_SHOW_BELOW" } else { "EXT_SHOW_BESIDE" }))
                .clicked()
            {
                self.table.ext_beside = !beside;
            }
        });
        egui::ScrollArea::vertical().id_salt("ext_scroll").max_height(max_height.max(40.0)).show(ui, |ui| {
            egui::Grid::new("summary_ext_grid").num_columns(3).striped(true).show(ui, |ui| {
                sortable_header(ui, &tr("COL_SIZE"), SortColumn::Size, &mut self.ext_sort);
                sortable_header(ui, &tr("COL_FILES"), SortColumn::Files, &mut self.ext_sort);
                sortable_header(ui, &tr("COL_EXTENSION"), SortColumn::Name, &mut self.ext_sort);
                ui.end_row();
                for (ext, size, count) in ext_rows {
                    ui.label(human_size(*size));
                    ui.label(format_count(*count));
                    ui.label(ext);
                    ui.end_row();
                }
            });
        });
    }

    /// The sunburst for the folder being viewed, with hover details,
    /// clicks and the right-click menu. `geom` is (center, hub radius,
    /// ring thickness).
    fn chart_ui(
        &mut self,
        ui: &mut egui::Ui,
        root: &Arc<Node>,
        response: &egui::Response,
        painter: &egui::Painter,
        geom: (Pos2, f32, f32),
    ) {
        let ctx = ui.ctx().clone();
        let (center, hub_radius, ring_thickness) = geom;
        let view_node = get_node(root, self.view_stack.last().unwrap());
        let bg = ui.visuals().panel_fill;
        let free_color = gamma_lighten(bg, self.settings.free_space_gamma);

        // hub (center circle) - click navigates up
        painter.circle_filled(center, hub_radius, bg);
        draw_hub_text(&painter, center, hub_radius, &view_node.name, view_node.size);

        let (root_free_bytes, root_total_capacity) = if self.view_stack.len() == 1 {
            (
                self.free_space.map(|(_, free)| free).unwrap_or(0),
                self.free_space.map(|(total, _)| total).unwrap_or(0),
            )
        } else {
            (0, 0)
        };
        let mut segs = Vec::new();
        layout_sunburst(
            view_node,
            vec![],
            0.0,
            std::f32::consts::TAU,
            0,
            &self.hidden,
            root_free_bytes,
            root_total_capacity,
            &self.settings,
            self.chart_order,
            &mut segs,
        );

        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let mut new_hover: Option<HoverInfo> = None;
        // Segment idx_paths are relative to view_node (layout_sunburst
        // starts from it), not to the scan root.
        let mut hover_idx_path: Option<Vec<usize>> = None;
        let mut hover_is_other = false;

        for seg in &segs {
            let r0 = hub_radius + ring_thickness * seg.ring as f32;
            let r1 = r0 + ring_thickness;
            let top_hue = hue_for_branch(*seg.idx_path.first().unwrap_or(&0));
            let color = if seg.is_free { free_color } else { segment_color(seg, top_hue, &self.settings) };
            draw_arc_mesh(&painter, center, r0, r1, seg.start_angle, seg.end_angle, color, &self.settings);

            if let Some(p) = pointer {
                let v = p - center;
                let dist = v.length();
                if dist >= r0 && dist <= r1 {
                    let mut ang = v.x.atan2(-v.y);
                    if ang < 0.0 {
                        ang += std::f32::consts::TAU;
                    }
                    if ang >= seg.start_angle && ang <= seg.end_angle {
                        // Only "real" segments (not the synthetic
                        // "other"/"free space" buckets) correspond to
                        // an actual Node — that's where mtime/ctime/
                        // uid/gid/mime can come from.
                        let real_node = if seg.is_other || seg.is_free {
                            None
                        } else {
                            Some(get_node(view_node, &seg.idx_path))
                        };
                        new_hover = Some(HoverInfo {
                            path: if seg.is_free {
                                PathBuf::from(&seg.name) // "Free space" — not a real path under view_node
                            } else if seg.is_other {
                                // An "other" bucket's idx_path is the folder
                                // holding the grouped items (which may be
                                // several rings out), not the viewed folder.
                                get_node(view_node, &seg.idx_path).path.join(&seg.name)
                            } else {
                                real_node.unwrap().path.clone()
                            },
                            size: seg.size,
                            file_count: seg.file_count,
                            is_dir: seg.is_dir || seg.is_other,
                            is_free: seg.is_free,
                            is_other: seg.is_other,
                            mode: seg.mode,
                            mtime: real_node.map(|n| n.mtime),
                            ctime: real_node.map(|n| n.ctime),
                            uid: real_node.map(|n| n.uid),
                            gid: real_node.map(|n| n.gid),
                        });
                        // Free space isn't a real tree node: don't let it
                        // be zoomed into or targeted by the context menu.
                        hover_idx_path = if seg.is_free { None } else { Some(seg.idx_path.clone()) };
                        hover_is_other = seg.is_other;
                    }
                }
            }
        }
        self.hovered = new_hover;

        // Slices the arrow keys can move between (real files/folders and
        // "other" — see OTHER_MARKER for how it stays navigable despite
        // not being a real tree node — but not the free-space slice),
        // and the highlight: a thin light outline, leaving the slice's
        // own colour untouched.
        self.chart_segs = segs
            .iter()
            .filter(|s| !s.is_free)
            .map(|s| (s.idx_path.clone(), s.start_angle))
            .collect();
        if let Some(rel) = self.selected_rel() {
            if let Some(seg) = segs.iter().find(|s| !s.is_free && s.idx_path == *rel) {
                let r0 = hub_radius + ring_thickness * seg.ring as f32;
                let r1 = r0 + ring_thickness;
                let stroke = egui::Stroke::new(1.5, ui.visuals().strong_text_color().gamma_multiply(0.85));
                draw_arc_outline(&painter, center, r0, r1, seg.start_angle, seg.end_angle, stroke);
            }
        }

        if response.clicked() {
            if let Some(p) = pointer {
                let dist = (p - center).length();
                if dist <= hub_radius {
                    if self.view_stack.len() > 1 {
                        self.view_stack.pop();
                    }
                } else if let Some(ip) = &hover_idx_path {
                    if is_other_marker(ip) {
                        self.open_other_bucket(ip);
                    } else {
                        let node = get_node(view_node, ip);
                        if node.is_dir {
                            let mut vp = self.view_stack.last().unwrap().clone();
                            vp.extend(ip.iter());
                            self.view_stack.push(vp);
                        }
                    }
                }
            }
        }

        // Right-click menu (egui's own: closes on a click elsewhere or
        // Esc). Its target is fixed as a path when it opens, so it keeps
        // meaning the same item even if the view changes underneath.
        // Nothing for an "other" bucket: its idx_path is its *parent*
        // folder's, so Trash/Delete there would hit that whole folder.
        if response.secondary_clicked() {
            self.context_target = match (&hover_idx_path, hover_is_other) {
                (Some(ip), false) => Some(get_node(view_node, ip).path.clone()),
                _ => None,
            };
        }
        response.context_menu(|ui| {
            let Some(target) = self.context_target.clone() else {
                ui.close();
                return;
            };
            if ui.button(tr("MENU_ZOOM")).clicked() {
                if let Some(vp) = index_path_to(&root, &target) {
                    self.view_stack.push(vp);
                }
                ui.close();
            }
            if ui.button(tr("MENU_RESCAN")).clicked() {
                self.start_scan(target.clone());
                ui.close();
            }
            if ui.button(tr("MENU_OPEN")).clicked() {
                let dir = if target.is_dir() { target.clone() } else { target.parent().map(Path::to_path_buf).unwrap_or(target.clone()) };
                let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                ui.close();
            }
            if ui.button(tr("MENU_HIDE")).clicked() {
                self.hidden.insert(target.clone());
                ui.close();
            }
            // Same as T / D: the chart updates in place, and a
            // permanent delete asks first.
            if ui.button(tr("MENU_TRASH")).clicked() {
                self.queue_trash(vec![target.clone()]);
                ui.close();
            }
            if ui.button(tr("MENU_DELETE")).clicked() {
                self.ask_delete(vec![target.clone()]);
                ui.close();
            }
        });

        // Floating tooltip-style panel near the pointer, rather than a
        // fixed strip of the layout: the reserved margin below the
        // chart (hover_strip_height) stays purely as blank breathing
        // room now, so the chart's size still doesn't jump between
        // scanning and completed states, but hover details no longer
        // permanently occupy that space — they only appear, floating,
        // while actually hovering.
        // Snapshot: ensure_mime_lookup below needs &mut self, which
        // can't coexist with an active &self.hovered borrow.
        let hover_snapshot = self.hovered.clone();
        // No hover card while the right-click menu is open (it would cover
        // the menu and describe a different item).
        let hover_snapshot = hover_snapshot.filter(|_| !egui::Popup::is_any_open(&ctx));
        if let (Some(h), Some(p)) = (&hover_snapshot, pointer) {
            if !h.is_dir {
                self.ensure_mime_lookup(&h.path);
            }
            let mime = if h.is_dir { None } else { self.mime_cache.get(&h.path).cloned().flatten() };

            // Flip which corner of the tooltip anchors to the pointer
            // based on which quadrant of the chart it's in, so the
            // popup opens away from the nearest edge instead of
            // routinely spilling off-window.
            let gap = 14.0;
            let (align, offset) = match (p.x > center.x, p.y > center.y) {
                (false, false) => (egui::Align2::LEFT_TOP, Vec2::new(gap, gap)),
                (true, false) => (egui::Align2::RIGHT_TOP, Vec2::new(-gap, gap)),
                (false, true) => (egui::Align2::LEFT_BOTTOM, Vec2::new(gap, -gap)),
                (true, true) => (egui::Align2::RIGHT_BOTTOM, Vec2::new(-gap, -gap)),
            };
            let is_real_folder = h.is_dir && !h.is_other;
            let is_real_file = !h.is_dir && !h.is_free;
            // None for the aggregate "other" bucket / free space:
            // neither is really a file or a folder.
            let icon: Option<fn(&egui::Painter, egui::Rect, Color32)> = if is_real_file {
                Some(draw_file_icon)
            } else if is_real_folder {
                Some(draw_folder_icon)
            } else {
                None
            };
            let path_str = show_path(&h.path);

            // Keyed by path: egui's Area/Grid persist and only ever
            // grow their sizing per Id across frames (to avoid jitter),
            // so reusing one fixed Id for every hover target would let
            // a wide value on one file (a huge file count, a long
            // date) stick around and bloat the box for the next,
            // shorter-named one too.
            egui::Area::new(egui::Id::new("hover_tooltip").with(&h.path))
                .pivot(align)
                .fixed_pos(p + offset)
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(draw_icon) = icon {
                                let (icon_rect, _resp) =
                                    ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::hover());
                                draw_icon(ui.painter(), icon_rect, ui.visuals().text_color());
                            }
                            // No wrap by default, so the tooltip sizes
                            // to fit the path on one line — only wraps
                            // (at a generous width) once it's long
                            // enough that "way big" is the honest
                            // description.
                            let galley = ui.painter().layout_no_wrap(
                                path_str.clone(),
                                egui::FontId::monospace(12.0),
                                ui.visuals().text_color(),
                            );
                            let label = egui::Label::new(egui::RichText::new(&path_str).monospace());
                            if galley.size().x > 900.0 {
                                ui.add(label.wrap());
                            } else {
                                ui.add(label.extend());
                            }
                        });
                        ui.separator();
                        details_grid(
                            ui,
                            egui::Id::new("hover_tooltip_grid").with(&h.path),
                            h,
                            mime.as_deref(),
                            &mut self.user_cache,
                            &mut self.group_cache,
                        );
                    });
                });
        }
    }

    /// Chart order, in the chart's top-right corner: largest first ("9")
    /// or A–Z ("A"), the active one highlighted.
    pub(crate) fn chart_order_buttons(&mut self, ctx: &egui::Context, area: egui::Rect) {
        egui::Area::new("chart_order".into())
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(area.right_top() + Vec2::new(-8.0, 8.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (order, glyph, tip) in [(ChartOrder::Size, "9", "CHART_ORDER_SIZE"), (ChartOrder::Name, "A", "CHART_ORDER_NAME")] {
                        if icon_toolbar_button(ui, self.chart_order == order, true, |p, r, c| draw_sort_order_icon(p, r, c, glyph))
                            .on_hover_text(tr(tip))
                            .clicked()
                        {
                            self.chart_order = order;
                        }
                    }
                });
            });
    }
}
