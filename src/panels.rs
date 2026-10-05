//! The window's panels: toolbar (top), filters and settings (right, when
//! open), Issues log (bottom), and the main area with the chart or the
//! Summary view.

use super::*;
use egui_extras::{Column, TableBuilder};

const FILTER_PANEL_WIDTH: f32 = 340.0;
const SETTINGS_PANEL_WIDTH: f32 = 480.0;

/// A window of its own (it can be moved anywhere, even off the app's
/// window), titled `title`, first `size` big; `open` turns false when it's
/// closed. Where separate windows aren't possible, and while a screen
/// reader is in use (it only sees the app's main window), it floats over
/// the app instead.
fn tool_window(
    ui: &egui::Ui,
    id: &str,
    title: &str,
    size: [f32; 2],
    open: &mut bool,
    mut add_contents: impl FnMut(&mut egui::Ui),
) {
    let ctx = ui.ctx();
    // The accessibility tree is only built while a screen reader asks for
    // it. (Asked with the floating window's own id, so no stray entry is
    // added: the window fills it in.)
    let window_id = egui::Id::new(id);
    let screen_reader = ctx.accesskit_node_builder(window_id, |_| ()).is_some();
    if screen_reader {
        egui::Window::new(title)
            .id(window_id)
            .default_size(size)
            .collapsible(false)
            .open(open)
            .show(ctx, |ui| add_contents(ui));
        return;
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(size)
        .with_min_inner_size([240.0, 200.0]);
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(id), viewport, |ui, _| {
        if ui.input(|i| i.viewport().close_requested()) {
            *open = false;
        }
        quit_on_ctrl_q(ui.ctx());
        egui::CentralPanel::default().show(ui, |ui| add_contents(ui));
    });
}

/// The project's page.
const WEBSITE_URL: &str = "https://github.com/a-kurczyn/spacescan";
/// Where bugs are reported (the issue forms).
const NEW_ISSUE_URL: &str = "https://github.com/a-kurczyn/spacescan/issues/new/choose";
const LICENSE_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";

/// What was chosen in the main menu.
enum MenuPick {
    Settings,
    Filters,
    Shortcuts,
    ReportBug,
    About,
    Quit,
}

/// Opens `target` (a web address or a file) in the desktop's default app.
fn open_link(target: &str) {
    if let Ok(mut child) = std::process::Command::new("xdg-open").arg(target).spawn() {
        // Collected when it ends, so no finished process lingers.
        std::thread::spawn(move || child.wait());
    }
}

/// The licenses of the libraries SpaceScan is built with: the copy
/// installed with the app if there is one, else the project's page.
fn third_party_licenses() -> String {
    // An AppImage runs from $APPDIR; packages install under /.
    let appdir = std::env::var_os("APPDIR").map(PathBuf::from);
    licenses_in(appdir.into_iter().chain([PathBuf::from("/")]))
}

/// The licenses file under the first of `bases` that has it, else the
/// project's page.
fn licenses_in(bases: impl IntoIterator<Item = PathBuf>) -> String {
    const FILE: &str = "usr/share/doc/spacescan/THIRD-PARTY-LICENSES.html";
    let installed = bases
        .into_iter()
        .map(|base| base.join(FILE))
        .find(|p| p.is_file());
    match installed {
        Some(p) => p.display().to_string(),
        None => format!("{WEBSITE_URL}/blob/main/THIRD-PARTY-LICENSES.html"),
    }
}

impl DiskScanApp {
    /// The About window: version, license and links.
    pub(crate) fn about_window(&mut self, ctx: &egui::Context) {
        let modal = egui::Modal::new("about".into()).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.vertical_centered(|ui| {
                ui.heading("SpaceScan");
                ui.label(trf("ABOUT_VERSION", &[env!("CARGO_PKG_VERSION")]));
                ui.add_space(6.0);
                ui.label(tr("ABOUT_DESCRIPTION"));
                ui.add_space(10.0);
                ui.label("Copyright © 2026 Alejandro Kurczyn");
                ui.add(egui::Label::new(tr("ABOUT_FREE_SOFTWARE")).wrap());
                ui.add_space(10.0);
                for (key, target) in [
                    ("ABOUT_WEBSITE", WEBSITE_URL.to_string()),
                    ("ABOUT_LICENSE_LINK", LICENSE_URL.to_string()),
                    ("ABOUT_THIRD_PARTY", third_party_licenses()),
                ] {
                    if ui.link(tr(key)).on_hover_text(&target).clicked() {
                        open_link(&target);
                    }
                }
                ui.add_space(10.0);
                ui.button(tr("HELP_CLOSE")).clicked()
            })
            .inner
        });
        if modal.inner || modal.should_close() {
            self.show_about = false;
        }
    }

    /// Top bar: starting points, path bar, view toggles.
    pub(crate) fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // `Sides` lays out the right-hand buttons first and gives the rest to the
        // left side. The closures only record clicks; `self` is changed after.
        let root_arc = self.root.clone();
        let cur_view_idx = self.current_view().clone();
        let can_reload = root_arc.is_some();
        let mut path_input = self.path_input.clone();
        let path_input_was_focused = self.path_input_focused;

        enum NavAction {
            None,
            Reload,
        }
        let mut nav_action = NavAction::None;
        let mut settings_toggled = false;
        let mut menu_pick: Option<MenuPick> = None;
        // F10 opens or closes the main menu from the keyboard, as in other
        // desktop apps (not while a window over the app asks something).
        let windows_open =
            self.show_about || self.table.show_help || self.quit_asked || self.delete_dialog_open();
        let menu_by_key = !windows_open && ctx.input(|i| i.key_pressed(egui::Key::F10));
        let settings_open = self.show_settings;
        let mut open_picker = false;
        let mut start_at: Option<PathBuf> = None;
        let picking_folder = self.folder_pick_rx.is_some();
        let summary_on = self.summary_view;
        let mut set_summary: Option<bool> = None;
        let mut empty_bin = false;
        let mut rescan = false;
        // The folder shown: the scan target while scanning, else the folder
        // zoomed into.
        let current_path: Option<PathBuf> = if self.scanning {
            Some(self.partial_root.path())
        } else {
            root_arc.as_ref().map(|r| get_node(r, &cur_view_idx).path())
        };
        let home = home_dir();
        let mut filters_toggled = false;
        let filter_active = self.filter.is_some() || self.pick.is_some();
        let filters_open = self.show_filters;
        let mut crumb_click: Option<PathBuf> = None;
        let mut start_path_edit = false;
        let mut stop_path_edit = false;
        let editing_path = self.path_editing || current_path.is_none();
        let focus_path_edit = std::mem::take(&mut self.path_edit_focus_pending);
        // Clickable path segments ("/", "mnt", "DATA", …). A very deep path shows
        // only its first and last few, with "…" (None) between.
        const HEAD: usize = 2;
        const TAIL: usize = 6;
        let crumbs: Vec<Option<(String, PathBuf)>> = current_path
            .as_ref()
            .map(|p| {
                let mut levels: Vec<&Path> = p.ancestors().collect();
                levels.reverse();
                let crumb = |a: &Path| {
                    let label = a.file_name().map(show_os).unwrap_or_else(|| show_path(a));
                    Some((label, a.to_path_buf()))
                };
                let n = levels.len();
                if n <= HEAD + TAIL + 1 {
                    levels.iter().map(|a| crumb(a)).collect()
                } else {
                    let mut v: Vec<_> = levels[..HEAD].iter().map(|a| crumb(a)).collect();
                    v.push(None);
                    v.extend(levels[n - TAIL..].iter().map(|a| crumb(a)));
                    v
                }
            })
            .unwrap_or_default();
        let mut submit: Option<String> = None;
        let mut path_input_focused = path_input_was_focused;

        egui::Panel::top("top").show(ui, |ui| {
            egui::Sides::new().shrink_left().show(
                ui,
                |ui| {
                    // Starting points: folder dialog, root, home (lit while scanned).
                    if glyph_toolbar_button(
                        ui,
                        ButtonRole::Action { lit: false },
                        !picking_folder,
                        "🔍",
                        &tr("TOOLBAR_PICK_FOLDER"),
                    )
                    .clicked()
                    {
                        open_picker = true;
                    }
                    let at_root = current_path.as_deref() == Some(Path::new("/"));
                    if glyph_toolbar_button(
                        ui,
                        ButtonRole::Action { lit: at_root },
                        true,
                        "/",
                        &tr("TOOLBAR_SCAN_ROOT"),
                    )
                    .clicked()
                    {
                        start_at = Some(PathBuf::from("/"));
                    }
                    let at_home = current_path.as_deref() == Some(home.as_path());
                    let home_name = trf("TOOLBAR_SCAN_HOME", &[&show_path(&home)]);
                    if glyph_toolbar_button(
                        ui,
                        ButtonRole::Action { lit: at_home },
                        true,
                        "🏠",
                        &home_name,
                    )
                    .clicked()
                    {
                        start_at = Some(home.clone());
                    }
                    if glyph_toolbar_button(
                        ui,
                        ButtonRole::Action { lit: false },
                        can_reload,
                        "⟳",
                        &tr("TOOLBAR_RESCAN"),
                    )
                    .clicked()
                    {
                        rescan = true;
                    }

                    // The path field follows navigation, except while being typed in.
                    if !path_input_was_focused {
                        let current = current_path
                            .as_ref()
                            .map(|p| show_path(p))
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
                        // Its name for screen readers (it has no label on screen).
                        ui.ctx().accesskit_node_builder(resp.id, |node| {
                            node.set_label(tr("A11Y_PATH_BAR"));
                        });
                        if focus_path_edit {
                            resp.request_focus();
                        }
                        path_input_focused = resp.has_focus();
                        if resp.lost_focus() {
                            // Enter goes there; Esc or a click elsewhere goes back to the segments.
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                submit = Some(path_input.clone());
                            }
                            stop_path_edit = true;
                        }
                    } else {
                        // The scroll area keeps the deepest folders visible.
                        let edit_w = 28.0;
                        egui::ScrollArea::horizontal()
                            .id_salt("path_crumbs")
                            // Full width, so ✏ sits at the far right.
                            .auto_shrink([false, true])
                            .stick_to_right(true)
                            .max_width((ui.available_width() - edit_w).max(0.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    let last = crumbs.len().saturating_sub(1);
                                    let root_first = crumbs
                                        .first()
                                        .is_some_and(|c| c.as_ref().is_some_and(|(l, _)| l == "/"));
                                    for (i, crumb) in crumbs.iter().enumerate() {
                                        if i > 0 && !(i == 1 && root_first) {
                                            ui.weak("/");
                                        }
                                        let Some((label, path)) = crumb else {
                                            ui.weak("…");
                                            continue;
                                        };
                                        let btn = egui::Button::new(if i == last {
                                            egui::RichText::new(label).strong()
                                        } else {
                                            egui::RichText::new(label)
                                        })
                                        .frame(false);
                                        if ui.add(btn).on_hover_text(short_path(path)).clicked()
                                            && i != last
                                        {
                                            crumb_click = Some(path.clone());
                                        }
                                    }
                                });
                            });
                        // A clickable label: no button padding.
                        let pencil = ui.add(egui::Label::new("✏").sense(egui::Sense::click()));
                        if pencil
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .named(&tr("TOOLBAR_PATH_EDIT_TOOLTIP"))
                            .clicked()
                        {
                            start_path_edit = true;
                        }
                    }
                },
                // Right to left: the main menu, settings, filters, then empty
                // trash and the view buttons.
                |ui| {
                    let menu = glyph_toolbar_button(
                        ui,
                        ButtonRole::Action { lit: false },
                        true,
                        "☰",
                        &tr("MENU_MAIN"),
                    );
                    if menu_by_key {
                        egui::Popup::toggle_id(ui.ctx(), egui::Popup::default_response_id(&menu));
                    }
                    egui::Popup::menu(&menu).show(|ui| {
                        let mut focus_first = menu_by_key;
                        let mut item = |ui: &mut egui::Ui, key: &str, keys: &str| {
                            let text = tr(key);
                            let resp = ui.add(egui::Button::new(&text).shortcut_text(keys));
                            // For screen readers: a menu item named by its text
                            // alone, its shortcut given separately.
                            ui.ctx().accesskit_node_builder(resp.id, |node| {
                                node.set_role(egui::accesskit::Role::MenuItem);
                                node.set_label(text);
                                if !keys.is_empty() {
                                    node.set_keyboard_shortcut(keys);
                                }
                            });
                            // Opened with F10: the keyboard starts on the first item.
                            if std::mem::take(&mut focus_first) {
                                resp.request_focus();
                            }
                            // Picked with the keyboard too, the menu closes.
                            if resp.clicked() {
                                ui.close();
                            }
                            resp.clicked()
                        };
                        if item(ui, "SETTINGS_TITLE", "") {
                            menu_pick = Some(MenuPick::Settings);
                        }
                        if item(ui, "FILTER_TITLE", "") {
                            menu_pick = Some(MenuPick::Filters);
                        }
                        if item(ui, "HELP_TITLE", "?") {
                            menu_pick = Some(MenuPick::Shortcuts);
                        }
                        ui.separator();
                        if item(ui, "MENU_REPORT_BUG", "") {
                            menu_pick = Some(MenuPick::ReportBug);
                        }
                        if item(ui, "MENU_ABOUT", "") {
                            menu_pick = Some(MenuPick::About);
                        }
                        ui.separator();
                        if item(ui, "MENU_QUIT", &tr("HELP_KEYS_QUIT")) {
                            menu_pick = Some(MenuPick::Quit);
                        }
                    });
                    if glyph_toolbar_button(
                        ui,
                        ButtonRole::Toggle { on: settings_open },
                        true,
                        "⚙",
                        &tr("SETTINGS_TITLE"),
                    )
                    .clicked()
                    {
                        settings_toggled = true;
                    }
                    // Lit while the panel is open or a filter is active.
                    if icon_toolbar_button(
                        ui,
                        filters_open || filter_active,
                        true,
                        &if filter_active {
                            tr("TOOLBAR_FILTERS_ACTIVE")
                        } else {
                            tr("FILTER_TITLE")
                        },
                        draw_filter_icon,
                    )
                    .clicked()
                    {
                        filters_toggled = true;
                    }
                    ui.separator();

                    if glyph_toolbar_button(
                        ui,
                        ButtonRole::Action { lit: false },
                        true,
                        "🗑",
                        &tr("TOOLBAR_EMPTY_TRASH"),
                    )
                    .clicked()
                    {
                        empty_bin = true;
                    }
                    if icon_toolbar_button(
                        ui,
                        summary_on,
                        true,
                        &tr("TOOLBAR_SUMMARY_VIEW"),
                        draw_table_icon,
                    )
                    .clicked()
                    {
                        set_summary = Some(true);
                    }
                    if icon_toolbar_button(
                        ui,
                        !summary_on,
                        true,
                        &tr("TOOLBAR_CHART_VIEW"),
                        draw_chart_icon,
                    )
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
        match menu_pick {
            Some(MenuPick::Settings) => self.show_settings = true,
            Some(MenuPick::Filters) => self.show_filters = true,
            Some(MenuPick::Shortcuts) => self.table.show_help = true,
            Some(MenuPick::ReportBug) => open_link(NEW_ISSUE_URL),
            Some(MenuPick::About) => self.show_about = true,
            Some(MenuPick::Quit) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            None => {}
        }
        if settings_toggled {
            self.show_settings = !self.show_settings;
        }
        if filters_toggled {
            self.show_filters = !self.show_filters;
        }
        if start_path_edit {
            self.path_editing = true;
            self.path_edit_focus_pending = true;
        }
        if stop_path_edit {
            self.path_editing = false;
        }
        if let Some(p) = crumb_click {
            // A folder inside the finished scan is zoomed to; anything else is
            // scanned.
            let in_tree = if self.scanning {
                None
            } else {
                self.root.as_ref().and_then(|r| index_path_to(r, &p))
            };
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
            let start_dir = root_arc.as_ref().map(|r| get_node(r, &cur_view_idx).path());
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
        if let Some(typed) = submit.as_deref() {
            if let Some(scheme_end) = typed.trim().find("://") {
                // A network URL (smb://…) isn't a path until it's mounted: explain.
                let scheme = &typed.trim()[..scheme_end];
                self.log_issue(trf("ERR_NETWORK_URL", &[scheme]));
            } else {
                let p = typed_path(typed);
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
                    let p = get_node(root, &cur_view_idx).path();
                    self.start_scan(p);
                }
            }
        }
    }

    /// Filters window, while open.
    pub(crate) fn filter_panel_ui(&mut self, ui: &mut egui::Ui) {
        // Applied with Enter or Apply; folders are re-totalled from the files
        // that match.
        if self.show_filters {
            let mut open = true;
            // Wide enough for a label and two date fields.
            let size = [FILTER_PANEL_WIDTH, 420.0];
            tool_window(
                ui,
                "filters_window",
                &tr("FILTER_TITLE"),
                size,
                &mut open,
                |ui| {
                    let mut submitted = false;
                    // A fixed-size field, checked as it's typed: an invalid value gets a red
                    // outline with the reason on hover. True when Enter was pressed.
                    let mut invalid_fields = 0;
                    let error_color = ui.visuals().error_fg_color;
                    // Screen readers name each field by its row and column labels.
                    let mut field = |ui: &mut egui::Ui,
                                     value: &mut String,
                                     hint: &str,
                                     check: &dyn Fn(&str) -> Result<(), String>,
                                     labels: [egui::Id; 2]|
                     -> bool {
                        let size = Vec2::new(130.0, ui.spacing().interact_size.y);
                        let mut r = ui
                            .add_sized(size, egui::TextEdit::singleline(value).hint_text(hint))
                            .labelled_by(labels[0])
                            .labelled_by(labels[1]);
                        if value.trim().is_empty() {
                            // empty: no limit
                        } else if let Err(e) = check(value) {
                            invalid_fields += 1;
                            ui.painter().rect_stroke(
                                r.rect,
                                2.0,
                                egui::Stroke::new(1.5, error_color),
                                egui::StrokeKind::Outside,
                            );
                            r = r.on_hover_text(e);
                        }
                        r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    };
                    let size_ok = |s: &str| parse_size(s).map(|_| ());
                    let date_ok = |s: &str| parse_date(s, false).map(|_| ());
                    let f = &mut self.filter_form;

                    let name_label = ui.label(tr("FILTER_NAME_LABEL")).id;
                    // The Aa toggle first; the name field fills the room left.
                    let r = ui
                        .horizontal(|ui| {
                            let case_tip = if f.case_sensitive {
                                tr("FILTER_CASE_SENSITIVE")
                            } else {
                                tr("FILTER_CASE_INSENSITIVE")
                            };
                            let case = ui.add(egui::Button::new("Aa").selected(f.case_sensitive));
                            let on = f.case_sensitive;
                            case.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::Button,
                                    true,
                                    on,
                                    tr("FILTER_CASE_NAME"),
                                )
                            });
                            if case.on_hover_text(case_tip).clicked() {
                                f.case_sensitive = !f.case_sensitive;
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut f.name)
                                    .hint_text(tr("FILTER_NAME_HINT"))
                                    .desired_width(f32::INFINITY),
                            )
                            .labelled_by(name_label)
                            .on_hover_text(tr("FILTER_NAME_HOVER"))
                        })
                        .inner;
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submitted = true;
                    }
                    ui.add_space(6.0);

                    let grid_enter = egui::Grid::new("filter_grid")
                        .num_columns(3)
                        .spacing([6.0, 6.0])
                        .show(ui, |ui| {
                            let mut enter = false;
                            ui.label("");
                            let from = ui.weak(tr("FILTER_COL_FROM_MIN")).id;
                            let to = ui.weak(tr("FILTER_COL_TO_MAX")).id;
                            ui.end_row();
                            let row = ui.label(tr("FILTER_ROW_SIZE")).id;
                            let hint_min = tr("FILTER_HINT_SIZE_MIN");
                            let hint_max = tr("FILTER_HINT_SIZE_MAX");
                            enter |= field(ui, &mut f.min_size, &hint_min, &size_ok, [row, from]);
                            enter |= field(ui, &mut f.max_size, &hint_max, &size_ok, [row, to]);
                            ui.end_row();
                            let row = ui.label(tr("FILTER_ROW_CREATED")).id;
                            let hint = tr("FILTER_HINT_DATE");
                            enter |= field(ui, &mut f.min_created, &hint, &date_ok, [row, from]);
                            enter |= field(ui, &mut f.max_created, &hint, &date_ok, [row, to]);
                            ui.end_row();
                            let row = ui.label(tr("FILTER_ROW_MODIFIED")).id;
                            enter |= field(ui, &mut f.min_modified, &hint, &date_ok, [row, from]);
                            enter |= field(ui, &mut f.max_modified, &hint, &date_ok, [row, to]);
                            ui.end_row();
                            enter
                        });
                    submitted |= grid_enter.inner;
                    ui.add_space(6.0);

                    let dirty = self.filter_form != self.filter_applied;
                    let valid = invalid_fields == 0;
                    let mut clear = false;
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(dirty && valid, egui::Button::new(tr("FILTER_APPLY")))
                            .clicked()
                        {
                            submitted = true;
                        }
                        if ui
                            .add_enabled(
                                self.filter.is_some()
                                    || self.pick.is_some()
                                    || self.filter_form != FilterForm::default(),
                                egui::Button::new(tr("FILTER_CLEAR")),
                            )
                            .clicked()
                        {
                            clear = true;
                        }
                    });
                    if clear {
                        self.filter_form = FilterForm::default();
                        self.pick = None;
                        submitted = true;
                    } else if !valid {
                        // Enter with an invalid field applies nothing.
                        submitted = false;
                    }
                    if submitted {
                        self.apply_filter_form();
                    }

                    if !valid {
                        // With an invalid field, say what the table is really showing.
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            tr(if self.filter.is_some() {
                                "FILTER_INVALID_KEEPS_PREVIOUS"
                            } else {
                                "FILTER_INVALID"
                            }),
                        );
                    } else if let Some(e) = &self.filter_error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    } else if self.filter.is_some() {
                        match (&self.root, &self.full_root) {
                            (Some(r), Some(full)) => {
                                ui.label(trn(
                                    "FILTER_SHOWING",
                                    full.file_count,
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
                    if let Some(pick) = &self.pick {
                        let mut clear_category = false;
                        ui.horizontal(|ui| {
                            ui.label(trf("CAT_ACTIVE", &[&self.cats.pick_label(pick)]));
                            clear_category = ui.small_button("×").named(&tr("CAT_CLEAR")).clicked();
                        });
                        if clear_category {
                            self.pick = None;
                            self.rebuild_view_tree();
                        }
                    }
                    ui.add_space(6.0);
                    ui.weak(tr("FILTER_HELP"));
                },
            );
            if !open {
                self.show_filters = false;
            }
        }
    }

    /// Settings window, while open.
    pub(crate) fn settings_panel_ui(&mut self, ui: &mut egui::Ui) {
        let mut lang_problem = None;
        if self.show_settings {
            let mut open = true;
            let size = [SETTINGS_PANEL_WIDTH, 620.0];
            tool_window(
                ui,
                "settings_window",
                &tr("SETTINGS_TITLE"),
                size,
                &mut open,
                |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let s = &mut self.settings;

                        ui.label(tr("SETTINGS_MEASURE"));
                        ui.horizontal(|ui| {
                            ui.radio_value(
                                &mut s.measure_files,
                                false,
                                tr("SETTINGS_MEASURE_BYTES"),
                            );
                            ui.radio_value(
                                &mut s.measure_files,
                                true,
                                tr("SETTINGS_MEASURE_FILES"),
                            );
                        })
                        .response
                        .on_hover_text(tr("SETTINGS_MEASURE_HOVER"));
                        ui.separator();

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
                        ui.checkbox(&mut s.color_blind_safe, tr("SETTINGS_COLOR_BLIND"))
                            .on_hover_text(tr("SETTINGS_COLOR_BLIND_HOVER"));
                        ui.add(
                            egui::Slider::new(&mut s.age_days, Settings::AGE_DAYS)
                                .logarithmic(true)
                                .text(tr("SETTINGS_AGE_DAYS")),
                        )
                        .on_hover_text(tr("SETTINGS_AGE_DAYS_HOVER"));
                        ui.add(
                            egui::Slider::new(&mut s.age_steps, Settings::AGE_STEPS)
                                .text(tr("SETTINGS_AGE_STEPS")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.age_darkest_pct, Settings::AGE_DARKEST)
                                .suffix("%")
                                .text(tr("SETTINGS_AGE_DARKEST")),
                        )
                        .on_hover_text(tr("SETTINGS_AGE_DARKEST_HOVER"));
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
                        ui.label(tr("SETTINGS_TABLE"));
                        ui.add_enabled(
                            !s.flat_all,
                            egui::Slider::new(&mut s.flat_rows, Settings::FLAT_ROWS)
                                .logarithmic(true)
                                .text(tr("SETTINGS_FLAT_ROWS")),
                        )
                        .on_hover_text(tr("SETTINGS_FLAT_ROWS_HOVER"));
                        ui.checkbox(&mut s.flat_all, tr("SETTINGS_FLAT_ALL"))
                            .on_hover_text(tr("SETTINGS_FLAT_ALL_HOVER"));
                        ui.checkbox(&mut s.paths_to_clipboard, tr("SETTINGS_PATHS_CLIPBOARD"))
                            .on_hover_text(tr("SETTINGS_PATHS_CLIPBOARD_HOVER"));

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

                        ui.separator();
                        if ui.button(tr("SETTINGS_DEFAULTS")).clicked() {
                            *s = Settings::default();
                        }

                        ui.separator();
                        let lang_label = ui.label(tr("SETTINGS_LANGUAGE")).id;
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
                                        lang_problem = lang_file_problem(code);
                                    }
                                }
                            })
                            .response
                            .labelled_by(lang_label);
                    });
                },
            );
            if !open {
                self.show_settings = false;
            }
        }
        if let Some(p) = lang_problem {
            self.log_issue(p);
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
                    // One line per issue, newest first; long lines lose their middle ("…")
                    // and show in full on hover.
                    let font = egui::TextStyle::Small.resolve(ui.style());
                    let row_h = ui.text_style_height(&egui::TextStyle::Small);
                    let n = self.log.len() + usize::from(self.log_truncated > 0);
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, true])
                        .show_rows(ui, row_h, n, |ui, range| {
                            let width = ui.available_width();
                            for i in range {
                                let Some(line) =
                                    self.log.len().checked_sub(i + 1).map(|k| &self.log[k])
                                else {
                                    ui.small(trf(
                                        "LOG_MORE_NOT_SHOWN",
                                        &[&format_count(self.log_truncated)],
                                    ));
                                    continue;
                                };
                                let shown = elide_middle(ui, line, &font, width);
                                let r = ui.add(
                                    egui::Label::new(egui::RichText::new(&shown).small()).extend(),
                                );
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
                // Both views grow live during a scan.
                if self.summary_view {
                    self.live_table_ui(ui);
                } else {
                    self.scan_preview_ui(ui);
                }
                return;
            }

            let root = match &self.root {
                Some(r) => r.clone(),
                None => return,
            };

            if self.summary_view {
                let view_node = get_node(&root, self.current_view());
                self.summary_ui(ui, view_node);
                return;
            }

            // Room above the chart for the top-left info's path line.
            ui.add_space(ui.text_style_height(&egui::TextStyle::Body) + 12.0);
            let avail = ui.available_size();

            // A fixed strip below the chart, the same as during a scan, so the chart
            // keeps its size.
            let hover_strip_height = ui.text_style_height(&egui::TextStyle::Body) * 3.0 + 12.0;
            let content_height = (avail.y - hover_strip_height).max(50.0);

            let (response, painter) = ui.allocate_painter(
                Vec2::new(avail.x, content_height),
                egui::Sense::click_and_drag(),
            );
            let (center, max_radius) = self.chart_view(&ctx, &response);
            let hub_radius = max_radius * self.settings.hub_radius_frac;
            let ring_thickness = (max_radius - hub_radius) / self.settings.max_render_depth as f32;
            self.chart_ui(
                ui,
                &root,
                &response,
                &painter,
                (center, hub_radius, ring_thickness),
            );
            ui.add_space(6.0);
            // Hidden while scanning (its space kept, so the chart stays put),
            // then fading in with the slice colors.
            let fade = if self.scanning {
                0.0
            } else {
                self.color_fade()
            };
            ui.scope(|ui| {
                ui.set_opacity(fade);
                age_legend_ui(ui, &self.settings.age_shades());
            });
        });
        central.response.rect
    }

    /// Table view while scanning: the progress bar, the category bar and the
    /// contents table so far, refreshed at most every 250 ms.
    fn live_table_ui(&mut self, ui: &mut egui::Ui) {
        if self.partial_gen != self.live_seen
            && self.live_refreshed.elapsed() >= std::time::Duration::from_millis(250)
        {
            self.live_gen += 1;
            self.live_seen = self.partial_gen;
            self.live_refreshed = Instant::now();
            self.live_view = flat_copy(&self.partial_root);
            self.cat_breakdown = category_rows(&self.live_exts, &self.cats);
            // The finished tree is broken down afresh when the scan ends.
            self.cat_breakdown_for = None;
        }
        let width = ui.available_width();
        self.scan_progress_bar(ui, width);
        ui.add_space(6.0);
        let avail = ui.available_height();
        let heading_h = ui.text_style_height(&egui::TextStyle::Heading) * 2.0 + 16.0;
        // Moved out for the call, since table_ui also needs &mut self.
        let view = std::mem::replace(&mut self.live_view, empty_node());
        self.bar_and_table(ui, avail, heading_h, &view);
        self.live_view = view;
        // Mid-scan, a picked category applies when the scan finishes.
        if let Some(cat) = self.pick_pending.take() {
            self.pick = cat;
        }
    }

    /// While scanning: a read-only live preview of the chart.
    fn scan_preview_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Same layout as the finished chart, so it doesn't jump when the scan
        // ends. No hover, clicks or menu: the data keeps changing.
        ui.add_space(ui.text_style_height(&egui::TextStyle::Body) + 12.0);
        let avail = ui.available_size();

        // The progress bar goes in the strip below the chart.
        let status_strip_height = ui.text_style_height(&egui::TextStyle::Body) * 3.0 + 12.0;
        let content_height = (avail.y - status_strip_height).max(50.0);

        let (response, painter) =
            ui.allocate_painter(Vec2::new(avail.x, content_height), egui::Sense::drag());
        let side = response.rect.width().min(response.rect.height());
        let (center, max_radius) = self.chart_view(&ctx, &response);
        let hub_radius = max_radius * self.settings.hub_radius_frac;
        let ring_thickness = (max_radius - hub_radius) / self.settings.max_render_depth as f32;

        let bg = ui.visuals().panel_fill;
        let free_color = free_space_color(ui.visuals(), self.settings.free_space_gamma);
        painter.circle_filled(center, hub_radius, bg);
        draw_hub_text(
            &painter,
            center,
            hub_radius,
            &self.partial_root.name,
            self.partial_root.size,
            ui.visuals().strong_text_color(),
        );

        // Free space is shown once the first folder has arrived.
        let free = if self.partial_root.children.is_empty() {
            None
        } else {
            self.free_space
        };
        let opts = LayoutOpts {
            hidden: &self.hidden,
            settings: &self.settings,
            order: self.chart_order,
        };
        let segs = layout_sunburst(&self.partial_root, free, opts);
        for seg in &segs {
            let colors = SliceColoring {
                free_color,
                view: Some(&self.partial_root),
                live: true,
                now: now_secs(),
            };
            self.draw_segment(&painter, seg, (center, hub_radius, ring_thickness), &colors);
        }

        // Centered under the chart, as wide as it.
        ui.add_space(4.0);
        let left_inset = ((avail.x - side) / 2.0).max(0.0);
        ui.horizontal(|ui| {
            ui.add_space(left_inset);
            self.scan_progress_bar(ui, side);
        });
    }

    /// Works out the categories of `view_node`, largest first, unless they
    /// are already known. Broken down from the tree without the category
    /// filter, so every category stays visible (and clickable) while one is
    /// picked.
    pub(crate) fn refresh_cat_breakdown(&mut self, view_node: &Node) {
        let key = (view_node.path(), self.tree_gen);
        if self.cat_breakdown_for.as_ref() != Some(&key) {
            let base = self
                .cat_base
                .as_deref()
                .and_then(|b| find_by_path(b, &view_node.path()));
            self.cat_breakdown = category_breakdown(base.unwrap_or(view_node), &self.cats);
            self.cat_breakdown_for = Some(key);
        }
    }

    /// Summary view: the category bar on the left, the contents table
    /// (table.rs) on the right.
    fn summary_ui(&mut self, ui: &mut egui::Ui, view_node: &Node) {
        self.refresh_cat_breakdown(view_node);
        let avail = ui.available_height();
        let heading_h = ui.text_style_height(&egui::TextStyle::Heading) * 2.0 + 16.0;
        self.bar_and_table(ui, avail, heading_h, view_node);
        // Only now: `view_node` belongs to the tree this frame started with,
        // and the table must not mix it with the rebuilt one.
        if let Some(cat) = self.pick_pending.take() {
            self.pick = cat;
            self.rebuild_view_tree();
            ui.ctx().request_repaint();
        }
    }

    /// Keys 1–9: picks the category at that position in the bar, or shows
    /// all files again if it's already picked. 0: shows all files. Only
    /// while the bar is the panel shown.
    pub(crate) fn pick_category_key(&mut self, n: usize) {
        if self.table.side != SidePanel::Categories {
            return;
        }
        let pick = match n {
            0 => None,
            _ => match self.cat_breakdown.get(n - 1) {
                Some(row) if self.pick != Some(Pick::Category(row.cat)) => {
                    Some(Pick::Category(row.cat))
                }
                Some(_) => None,
                None => return, // no category at that position
            },
        };
        if pick != self.pick {
            self.pick_pending = Some(pick);
        }
    }

    /// The extensions table's rows: every extension in the folder (also
    /// while something is picked, so each stays clickable), in the chosen
    /// order.
    fn extension_rows(&self) -> Vec<ExtRow> {
        let mut rows: Vec<ExtRow> = self
            .cat_breakdown
            .iter()
            .flat_map(|r| {
                r.exts.iter().map(move |(e, size, files)| ExtRow {
                    ext: e.clone(),
                    label: ext_label(e),
                    size: *size,
                    files: *files,
                    cat: r.cat,
                })
            })
            .collect();
        let sort = self.ext_sort;
        rows.sort_by(|a, b| {
            let order = match sort.column {
                SortColumn::Files => a.files.cmp(&b.files),
                SortColumn::Name => natural_cmp(&a.label, &b.label),
                _ => a.size.cmp(&b.size),
            };
            if sort.ascending {
                order
            } else {
                order.reverse()
            }
        });
        rows
    }

    /// The left panel's table of sizes by file extension, sortable by its
    /// headers. Clicking a row shows only that extension's files (again:
    /// all files); the picked one is highlighted and the others dimmed. Each
    /// row is a named button for screen readers. Only the visible rows are
    /// drawn, so thousands of extensions are fine.
    fn extension_table_ui(&mut self, ui: &mut egui::Ui, height: f32) {
        let rows = self.extension_rows();
        let total: u64 = rows
            .iter()
            .map(|r| r.weight())
            .fold(0u64, u64::saturating_add);
        let dark = ui.visuals().dark_mode;
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        // The extension clicked, and whether Ctrl was held.
        let mut clicked: Option<(String, bool)> = None;
        // A steady scroll bar, beside the rows rather than over them.
        ui.spacing_mut().scroll = steady_scroll_style();
        // The number columns are exactly as wide as their widest value or
        // header, so they never take width from the extension names.
        let font = egui::TextStyle::Body.resolve(ui.style());
        let text_w = |t: &str| {
            ui.painter()
                .layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE)
                .size()
                .x
        };
        // A header button holds its title, the sort arrow and padding.
        let header_w = |t: &str| text_w(t) + 2.0 * ui.spacing().button_padding.x + 16.0;
        let size_w = rows
            .iter()
            .map(|r| text_w(&human_size(r.size)))
            .fold(header_w(&tr("COL_SIZE")), f32::max);
        let files_w = rows
            .iter()
            .map(|r| text_w(&format_count(r.files)))
            .fold(header_w(&tr("COL_FILES")), f32::max);
        let ext_sort = &mut self.ext_sort;
        TableBuilder::new(ui)
            .id_salt("ext_table")
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .min_scrolled_height(0.0)
            .max_scroll_height(height.max(40.0))
            .animate_scrolling(false)
            .auto_shrink([false, true])
            .column(Column::remainder().at_least(60.0))
            .column(Column::exact(size_w))
            .column(Column::exact(files_w))
            .header(row_h, |mut header| {
                header.col(|ui| {
                    sortable_header(ui, &tr("COL_EXTENSION"), SortColumn::Name, ext_sort);
                });
                header.col(|ui| {
                    sortable_header(ui, &tr("COL_SIZE"), SortColumn::Size, ext_sort);
                });
                header.col(|ui| {
                    sortable_header(ui, &tr("COL_FILES"), SortColumn::Files, ext_sort);
                });
            })
            .body(|body| {
                body.rows(row_h, rows.len(), |mut table_row| {
                    let row = &rows[table_row.index()];
                    let picked = self
                        .pick
                        .as_ref()
                        .is_some_and(|p| p.has_extension(&row.ext));
                    let dimmed = !picked
                        && self.pick.as_ref().is_some_and(|p| match p {
                            Pick::Category(c) => row.cat != *c,
                            Pick::Extensions(_) => true,
                        });
                    table_row.set_selected(picked);
                    let color = |ui: &egui::Ui| {
                        if picked {
                            ui.visuals().selection.stroke.color
                        } else if dimmed {
                            ui.visuals().weak_text_color()
                        } else {
                            ui.visuals().text_color()
                        }
                    };
                    table_row.col(|ui| {
                        // A swatch in the extension's category color.
                        let (swatch, _) =
                            ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                        let c = self.cats.color(row.cat, dark);
                        ui.painter().rect_filled(
                            swatch,
                            2.0,
                            if dimmed { c.gamma_multiply(0.3) } else { c },
                        );
                        let c = color(ui);
                        ui.add(
                            egui::Label::new(egui::RichText::new(&row.label).color(c))
                                .selectable(false)
                                .truncate(),
                        );
                    });
                    table_row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let c = color(ui);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(human_size(row.size)).color(c),
                                )
                                .selectable(false),
                            );
                        });
                    });
                    table_row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let c = color(ui);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format_count(row.files)).color(c),
                                )
                                .selectable(false),
                            );
                        });
                    });
                    let hit = table_row.response();
                    // The name screen readers announce, as for the category bar.
                    let pct = row.weight() as f64 * 100.0 / total.max(1) as f64;
                    let spoken = trf(
                        "A11Y_CATEGORY",
                        &[
                            &row.label,
                            &format!("{pct:.1}%"),
                            &human_size(row.size),
                            &format_count(row.files),
                        ],
                    );
                    hit.widget_info(|| {
                        egui::WidgetInfo::selected(egui::WidgetType::Button, true, picked, &spoken)
                    });
                    let hit = hit
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(format!(
                            "{}\n{}",
                            tr(if picked {
                                "CAT_CLICK_AGAIN"
                            } else {
                                "CAT_CLICK"
                            }),
                            tr("EXT_CTRL_CLICK")
                        ));
                    if hit.clicked() {
                        let ctrl = hit.ctx.input(|i| i.modifiers.command);
                        clicked = Some((row.ext.clone(), ctrl));
                    }
                });
            });
        if let Some((ext, ctrl)) = clicked {
            self.pick_pending = Some(pick_after_click(self.pick.as_ref(), &ext, ctrl));
        }
    }

    /// The category bar on the left, the contents table of `view_node` on
    /// the right. Drawn in the main area itself (not floating over it), so
    /// a side panel always clips it instead of being drawn over.
    fn bar_and_table(&mut self, ui: &mut egui::Ui, avail: f32, heading_h: f32, view_node: &Node) {
        let full_w = ui.available_width();
        // Narrow windows get the bar alone, without labels.
        let compact = full_w < 560.0;
        let cat_w = if compact {
            40.0
        } else {
            (full_w * 0.3).clamp(150.0, 260.0)
        };
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(cat_w, avail),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(cat_w);
                    self.category_bar_ui(ui, avail - heading_h, compact);
                },
            );
            ui.separator();
            // Whatever width is left, so the table never reaches past the window.
            let table_w = ui.available_width().max(200.0);
            ui.allocate_ui_with_layout(
                Vec2::new(table_w, avail),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_max_width(table_w);
                    self.table_ui(ui, view_node, avail - heading_h);
                },
            );
        });
    }

    /// "Categories": a vertical bar split by the space each category takes
    /// in the viewed folder, labelled beside it. Clicking a segment (or its
    /// label) shows only that category's files everywhere; clicking it
    /// again, or ✕, shows everything.
    /// `compact`: a narrow panel, with the bar alone (no heading or labels;
    /// tooltips and keys still work).
    fn category_bar_ui(&mut self, ui: &mut egui::Ui, height: f32, compact: bool) {
        let mut clicked: Option<Category> = None;
        let mut clear = false;
        ui.horizontal(|ui| {
            // Toggle: category bar or extensions table.
            if !compact {
                let views: [(SidePanel, &str, DrawIcon); 2] = [
                    (
                        SidePanel::Categories,
                        "SUMMARY_CATEGORIES",
                        draw_category_bar_icon,
                    ),
                    (
                        SidePanel::Extensions,
                        "SUMMARY_EXTENSIONS",
                        draw_extension_icon,
                    ),
                ];
                for (side, key, icon) in views {
                    if icon_toolbar_button(ui, self.table.side == side, true, &tr(key), icon)
                        .clicked()
                    {
                        self.table.side = side;
                    }
                }
            }
            if self.pick.is_some() && ui.small_button("×").named(&tr("CAT_CLEAR")).clicked() {
                clear = true;
            }
        });
        let mut height = height;
        let line_h = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
        // Notes under the heading (not in the narrow panel).
        if !compact && self.scanning && self.pick.is_some() {
            ui.weak(tr("FILTER_APPLIES_ON_FINISH"));
            height -= line_h;
        } else if !compact
            && let Some(pick) = &self.pick
            && !self.cat_breakdown.iter().any(|r| match pick {
                Pick::Category(c) => r.cat == *c,
                Pick::Extensions(set) => r.exts.iter().any(|x| set.contains(&x.0)),
            })
        {
            // The picked category or extension has no files in this folder.
            let hidden: u64 = self.cat_breakdown.iter().map(|r| r.files).sum();
            let key = match pick {
                Pick::Category(_) => "CAT_NONE_HERE",
                Pick::Extensions(_) => "EXT_NONE_HERE",
            };
            let text = trn(
                key,
                hidden,
                &[&self.cats.pick_label(pick), &format_count(hidden)],
            );
            let label = ui.add(
                egui::Label::new(egui::RichText::new(text).color(ui.visuals().warn_fg_color))
                    .wrap(),
            );
            height -= label.rect.height() + ui.spacing().item_spacing.y;
        }

        if !compact && self.table.side == SidePanel::Extensions {
            self.extension_table_ui(ui, height);
            if clear {
                self.pick_pending = Some(None);
            }
            return;
        }

        let rows = &self.cat_breakdown;
        let total: u64 = rows
            .iter()
            .map(|r| r.weight())
            .fold(0u64, u64::saturating_add);
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), height.max(80.0)),
            egui::Sense::hover(),
        );
        if rows.is_empty() || total == 0 {
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.weak(tr("CAT_NO_FILES"));
            });
        } else {
            let painter = ui.painter_at(rect);
            let dark = ui.visuals().dark_mode;
            let gap = 2.0;
            let bar_w = 22.0;
            let min_h = 3.0;
            // Heights proportional to size; a sliver too thin to see or
            // click gets `min_h`, taken from the others.
            let usable = rect.height() - gap * (rows.len() - 1) as f32;
            let raw: Vec<f32> = rows
                .iter()
                .map(|r| r.weight() as f32 / total as f32 * usable)
                .collect();
            let thin = raw.iter().filter(|h| **h < min_h).count() as f32;
            let big_sum: f32 = raw.iter().filter(|h| **h >= min_h).sum();
            let scale = if big_sum > 0.0 {
                (usable - thin * min_h).max(0.0) / big_sum
            } else {
                1.0
            };
            let heights: Vec<f32> = raw
                .iter()
                .map(|h| if *h < min_h { min_h } else { h * scale })
                .collect();

            // Labels: two lines each if they all fit, else one line; if even
            // that doesn't fit, only the largest categories are labelled (the
            // rest keep their tooltips and keys).
            let font = egui::FontId::default();
            let line_h = ui.text_style_height(&egui::TextStyle::Body);
            // Half a line of space above and below each label.
            let pad = line_h * 0.5;
            let n = rows.len();
            let two_lines = (line_h * 2.0 + 2.0 * pad) * n as f32 <= rect.height();
            let label_h = if two_lines {
                line_h * 2.0 + 2.0 * pad
            } else {
                line_h + 2.0 * pad
            };
            let room = (rect.height() / label_h).floor() as usize;
            let mut by_size: Vec<usize> = (0..n).collect();
            by_size.sort_by_key(|&i| std::cmp::Reverse(rows[i].weight()));
            let mut labelled = vec![false; n];
            if !compact {
                for &i in by_size.iter().take(room) {
                    labelled[i] = true;
                }
            }
            let mut y = rect.top();
            let mut spans = Vec::with_capacity(rows.len());
            for h in &heights {
                spans.push((y, y + h));
                y += h + gap;
            }
            let mut label_y: Vec<f32> = spans
                .iter()
                .map(|(a, b)| (a + b) / 2.0 - label_h / 2.0)
                .collect();
            // Each label at its segment's middle, pushed apart so they never
            // overlap (then pulled back up if they ran off the bottom).
            let shown: Vec<usize> = (0..n).filter(|&i| labelled[i]).collect();
            for k in 0..shown.len() {
                let min = if k == 0 {
                    rect.top()
                } else {
                    label_y[shown[k - 1]] + label_h
                };
                label_y[shown[k]] = label_y[shown[k]].max(min);
            }
            for k in (0..shown.len()).rev() {
                let max = if k + 1 == shown.len() {
                    rect.bottom() - label_h
                } else {
                    label_y[shown[k + 1]] - label_h
                };
                label_y[shown[k]] = label_y[shown[k]].min(max).max(rect.top());
            }

            // Room for leader lines to slope gently to labels that moved.
            let label_x = rect.left() + bar_w + 34.0;
            let name_w = (rect.right() - label_x - 4.0).max(0.0);
            for (i, row) in rows.iter().enumerate() {
                let (y0, y1) = spans[i];
                let seg = egui::Rect::from_min_max(
                    Pos2::new(rect.left(), y0),
                    Pos2::new(rect.left() + bar_w, y1),
                );
                let label_rect = egui::Rect::from_min_size(
                    Pos2::new(label_x - 6.0, label_y[i]),
                    Vec2::new(rect.right() - label_x + 6.0, label_h),
                );
                // An extension pick highlights its category without
                // selecting it.
                let picked = self.pick == Some(Pick::Category(row.cat));
                let dimmed = self
                    .pick
                    .as_ref()
                    .is_some_and(|p| !self.cats.pick_in_category(p, row.cat));

                let id = ui.id().with(("cat", i));
                let mut hit = ui.interact(
                    seg.expand2(Vec2::new(0.0, gap / 2.0)),
                    id,
                    egui::Sense::click(),
                );
                if labelled[i] {
                    hit |= ui.interact(label_rect, id.with("label"), egui::Sense::click());
                }
                let hovered = hit.hovered();

                if picked && labelled[i] {
                    painter.rect_filled(
                        label_rect,
                        egui::CornerRadius::same(4),
                        ui.visuals().selection.bg_fill.gamma_multiply(0.5),
                    );
                } else if hovered && labelled[i] {
                    painter.rect_filled(
                        label_rect,
                        egui::CornerRadius::same(4),
                        ui.visuals().widgets.hovered.weak_bg_fill,
                    );
                }
                let r = 4u8;
                let radius = egui::CornerRadius {
                    nw: if i == 0 { r } else { 0 },
                    ne: if i == 0 { r } else { 0 },
                    sw: if i + 1 == n { r } else { 0 },
                    se: if i + 1 == n { r } else { 0 },
                };
                let color = self.cats.color(row.cat, dark);
                painter.rect_filled(
                    seg,
                    radius,
                    if dimmed {
                        color.gamma_multiply(0.3)
                    } else {
                        color
                    },
                );
                if hovered || picked {
                    painter.rect_stroke(
                        seg,
                        radius,
                        egui::Stroke::new(1.5, ui.visuals().strong_text_color()),
                        egui::StrokeKind::Outside,
                    );
                }

                // A short leader from the segment to a label that had to move.
                let mid = (y0 + y1) / 2.0;
                let ink = if dimmed {
                    ui.visuals().weak_text_color()
                } else {
                    ui.visuals().strong_text_color()
                };
                let name_mid = label_y[i] + pad + line_h / 2.0;
                if labelled[i] && (mid - name_mid).abs() > 2.0 {
                    let stroke =
                        egui::Stroke::new(1.0, ui.visuals().weak_text_color().gamma_multiply(0.6));
                    let elbow = Pos2::new(seg.right() + 8.0, mid);
                    painter.line_segment([Pos2::new(seg.right() + 2.0, mid), elbow], stroke);
                    painter.line_segment([elbow, Pos2::new(label_x - 10.0, name_mid)], stroke);
                }
                // Key 1–9 for the first nine (in the spoken name, not drawn).
                let key = (i < 9).then(|| (i + 1).to_string());
                if labelled[i] {
                    // A long name ends in "…"; the tooltip has it in full.
                    let mut job = egui::text::LayoutJob::simple_singleline(
                        self.cats.label(row.cat),
                        font.clone(),
                        ink,
                    );
                    job.wrap = egui::text::TextWrapping::truncate_at_width(name_w);
                    painter.galley(
                        Pos2::new(label_x, label_y[i] + pad),
                        painter.layout_job(job),
                        ink,
                    );
                }
                let pct = row.weight() as f64 * 100.0 / total as f64;
                // The name screen readers announce.
                let spoken = trf(
                    "A11Y_CATEGORY",
                    &[
                        &self.cats.label(row.cat),
                        &format!("{pct:.1}%"),
                        &human_size(row.size),
                        &format_count(row.files),
                    ],
                );
                let spoken = match &key {
                    Some(key) => trf("A11Y_CATEGORY_KEY", &[&spoken, key]),
                    None => spoken,
                };
                hit.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::Button, true, picked, &spoken)
                });
                if labelled[i] && two_lines {
                    painter.text(
                        Pos2::new(label_x, label_y[i] + pad + line_h),
                        egui::Align2::LEFT_TOP,
                        format!("{pct:.1}% · {}", measure_text(row.size, row.files)),
                        font.clone(),
                        ui.visuals().weak_text_color(),
                    );
                }

                let hit = hit.on_hover_ui(|ui| {
                    ui.strong(self.cats.label(row.cat));
                    ui.label(format!(
                        "{} · {pct:.1}% · {} {}",
                        human_size(row.size),
                        tr("HOVER_FILES"),
                        format_count(row.files)
                    ));
                    let exts: Vec<String> = row
                        .exts
                        .iter()
                        .take(6)
                        .map(|(e, sz, _)| format!("{} {}", ext_label(e), human_size(*sz)))
                        .collect();
                    ui.weak(exts.join("  ·  "));
                    ui.weak(tr(if picked {
                        "CAT_CLICK_AGAIN"
                    } else {
                        "CAT_CLICK"
                    }));
                });
                if hit.clicked() {
                    clicked = Some(row.cat);
                }
            }
        }

        if clear || clicked.is_some() {
            self.pick_pending = Some(match clicked {
                Some(c) if self.pick != Some(Pick::Category(c)) => Some(Pick::Category(c)),
                _ => None,
            });
        }
    }

    /// How far the chart's legends have faded in after the last scan: 0
    /// (hidden) to 1.
    pub(crate) fn color_fade(&self) -> f32 {
        color_fade(self.colored_at)
    }

    /// Draws one slice of the chart in its color. `geom` is (center, hub
    /// radius, ring thickness).
    fn draw_segment(
        &self,
        painter: &egui::Painter,
        seg: &Segment,
        geom: (Pos2, f32, f32),
        colors: &SliceColoring,
    ) {
        let (center, hub_radius, ring_thickness) = geom;
        let SliceColoring {
            free_color,
            view,
            live,
            now,
        } = *colors;
        let radii = ring_radii(seg.ring, hub_radius, ring_thickness);
        let dark = painter.ctx().global_style().visuals.dark_mode;
        let gray = self.cats.color(self.cats.other(), dark);
        // The slice's look and when it became known (to fade in from then).
        let known = |view: &Node| -> Option<(Look, Option<Instant>)> {
            let parent = || get_node(view, &seg.idx_path[..seg.idx_path.len() - 1]);
            if live {
                // During a scan: what's classified under each folder so far;
                // an "other" slice from those of its items that have any.
                if seg.is_other {
                    let parent = parent();
                    let found: Vec<(Look, Instant)> = seg
                        .rest
                        .iter()
                        .filter_map(|&i| parent.children.get(i))
                        .filter_map(|n| self.live_looks.get(n))
                        .collect();
                    let since = found.iter().map(|f| f.1).min()?;
                    Some((
                        group_look(found.into_iter().map(|f| f.0), &self.cats),
                        Some(since),
                    ))
                } else {
                    let (look, since) = self.live_looks.get(get_node(view, &seg.idx_path))?;
                    Some((look, Some(since)))
                }
            } else {
                let (_, looks) = self.looks.as_ref()?;
                if seg.is_other {
                    let rest = seg.rest.iter().filter_map(|&i| parent().children.get(i));
                    Some((looks.of_group(rest, &self.cats), self.colored_at))
                } else {
                    let n = get_node(view, &seg.idx_path);
                    // A folder colored during the scan keeps its fade.
                    let since = self.live_looks.colored_at(n).or(self.colored_at);
                    Some((looks.of(n, &self.cats), since))
                }
            }
        };
        let colors = if seg.is_free {
            (free_color, free_color)
        } else {
            match view.and_then(known) {
                Some((look, since)) => {
                    let (newest, oldest) = self
                        .settings
                        .age_shades()
                        .look_colors(&look, now, &self.cats, dark);
                    let fade = color_fade(since);
                    let finish = |c: Color32| {
                        // "Other" is paler, to read as a group.
                        let c = if seg.is_other {
                            c.lerp_to_gamma(free_color, 0.35)
                        } else {
                            c
                        };
                        // Colors fade in from grey once known.
                        gray.lerp_to_gamma(c, fade)
                    };
                    (finish(newest), finish(oldest))
                }
                // Grey until the colors are known.
                None => (gray, gray),
            }
        };
        draw_arc_mesh(
            painter,
            center,
            radii,
            (seg.start_angle, seg.end_angle),
            colors,
            &self.settings,
        );
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
        let view_node = get_node(root, self.current_view());
        // The chart is one painted area: give it a name for screen readers.
        let chart_name = trf(
            "A11Y_CHART",
            &[&show_path(&view_node.path()), &human_size(view_node.size)],
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &chart_name));
        let bg = ui.visuals().panel_fill;
        let free_color = free_space_color(ui.visuals(), self.settings.free_space_gamma);

        // The hub; clicking it goes up a level.
        painter.circle_filled(center, hub_radius, bg);
        draw_hub_text(
            painter,
            center,
            hub_radius,
            &view_node.name,
            view_node.size,
            ui.visuals().strong_text_color(),
        );

        // Free space only on the scanned folder's own chart.
        let free = if self.view_stack.len() == 1 {
            self.free_space
        } else {
            None
        };
        let opts = LayoutOpts {
            hidden: &self.hidden,
            settings: &self.settings,
            order: self.chart_order,
        };
        let segs = layout_sunburst(view_node, free, opts);
        // Slice looks of the whole tree, worked out once per tree.
        if self
            .looks
            .as_ref()
            .is_none_or(|(generation, _)| *generation != self.tree_gen)
        {
            // The files' stored categories hold if the tree was scanned with
            // the categories in use now.
            let stored = self
                .tree_cats
                .as_ref()
                .is_some_and(|c| Arc::ptr_eq(c, &self.cats));
            self.looks = Some((self.tree_gen, Looks::build(root, &self.cats, stored)));
        }
        let now = now_secs();
        if self.color_fade() < 1.0 {
            ui.ctx().request_repaint();
        }

        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let mut new_hover: Option<HoverInfo> = None;
        // Relative to view_node, like the segments' idx_paths.
        let mut hover_idx_path: Option<Vec<usize>> = None;
        let mut hover_is_other = false;

        for seg in &segs {
            let colors = SliceColoring {
                free_color,
                view: Some(view_node),
                live: false,
                now,
            };
            self.draw_segment(painter, seg, geom, &colors);
            let (r0, r1) = ring_radii(seg.ring, hub_radius, ring_thickness);

            if let Some(p) = pointer {
                let v = p - center;
                let dist = v.length();
                if dist >= r0 && dist <= r1 {
                    let mut ang = v.x.atan2(-v.y);
                    if ang < 0.0 {
                        ang += std::f32::consts::TAU;
                    }
                    if ang >= seg.start_angle && ang <= seg.end_angle {
                        // "Other" and free space have no node of their own.
                        let real_node = if seg.is_other || seg.is_free {
                            None
                        } else {
                            Some(get_node(view_node, &seg.idx_path))
                        };
                        new_hover = Some(HoverInfo {
                            path: match real_node {
                                Some(n) => n.path(),
                                None if seg.is_free => PathBuf::from(&seg.name), // "Free space"
                                // "Other": its idx_path leads to the folder holding the grouped items.
                                None => get_node(view_node, &seg.idx_path).path().join(&seg.name),
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
                            age_range: self.looks.as_ref().and_then(|(_, looks)| {
                                let look = if seg.is_other {
                                    let parent = get_node(
                                        view_node,
                                        &seg.idx_path[..seg.idx_path.len() - 1],
                                    );
                                    let rest =
                                        seg.rest.iter().filter_map(|&i| parent.children.get(i));
                                    looks.of_group(rest, &self.cats)
                                } else {
                                    let n = real_node.filter(|n| n.is_dir)?;
                                    looks.of(n, &self.cats)
                                };
                                Some((look.newest, look.oldest))
                            }),
                        });
                        // Free space can't be opened or used by the menu.
                        hover_idx_path = if seg.is_free {
                            None
                        } else {
                            Some(seg.idx_path.clone())
                        };
                        hover_is_other = seg.is_other;
                    }
                }
            }
        }
        self.hovered = new_hover;

        // Slices the arrow keys move between (all but free space), and the
        // highlighted slice's outline.
        self.chart_segs = segs
            .iter()
            .filter(|s| !s.is_free)
            .map(|s| (s.idx_path.clone(), s.start_angle))
            .collect();
        if let Some(rel) = self.selected_rel()
            && let Some(seg) = segs.iter().find(|s| !s.is_free && s.idx_path == *rel)
        {
            let radii = ring_radii(seg.ring, hub_radius, ring_thickness);
            let stroke =
                egui::Stroke::new(1.5, ui.visuals().strong_text_color().gamma_multiply(0.85));
            draw_arc_outline(
                painter,
                center,
                radii,
                (seg.start_angle, seg.end_angle),
                stroke,
            );
        }

        if response.clicked()
            && let Some(p) = pointer
        {
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
                        let mut vp = self.current_view().clone();
                        vp.extend(ip.iter());
                        self.view_stack.push(vp);
                    }
                }
            }
        }

        // Right-click menu. Its target is fixed as a path when it opens. None for
        // "other", whose idx_path is its parent folder's.
        if response.secondary_clicked() {
            self.context_target = match (&hover_idx_path, hover_is_other) {
                (Some(ip), false) => Some(get_node(view_node, ip).path()),
                _ => None,
            };
        }
        response.context_menu(|ui| {
            let Some(target) = self.context_target.clone() else {
                ui.close();
                return;
            };
            if ui.button(tr("MENU_ZOOM")).clicked() {
                if let Some(vp) = index_path_to(root, &target) {
                    self.view_stack.push(vp);
                }
                ui.close();
            }
            if ui.button(tr("MENU_RESCAN")).clicked() {
                self.start_scan(target.clone());
                ui.close();
            }
            if ui.button(tr("MENU_OPEN")).clicked() {
                let dir = if target.is_dir() {
                    target.clone()
                } else {
                    target
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or(target.clone())
                };
                let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                ui.close();
            }
            if ui.button(tr("MENU_HIDE")).clicked() {
                self.hidden.insert(target.clone());
                ui.close();
            }
            // Like Ctrl+C, Ctrl+X and Ctrl+V.
            if ui.button(tr("MENU_COPY")).clicked() {
                self.clip(&ctx, vec![target.clone()], ClipMode::Copy);
                ui.close();
            }
            if ui.button(tr("MENU_CUT")).clicked() {
                self.clip(&ctx, vec![target.clone()], ClipMode::Move);
                ui.close();
            }
            if target.is_dir()
                && ui
                    .add_enabled(
                        self.transfer.clip.is_some(),
                        egui::Button::new(tr("MENU_PASTE_INTO")),
                    )
                    .clicked()
            {
                self.paste_into(target.clone(), "");
                ui.close();
            }
            // Like T and D; a permanent delete asks first.
            if ui.button(tr("MENU_TRASH")).clicked() {
                self.queue_trash(vec![target.clone()]);
                ui.close();
            }
            if ui.button(tr("MENU_DELETE")).clicked() {
                self.ask_delete(vec![target.clone()]);
                ui.close();
            }
        });

        // Hover details float next to the pointer. (A copy, since
        // ensure_mime_lookup needs &mut self.)
        let hover_snapshot = self.hovered.clone();
        // None while the right-click menu is open.
        let hover_snapshot = hover_snapshot.filter(|_| !egui::Popup::is_any_open(&ctx));
        if let (Some(h), Some(p)) = (&hover_snapshot, pointer) {
            if !h.is_dir {
                self.ensure_mime_lookup(&h.path);
            }
            let mime = if h.is_dir {
                None
            } else {
                self.mime_cache.get(&h.path).cloned().flatten()
            };

            // Opens away from the nearest window edge.
            let gap = 14.0;
            let (align, offset) = match (p.x > center.x, p.y > center.y) {
                (false, false) => (egui::Align2::LEFT_TOP, Vec2::new(gap, gap)),
                (true, false) => (egui::Align2::RIGHT_TOP, Vec2::new(-gap, gap)),
                (false, true) => (egui::Align2::LEFT_BOTTOM, Vec2::new(gap, -gap)),
                (true, true) => (egui::Align2::RIGHT_BOTTOM, Vec2::new(-gap, -gap)),
            };
            let is_real_folder = h.is_dir && !h.is_other;
            let is_real_file = !h.is_dir && !h.is_free;
            // No icon for "other" or free space.
            let icon: Option<DrawIcon> = if is_real_file {
                Some(draw_file_icon)
            } else if is_real_folder {
                Some(draw_folder_icon)
            } else {
                None
            };
            let path_str = short_path(&h.path);

            // Keyed by path: egui remembers an area's size per id, and one id for
            // every item would keep the box as wide as the widest seen.
            // Above the chart's other overlays (the category legend, …).
            egui::Area::new(egui::Id::new("hover_tooltip").with(&h.path))
                .order(egui::Order::Tooltip)
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
                            // One line, unless the path is very long.
                            let galley = ui.painter().layout_no_wrap(
                                path_str.clone(),
                                egui::FontId::monospace(12.0),
                                ui.visuals().text_color(),
                            );
                            let label =
                                egui::Label::new(egui::RichText::new(&path_str).monospace());
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
        let mut clear_category = false;
        egui::Area::new("chart_order".into())
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(area.right_top() + Vec2::new(-8.0, 8.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(pick) = &self.pick {
                        ui.label(trf("CAT_ACTIVE", &[&self.cats.pick_label(pick)]));
                        if ui.small_button("×").named(&tr("CAT_CLEAR")).clicked() {
                            clear_category = true;
                        }
                        ui.separator();
                    }
                    for (order, glyph, tip) in [
                        (ChartOrder::Size, "9", "CHART_ORDER_SIZE"),
                        (ChartOrder::Name, "A", "CHART_ORDER_NAME"),
                    ] {
                        if icon_toolbar_button(
                            ui,
                            self.chart_order == order,
                            true,
                            &tr(tip),
                            |p, r, c| draw_sort_order_icon(p, r, c, glyph),
                        )
                        .clicked()
                        {
                            self.chart_order = order;
                        }
                    }
                });
            });
        if clear_category {
            self.pick = None;
            self.rebuild_view_tree();
        }
    }

    /// Scan progress, `width` wide: for a whole drive, a bar of the bytes
    /// scanned out of its used space with the percentage; for anything else,
    /// a spinner. Both show the files and bytes found so far.
    pub(crate) fn scan_progress_bar(&mut self, ui: &mut egui::Ui, width: f32) {
        let files = self.partial_root.file_count;
        let counts = trf(
            "SCAN_PROGRESS_COUNTS",
            &[
                &trn("COUNT_FILES", files, &[&format_count(files)]),
                &human_size(self.partial_root.size),
            ],
        );
        let used = self
            .free_space
            .map(|(total, free)| total.saturating_sub(free))
            .filter(|&u| u > 0);
        let Some(used) = used else {
            let h = ui.spacing().interact_size.y;
            ui.allocate_ui_with_layout(
                egui::vec2(width, h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(egui::Spinner::new().size(h * 0.8));
                    ui.label(counts);
                },
            );
            return;
        };
        let raw = self.partial_root.size as f64 / used as f64;
        // The bar never moves backwards.
        self.progress_shown = self.progress_shown.max(raw.clamp(0.0, 1.0) as f32);
        let fraction = self.progress_shown;
        let percent = ((fraction * 100.0) as u32).to_string();
        let label = trf("SCAN_PROGRESS_PERCENT", &[&percent, &counts]);
        let bar_resp = ui.add(egui::ProgressBar::new(fraction).desired_width(width));
        // The text is painted over the bar: screen readers get it as its name.
        ui.ctx().accesskit_node_builder(bar_resp.id, |node| {
            node.set_label(label.clone());
        });
        // Centered on the bar. (egui's built-in ProgressBar text sits at the
        // left edge instead.)
        let text_color = ui.visuals().selection.stroke.color;
        let galley = ui
            .painter()
            .layout_no_wrap(label, egui::FontId::default(), text_color);
        ui.painter().galley(
            bar_resp.rect.center() - galley.size() / 2.0,
            galley,
            text_color,
        );
    }
}

/// One row of the extensions table.
struct ExtRow {
    /// The extension, as an `ext_key` ("" for files without one).
    ext: String,
    label: String,
    size: u64,
    files: u64,
    cat: Category,
}

impl ExtRow {
    /// Its weight in the measure in use: bytes, or files.
    fn weight(&self) -> u64 {
        if measure_files() {
            self.files
        } else {
            self.size
        }
    }
}

/// `size` bytes in `files` files, as the measure in use counts them.
fn measure_text(size: u64, files: u64) -> String {
    if measure_files() {
        trn("COUNT_FILES", files, &[&format_count(files)])
    } else {
        human_size(size)
    }
}

/// How far a color known since `since` has faded in from grey: 0 to 1
/// over 0.8 s (1 if there's no time).
fn color_fade(since: Option<Instant>) -> f32 {
    const FADE: f32 = 0.8;
    since.map_or(1.0, |t| (t.elapsed().as_secs_f32() / FADE).min(1.0))
}

/// How slices are colored: by category and age when `view` (the folder the
/// chart shows) is given, else by branch (during a scan).
#[derive(Clone, Copy)]
struct SliceColoring<'a> {
    free_color: Color32,
    view: Option<&'a Node>,
    /// `view` is the live tree of a running scan.
    live: bool,
    /// The current time, for slice ages.
    now: i64,
}

/// The legend for slice brightness: one swatch per step, from new (bright)
/// to `shades.days` or older (dark), centered under the chart.
fn age_legend_ui(ui: &mut egui::Ui, shades: &AgeShades) {
    let base = ui.visuals().strong_text_color();
    let swatch = Vec2::new(14.0, 10.0);
    let new_text = tr("AGE_LEGEND_NEW");
    let old_text = trf("AGE_LEGEND_OLD", &[&shades.days.to_string()]);
    let font = egui::TextStyle::Small.resolve(ui.style());
    let text_w = |t: &str| {
        ui.painter()
            .layout_no_wrap(t.to_string(), font.clone(), base)
            .size()
            .x
    };
    let spacing = ui.spacing().item_spacing.x;
    let width = text_w(&new_text)
        + text_w(&old_text)
        + f32::from(shades.steps) * (swatch.x + 2.0)
        + 2.0 * spacing;
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
        ui.label(egui::RichText::new(&new_text).small());
        for step in 0..shades.steps {
            let (rect, _) = ui.allocate_exact_size(swatch, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 1.0, shades.shade(base, step));
            ui.add_space(2.0 - spacing);
        }
        ui.add_space(spacing);
        ui.label(egui::RichText::new(&old_text).small());
    });
}

/// The pick after a click on extension `ext` in the extensions table. A
/// click shows only that extension, or everything again if it was the only
/// one picked; a Ctrl+click adds it to the picked extensions or takes it
/// out.
fn pick_after_click(pick: Option<&Pick>, ext: &str, ctrl: bool) -> Option<Pick> {
    let mut set = match pick {
        Some(Pick::Extensions(set)) => set.clone(),
        _ => Default::default(),
    };
    if ctrl {
        if !set.remove(ext) {
            set.insert(ext.to_string());
        }
    } else if set.len() == 1 && set.contains(ext) {
        set.clear();
    } else {
        set = [ext.to_string()].into();
    }
    (!set.is_empty()).then_some(Pick::Extensions(set))
}
#[cfg(test)]
mod category_bar_tests {
    use super::*;

    /// Draws one Summary view frame, headless.
    fn frame(app: &mut DiskScanApp) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let root = app.root.clone().unwrap();
            let view = get_node(&root, app.view_stack.last().unwrap());
            app.summary_ui(ui, view);
        });
    }

    /// The live table during a scan lists the folder's contents, with the
    /// flat list switched on too.
    #[test]
    fn the_live_table_lists_rows_in_both_views() {
        for flat in [false, true] {
            let mut app = DiskScanApp {
                summary_view: true,
                scanning: true,
                ..DiskScanApp::default()
            };
            app.table.flat = flat;
            app.partial_root = test_node(
                "/scan",
                30,
                true,
                vec![test_node("/scan/a", 30, true, vec![])],
            );
            app.partial_gen = 1;
            app.live_refreshed = Instant::now() - std::time::Duration::from_secs(1);
            let ctx = egui::Context::default();
            for _ in 0..2 {
                let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.live_table_ui(ui));
            }
            assert_eq!(
                app.table.cursor,
                Some(PathBuf::from("/scan/a")),
                "flat: {flat}"
            );
        }
    }

    /// A click picks one extension (or drops it if it's the only one);
    /// Ctrl+click adds or removes extensions.
    #[test]
    fn ctrl_click_picks_several_extensions() {
        let set = |exts: &[&str]| {
            Some(Pick::Extensions(
                exts.iter().map(|e| e.to_string()).collect(),
            ))
        };
        let video = Pick::Category(Category(0));
        assert_eq!(pick_after_click(None, "mkv", false), set(&["mkv"]));
        assert_eq!(pick_after_click(set(&["mkv"]).as_ref(), "mkv", false), None);
        assert_eq!(
            pick_after_click(set(&["mkv"]).as_ref(), "mp4", true),
            set(&["mkv", "mp4"])
        );
        assert_eq!(
            pick_after_click(set(&["mkv", "mp4"]).as_ref(), "mkv", true),
            set(&["mp4"])
        );
        assert_eq!(pick_after_click(set(&["mp4"]).as_ref(), "mp4", true), None);
        // A plain click on one of several picks just that one.
        assert_eq!(
            pick_after_click(set(&["mkv", "mp4"]).as_ref(), "mkv", false),
            set(&["mkv"])
        );
        // From a picked category, Ctrl+click starts an extension pick.
        assert_eq!(pick_after_click(Some(&video), "pdf", true), set(&["pdf"]));

        let cats = CategoryModel::defaults();
        let five = Pick::Extensions(["a", "b", "c", "d", "e"].map(String::from).into());
        assert_eq!(cats.pick_label(&five), ".a, .b, .c and 2 more");
        let mkv_pdf = set(&["mkv", "pdf"]).unwrap();
        assert!(cats.pick_matches(&mkv_pdf, "x.PDF") && !cats.pick_matches(&mkv_pdf, "x.mp4"));
        assert!(cats.pick_in_category(&mkv_pdf, cats.of_name("x.pdf")));
    }

    /// Picking a category (or dropping it) while the table is on screen
    /// takes effect after the frame, without mixing up the table's rows.
    #[test]
    fn picking_a_category_refilters_after_the_frame() {
        let mut app = DiskScanApp {
            summary_view: true,
            ..DiskScanApp::default()
        };
        let files = |d: &str| {
            (0..30)
                .map(|i| test_node(&format!("/t/{d}/f{i}.xyz"), 10, false, vec![]))
                .collect::<Vec<_>>()
        };
        let mut kids = vec![
            test_node("/t/a.mkv", 5000, false, vec![]),
            test_node("/t/b.eml", 50, false, vec![]),
        ];
        kids.extend(
            (0..20).map(|i| test_node(&format!("/t/d{i}"), 300, true, files(&format!("d{i}")))),
        );
        app.full_root = Some(Arc::new(test_node("/t", 11050, true, kids)));
        app.rebuild_view_tree();
        frame(&mut app);
        assert_eq!(app.root.as_ref().unwrap().children.len(), 22);

        let video = Pick::Category(app.cats.of_name("x.mkv"));
        let other = Pick::Category(app.cats.other());
        let eml = Pick::extension("eml");
        for (pick, rows) in [
            (Some(video), 1),
            (None, 22),
            (Some(other), 21),
            (Some(eml), 1),
        ] {
            app.pick_pending = Some(pick.clone());
            frame(&mut app);
            assert_eq!(app.pick, pick);
            assert_eq!(app.root.as_ref().unwrap().children.len(), rows);
            frame(&mut app);
        }
    }

    /// Keys 1–9 pick by position in the bar; the same key again, or 0,
    /// shows all files; a key past the last category does nothing.
    #[test]
    fn number_keys_pick_by_position() {
        let mut app = DiskScanApp::default();
        let (video, docs) = (app.cats.of_name("a.mkv"), app.cats.of_name("a.pdf"));
        let row = |cat| CategoryRow {
            cat,
            size: 1,
            files: 1,
            exts: Vec::new(),
        };
        app.cat_breakdown = vec![row(docs), row(video)];
        let press = |app: &mut DiskScanApp, n| {
            app.pick_category_key(n);
            if let Some(pick) = app.pick_pending.take() {
                app.pick = pick;
            }
        };
        press(&mut app, 1);
        assert_eq!(app.pick, Some(Pick::Category(docs)));
        press(&mut app, 2);
        assert_eq!(app.pick, Some(Pick::Category(video)));
        press(&mut app, 2);
        assert_eq!(app.pick, None);
        press(&mut app, 1);
        press(&mut app, 3);
        assert_eq!(app.pick, Some(Pick::Category(docs)));
        press(&mut app, 0);
        assert_eq!(app.pick, None);
    }

    /// The extensions table lists every extension (also while something is
    /// picked) and sorts by its headers, "(no extension)" included.
    #[test]
    fn extension_rows_follow_category_and_sort() {
        let mut app = DiskScanApp::default();
        let (video, docs) = (app.cats.of_name("a.mkv"), app.cats.of_name("a.pdf"));
        let exts = |list: &[(&str, u64, u64)]| {
            list.iter()
                .map(|(e, s, f)| (e.to_string(), *s, *f))
                .collect()
        };
        app.cat_breakdown = vec![
            CategoryRow {
                cat: video,
                size: 30,
                files: 3,
                exts: exts(&[("mkv", 20, 1), ("srt", 10, 2)]),
            },
            CategoryRow {
                cat: docs,
                size: 5,
                files: 9,
                exts: exts(&[("pdf", 5, 9)]),
            },
        ];
        let names = |rows: Vec<ExtRow>| rows.into_iter().map(|r| r.label).collect::<Vec<_>>();
        assert_eq!(names(app.extension_rows()), [".mkv", ".srt", ".pdf"]);
        app.ext_sort = SortState {
            column: SortColumn::Files,
            ascending: false,
        };
        assert_eq!(names(app.extension_rows()), [".pdf", ".srt", ".mkv"]);
        // A pick doesn't hide rows: every extension stays clickable.
        app.pick = Some(Pick::Category(video));
        assert_eq!(names(app.extension_rows()).len(), 3);

        app.pick = None;
        app.ext_sort = SortState {
            column: SortColumn::Size,
            ascending: false,
        };
        // Every extension is listed (no limit), and "(no extension)" sorts
        // like any other row.
        let mut many: Vec<(String, u64, u64)> = (0..45)
            .map(|i| (format!("e{i}"), 100 - i as u64, 1))
            .collect();
        many.push((String::new(), 1000, 1));
        app.cat_breakdown = vec![CategoryRow {
            cat: app.cats.other(),
            size: 0,
            files: 46,
            exts: many,
        }];
        for ascending in [false, true] {
            app.ext_sort = SortState {
                column: SortColumn::Size,
                ascending,
            };
            let rows = app.extension_rows();
            assert_eq!(rows.len(), 46);
            let no_ext = if ascending { rows.last() } else { rows.first() };
            assert!(no_ext.unwrap().ext.is_empty());
        }
    }

    /// The Extensions panel draws, and the choice is saved with the table's
    /// settings.
    #[test]
    fn extensions_panel_draws_and_is_saved() {
        let mut app = DiskScanApp {
            summary_view: true,
            ..DiskScanApp::default()
        };
        app.full_root = Some(Arc::new(test_node(
            "/t",
            10,
            true,
            vec![test_node("/t/a.mkv", 10, false, vec![])],
        )));
        app.rebuild_view_tree();
        app.table.side = SidePanel::Extensions;
        frame(&mut app);
        let prefs = app.table_prefs();
        assert!(prefs.side == SidePanel::Extensions);
        let json = serde_json::to_string(&prefs).unwrap();
        assert!(json.contains("\"side\":\"extensions\""), "{json}");
    }

    /// Extension rows are named buttons for screen readers, marked
    /// selected when picked.
    #[test]
    fn extension_rows_are_named_for_screen_readers() {
        let mut app = DiskScanApp {
            summary_view: true,
            ..DiskScanApp::default()
        };
        app.full_root = Some(Arc::new(test_node(
            "/t",
            10,
            true,
            vec![test_node("/t/a.mkv", 10, false, vec![])],
        )));
        app.rebuild_view_tree();
        app.table.side = SidePanel::Extensions;
        app.pick = Some(Pick::extension("mkv"));
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut update = None;
        for _ in 0..2 {
            let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let root = app.root.clone().unwrap();
                let view = get_node(&root, app.view_stack.last().unwrap());
                app.summary_ui(ui, view);
            });
            update = out.platform_output.accesskit_update;
        }
        let update = update.expect("accesskit output");
        let row = update
            .nodes
            .iter()
            .find(|(_, n)| n.label().is_some_and(|l| l.starts_with(".mkv,")))
            .map(|(_, n)| n.clone())
            .expect("a named .mkv row");
        assert!(row.toggled().is_some());
    }
}

/// The path typed in the path bar: as typed if that exists (names may end
/// in spaces), else without the spaces around it. "~" and "~/…" mean the
/// home folder, as in a shell.
pub(crate) fn typed_path(typed: &str) -> PathBuf {
    let expand = |t: &str| match t.strip_prefix('~') {
        Some("") => home_dir(),
        Some(rest) if rest.starts_with('/') => home_dir().join(&rest[1..]),
        _ => PathBuf::from(t),
    };
    let exact = expand(typed);
    if typed != typed.trim() && std::fs::symlink_metadata(&exact).is_ok() {
        exact
    } else {
        expand(typed.trim())
    }
}

#[cfg(test)]
mod typed_path_tests {
    use super::*;

    /// Names with spaces at either end open as typed; a pasted path with a
    /// stray space or line break still opens when the exact name doesn't
    /// exist; a name with the space wins over the same name without it.
    #[test]
    fn spaces_around_typed_paths() {
        let dir = std::env::temp_dir().join(format!("spacescan-typed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["sp ", " lead", "both", "both ", "  "] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        let d = dir.display();
        let at = |name: &str| dir.join(name);
        assert_eq!(typed_path(&format!("{d}/sp ")), at("sp "));
        assert_eq!(typed_path(&format!("{d}/ lead")), at(" lead"));
        assert_eq!(typed_path(&format!("{d}/both ")), at("both "));
        assert_eq!(typed_path(&format!("{d}/both")), at("both"));
        assert_eq!(typed_path(&format!("  {d}/both\n")), at("both"));
        assert_eq!(typed_path(&format!("{d}/both\n")), at("both"));
        assert_eq!(typed_path(&format!("{d}/  ")), at("  "));
        assert_eq!(typed_path(&format!("{d}/missing ")), at("missing"));
        assert_eq!(typed_path("~"), home_dir());
        assert_eq!(typed_path(" ~/x "), home_dir().join("x"));
        assert_eq!(typed_path("~user"), PathBuf::from("~user"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod about_tests {
    use super::*;

    /// F10 opens and closes the main menu, but not while a window over the
    /// app asks something.
    #[test]
    fn f10_opens_the_main_menu() {
        let ctx = egui::Context::default();
        let mut app = DiskScanApp::default();
        let press = |app: &mut DiskScanApp, key: Option<egui::Key>| {
            let events = key
                .map(|key| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                })
                .into_iter()
                .collect();
            let raw = egui::RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(raw, |ui| app.toolbar_ui(ui));
            egui::Popup::is_any_open(&ctx)
        };
        assert!(!press(&mut app, None));
        assert!(press(&mut app, Some(egui::Key::F10)));
        assert!(press(&mut app, None));
        assert!(!press(&mut app, Some(egui::Key::F10)));
        app.show_about = true;
        assert!(!press(&mut app, Some(egui::Key::F10)));
    }

    /// Esc closes an open About or shortcuts window and leaves the scan
    /// running; with nothing open it cancels the scan.
    #[test]
    fn esc_closes_windows_before_cancelling_a_scan() {
        let ctx = egui::Context::default();
        let mut app = DiskScanApp::default();
        let esc = |app: &mut DiskScanApp| {
            let raw = egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            };
            let _ = ctx.run_ui(raw, |ui| app.esc_cancels_scan(ui.ctx()));
        };
        app.scanning = true;
        app.show_about = true;
        esc(&mut app);
        assert!(app.scanning);
        app.show_about = false;
        app.table.show_help = true;
        esc(&mut app);
        assert!(app.scanning);
        app.table.show_help = false;
        esc(&mut app);
        assert!(!app.scanning);
    }

    /// The installed licenses file wins (the AppImage's before the
    /// system's); with none, the project's page.
    #[test]
    fn third_party_licenses_are_found() {
        let dir = std::env::temp_dir().join(format!("spacescan-about-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (app, system) = (dir.join("app"), dir.join("system"));
        let file = "usr/share/doc/spacescan/THIRD-PARTY-LICENSES.html";
        let page = format!("{WEBSITE_URL}/blob/main/THIRD-PARTY-LICENSES.html");
        assert_eq!(licenses_in([app.clone(), system.clone()]), page);
        std::fs::create_dir_all(system.join(file).parent().unwrap()).unwrap();
        std::fs::write(system.join(file), "x").unwrap();
        let both = || licenses_in([app.clone(), system.clone()]);
        assert_eq!(both(), system.join(file).display().to_string());
        // A folder with the file's name is not the file.
        std::fs::create_dir_all(app.join(file)).unwrap();
        assert_eq!(both(), system.join(file).display().to_string());
        std::fs::remove_dir(app.join(file)).unwrap();
        std::fs::write(app.join(file), "x").unwrap();
        assert_eq!(both(), app.join(file).display().to_string());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
