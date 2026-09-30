//! The Summary view's contents table: an ncdu-style, keyboard-driven listing
//! of the folder being viewed. `table_ui` draws it, `table_keys` handles its
//! keys (listed in the `?` help overlay, see `HELP_ROWS`), `table_overlays`
//! draws the details panel and dialogs, and
//! `table_frame_start` carries out deletes queued by the previous frame.

use super::*;
use config::TablePrefs;
use egui_extras::{Column, TableBuilder};

/// Width of the details panel ("i").
const INFO_PANEL_WIDTH: f32 = 340.0;

/// Columns that can be shown or hidden (Name is always shown).
#[derive(Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TableCol {
    Bar,
    Percent,
    Size,
    Files,
    Modified,
    Changed,
    Perms,
}

impl TableCol {
    pub(crate) const ALL: [TableCol; 7] = [
        TableCol::Bar,
        TableCol::Percent,
        TableCol::Size,
        TableCol::Files,
        TableCol::Modified,
        TableCol::Changed,
        TableCol::Perms,
    ];

    fn key(self) -> &'static str {
        match self {
            TableCol::Bar => "bar",
            TableCol::Percent => "percent",
            TableCol::Size => "size",
            TableCol::Files => "files",
            TableCol::Modified => "modified",
            TableCol::Changed => "changed",
            TableCol::Perms => "perms",
        }
    }

    /// The sort column this column shows, if it's sortable on its own.
    fn sort_column(self) -> Option<SortColumn> {
        match self {
            TableCol::Size => Some(SortColumn::Size),
            TableCol::Files => Some(SortColumn::Files),
            TableCol::Modified => Some(SortColumn::Modified),
            TableCol::Changed => Some(SortColumn::Changed),
            TableCol::Perms => Some(SortColumn::Perms),
            TableCol::Bar | TableCol::Percent => None,
        }
    }
}

impl SortColumn {
    /// The column that has to be visible to sort by this one.
    fn table_col(self) -> Option<TableCol> {
        match self {
            SortColumn::Size => Some(TableCol::Size),
            SortColumn::Files => Some(TableCol::Files),
            SortColumn::Modified => Some(TableCol::Modified),
            SortColumn::Changed => Some(TableCol::Changed),
            SortColumn::Perms => Some(TableCol::Perms),
            SortColumn::Name => None,
        }
    }
}

/// Everything the table's row order depends on.
#[derive(PartialEq)]
struct OrderKey {
    /// Bumped on every change to the displayed tree (see `tree_gen`), or,
    /// for the live table during a scan, on every refresh (`live_gen`).
    tree_gen: u64,
    /// Rows of the live table during a scan (from the preview tree).
    live: bool,
    view: Vec<usize>,
    sort: SortState,
    dirs_first: bool,
    show_dotfiles: bool,
    /// How many items are hidden (from the chart's right-click menu).
    hidden: usize,
}

/// The table's rows as indices into the viewed folder's children, in
/// display order, plus totals derived from the same pass.
struct RowOrder {
    key: OrderKey,
    idx: Vec<usize>,
    shown_size: u64,
    dotfile_size: u64,
}

/// Where `cursor` is in the rows `idx` (indices into `view`'s children).
/// `pos` is the last answer, checked first, so large folders aren't
/// searched every frame.
fn find_cursor(
    cursor: &Path,
    pos: &std::cell::Cell<Option<usize>>,
    view: &Node,
    idx: &[usize],
) -> Option<usize> {
    let at = |i: usize| idx.get(i).is_some_and(|&k| view.children[k].path == cursor);
    if let Some(i) = pos.get().filter(|&i| at(i)) {
        return Some(i);
    }
    let found = (0..idx.len()).find(|&i| at(i));
    pos.set(found);
    found
}

/// A rescan of one folder ("r"), to be spliced back into the full tree
/// when it finishes rather than replacing it.
pub(crate) struct Graft {
    target: PathBuf,
    /// The zoom history and cursor to come back to afterwards (or on abort).
    view_paths: Vec<PathBuf>,
    cursor: Option<PathBuf>,
    free_space: Option<(u64, u64)>,
}

pub(crate) struct TableState {
    /// Row under the cursor, highlighted and moved like ncdu's.
    pub cursor: Option<PathBuf>,
    /// Where `cursor` was last found in the rows (see `find_cursor`).
    cursor_pos: std::cell::Cell<Option<usize>>,
    /// Row order as last computed (see `OrderKey`).
    order: Option<RowOrder>,
    /// Scroll the table to the cursor row on the next draw.
    pub scroll_pending: bool,
    /// Rows that fit on screen, for PageUp/PageDown.
    page_rows: usize,
    pub dirs_first: bool,
    /// Off only until the app is closed ("e"); never saved.
    show_dotfiles: bool,
    hidden_cols: HashSet<TableCol>,
    /// Left-to-right order of the optional columns (Name is always last).
    col_order: Vec<TableCol>,
    /// Column `<`/`>` move: the one last sorted by, shown or hidden.
    active_col: Option<TableCol>,
    /// Rows marked with Space; D/T act on these instead of the cursor row.
    /// Only ever holds rows of `marks_for`, the folder they were made in.
    marked: HashSet<PathBuf>,
    marks_for: Option<PathBuf>,
    /// Text typed after "/", while the jump field is open.
    jump: Option<String>,
    jump_focus_pending: bool,
    show_info: bool,
    show_help: bool,
}

impl Default for TableState {
    fn default() -> Self {
        TableState {
            cursor: None,
            order: None,
            cursor_pos: Default::default(),
            scroll_pending: false,
            page_rows: 10,
            dirs_first: false,
            show_dotfiles: true,
            hidden_cols: HashSet::new(),
            col_order: TableCol::ALL.to_vec(),
            active_col: None,
            marked: HashSet::new(),
            marks_for: None,
            jump: None,
            jump_focus_pending: false,
            show_info: false,
            show_help: false,
        }
    }
}

/// Replaces the node at `target` with `new`, adjusting the size and file
/// count of every folder above it. Returns the (old, new) (size, file
/// count), or None if `target` isn't in the tree.
pub(crate) fn replace_in_tree(
    node: &mut Node,
    target: &Path,
    new: Node,
) -> Option<((u64, u64), (u64, u64))> {
    let parts = rel_parts(&node.path, target)?;
    replace_at(node, &parts, new)
}

fn replace_at(
    node: &mut Node,
    parts: &[&std::ffi::OsStr],
    new: Node,
) -> Option<((u64, u64), (u64, u64))> {
    let (first, rest) = parts.split_first()?;
    let i = child_named(node, first)?;
    let (old, new) = if rest.is_empty() {
        let n = (new.size, new.file_count);
        let o = std::mem::replace(&mut node.children[i], new);
        ((o.size, o.file_count), n)
    } else {
        deep(|| replace_at(&mut node.children[i], rest, new))?
    };
    node.size = node.size.saturating_sub(old.0).saturating_add(new.0);
    node.file_count = node.file_count.saturating_sub(old.1).saturating_add(new.1);
    Some((old, new))
}

/// Keys and what they do, for the `?` overlay: (keys lang key, description
/// lang key), grouped by section heading.
const HELP_ROWS: &[(&str, &[(&str, &str)])] = &[
    (
        "HELP_SECTION_MOVE",
        &[
            ("HELP_KEYS_UPDOWN", "HELP_UPDOWN"),
            ("HELP_KEYS_PAGE", "HELP_PAGE"),
            ("HELP_KEYS_HOMEEND", "HELP_HOMEEND"),
            ("HELP_KEYS_OPEN", "HELP_OPEN"),
            ("HELP_KEYS_ENTER", "HELP_ENTER"),
            ("HELP_KEYS_PARENT", "HELP_PARENT"),
            ("HELP_KEYS_JUMP", "HELP_JUMP"),
        ],
    ),
    (
        "HELP_SECTION_SORT",
        &[
            ("HELP_KEYS_SORT", "HELP_SORT"),
            ("HELP_KEYS_COLUMNS", "HELP_COLUMNS"),
            ("HELP_KEYS_MOVE_COL", "HELP_MOVE_COL"),
            ("HELP_KEYS_DIRS_FIRST", "HELP_DIRS_FIRST"),
            ("HELP_KEYS_DOTFILES", "HELP_DOTFILES"),
        ],
    ),
    (
        "HELP_SECTION_ACT",
        &[
            ("HELP_KEYS_MARK", "HELP_MARK"),
            ("HELP_KEYS_DELETE", "HELP_DELETE"),
            ("HELP_KEYS_TRASH", "HELP_TRASH"),
            ("HELP_KEYS_RESCAN", "HELP_RESCAN"),
            ("HELP_KEYS_INFO", "HELP_INFO"),
            ("HELP_KEYS_ESC", "HELP_ESC"),
            ("HELP_KEYS_HELP", "HELP_HELP"),
        ],
    ),
];

/// A cell of the table, left to right.
#[derive(Clone, Copy, PartialEq)]
enum Cell {
    Mark,
    Opt(TableCol),
    Name,
}

impl DiskScanApp {
    /// The table's layout, as saved in settings.json.
    pub(crate) fn table_prefs(&self) -> TablePrefs {
        TablePrefs {
            sort: self.contents_sort.column,
            ascending: self.contents_sort.ascending,
            hidden_columns: TableCol::ALL
                .into_iter()
                .filter(|c| self.table.hidden_cols.contains(c))
                .collect(),
            column_order: self.table.col_order.clone(),
            dirs_first: self.table.dirs_first,
        }
    }

    pub(crate) fn apply_table_prefs(&mut self, p: &TablePrefs) {
        self.contents_sort = SortState {
            column: p.sort,
            ascending: p.ascending,
        };
        self.table.hidden_cols = p.hidden_columns.iter().copied().collect();
        // Every column exactly once: unknown ones dropped, missing ones appended.
        let mut order: Vec<TableCol> = Vec::new();
        for c in p.column_order.iter().copied().chain(TableCol::ALL) {
            if !order.contains(&c) {
                order.push(c);
            }
        }
        self.table.col_order = order;
        self.table.dirs_first = p.dirs_first;
    }

    /// Start of every frame: keeps keyboard focus off the table's buttons,
    /// so a clicked header doesn't also react to Enter or Space.
    pub(crate) fn table_frame_start(&mut self, ctx: &egui::Context) {
        if self.summary_view && !self.typing && !self.delete_dialog_open() && !self.table.show_help
        {
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
    }

    /// Draws the contents table of `view_node`, no taller than `max_height`.
    pub(crate) fn table_ui(&mut self, ui: &mut egui::Ui, view_node: &Node, max_height: f32) {
        // Marks belong to the folder they were made in.
        if self.table.marks_for.as_deref() != Some(view_node.path.as_path()) {
            self.table.marked.clear();
            self.table.marks_for = Some(view_node.path.clone());
        }

        // The row order is recomputed only when something it depends on changes.
        let key = OrderKey {
            tree_gen: if self.scanning {
                self.live_gen
            } else {
                self.tree_gen
            },
            view: if self.scanning {
                vec![]
            } else {
                self.current_view().clone()
            },
            live: self.scanning,
            sort: self.contents_sort,
            dirs_first: self.table.dirs_first,
            show_dotfiles: self.table.show_dotfiles,
            hidden: self.hidden.len(),
        };
        let order = match self.table.order.take() {
            Some(o) if o.key == key => o,
            _ => self.row_order(view_node, key),
        };
        let row = |i: usize| &view_node.children[order.idx[i]];
        let n_rows = order.idx.len();
        let shown_size = order.shown_size;
        // The cursor goes to the first row when it isn't in this folder.
        let found = self
            .table
            .cursor
            .as_ref()
            .and_then(|c| find_cursor(c, &self.table.cursor_pos, view_node, &order.idx));
        let cursor_row = match found {
            Some(i) => Some(i),
            None => {
                self.table.cursor = (n_rows > 0).then(|| row(0).path.clone());
                let first = self.table.cursor.is_some().then_some(0);
                self.table.cursor_pos.set(first);
                first
            }
        };
        let scroll_to_cursor = std::mem::take(&mut self.table.scroll_pending);

        // Heading line: title, what's switched on, and where the keys are.
        ui.horizontal(|ui| {
            ui.heading(tr("SUMMARY_CONTENTS"));
            if self.table.dirs_first {
                ui.weak(format!("· {}", tr("TABLE_TAG_DIRS_FIRST")));
            }
            if !self.table.show_dotfiles {
                ui.weak(format!(
                    "· {}",
                    trf(
                        "TABLE_TAG_DOTFILES_HIDDEN",
                        &[&human_size(order.dotfile_size)]
                    )
                ));
            }
            if !self.table.marked.is_empty() {
                let size: u64 = (0..n_rows)
                    .map(row)
                    .filter(|c| self.table.marked.contains(&c.path))
                    .map(|c| c.size)
                    .fold(0u64, u64::saturating_add);
                ui.strong(format!(
                    "· {}",
                    trf(
                        "TABLE_TAG_MARKED",
                        &[
                            &format_count(self.table.marked.len() as u64),
                            &human_size(size)
                        ]
                    )
                ));
            }
            if self.scanning {
                ui.weak(format!("· {}", tr("TABLE_TAG_SCANNING")));
            }
            ui.weak(format!("· {}", tr("TABLE_HELP_HINT")));
        });
        if let Some(query) = &mut self.table.jump {
            let mut close = false;
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(tr("TABLE_JUMP"));
                let r = ui.add(egui::TextEdit::singleline(query).desired_width(240.0));
                if std::mem::take(&mut self.table.jump_focus_pending) {
                    r.request_focus();
                }
                changed = r.changed();
                // Enter keeps the cursor where the search put it; Esc or a click
                // elsewhere closes the field.
                close = r.lost_focus();
            });
            if changed && !query.is_empty() {
                let q = query.to_lowercase();
                let hit = (0..n_rows)
                    .position(|i| row(i).name.to_lowercase().starts_with(&q))
                    .or_else(|| (0..n_rows).position(|i| row(i).name.to_lowercase().contains(&q)));
                if let Some(i) = hit {
                    self.table.cursor = Some(row(i).path.clone());
                    self.table.scroll_pending = true;
                }
            }
            if close {
                self.table.jump = None;
            }
        }
        ui.add_space(4.0);

        let mut cells = vec![Cell::Mark];
        cells.extend(
            self.table
                .col_order
                .iter()
                .filter(|c| !self.table.hidden_cols.contains(c))
                .map(|c| Cell::Opt(*c)),
        );
        cells.push(Cell::Name);

        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let header_h = ui.spacing().interact_size.y + 4.0;
        let table_h = (max_height - header_h).max(row_h * 3.0);
        self.table.page_rows = ((table_h / row_h) as usize).saturating_sub(1).max(1);

        // Percentages and bars are shares of the listed rows, so they add up
        // to 100% even with dotfiles hidden.
        let total = shown_size.max(1);
        let bar_fill = ui.visuals().selection.bg_fill;
        let bar_frame = ui.visuals().weak_text_color();
        let dir_color = ui.visuals().hyperlink_color;
        let mark_color = ui.visuals().warn_fg_color;
        // The cursor row is drawn in the selection's text color...
        let selected_fg = ui.visuals().selection.stroke.color;
        // ...and its size bar gets a track in the window background color.
        let panel_bg = ui.visuals().panel_fill;
        let marked = &self.table.marked;

        let mut clicked: Option<usize> = None;
        let mut double_clicked: Option<usize> = None;
        let mut ctrl_clicked: Option<usize> = None;
        let show_info = self.table.show_info;
        // During a scan, an unfinished folder has only a name and running totals
        // (no mode yet).
        let live = self.scanning;
        let pending = |c: &Node| live && c.is_dir && c.mode == 0;
        let sort_before = self.contents_sort;
        let contents_sort = &mut self.contents_sort;
        let user_cache = &mut self.user_cache;
        let group_cache = &mut self.group_cache;
        // Column widths are remembered per set of visible columns, so hiding one
        // doesn't give its width to a neighbor.
        let layout_key: Vec<&str> = cells
            .iter()
            .map(|c| match c {
                Cell::Mark => "mark",
                Cell::Opt(t) => t.key(),
                Cell::Name => "name",
            })
            .collect();
        ui.scope(|ui| {
            // Cell text isn't selectable, so a click reaches the row.
            ui.style_mut().interaction.selectable_labels = false;
            // Column resize handles show only while hovered or dragged.
            ui.visuals_mut().widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
            if show_info {
                ui.set_max_width(ui.available_width() - INFO_PANEL_WIDTH - 8.0);
            }
            let mut tb = TableBuilder::new(ui)
                .id_salt(("contents_table", layout_key.join(",")))
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .min_scrolled_height(0.0)
                .max_scroll_height(table_h)
                // Scroll to the cursor row at once, without animation.
                .animate_scrolling(false)
                .auto_shrink([false, true]);
            for cell in &cells {
                tb = tb.column(match cell {
                    Cell::Mark => Column::exact(12.0),
                    Cell::Opt(TableCol::Bar) => Column::exact(92.0),
                    Cell::Opt(_) => Column::auto().at_least(40.0),
                    Cell::Name => Column::remainder().at_least(120.0),
                });
            }
            if let (true, Some(r)) = (scroll_to_cursor, cursor_row) {
                tb = tb.scroll_to_row(r, None);
            }
            tb.header(header_h, |mut header| {
                for cell in &cells {
                    header.col(|ui| match cell {
                        Cell::Mark | Cell::Opt(TableCol::Bar) => {}
                        Cell::Opt(TableCol::Percent) => {
                            ui.strong(tr("COL_PERCENT"));
                        }
                        Cell::Opt(TableCol::Size) => {
                            sortable_header(ui, &tr("COL_SIZE"), SortColumn::Size, contents_sort);
                        }
                        Cell::Opt(TableCol::Files) => {
                            sortable_header(ui, &tr("COL_FILES"), SortColumn::Files, contents_sort);
                        }
                        Cell::Opt(TableCol::Modified) => {
                            sortable_header(
                                ui,
                                &tr("COL_MODIFIED"),
                                SortColumn::Modified,
                                contents_sort,
                            )
                            .on_hover_text(tr("COL_MODIFIED_TIP"));
                        }
                        Cell::Opt(TableCol::Changed) => {
                            sortable_header(
                                ui,
                                &tr("COL_CHANGED"),
                                SortColumn::Changed,
                                contents_sort,
                            )
                            .on_hover_text(tr("COL_CHANGED_TIP"));
                        }
                        Cell::Opt(TableCol::Perms) => {
                            sortable_header(ui, &tr("COL_PERMS"), SortColumn::Perms, contents_sort);
                        }
                        Cell::Name => {
                            sortable_header(ui, &tr("COL_NAME"), SortColumn::Name, contents_sort);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, n_rows, |mut tr_row| {
                    let i = tr_row.index();
                    let c = row(i);
                    let is_marked = marked.contains(&c.path);
                    let selected = Some(i) == cursor_row;
                    tr_row.set_selected(selected);
                    let pick = |normal: Color32| if selected { selected_fg } else { normal };
                    for cell in &cells {
                        tr_row.col(|ui| match cell {
                            Cell::Mark => {
                                if is_marked {
                                    let (r, _) = ui.allocate_exact_size(
                                        Vec2::splat(10.0),
                                        egui::Sense::hover(),
                                    );
                                    let stroke = egui::Stroke::new(1.8, pick(mark_color));
                                    ui.painter().add(egui::Shape::line(
                                        vec![
                                            Pos2::new(r.left(), r.center().y),
                                            Pos2::new(r.left() + r.width() * 0.4, r.bottom() - 1.0),
                                            Pos2::new(r.right(), r.top() + 1.0),
                                        ],
                                        stroke,
                                    ));
                                }
                            }
                            Cell::Opt(TableCol::Bar) => {
                                // Share of the folder, matching the % column.
                                let (r, _) = ui.allocate_exact_size(
                                    Vec2::new(86.0, row_h * 0.55),
                                    egui::Sense::hover(),
                                );
                                let frac = c.size as f32 / total as f32;
                                if selected {
                                    // A dark track under the fill.
                                    ui.painter()
                                        .rect_filled(r, egui::CornerRadius::ZERO, panel_bg);
                                }
                                if frac > 0.0 {
                                    let filled = egui::Rect::from_min_size(
                                        r.min,
                                        Vec2::new((r.width() * frac).max(1.0), r.height()),
                                    );
                                    ui.painter().rect_filled(
                                        filled,
                                        egui::CornerRadius::ZERO,
                                        bar_fill,
                                    );
                                }
                                ui.painter().rect_stroke(
                                    r,
                                    egui::CornerRadius::ZERO,
                                    egui::Stroke::new(
                                        1.0,
                                        if selected { panel_bg } else { bar_frame },
                                    ),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            Cell::Opt(TableCol::Percent) => {
                                ui.label(format!("{:.1}%", c.size as f64 * 100.0 / total as f64));
                            }
                            Cell::Opt(TableCol::Size) => {
                                // Right-aligned, so sizes line up by unit.
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(human_size(c.size));
                                    },
                                );
                            }
                            Cell::Opt(TableCol::Files) => {
                                ui.label(format_count(c.file_count));
                            }
                            // A folder still being scanned shows "…".
                            Cell::Opt(TableCol::Modified | TableCol::Changed | TableCol::Perms)
                                if pending(c) =>
                            {
                                ui.weak("…");
                            }
                            Cell::Opt(TableCol::Modified) => {
                                ui.label(format_epoch(c.mtime));
                            }
                            Cell::Opt(TableCol::Changed) => {
                                ui.label(format_epoch(c.ctime));
                            }
                            Cell::Opt(TableCol::Perms) => {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} {}",
                                        format_mode_ls(c.mode, c.is_dir),
                                        format_owner(c.uid, c.gid, user_cache, group_cache)
                                    ))
                                    .monospace(),
                                );
                            }
                            Cell::Name => {
                                // Folders stand out by color alone.
                                let mut text = egui::RichText::new(&c.name);
                                if c.is_dir || selected {
                                    text = text.color(pick(dir_color));
                                }
                                if is_marked {
                                    text = text.strong();
                                }
                                ui.add(egui::Label::new(text).truncate());
                            }
                        });
                    }
                    let r = tr_row.response();
                    // Ctrl+click marks like Space (a second click unmarks, never opens).
                    if r.clicked() && r.ctx.input(|i| i.modifiers.command) {
                        ctrl_clicked = Some(i);
                    } else if r.double_clicked() {
                        double_clicked = Some(i);
                    } else if r.clicked() {
                        clicked = Some(i);
                    }
                });
            });
        });

        // A clicked header's column becomes the one `<`/`>` move.
        if self.contents_sort != sort_before {
            self.table.active_col = self.contents_sort.column.table_col();
        }
        if let Some(i) = clicked.or(double_clicked).or(ctrl_clicked) {
            self.table.cursor = Some(row(i).path.clone());
        }
        if let Some(i) = ctrl_clicked {
            self.toggle_mark_of(row(i).path.clone());
        }
        self.table.order = Some(order);
        if double_clicked.is_some() {
            self.open_cursor();
        }
    }

    /// Filters and sorts `view_node`'s children for the table (see
    /// `OrderKey`), refreshing the row list the keys work on.
    fn row_order(&mut self, view_node: &Node, key: OrderKey) -> RowOrder {
        let children = &view_node.children;
        let hidden = &self.hidden;
        let mut idx: Vec<usize> = (0..children.len())
            .filter(|&i| {
                let c = &children[i];
                (key.show_dotfiles || !c.name.starts_with('.'))
                    && (hidden.is_empty() || !hidden.contains(&c.path))
            })
            .collect();
        let cs = key.sort;
        // Sort keys are built once into compact arrays, then sorted (much faster
        // on huge folders than comparing nodes). Folders-first puts group 0
        // (folders) before 1; the index breaks ties, so the order is stable.
        let group = |c: &Node| u8::from(key.dirs_first && !c.is_dir);
        if cs.column == SortColumn::Name {
            // Natural, case-insensitive order (see natural_key).
            let mut keyed: Vec<(u8, Vec<u8>, &str, u32)> = idx
                .par_iter()
                .map(|&i| {
                    (
                        group(&children[i]),
                        natural_key(&children[i].name),
                        children[i].name.as_str(),
                        i as u32,
                    )
                })
                .collect();
            keyed.par_sort_unstable_by(|a, b| {
                let by_name = a.1.cmp(&b.1).then_with(|| a.2.cmp(b.2));
                let by_name = if cs.ascending {
                    by_name
                } else {
                    by_name.reverse()
                };
                a.0.cmp(&b.0).then(by_name).then(a.3.cmp(&b.3))
            });
            idx = keyed.into_iter().map(|(_, _, _, i)| i as usize).collect();
        } else {
            // One u128 per row: group (bit 108) | value (76 bits, enough for
            // mode+uid+gid) | index (32 bits). Descending flips the value.
            const VALUE_BITS: u32 = 76;
            const VALUE_MAX: u128 = (1 << VALUE_BITS) - 1;
            let signed = |v: i64| (v as u64 ^ (1 << 63)) as u128;
            let mut keyed: Vec<u128> = idx
                .iter()
                .map(|&i| {
                    let c = &children[i];
                    let v = match cs.column {
                        SortColumn::Size => c.size as u128,
                        SortColumn::Files => c.file_count as u128,
                        SortColumn::Modified => signed(c.mtime),
                        SortColumn::Changed => signed(c.ctime),
                        SortColumn::Perms => {
                            ((c.mode & 0o7777) as u128) << 64
                                | (c.uid as u128) << 32
                                | c.gid as u128
                        }
                        SortColumn::Name => 0,
                    };
                    let v = if cs.ascending { v } else { VALUE_MAX - v };
                    (group(c) as u128) << (VALUE_BITS + 32) | v << 32 | i as u128
                })
                .collect();
            keyed.par_sort_unstable();
            idx = keyed
                .into_iter()
                .map(|k| (k & 0xFFFF_FFFF) as usize)
                .collect();
        }
        RowOrder {
            shown_size: idx
                .iter()
                .map(|&i| children[i].size)
                .fold(0u64, u64::saturating_add),
            dotfile_size: children
                .iter()
                .filter(|c| c.name.starts_with('.'))
                .map(|c| c.size)
                .fold(0u64, u64::saturating_add),
            idx,
            key,
        }
    }

    /// Details panel (i) and help (?), drawn over the main area `area`.
    pub(crate) fn table_overlays(&mut self, ctx: &egui::Context, area: egui::Rect) {
        if self.table.show_info && !self.scanning {
            self.info_panel(ctx, area);
        }
        if self.table.show_help {
            self.help_overlay(ctx);
        }
    }

    /// Details of the row under the cursor, in the top-right corner.
    fn info_panel(&mut self, ctx: &egui::Context, area: egui::Rect) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let Some(cursor) = self.table.cursor.clone() else {
            return;
        };
        let Some(n) = self
            .current_view_node(&root)
            .children
            .iter()
            .find(|c| c.path == cursor)
        else {
            return;
        };
        let h = HoverInfo {
            path: n.path.clone(),
            size: n.size,
            file_count: n.file_count,
            is_dir: n.is_dir,
            is_free: false,
            is_other: false,
            mode: Some(n.mode),
            mtime: Some(n.mtime),
            ctime: Some(n.ctime),
            uid: Some(n.uid),
            gid: Some(n.gid),
        };
        if !h.is_dir {
            self.ensure_mime_lookup(&h.path);
        }
        let mime = if h.is_dir {
            None
        } else {
            self.mime_cache.get(&h.path).cloned().flatten()
        };
        egui::Area::new("table_info".into())
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(area.right_top() + Vec2::new(-8.0, 8.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(INFO_PANEL_WIDTH - 16.0);
                    ui.horizontal(|ui| {
                        let (r, _) =
                            ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::hover());
                        let color = ui.visuals().text_color();
                        if h.is_dir {
                            draw_folder_icon(ui.painter(), r, color);
                        } else {
                            draw_file_icon(ui.painter(), r, color);
                        }
                        ui.add(
                            egui::Label::new(egui::RichText::new(short_path(&h.path)).monospace())
                                .wrap(),
                        );
                    });
                    ui.separator();
                    details_grid(
                        ui,
                        egui::Id::new("table_info_grid"),
                        &h,
                        mime.as_deref(),
                        &mut self.user_cache,
                        &mut self.group_cache,
                    );
                });
            });
    }

    fn help_overlay(&mut self, ctx: &egui::Context) {
        let modal = egui::Modal::new("table_help".into()).show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.heading(tr("HELP_TITLE"));
            for (section, rows) in HELP_ROWS {
                ui.add_space(8.0);
                ui.strong(tr(section));
                egui::Grid::new(egui::Id::new("help_grid").with(section))
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for (keys, desc) in *rows {
                            ui.label(egui::RichText::new(tr(keys)).monospace());
                            ui.label(tr(desc));
                            ui.end_row();
                        }
                    });
            }
            ui.add_space(10.0);
            ui.button(tr("HELP_CLOSE")).clicked()
        });
        if modal.inner || modal.should_close() {
            self.table.show_help = false;
        }
    }

    /// Table keys, once per frame after drawing.
    pub(crate) fn table_keys(&mut self, ctx: &egui::Context) {
        if self.table.show_help {
            // "?" closes the help. Checked before the overlay is drawn, so the press
            // that opened it doesn't also close it.
            if ctx.input(|i| {
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Text(t) if t == "?"))
            }) {
                self.table.show_help = false;
            }
            return;
        }
        if self.typing || self.delete_dialog_open() {
            return;
        }
        use egui::Key;
        let (arrow, named, typed) = ctx.input(|i| {
            let plain = !i.modifiers.command && !i.modifiers.alt;
            let arrow = arrow_nav(i);
            let named: Vec<Key> = [
                Key::PageUp,
                Key::PageDown,
                Key::Home,
                Key::End,
                Key::Enter,
                Key::Backspace,
                Key::Escape,
                Key::Space,
            ]
            .into_iter()
            .filter(|k| plain && i.key_pressed(*k))
            .collect();
            // Letters and symbols come from text input, so they follow the keyboard
            // layout, and Shift gives the uppercase commands.
            let typed: String = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Text(t) if plain => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            (arrow, named, typed)
        });

        if let Some(d) = arrow {
            self.table_nav(d);
        }
        for k in named {
            match k {
                Key::PageUp => self.move_cursor(-(self.table.page_rows as isize)),
                Key::PageDown => self.move_cursor(self.table.page_rows as isize),
                Key::Home => self.move_cursor(isize::MIN / 2),
                Key::End => self.move_cursor(isize::MAX / 2),
                Key::Enter => self.open_cursor(),
                Key::Backspace => self.go_parent(),
                Key::Escape => {
                    if !self.table.marked.is_empty() {
                        self.table.marked.clear();
                    } else {
                        self.table.show_info = false;
                    }
                }
                Key::Space => self.toggle_mark(),
                _ => {}
            }
        }
        for ch in typed.chars() {
            match ch {
                's' => self.sort_by(SortColumn::Size),
                'n' => self.sort_by(SortColumn::Name),
                'f' => self.sort_by(SortColumn::Files),
                'm' => self.sort_by(SortColumn::Modified),
                'c' => self.sort_by(SortColumn::Changed),
                'p' => self.sort_by(SortColumn::Perms),
                'S' => self.toggle_col(TableCol::Size),
                'F' => self.toggle_col(TableCol::Files),
                'M' => self.toggle_col(TableCol::Modified),
                'C' => self.toggle_col(TableCol::Changed),
                'P' => self.toggle_col(TableCol::Perms),
                '%' => self.toggle_col(TableCol::Percent),
                '<' => self.move_col(false),
                '>' => self.move_col(true),
                'G' => self.toggle_col(TableCol::Bar),
                't' => {
                    self.table.dirs_first = !self.table.dirs_first;
                    self.table.scroll_pending = true;
                }
                'e' => {
                    self.table.show_dotfiles = !self.table.show_dotfiles;
                    self.table.scroll_pending = true;
                }
                'i' if !self.scanning => self.table.show_info = !self.table.show_info,
                'r' if !self.scanning => self.rescan_current(),
                '?' => self.table.show_help = true,
                '/' => {
                    self.table.jump = Some(String::new());
                    self.table.jump_focus_pending = true;
                }
                'D' => self.request_delete(),
                'T' if self.scanning => {}
                'T' => {
                    self.queue_trash(self.selected_targets());
                    ctx.request_repaint();
                }
                _ => {}
            }
        }
    }

    /// The folder shown and its children's indices in display order; None if
    /// the tree changed since the rows were computed.
    fn listed(&self) -> Option<(&Node, &[usize])> {
        let order = self.table.order.as_ref()?;
        if self.scanning {
            // The live table: rows of its snapshot of the preview tree.
            let current = order.key.live && order.key.tree_gen == self.live_gen;
            return current.then_some((&self.live_view, &order.idx[..]));
        }
        if order.key.live
            || order.key.tree_gen != self.tree_gen
            || Some(&order.key.view) != self.view_stack.last()
        {
            return None;
        }
        Some((get_node(self.root.as_ref()?, &order.key.view), &order.idx))
    }

    fn cursor_index(&self) -> Option<usize> {
        let (view, idx) = self.listed()?;
        find_cursor(
            self.table.cursor.as_ref()?,
            &self.table.cursor_pos,
            view,
            idx,
        )
    }

    /// Path of the cursor row, and whether it's a folder.
    fn cursor_row(&self) -> Option<(PathBuf, bool)> {
        let i = self.cursor_index()?;
        let (view, idx) = self.listed()?;
        let n = &view.children[idx[i]];
        Some((n.path.clone(), n.is_dir))
    }

    /// Arrow keys: ⬆⬇ previous/next row, ⬅ parent folder, ➡ open the
    /// folder under the cursor.
    fn table_nav(&mut self, dir: NavDir) {
        match dir {
            NavDir::Prev => self.move_cursor(-1),
            NavDir::Next => self.move_cursor(1),
            NavDir::Out => self.go_parent(),
            NavDir::In => {
                if let Some((path, true)) = self.cursor_row() {
                    self.open_dir(&path);
                }
            }
        }
    }

    /// Moves the cursor `delta` rows, stopping at the first/last row.
    fn move_cursor(&mut self, delta: isize) {
        let from = self.cursor_index();
        let Some((view, idx)) = self.listed() else {
            return;
        };
        let n = idx.len();
        if n == 0 {
            return;
        }
        let i = match from {
            None => 0,
            Some(i) => (i as isize).saturating_add(delta).clamp(0, n as isize - 1) as usize,
        };
        self.table.cursor = Some(view.children[idx[i]].path.clone());
        self.table.cursor_pos.set(Some(i));
        self.table.scroll_pending = true;
    }

    fn open_dir(&mut self, path: &Path) {
        if self.scanning {
            return; // opening folders waits for the scan to finish
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        if let Some(vp) = index_path_to(&root, path) {
            self.view_stack.push(vp);
            self.table.cursor = None;
            self.table.scroll_pending = true;
        }
    }

    /// Up to the parent folder, with the folder just left under the cursor.
    fn go_parent(&mut self) {
        if self.scanning {
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        let view = self.current_view().clone();
        if let Some((_, parent_view)) = view.split_last() {
            let left = get_node(&root, &view).path.clone();
            self.set_current_view(parent_view.to_vec());
            self.table.cursor = Some(left);
            self.table.scroll_pending = true;
        }
    }

    /// Enter / double-click: opens the folder under the cursor, or hands a
    /// file to the desktop's default application for it.
    fn open_cursor(&mut self) {
        if self.scanning {
            return;
        }
        let Some((path, is_dir)) = self.cursor_row() else {
            return;
        };
        if is_dir {
            self.open_dir(&path);
            return;
        }
        match std::process::Command::new("xdg-open").arg(&path).spawn() {
            // Waited for on another thread, so it doesn't become a zombie.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => self.log_issue(trf("ERR_OPEN_FAILED", &[&show_path(&path), &e.to_string()])),
        }
    }

    /// s/n/f/m/c/p: sort by `col`, or flip the order if already sorted by it.
    /// A hidden column is shown again, so the sort is visible.
    fn sort_by(&mut self, col: SortColumn) {
        let s = &mut self.contents_sort;
        if s.column == col {
            s.ascending = !s.ascending;
        } else {
            s.column = col;
            s.ascending = col.default_ascending();
        }
        if let Some(tc) = col.table_col() {
            self.table.hidden_cols.remove(&tc);
        }
        self.table.active_col = col.table_col();
        self.table.scroll_pending = true;
    }

    /// S/F/M/C/P/%/G: show or hide a column. Hiding the column the table is
    /// sorted by falls back to size (largest first), or name if size is
    /// hidden too.
    fn toggle_col(&mut self, col: TableCol) {
        self.table.active_col = Some(col);
        if !self.table.hidden_cols.remove(&col) {
            self.table.hidden_cols.insert(col);
            if col.sort_column() == Some(self.contents_sort.column) {
                self.contents_sort = if self.table.hidden_cols.contains(&TableCol::Size) {
                    SortState {
                        column: SortColumn::Name,
                        ascending: true,
                    }
                } else {
                    SortState {
                        column: SortColumn::Size,
                        ascending: false,
                    }
                };
                self.table.scroll_pending = true;
            }
        }
    }

    /// `<`/`>`: moves the active column (last sorted by, shown or hidden —
    /// else the sort column) one visible column left/right. Name stays
    /// last: it takes whatever width is left.
    fn move_col(&mut self, right: bool) {
        let Some(col) = self
            .table
            .active_col
            .or_else(|| self.contents_sort.column.table_col())
        else {
            return;
        };
        let TableState {
            col_order,
            hidden_cols,
            ..
        } = &mut self.table;
        if hidden_cols.contains(&col) {
            return;
        }
        let Some(i) = col_order.iter().position(|c| *c == col) else {
            return;
        };
        let visible = |c: &TableCol| !hidden_cols.contains(c);
        // Hop over hidden columns to the next visible one.
        let target = if right {
            col_order[i + 1..]
                .iter()
                .position(visible)
                .map(|j| i + 1 + j)
        } else {
            col_order[..i].iter().rposition(visible)
        };
        if let Some(t) = target {
            let c = col_order.remove(i);
            col_order.insert(t, c);
        }
    }

    /// Space: marks/unmarks the cursor row and moves down, like ncdu.
    fn toggle_mark(&mut self) {
        let Some(c) = self.table.cursor.clone() else {
            return;
        };
        self.toggle_mark_of(c);
        self.move_cursor(1);
    }

    fn toggle_mark_of(&mut self, path: PathBuf) {
        if !self.table.marked.remove(&path) {
            self.table.marked.insert(path);
        }
    }

    /// What D/T act on: the marked rows (in table order), or else the
    /// cursor row.
    fn selected_targets(&self) -> Vec<PathBuf> {
        if self.table.marked.is_empty() {
            self.table.cursor.iter().cloned().collect()
        } else {
            match self.listed() {
                Some((view, idx)) => idx
                    .iter()
                    .map(|&k| &view.children[k].path)
                    .filter(|p| self.table.marked.contains(*p))
                    .cloned()
                    .collect(),
                None => self.table.marked.iter().cloned().collect(),
            }
        }
    }

    /// D: asks to permanently delete the marked rows or the cursor row.
    fn request_delete(&mut self) {
        // Sizes are still partial while scanning: delete waits for the end.
        if self.scanning {
            return;
        }
        self.ask_delete(self.selected_targets());
    }

    /// Rows about to leave the tree (deleted, trashed): the cursor moves to
    /// the next row that stays, and their marks go.
    pub(crate) fn table_forget(&mut self, gone: &[PathBuf]) {
        let gone: HashSet<&PathBuf> = gone.iter().collect();
        if let (Some(pos), Some((view, idx))) = (self.cursor_index(), self.listed()) {
            let path = |k: &usize| &view.children[*k].path;
            let next = idx[pos..]
                .iter()
                .find(|k| !gone.contains(path(k)))
                .or_else(|| idx[..pos].iter().rev().find(|k| !gone.contains(path(k))));
            self.table.cursor = next.map(|k| path(k).clone());
        }
        self.table.marked.retain(|p| !gone.contains(p));
        self.table.scroll_pending = true;
    }

    /// r: rescans just the folder being viewed; the result is spliced into
    /// the full tree when done (see finish_graft).
    pub(crate) fn rescan_current(&mut self) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let target = self.current_view_node(&root).path.clone();
        let graft = Graft {
            target: target.clone(),
            view_paths: self.view_paths(&root),
            cursor: self.table.cursor.clone(),
            free_space: self.free_space,
        };
        self.start_scan(target);
        self.graft = Some(graft);
    }

    /// A folder rescan finished: splices `node` into the full tree in place
    /// of the folder's old contents and returns to where the user was.
    /// False if no folder rescan was in progress.
    pub(crate) fn finish_graft(&mut self, node: Node) -> bool {
        let Some(g) = self.graft.take() else {
            return false;
        };
        self.root = None;
        let Some(mut full) = self.full_root.take() else {
            return false;
        };
        if full.path == g.target {
            full = Arc::new(node);
        } else {
            replace_in_tree(Arc::make_mut(&mut full), &g.target, node);
        }
        self.full_root = Some(full);
        self.rebuild_view_tree();
        self.restore_view(&g.view_paths);
        self.free_space = g
            .free_space
            .and(self.root.as_ref().and_then(|r| fs_space(&r.path)));
        self.table.cursor = g.cursor;
        self.table.scroll_pending = true;
        true
    }

    /// A folder rescan was aborted or failed: back to the tree as it was.
    pub(crate) fn cancel_graft(&mut self) {
        if let Some(g) = self.graft.take() {
            self.restore_view(&g.view_paths);
            self.free_space = g.free_space;
            self.table.cursor = g.cursor;
            self.table.scroll_pending = true;
        }
    }
}

#[cfg(test)]
mod perf {
    use super::*;

    /// Timing of the row order on a 300k-entry folder (run with
    /// `cargo test --release -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn row_order_300k() {
        let mut app = DiskScanApp::default();
        let children: Vec<Node> = (0..300_000u64)
            .map(|i| {
                let path = format!("/t/many/file_{:06}.dat", (i * 7919) % 300_000);
                test_node(
                    &path,
                    [0, 0, 4096, 8192, 20480, 69632][(i % 6) as usize],
                    false,
                    vec![],
                )
            })
            .collect();
        let folder = test_node("/t/many", 0, true, children);
        for (column, ascending) in [
            (SortColumn::Size, false),
            (SortColumn::Size, true),
            (SortColumn::Name, true),
            (SortColumn::Modified, false),
            (SortColumn::Perms, true),
        ] {
            let key = OrderKey {
                tree_gen: 0,
                view: vec![],
                sort: SortState { column, ascending },
                dirs_first: false,
                show_dotfiles: true,
                hidden: 0,
                live: false,
            };
            let t = Instant::now();
            let order = app.row_order(&folder, key);
            eprintln!(
                "{column:?} asc={ascending}: {:?} ({} rows)",
                t.elapsed(),
                order.idx.len()
            );
        }
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    /// A table showing a folder of `names` (sizes descending), sorted by
    /// size, largest first.
    fn app_with(names: &[&str]) -> DiskScanApp {
        let mut app = DiskScanApp::default();
        let children: Vec<Node> = names
            .iter()
            .enumerate()
            .map(|(i, n)| test_node(&format!("/t/{n}"), 1000 - i as u64, false, vec![]))
            .collect();
        let root = Arc::new(test_node("/t", 0, true, children));
        app.root = Some(root.clone());
        app.summary_view = true;
        let key = OrderKey {
            tree_gen: app.tree_gen,
            view: vec![],
            sort: SortState {
                column: SortColumn::Size,
                ascending: false,
            },
            dirs_first: false,
            show_dotfiles: true,
            hidden: 0,
            live: false,
        };
        let order = app.row_order(&root, key);
        app.table.order = Some(order);
        app.table.cursor = Some(PathBuf::from("/t/a"));
        app
    }

    fn cursor(app: &DiskScanApp) -> String {
        file_name_of(app.table.cursor.as_ref().unwrap())
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let mut app = app_with(&["a", "b", "c", "d"]);
        app.move_cursor(1);
        assert_eq!(cursor(&app), "b");
        app.move_cursor(isize::MAX / 2);
        assert_eq!(cursor(&app), "d");
        app.move_cursor(-10);
        assert_eq!(cursor(&app), "a");
        assert_eq!(app.cursor_row(), Some((PathBuf::from("/t/a"), false)));
    }

    #[test]
    fn marks_come_back_in_table_order_and_forget_moves_cursor() {
        let mut app = app_with(&["a", "b", "c", "d"]);
        app.table.marked.insert(PathBuf::from("/t/c"));
        app.table.marked.insert(PathBuf::from("/t/a"));
        assert_eq!(
            app.selected_targets(),
            vec![PathBuf::from("/t/a"), PathBuf::from("/t/c")]
        );
        app.table.cursor = Some(PathBuf::from("/t/b"));
        app.table_forget(&[PathBuf::from("/t/b"), PathBuf::from("/t/c")]);
        assert_eq!(cursor(&app), "d");
        assert!(!app.table.marked.contains(&PathBuf::from("/t/c")));
    }

    #[test]
    fn stale_order_is_not_used() {
        let mut app = app_with(&["a", "b"]);
        app.tree_gen += 1; // the tree changed since the order was computed
        assert_eq!(app.cursor_row(), None);
        app.move_cursor(1);
        assert_eq!(cursor(&app), "a");
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    fn folder(name: &str, size: u64) -> Node {
        test_node(&format!("/scan/{name}"), size, true, vec![])
    }

    /// Re-sorts the live table as table_ui does on a refresh.
    fn refresh(app: &mut DiskScanApp) {
        app.live_gen += 1;
        app.live_view = flat_copy(&app.partial_root);
        let partial = std::mem::replace(&mut app.live_view, empty_node());
        let key = OrderKey {
            tree_gen: app.live_gen,
            view: vec![],
            sort: app.contents_sort,
            dirs_first: false,
            show_dotfiles: true,
            hidden: 0,
            live: true,
        };
        let order = app.row_order(&partial, key);
        app.table.order = Some(order);
        app.live_view = partial;
    }

    fn names(app: &DiskScanApp) -> Vec<String> {
        let (view, idx) = app.listed().unwrap();
        idx.iter().map(|&k| view.children[k].name.clone()).collect()
    }

    #[test]
    fn live_rows_follow_the_scan_and_the_cursor_stays_put() {
        let mut app = DiskScanApp::default();
        app.scanning = true;
        app.contents_sort = SortState {
            column: SortColumn::Size,
            ascending: false,
        };
        app.partial_root = test_node("/scan", 0, true, vec![folder("a", 30), folder("b", 20)]);
        refresh(&mut app);
        assert_eq!(names(&app), ["a", "b"]);
        app.table.cursor = Some(PathBuf::from("/scan/b"));

        // More data arrives: b grows past a, c appears.
        app.partial_root.children[1].size = 50;
        app.partial_root.children.push(folder("c", 40));
        refresh(&mut app);
        assert_eq!(names(&app), ["b", "c", "a"]);
        assert_eq!(
            app.cursor_row().map(|(p, _)| p),
            Some(PathBuf::from("/scan/b"))
        );
        // The preview tree re-sorts itself as data arrives; until the next
        // refresh the table keeps showing its snapshot, row for row.
        app.partial_root.children.reverse();
        app.partial_root.children.push(folder("d", 99));
        assert_eq!(names(&app), ["b", "c", "a"]);
        app.move_cursor(1);
        assert_eq!(app.table.cursor, Some(PathBuf::from("/scan/c")));

        // Opening, delete and trash wait for the scan to finish.
        let views_before = app.view_stack.clone();
        app.open_dir(Path::new("/scan/c"));
        app.request_delete();
        assert_eq!(app.view_stack, views_before);
        assert!(!app.delete_dialog_open());

        // When the scan ends, rows from the live tree are no longer used.
        app.scanning = false;
        assert!(app.listed().is_none());
    }
}
