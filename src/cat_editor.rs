//! The Categories window, opened from Settings: each category's name,
//! extensions and color, which can be added, removed and reset to the
//! built-in ones, then saved to categories.json.

use super::*;
use panels::tool_window;

/// The categories being edited, until saved or dropped.
pub(crate) struct CategoryEditor {
    rows: Vec<Row>,
    /// Gives each row a widget id that stays with it when it moves.
    next_id: u64,
    /// Why saving failed.
    error: Option<String>,
}

struct Row {
    id: u64,
    /// The built-in name's language token, while the name isn't retyped.
    token: Option<String>,
    /// The name as shown and typed.
    name: String,
    /// The extensions as typed.
    extensions: String,
    color: Option<CatColor>,
    /// The color picker's "#RRGGBB" field.
    hex: String,
}

/// Something to fix or to know about a row.
struct Problem {
    /// Saving waits until it's fixed.
    blocks: bool,
    text: String,
}

impl CategoryEditor {
    pub(crate) fn new(entries: Vec<CategoryEntry>) -> Self {
        let mut editor = CategoryEditor {
            rows: Vec::new(),
            next_id: 0,
            error: None,
        };
        editor.set_entries(entries);
        editor
    }

    fn set_entries(&mut self, entries: Vec<CategoryEntry>) {
        self.rows = entries.into_iter().map(|e| self.row(e)).collect();
    }

    fn row(&mut self, entry: CategoryEntry) -> Row {
        self.next_id += 1;
        // A name with a translation is a token, shown translated.
        let shown = tr(&entry.name);
        let token = (shown != entry.name).then_some(entry.name);
        Row {
            id: self.next_id,
            token,
            name: shown,
            extensions: entry.extensions.join(", "),
            color: entry.color,
            hex: match entry.color {
                Some(CatColor::Custom(c)) => hex_text(c),
                _ => String::new(),
            },
        }
    }

    /// The categories as they would be saved.
    pub(crate) fn entries(&self) -> Vec<CategoryEntry> {
        self.rows.iter().map(Row::entry).collect()
    }

    /// Each row's problems, given the rows' colors on the current
    /// background (`dark`) and whether colors must suit color blindness.
    fn problems(&self, dark: bool, color_blind: bool) -> Vec<Vec<Problem>> {
        let entries = self.entries();
        let colors: Vec<Color32> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| match e.color {
                Some(c) => c.on(dark),
                None => position_color(i, dark, color_blind),
            })
            .collect();
        let shown_name = |i: usize| tr(&entries[i].name);
        entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let mut list = Vec::new();
                let mut add = |blocks: bool, text: String| list.push(Problem { blocks, text });
                if entry.name.is_empty() {
                    add(true, tr("CATS_NO_NAME"));
                } else if is_other_name(&entry.name) {
                    add(true, trf("CATS_OTHER_NAME", &[&tr("CAT_OTHER")]));
                } else if entries[..i]
                    .iter()
                    .any(|e| tr(&e.name).to_lowercase() == tr(&entry.name).to_lowercase())
                {
                    add(true, tr("CATS_SAME_NAME"));
                }
                for ext in &entry.extensions {
                    if let Some(k) = entries[..i].iter().position(|e| e.extensions.contains(ext)) {
                        add(
                            true,
                            trf("CATS_EXT_TWICE", &[&format!(".{ext}"), &shown_name(k)]),
                        );
                    }
                    if ext.contains('.') {
                        add(false, trf("CATS_EXT_DOT", &[&format!(".{ext}")]));
                    }
                }
                if entry.color.is_none() && i >= PALETTE_SIZE {
                    add(false, tr("CATS_PICK_COLOR"));
                }
                if let Some(CatColor::Custom(c)) = entry.color {
                    for (bg, key) in [
                        (0x1b1b1b, "CATS_LOW_CONTRAST_DARK"),
                        (0xf8f8f8, "CATS_LOW_CONTRAST_LIGHT"),
                    ] {
                        if contrast(c, rgb(bg)) < 3.0 {
                            add(false, tr(key));
                        }
                    }
                }
                let others = (0..entries.len())
                    .filter(|&k| k != i)
                    .map(|k| (colors[k], shown_name(k)))
                    .chain([(CategoryModel::other_color(dark), tr("CAT_OTHER"))]);
                for (color, name) in others {
                    match too_close(colors[i], color, color_blind) {
                        None => {}
                        Some(Vision::Normal) => add(false, trf("CATS_TOO_CLOSE", &[&name])),
                        Some(v) => add(false, trf("CATS_TOO_CLOSE_CB", &[&name, &vision_name(v)])),
                    }
                }
                list
            })
            .collect()
    }
}

impl Row {
    fn entry(&self) -> CategoryEntry {
        let name = match &self.token {
            Some(token) if tr(token) == self.name => token.clone(),
            _ => self.name.trim().to_string(),
        };
        let mut extensions: Vec<String> = Vec::new();
        for ext in self.extensions.split([',', ';', ' ', '\t', '\n']) {
            let ext = ext.trim().trim_start_matches('.').to_lowercase();
            if !ext.is_empty() && !extensions.contains(&ext) {
                extensions.push(ext);
            }
        }
        CategoryEntry {
            name,
            extensions,
            color: self.color,
        }
    }
}

/// How many categories get palette colors before the hues are spread out.
const PALETTE_SIZE: usize = 8;

/// The kind of color blindness, as named in warnings.
fn vision_name(v: Vision) -> String {
    match v {
        Vision::Protan => tr("CATS_PROTAN"),
        Vision::Deutan => tr("CATS_DEUTAN"),
        Vision::Tritan => tr("CATS_TRITAN"),
        Vision::Normal => String::new(),
    }
}

/// A button showing `color`, `size` big, outlined when `selected`.
fn swatch(
    ui: &mut egui::Ui,
    color: Color32,
    size: Vec2,
    selected: bool,
    label: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = ui.visuals();
    let stroke = if selected || response.has_focus() {
        egui::Stroke::new(2.0, visuals.strong_text_color())
    } else if response.hovered() {
        egui::Stroke::new(1.0, visuals.strong_text_color())
    } else {
        egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
    };
    ui.painter()
        .rect(rect, 4.0, color, stroke, egui::StrokeKind::Inside);
    let label = label.to_string();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    response
}

impl DiskScanApp {
    /// The Categories window, while it's open.
    pub(crate) fn categories_window_ui(&mut self, ui: &mut egui::Ui) {
        let Some(mut editor) = self.cat_editor.take() else {
            return;
        };
        let mut open = true;
        let (mut keep_open, mut saved) = (true, false);
        let color_blind = self.settings.color_blind_safe;
        tool_window(
            ui,
            "categories_window",
            &tr("CATS_TITLE"),
            [640.0, 640.0],
            &mut open,
            |ui| {
                let (stay, did_save) = editor.ui(ui, color_blind);
                keep_open &= stay;
                saved |= did_save;
            },
        );
        open &= keep_open;
        if saved {
            self.reload_categories();
        }
        if open {
            self.cat_editor = Some(editor);
        }
    }
}

impl CategoryEditor {
    /// The window's contents: whether it stays open, and whether the
    /// categories were saved.
    fn ui(&mut self, ui: &mut egui::Ui, color_blind: bool) -> (bool, bool) {
        let dark = ui.visuals().dark_mode;
        let problems = self.problems(dark, color_blind);
        let blocked = problems.iter().flatten().any(|p| p.blocks);
        let (mut keep_open, mut saved) = (true, false);
        // Rows are added, reset or removed after both panels are drawn, so
        // `problems` matches the rows drawn.
        let mut action = None;

        egui::Panel::bottom("cats_buttons").show(ui, |ui| {
            ui.add_space(6.0);
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else if blocked {
                ui.colored_label(ui.visuals().warn_fg_color, tr("CATS_FIX_FIRST"));
            }
            ui.horizontal(|ui| {
                if ui.button(tr("CATS_ADD")).clicked() {
                    action = Some(EditAction::Add);
                }
                if ui.button(tr("CATS_RESET")).clicked() {
                    action = Some(EditAction::Reset);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!blocked, egui::Button::new(tr("CATS_SAVE")))
                        .clicked()
                    {
                        match save_categories(&self.entries()) {
                            Ok(()) => (keep_open, saved) = (false, true),
                            Err(e) => {
                                let path = config_dir().join("categories.json");
                                self.error = Some(trf(
                                    "ERR_CATEGORIES_SAVE",
                                    &[&show_path(&path), &e.to_string()],
                                ));
                            }
                        }
                    }
                    if ui.button(tr("CATS_CANCEL")).clicked() {
                        keep_open = false;
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.label(egui::RichText::new(tr("CATS_INTRO")).small().weak());
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (i, (row, problems)) in self.rows.iter_mut().zip(&problems).enumerate() {
                        ui.push_id(row.id, |ui| {
                            if row_ui(ui, row, i, dark, color_blind, problems) {
                                action = Some(EditAction::Remove(i));
                            }
                        });
                        ui.separator();
                    }
                });
        });
        match action {
            Some(EditAction::Add) => {
                let row = self.row(CategoryEntry {
                    name: tr("CATS_NEW_NAME"),
                    extensions: Vec::new(),
                    color: None,
                });
                self.rows.push(row);
            }
            Some(EditAction::Reset) => self.set_entries(CategoryModel::defaults().entries()),
            Some(EditAction::Remove(i)) => {
                self.rows.remove(i);
            }
            None => {}
        }
        (keep_open, saved)
    }
}

/// A change to the list of rows, asked for this frame.
enum EditAction {
    Add,
    Reset,
    Remove(usize),
}

/// One category: color, name, remove button, its extensions and its
/// problems. Returns true when it's to be removed.
fn row_ui(
    ui: &mut egui::Ui,
    row: &mut Row,
    index: usize,
    dark: bool,
    color_blind: bool,
    problems: &[Problem],
) -> bool {
    let mut remove = false;
    let color = row
        .color
        .map_or(position_color(index, dark, color_blind), |c| c.on(dark));
    ui.horizontal(|ui| {
        let button = swatch(ui, color, Vec2::new(32.0, 22.0), false, &tr("CATS_COLOR"))
            .on_hover_text(tr("CATS_COLOR"));
        egui::Popup::from_toggle_button_response(&button)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| color_picker_ui(ui, row, dark));

        let name = ui.add(
            egui::TextEdit::singleline(&mut row.name)
                .desired_width(200.0)
                .hint_text(tr("CATS_NAME")),
        );
        let follows = row.token.as_ref().is_some_and(|t| tr(t) == row.name);
        if follows {
            name.on_hover_text(tr("CATS_NAME_BUILT_IN"));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("🗑").on_hover_text(tr("CATS_REMOVE")).clicked() {
                remove = true;
            }
        });
    });
    ui.add(
        egui::TextEdit::multiline(&mut row.extensions)
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .hint_text(tr("CATS_EXTENSIONS_HINT")),
    );
    for p in problems {
        let color = if p.blocks {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().warn_fg_color
        };
        ui.label(egui::RichText::new(&p.text).small().color(color));
    }
    remove
}

/// The color picker: automatic, one of the listed colors, or a typed code.
fn color_picker_ui(ui: &mut egui::Ui, row: &mut Row, dark: bool) {
    if ui
        .radio(row.color.is_none(), tr("CATS_COLOR_AUTO"))
        .clicked()
    {
        row.color = None;
    }
    ui.add_space(4.0);
    egui::Grid::new("color_list")
        .spacing([4.0, 4.0])
        .show(ui, |ui| {
            for (i, (id, _, _)) in COLOR_LIST.iter().enumerate() {
                let listed = CatColor::Listed(i);
                if swatch(
                    ui,
                    listed.on(dark),
                    Vec2::new(30.0, 20.0),
                    row.color == Some(listed),
                    id,
                )
                .on_hover_text(*id)
                .clicked()
                {
                    row.color = Some(listed);
                    row.hex = hex_text(listed.on(dark));
                }
                if i % SHADES == SHADES - 1 {
                    ui.end_row();
                }
            }
        });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let label = ui.label(tr("CATS_COLOR_CODE")).id;
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut row.hex)
                    .desired_width(80.0)
                    .hint_text("#RRGGBB"),
            )
            .labelled_by(label);
        if field.changed()
            && let Some(CatColor::Custom(c)) = CatColor::parse(&row.hex)
        {
            row.color = Some(CatColor::Custom(c));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> CategoryEditor {
        CategoryEditor::new(CategoryModel::defaults().entries())
    }

    #[test]
    fn unchanged_editor_gives_the_same_categories() {
        let e = editor();
        assert_eq!(e.entries(), CategoryModel::defaults().entries());
        assert!(e.problems(true, false).iter().all(Vec::is_empty));
        assert!(e.problems(false, true).iter().all(Vec::is_empty));
    }

    #[test]
    fn typed_names_and_extensions_are_read() {
        let mut e = editor();
        e.rows[0].extensions = ".MKV, mp4;;  webm\n.mkv ".to_string();
        e.rows[1].name = "  Sounds ".to_string();
        let entries = e.entries();
        assert_eq!(entries[0].extensions, ["mkv", "mp4", "webm"]);
        assert_eq!(entries[0].name, "CAT_VIDEO");
        assert_eq!(entries[1].name, "Sounds");
    }

    #[test]
    fn problems_block_saving_or_warn() {
        let blocking = |e: &CategoryEditor, row: usize| -> Vec<bool> {
            e.problems(true, false)[row]
                .iter()
                .map(|p| p.blocks)
                .collect()
        };
        let mut e = editor();
        e.rows[1].extensions.push_str(", mp4");
        assert_eq!(blocking(&e, 1), [true]);
        e.rows[1].name = String::new();
        assert_eq!(blocking(&e, 1), [true, true]);
        e.rows[1].name = tr("CAT_OTHER");
        assert_eq!(blocking(&e, 1), [true, true]);
        e.rows[1].name = tr("CAT_VIDEO").to_uppercase();
        assert_eq!(blocking(&e, 1), [true, true]);

        let mut e = editor();
        e.rows[2].extensions.push_str(", tar.gz");
        assert_eq!(blocking(&e, 2), [false]);
        // A ninth category without a color, and one in the same color as
        // the first.
        let row = e.row(CategoryEntry {
            name: "Ninth".to_string(),
            extensions: Vec::new(),
            color: None,
        });
        e.rows.push(row);
        assert!(blocking(&e, 8).contains(&false));
        e.rows[8].color = Some(CatColor::Custom(position_color(0, true, false)));
        let texts: Vec<String> = e.problems(true, false)[8]
            .iter()
            .map(|p| p.text.clone())
            .collect();
        assert!(
            texts.iter().any(|t| t.contains(&tr("CAT_VIDEO"))),
            "{texts:?}"
        );
        assert!(!texts.contains(&tr("CATS_PICK_COLOR")));
        // Black: hard to see on a dark background.
        e.rows[8].color = Some(CatColor::Custom(Color32::BLACK));
        let texts: Vec<String> = e.problems(true, false)[8]
            .iter()
            .map(|p| p.text.clone())
            .collect();
        assert!(texts.contains(&tr("CATS_LOW_CONTRAST_DARK")));
    }
}
