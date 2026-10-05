//! The Summary view's contents table: an ncdu-style, keyboard-driven listing
//! of the folder being viewed. `table_ui` draws it, `table_keys` handles its
//! keys (listed in the `?` help overlay, see `HELP_ROWS`), `table_overlays`
//! draws the details panel and dialogs, and
//! `table_frame_start` carries out deletes queued by the previous frame.

use super::*;
use config::TablePrefs;
use egui_extras::{Column, TableBuilder};
use panels::open_externally;

/// Width of the details panel ("i").
const INFO_PANEL_WIDTH: f32 = 340.0;

/// Columns that can be shown or hidden (Name is always shown).
/// What the Summary view's left panel shows.
#[derive(Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SidePanel {
    /// The category bar.
    #[default]
    Categories,
    /// The table of sizes by file extension.
    Extensions,
}

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
#[derive(Clone, PartialEq)]
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
    /// The flat list's row limit, or None for the folder's own contents.
    flat: Option<usize>,
}

/// Where the table's rows sit in its scroll area, to scroll to a row.
#[derive(Clone, Copy)]
struct RowGeometry {
    /// Distance from the top of one row to the next.
    pitch: f32,
    /// Top of the first row, from the top of the scrolled content.
    first_top: f32,
    /// Current scroll position and visible height.
    offset: f32,
    visible: f32,
}

impl RowGeometry {
    /// The geometry, if every value is a real position. (A row not laid out
    /// yet reports an endless rectangle, which must never become a scroll
    /// position: nothing could be drawn there again.)
    fn checked(self) -> Option<RowGeometry> {
        let RowGeometry {
            pitch,
            first_top,
            offset,
            visible,
        } = self;
        ([first_top, offset, visible].iter().all(|v| v.is_finite())
            && pitch.is_finite()
            && pitch > 0.0)
            .then_some(self)
    }

    /// The scroll position that brings row `i` into view, moving as little
    /// as possible; None if it's in view already.
    fn offset_showing(&self, i: usize) -> Option<f32> {
        let top = self.first_top + i as f32 * self.pitch;
        let bottom = top + self.pitch;
        if top < self.offset {
            Some(top)
        } else if bottom > self.offset + self.visible {
            Some(bottom - self.visible)
        } else {
            None
        }
    }
}

/// The table's rows in display order.
enum Rows {
    /// The viewed folder's own contents, as indices into its children.
    Children(Vec<usize>),
    /// Files from the viewed folder and all its subfolders.
    Files(FlatRows),
}

/// The flat list's files: each is a folder (an index path from the viewed
/// folder, stored once per folder) and the file's index in it.
struct FlatRows {
    folders: Vec<Vec<usize>>,
    files: Vec<(u32, u32)>,
}

impl FlatRows {
    fn node<'a>(&self, view: &'a Node, i: usize) -> &'a Node {
        let (folder, child) = self.files[i];
        &get_node(view, &self.folders[folder as usize]).children[child as usize]
    }
}

impl Rows {
    fn len(&self) -> usize {
        match self {
            Rows::Children(idx) => idx.len(),
            Rows::Files(flat) => flat.files.len(),
        }
    }

    /// Row `i`'s node, in the viewed folder `view`.
    fn node<'a>(&self, view: &'a Node, i: usize) -> &'a Node {
        match self {
            Rows::Children(idx) => &view.children[idx[i]],
            Rows::Files(flat) => flat.node(view, i),
        }
    }
}

/// The table's rows, plus totals derived from the same pass.
struct RowOrder {
    key: OrderKey,
    rows: Rows,
    /// Total size of everything listed (the flat list: of all its files,
    /// shown or past the limit).
    shown_size: u64,
    /// Files in everything listed, as `shown_size` counts.
    shown_files: u64,
    dotfile_size: u64,
    /// The flat list: how many files it has, shown or past the limit.
    flat_files: u64,
    /// The folder's own contents: how many of the listed rows are folders.
    folders: u64,
    /// Width of the longest name or path in the rows, once measured, and
    /// the rows it's measured on (see `longest_rows`).
    name_width: Option<f32>,
    longest: Vec<usize>,
    /// The marked rows' total size, and the marks it's for.
    marked_size: Option<(HashSet<PathBuf>, u64)>,
    /// Rows shown while the ones for a new key are worked out on another
    /// thread (see `order_for`).
    pending: bool,
}

impl RowOrder {
    /// What the table shows while the rows for `key` are worked out: the
    /// rows shown so far, if they're of the same tree and folder, else none.
    fn waiting(shown: Option<RowOrder>, key: OrderKey) -> RowOrder {
        match shown {
            Some(mut o)
                if o.key.tree_gen == key.tree_gen
                    && o.key.view == key.view
                    && o.key.live == key.live =>
            {
                o.pending = true;
                o
            }
            _ => RowOrder {
                rows: match key.flat {
                    Some(_) => Rows::Files(FlatRows {
                        folders: Vec::new(),
                        files: Vec::new(),
                    }),
                    None => Rows::Children(Vec::new()),
                },
                shown_size: 0,
                shown_files: 0,
                dotfile_size: 0,
                flat_files: 0,
                folders: 0,
                name_width: None,
                longest: Vec::new(),
                marked_size: None,
                pending: true,
                key,
            },
        }
    }
}

/// Rows being worked out on another thread (they carry their key).
struct Sorting {
    rows: Receiver<RowOrder>,
}

/// Flat lists of more files than this are sorted on another thread.
const SORT_APART: u64 = 200_000;

/// The Name column's text for `node`: its name, or in the flat list its
/// full path.
fn row_text(node: &Node, flat: bool) -> std::borrow::Cow<'_, str> {
    if flat {
        show_path(&node.path()).into()
    } else {
        node.name.as_ref().into()
    }
}

/// The rows with the longest names in `rows` (paths, in the flat list),
/// the only ones whose width is measured, so huge folders stay fast.
fn longest_rows(rows: &Rows, view: &Node, flat: bool) -> Vec<usize> {
    const MEASURED: usize = 64;
    // Length in bytes: quick to get, and close enough to pick them.
    let len = |n: &Node| {
        if flat { n.path_len() } else { n.name.len() }
    };
    let mut by_len: Vec<(usize, usize)> = (0..rows.len())
        .into_par_iter()
        .map(|i| (len(rows.node(view, i)), i))
        .collect();
    if by_len.len() > MEASURED {
        by_len.select_nth_unstable_by(MEASURED, |a, b| b.cmp(a));
        by_len.truncate(MEASURED);
    }
    by_len.into_iter().map(|(_, i)| i).collect()
}

/// Width of the longest name of rows `longest` (see `longest_rows`).
fn widest_name(ui: &egui::Ui, longest: &[usize], rows: &Rows, view: &Node, flat: bool) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    longest
        .iter()
        .map(|&i| {
            let text = row_text(rows.node(view, i), flat).into_owned();
            ui.painter()
                .layout_no_wrap(text, font.clone(), color)
                .size()
                .x
        })
        .fold(0.0, f32::max)
        + ui.spacing().item_spacing.x
}

/// Where `cursor` is in `rows`. `pos` is the last answer, checked first,
/// so large folders aren't searched every frame.
fn find_cursor(
    cursor: &Path,
    pos: &std::cell::Cell<Option<usize>>,
    view: &Node,
    rows: &Rows,
) -> Option<usize> {
    let n = rows.len();
    let at = |i: usize| i < n && rows.node(view, i).path_is(cursor);
    if let Some(i) = pos.get().filter(|&i| at(i)) {
        return Some(i);
    }
    // Where it is in the tree, then which row that is: numbers compared,
    // not paths, so a re-sorted list of millions finds it at once.
    let found = place_in(view, cursor).and_then(|(folder, child)| match rows {
        Rows::Children(idx) if folder.is_empty() => idx.iter().position(|&i| i == child),
        Rows::Children(_) => None,
        Rows::Files(flat) => {
            let f = flat.folders.iter().position(|p| *p == folder)?;
            flat.files
                .iter()
                .position(|&(fo, ch)| fo as usize == f && ch as usize == child)
        }
    });
    pos.set(found);
    found
}

/// Where `path` is under `view`: the child-index path of its folder from
/// `view`, and its index in that folder; None if it isn't there.
fn place_in(view: &Node, path: &Path) -> Option<(Vec<usize>, usize)> {
    let mut at = find_index_path(view, path)?;
    let child = at.pop()?;
    Some((at, child))
}

/// Bits of a row's value in a compact sort key: enough for mode, owner and
/// group together.
const VALUE_BITS: u32 = 76;

/// `c`'s value for sorting by `sort` (any column but Name), in `VALUE_BITS`
/// bits, flipped for a descending sort, so smaller always comes first.
/// Sorting these as plain numbers is much faster than comparing nodes.
fn sort_value(c: &Node, sort: SortState) -> u128 {
    let signed = |v: i64| (v as u64 ^ (1 << 63)) as u128;
    let v = match sort.column {
        SortColumn::Size => c.size as u128,
        SortColumn::Files => c.file_count as u128,
        SortColumn::Modified => signed(c.mtime),
        SortColumn::Changed => signed(c.ctime),
        SortColumn::Perms => {
            ((c.mode & 0o7777) as u128) << 64 | (c.uid as u128) << 32 | c.gid as u128
        }
        SortColumn::Name => 0,
    };
    if sort.ascending {
        v
    } else {
        ((1 << VALUE_BITS) - 1) - v
    }
}

/// Files of a flat list: the files under `view`, leaving out dot-named
/// entries (and everything in them) unless `show_dotfiles`, and anything in
/// `hidden`. Returns the first `limit` in `sort` order, how many files there
/// are, their total size, and the total size of dot-named entries.
fn flat_files(
    view: &Node,
    sort: SortState,
    show_dotfiles: bool,
    hidden: &HashSet<PathBuf>,
    limit: usize,
) -> (FlatRows, u64, u64, u64) {
    /// A file found: its node, its folder (an index into `folders`) and
    /// its index in that folder.
    struct Found<'a> {
        node: &'a Node,
        folder: u32,
        child: u32,
    }
    struct Walk<'a, 'h> {
        show_dotfiles: bool,
        hidden: &'h HashSet<PathBuf>,
        /// Index paths of the folders holding files, from `view`.
        folders: Vec<Vec<usize>>,
        /// The files found, folder by folder.
        files: Vec<Vec<Found<'a>>>,
        size: u64,
        dot_size: u64,
    }
    fn walk<'a>(w: &mut Walk<'a, '_>, dir: &'a Node, at: &mut Vec<usize>, in_dot: bool) {
        let mut folder = None;
        for (i, c) in dir.children.iter().enumerate() {
            let dot = c.name.starts_with('.');
            if dot && !in_dot {
                w.dot_size = w.dot_size.saturating_add(c.size);
            }
            if (dot && !w.show_dotfiles) || c.is_in(w.hidden) {
                continue;
            }
            if c.is_dir {
                at.push(i);
                deep(|| walk(w, c, at, in_dot || dot));
                at.pop();
            } else {
                let folder = *folder.get_or_insert_with(|| {
                    w.folders.push(at.clone());
                    w.files.push(Vec::new());
                    (w.folders.len() - 1) as u32
                });
                w.files[folder as usize].push(Found {
                    node: c,
                    folder,
                    child: i as u32,
                });
                w.size = w.size.saturating_add(c.size);
            }
        }
    }
    let mut w = Walk {
        show_dotfiles,
        hidden,
        folders: Vec::new(),
        files: Vec::new(),
        size: 0,
        dot_size: 0,
    };
    walk(&mut w, view, &mut Vec::new(), false);
    // The order equal ones keep: folder by folder (in the order each
    // folder's first file was met), each folder's files in its own order.
    let files: Vec<Found> = std::mem::take(&mut w.files).into_iter().flatten().collect();
    let count = files.len() as u64;

    let files: Vec<(u32, u32)> = if sort.column == SortColumn::Name {
        // Name keys are built once per file, then used to pick and sort
        // (much faster on long lists than building them at every comparison).
        let mut keyed: Vec<(Vec<u8>, &str, u32, u32)> = files
            .par_iter()
            .map(|f| (natural_key(&f.node.name), &*f.node.name, f.folder, f.child))
            .collect();
        let by_key = |a: &(Vec<u8>, &str, u32, u32), b: &(Vec<u8>, &str, u32, u32)| {
            let by = a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1));
            let by = if sort.ascending { by } else { by.reverse() };
            by.then((a.2, a.3).cmp(&(b.2, b.3)))
        };
        if keyed.len() > limit {
            keyed.select_nth_unstable_by(limit, by_key);
            keyed.truncate(limit);
        }
        keyed.par_sort_unstable_by(by_key);
        keyed.into_iter().map(|k| (k.2, k.3)).collect()
    } else {
        // One u128 per file: its value (see `sort_value`) | its place in
        // `files` (32 bits), so equal ones keep that order.
        let mut keyed: Vec<u128> = files
            .par_iter()
            .enumerate()
            .map(|(i, f)| sort_value(f.node, sort) << 32 | i as u128)
            .collect();
        if keyed.len() > limit {
            keyed.select_nth_unstable(limit);
            keyed.truncate(limit);
        }
        keyed.par_sort_unstable();
        keyed
            .iter()
            .map(|k| {
                let f = &files[(k & 0xFFFF_FFFF) as usize];
                (f.folder, f.child)
            })
            .collect()
    };
    let rows = FlatRows {
        folders: w.folders,
        files,
    };
    (rows, count, w.size, w.dot_size)
}

/// Filters and sorts `view_node`'s children, or for the flat list all
/// the files under it, for the table (see `OrderKey`), leaving out
/// `hidden` items.
fn order_rows(view_node: &Node, key: OrderKey, hidden: &HashSet<PathBuf>) -> RowOrder {
    if let Some(limit) = key.flat {
        let (rows, count, size, dot_size) =
            flat_files(view_node, key.sort, key.show_dotfiles, hidden, limit);
        let rows = Rows::Files(rows);
        return RowOrder {
            longest: longest_rows(&rows, view_node, true),
            rows,
            shown_size: size,
            shown_files: count,
            dotfile_size: dot_size,
            flat_files: count,
            folders: 0,
            name_width: None,
            marked_size: None,
            pending: false,
            key,
        };
    }
    let children = &view_node.children;
    let mut idx: Vec<usize> = (0..children.len())
        .filter(|&i| {
            let c = &children[i];
            (key.show_dotfiles || !c.name.starts_with('.')) && !c.is_in(hidden)
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
                    &*children[i].name,
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
        // One u128 per row: group (bit 108) | value (see `sort_value`) |
        // index (32 bits).
        let mut keyed: Vec<u128> = idx
            .iter()
            .map(|&i| {
                let c = &children[i];
                (group(c) as u128) << (VALUE_BITS + 32) | sort_value(c, cs) << 32 | i as u128
            })
            .collect();
        keyed.par_sort_unstable();
        idx = keyed
            .into_iter()
            .map(|k| (k & 0xFFFF_FFFF) as usize)
            .collect();
    }
    let shown_size = idx
        .iter()
        .map(|&i| children[i].size)
        .fold(0u64, u64::saturating_add);
    let shown_files = idx.iter().map(|&i| children[i].file_count).sum();
    let folders = idx.iter().filter(|&&i| children[i].is_dir).count() as u64;
    let rows = Rows::Children(idx);
    RowOrder {
        longest: longest_rows(&rows, view_node, false),
        rows,
        shown_size,
        shown_files,
        dotfile_size: children
            .iter()
            .filter(|c| c.name.starts_with('.'))
            .map(|c| c.size)
            .fold(0u64, u64::saturating_add),
        folders,
        flat_files: 0,
        name_width: None,
        marked_size: None,
        pending: false,
        key,
    }
}

/// A rescan of one folder ("r"), to be spliced back into the full tree
/// when it finishes rather than replacing it.
pub(crate) struct Graft {
    pub(crate) target: PathBuf,
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
    /// The contents table's egui id, as last drawn.
    table_id: std::cell::Cell<Option<egui::Id>>,
    /// Where the rows were in the scroll area, as last drawn.
    row_geometry: Option<RowGeometry>,
    /// Row order as last computed (see `OrderKey`).
    order: Option<RowOrder>,
    /// Rows being sorted on another thread (see `order_for`).
    sorting: Option<Sorting>,
    /// Scroll the table to the cursor row on the next draw.
    pub scroll_pending: bool,
    /// Rows that fit on screen, for PageUp/PageDown.
    page_rows: usize,
    pub dirs_first: bool,
    /// What the left panel shows.
    pub side: SidePanel,
    /// Files from all subfolders in one list, instead of the folder's own
    /// contents.
    pub flat: bool,
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
    pub(crate) show_help: bool,
}

impl TableState {
    /// The rows shown are waiting for a sort on another thread.
    #[cfg(test)]
    pub(crate) fn sorting(&self) -> bool {
        self.order.as_ref().is_some_and(|o| o.pending)
    }
}

impl Default for TableState {
    fn default() -> Self {
        TableState {
            cursor: None,
            order: None,
            sorting: None,
            cursor_pos: Default::default(),
            table_id: Default::default(),
            row_geometry: None,
            scroll_pending: false,
            page_rows: 10,
            dirs_first: false,
            side: SidePanel::Categories,
            flat: false,
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
    let node_path = node.path();
    let parts = rel_parts(&node_path, target)?;
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
            ("HELP_KEYS_FLAT", "HELP_FLAT"),
        ],
    ),
    (
        "HELP_SECTION_ACT",
        &[
            ("HELP_KEYS_CATEGORY", "HELP_CATEGORY"),
            ("HELP_KEYS_MARK", "HELP_MARK"),
            ("HELP_KEYS_DELETE", "HELP_DELETE"),
            ("HELP_KEYS_TRASH", "HELP_TRASH"),
            ("HELP_KEYS_COPY", "HELP_COPY"),
            ("HELP_KEYS_CUT", "HELP_CUT"),
            ("HELP_KEYS_PASTE", "HELP_PASTE"),
            ("HELP_KEYS_RESCAN", "HELP_RESCAN"),
            ("HELP_KEYS_INFO", "HELP_INFO"),
            ("HELP_KEYS_ESC", "HELP_ESC"),
            ("HELP_KEYS_HELP", "HELP_HELP"),
            ("HELP_KEYS_MENU", "MENU_MAIN"),
            ("HELP_KEYS_QUIT", "MENU_QUIT"),
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
            side: self.table.side,
            flat: self.table.flat,
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
        self.table.side = p.side;
        self.table.flat = p.flat;
    }

    /// Start of every frame: keeps keyboard focus off the table's buttons,
    /// so a clicked header doesn't also react to Enter or Space.
    pub(crate) fn table_frame_start(&mut self, ctx: &egui::Context) {
        if self.summary_view
            && !self.typing
            && !self.delete_dialog_open()
            && !self.table.show_help
            && !self.show_about
            && !self.quit_asked
            && !egui::Popup::is_any_open(ctx)
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
        if !self
            .table
            .marks_for
            .as_deref()
            .is_some_and(|p| view_node.path_is(p))
        {
            self.table.marked.clear();
            self.table.marks_for = Some(view_node.path());
        }

        // The live table during a scan always lists the folder's contents.
        let flat_on = self.table.flat && !self.scanning;
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
            sort: self.table_sort(),
            dirs_first: self.table.dirs_first,
            show_dotfiles: self.table.show_dotfiles,
            hidden: self.hidden.len(),
            flat: flat_on.then_some(if self.settings.flat_all {
                usize::MAX
            } else {
                self.settings.flat_rows
            }),
        };
        let order = match self.table.order.take() {
            Some(o) if o.key == key && !o.pending => o,
            shown => self.order_for(view_node, key, shown, ui.ctx()),
        };
        let mut order = order;
        let name_width = *order.name_width.get_or_insert_with(|| {
            let flat = order.key.flat.is_some();
            widest_name(ui, &order.longest, &order.rows, view_node, flat)
        });
        // The marked rows' total size, worked out again only when the marks
        // or the rows change.
        let marked_size = match &order.marked_size {
            _ if self.table.marked.is_empty() => 0,
            Some((marks, size)) if *marks == self.table.marked => *size,
            _ => {
                let size = (0..order.rows.len())
                    .map(|i| order.rows.node(view_node, i))
                    .filter(|c| c.is_in(&self.table.marked))
                    .map(|c| c.size)
                    .fold(0u64, u64::saturating_add);
                order.marked_size = Some((self.table.marked.clone(), size));
                size
            }
        };
        let row = |i: usize| order.rows.node(view_node, i);
        let n_rows = order.rows.len();
        let flat = order.key.flat.is_some();
        let shown_weight = if measure_files() {
            order.shown_files
        } else {
            order.shown_size
        };
        // The cursor goes to the first row when it isn't in this folder.
        let found = self
            .table
            .cursor
            .as_ref()
            .and_then(|c| find_cursor(c, &self.table.cursor_pos, view_node, &order.rows));
        let cursor_row = match found {
            Some(i) => Some(i),
            // The rows are still being worked out: the cursor waits for them.
            None if order.pending => None,
            None => {
                self.table.cursor = (n_rows > 0).then(|| row(0).path());
                let first = self.table.cursor.is_some().then_some(0);
                self.table.cursor_pos.set(first);
                first
            }
        };
        // The cursor row is scrolled into view using where the rows were last
        // drawn; until they have been, the request waits.
        let scroll_to_cursor = std::mem::take(&mut self.table.scroll_pending);
        let geometry = self.table.row_geometry;
        let scroll_to = match (scroll_to_cursor, cursor_row, geometry) {
            (true, Some(r), Some(g)) => g.offset_showing(r),
            (true, Some(_), None) => {
                self.table.scroll_pending = true;
                None
            }
            _ => None,
        };
        let mut first_drawn: Option<(usize, f32)> = None;

        // Heading line: title, what's switched on, and where the keys are.
        let mut set_flat = None;
        ui.horizontal(|ui| {
            // Toggle: the folder's own contents, or files from all subfolders.
            let views: [(bool, &str, DrawIcon); 2] = [
                (false, "TABLE_VIEW_TREE", draw_tree_icon),
                (true, "TABLE_VIEW_FLAT", draw_flat_list_icon),
            ];
            for (on, key, icon) in views {
                if icon_toolbar_button(ui, self.table.flat == on, true, &tr(key), icon).clicked() {
                    set_flat = Some(on);
                }
            }
            ui.heading(tr("SUMMARY_CONTENTS"));
            if flat {
                let shown = format_count(n_rows as u64);
                let tag = if order.flat_files > n_rows as u64 {
                    trf(
                        "TABLE_TAG_FLAT_SOME",
                        &[&shown, &format_count(order.flat_files)],
                    )
                } else {
                    trn("TABLE_TAG_FLAT", n_rows as u64, &[&shown])
                };
                ui.label(format!("· {tag}"));
            } else if self.scanning {
                // Files show once their folder is read; the count runs ahead.
                let files = view_node.file_count;
                let so_far = trn("COUNT_FILES", files, &[&format_count(files)]);
                ui.label(format!("· {}", trf("TABLE_TAG_SO_FAR", &[&so_far])));
                if self.table.flat {
                    ui.weak(format!("· {}", tr("TABLE_TAG_FLAT_ON_FINISH")));
                }
            } else {
                // The folder's own contents, as listed.
                let files = (n_rows as u64).saturating_sub(order.folders);
                ui.label(format!(
                    "· {}",
                    trf(
                        "TABLE_TAG_FOLDER_COUNTS",
                        &[
                            &trn(
                                "COUNT_FOLDERS",
                                order.folders,
                                &[&format_count(order.folders)]
                            ),
                            &trn("COUNT_FILES", files, &[&format_count(files)]),
                        ]
                    )
                ));
                // (While the flat list is sorted, "sorting…" says so.)
                if self.table.flat && !order.pending {
                    ui.weak(format!("· {}", tr("TABLE_TAG_FLAT_ON_FINISH")));
                }
            }
            // During a scan a picked extension waits for the end, and says so.
            if let Some(pick) = &self.pick {
                let key = if self.scanning && matches!(pick, Pick::Extensions(_)) {
                    "TABLE_TAG_CATEGORY_AFTER_SCAN"
                } else {
                    "TABLE_TAG_CATEGORY"
                };
                let tag = trf(key, &[&self.cats.pick_label(pick)]);
                ui.label(egui::RichText::new(format!("· {tag}")).color(ui.visuals().warn_fg_color));
            }
            if self.table.dirs_first && !flat {
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
                ui.strong(format!(
                    "· {}",
                    trf(
                        "TABLE_TAG_MARKED",
                        &[
                            &format_count(self.table.marked.len() as u64),
                            &human_size(marked_size)
                        ]
                    )
                ));
            }
            if let Some(clip) = &self.transfer.clip {
                let key = match clip.mode {
                    ClipMode::Copy => "TABLE_TAG_CLIP_COPY",
                    ClipMode::Move => "TABLE_TAG_CLIP_MOVE",
                };
                ui.weak(format!(
                    "· {}",
                    trf(key, &[&format_count(clip.paths.len() as u64)])
                ));
            }
            if self.scanning {
                ui.weak(format!("· {}", tr("TABLE_TAG_SCANNING")));
            }
            if order.pending {
                ui.weak(format!("· {}", tr("TABLE_TAG_SORTING")));
            }
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
                    self.table.cursor = Some(row(i).path());
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
                // The flat list has only files: no Files column.
                .filter(|c| !(flat && **c == TableCol::Files))
                .map(|c| Cell::Opt(*c)),
        );
        cells.push(Cell::Name);

        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let header_h = ui.spacing().interact_size.y + 4.0;
        let table_h = (max_height - header_h).max(row_h * 3.0);
        self.table.page_rows = ((table_h / row_h) as usize).saturating_sub(1).max(1);

        // Percentages and bars are shares of the listed rows, so they add up
        // to 100% even with dotfiles hidden.
        let total = shown_weight.max(1);
        let bar_fill = ui.visuals().selection.bg_fill;
        let bar_frame = ui.visuals().weak_text_color();
        let dir_color = ui.visuals().hyperlink_color;
        let mark_color = ui.visuals().warn_fg_color;
        // The cursor row is drawn in the selection's text color...
        let selected_fg = ui.visuals().selection.stroke.color;
        // ...and its size bar gets a track in the window background color.
        let panel_bg = ui.visuals().panel_fill;
        let marked = &self.table.marked;
        let table_id_out = &self.table.table_id;
        // Filled in below, where `self` can't be reached.
        let drawn_geometry = std::cell::Cell::new(None);

        let mut clicked: Option<usize> = None;
        let mut double_clicked: Option<usize> = None;
        let mut ctrl_clicked: Option<usize> = None;
        let show_info = self.table.show_info;
        // During a scan, an unfinished folder has only a name and running totals
        // (no mode yet).
        let live = self.scanning;
        let pending = |c: &Node| live && c.is_dir && c.mode == 0;
        let sort_before = self.table_sort();
        let mut shown_sort = sort_before;
        let contents_sort = &mut shown_sort;
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
            // Steady scroll bars, beside the rows rather than over them.
            ui.spacing_mut().scroll = steady_scroll_style();
            // Column resize handles show only while hovered or dragged.
            ui.visuals_mut().widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
            if show_info {
                ui.set_max_width(ui.available_width() - INFO_PANEL_WIDTH - 8.0);
            }
            // One scroll area for both directions, so both bars stay at the
            // window edges (the header row scrolls with the rows). Each bar
            // shows only when the rows don't fit that way.
            let mut scroll_area = egui::ScrollArea::both()
                .id_salt("contents_scroll")
                .max_height(max_height)
                .auto_shrink([false, true])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded);
            if let Some(y) = scroll_to.filter(|y| y.is_finite()) {
                scroll_area = scroll_area.vertical_scroll_offset(y.max(0.0));
            }
            let scrolled = scroll_area.show(ui, |ui| {
                let table_salt = ("contents_table", layout_key.join(","));
                // A double-click on the divider after a column fits it to its
                // widest visible cell.
                // The id the table gives its state (it turns its salt into an IdSalt).
                let table_id = ui.id().with(egui::IdSalt::new(&table_salt));
                table_id_out.set(Some(table_id));
                let fit: Vec<bool> = (0..cells.len())
                    .map(|i| {
                        ui.ctx()
                            .read_response(table_id.with("resize_column").with(i))
                            .is_some_and(|r| r.double_clicked())
                    })
                    .collect();
                let mut tb = TableBuilder::new(ui)
                    .id_salt(table_salt.clone())
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    // The scroll area around the table scrolls both ways.
                    .vscroll(false)
                    .auto_shrink([false, true]);
                for (i, cell) in cells.iter().enumerate() {
                    let column = match cell {
                        Cell::Mark => Column::exact(12.0),
                        Cell::Opt(TableCol::Bar) => Column::exact(92.0),
                        Cell::Opt(_) => Column::auto().at_least(40.0),
                        // Wide enough for the longest name in full; the table
                        // scrolls sideways when that's wider than the window.
                        // Clipped, so it narrows again to that width.
                        Cell::Name => Column::remainder()
                            .at_least(name_width.max(120.0))
                            .clip(true),
                    };
                    tb = tb.column(column.auto_size_this_frame(fit[i]));
                }
                tb.header(header_h, |mut header| {
                    for cell in &cells {
                        header.col(|ui| match cell {
                            Cell::Mark | Cell::Opt(TableCol::Bar) => {}
                            Cell::Opt(TableCol::Percent) => {
                                ui.strong(tr("COL_PERCENT"));
                            }
                            Cell::Opt(TableCol::Size) => {
                                sortable_header(
                                    ui,
                                    &tr("COL_SIZE"),
                                    SortColumn::Size,
                                    contents_sort,
                                );
                            }
                            Cell::Opt(TableCol::Files) => {
                                sortable_header(
                                    ui,
                                    &tr("COL_FILES"),
                                    SortColumn::Files,
                                    contents_sort,
                                );
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
                                sortable_header(
                                    ui,
                                    &tr("COL_PERMS"),
                                    SortColumn::Perms,
                                    contents_sort,
                                );
                            }
                            Cell::Name => {
                                sortable_header(
                                    ui,
                                    &tr("COL_NAME"),
                                    SortColumn::Name,
                                    contents_sort,
                                );
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, n_rows, |mut tr_row| {
                        let i = tr_row.index();
                        let c = row(i);
                        let is_marked = c.is_in(marked);
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
                                                Pos2::new(
                                                    r.left() + r.width() * 0.4,
                                                    r.bottom() - 1.0,
                                                ),
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
                                    let frac = weight(c) as f32 / total as f32;
                                    if selected {
                                        // A dark track under the fill.
                                        ui.painter().rect_filled(
                                            r,
                                            egui::CornerRadius::ZERO,
                                            panel_bg,
                                        );
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
                                    ui.label(format!(
                                        "{:.1}%",
                                        weight(c) as f64 * 100.0 / total as f64
                                    ));
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
                                Cell::Opt(
                                    TableCol::Modified | TableCol::Changed | TableCol::Perms,
                                ) if pending(c) => {
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
                                    #[cfg(test)]
                                    tests_probe::NAME_WIDTH.set(ui.available_width());
                                    // The flat list shows each file's full path.
                                    let mut text = egui::RichText::new(row_text(c, flat));
                                    // Folders stand out by color alone.
                                    if c.is_dir || selected {
                                        text = text.color(pick(dir_color));
                                    }
                                    if is_marked {
                                        text = text.strong();
                                    }
                                    ui.add(egui::Label::new(text).extend());
                                }
                            });
                        }
                        let r = tr_row.response();
                        if first_drawn.is_none() && r.rect.top().is_finite() {
                            first_drawn = Some((i, r.rect.top()));
                        }
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
            if let Some((i, top)) = first_drawn {
                let pitch = row_h + ui.spacing().item_spacing.y;
                let offset = scrolled.state.offset.y;
                let content_top = scrolled.inner_rect.top() - offset;
                drawn_geometry.set(
                    RowGeometry {
                        pitch,
                        first_top: top - content_top - i as f32 * pitch,
                        offset,
                        visible: scrolled.inner_rect.height(),
                    }
                    .checked(),
                );
            }
        });
        self.table.row_geometry = drawn_geometry.get().or(self.table.row_geometry);

        // A clicked header sorts by its column, which becomes the one `<`/`>`
        // move.
        if shown_sort != sort_before {
            self.contents_sort = shown_sort;
            self.table.active_col = shown_sort.column.table_col();
        }
        if let Some(i) = clicked.or(double_clicked).or(ctrl_clicked) {
            self.table.cursor = Some(row(i).path());
        }
        if let Some(i) = ctrl_clicked {
            self.toggle_mark_of(row(i).path());
        }
        self.table.order = Some(order);
        if double_clicked.is_some() {
            self.open_cursor();
        }
        if let Some(on) = set_flat {
            self.set_flat(on);
        }
    }

    /// The table's sort order. The flat list has no Files column: sorted by
    /// files, it sorts by size the same way, and folder view sorts by files
    /// again.
    fn table_sort(&self) -> SortState {
        if self.table.flat && !self.scanning && self.contents_sort.column == SortColumn::Files {
            SortState {
                column: SortColumn::Size,
                ..self.contents_sort
            }
        } else {
            self.contents_sort
        }
    }

    /// Switches between the folder's own contents and the flat list of all
    /// its files. Marks go, so nothing marked out of sight gets deleted.
    fn set_flat(&mut self, on: bool) {
        if self.table.flat != on {
            self.table.flat = on;
            self.table.marked.clear();
            self.table.scroll_pending = true;
        }
    }

    /// The rows for `key` (see `order_rows`), worked out at once.
    fn row_order(&mut self, view_node: &Node, key: OrderKey) -> RowOrder {
        order_rows(view_node, key, &self.hidden)
    }

    /// The rows for `key`, in place of `shown`. Worked out at once, except
    /// a flat list of many files (`SORT_APART`): that's sorted on another
    /// thread, one list at a time, while `shown` stays (see
    /// `RowOrder::waiting`), so the window never waits for it.
    fn order_for(
        &mut self,
        view_node: &Node,
        key: OrderKey,
        shown: Option<RowOrder>,
        ctx: &egui::Context,
    ) -> RowOrder {
        if let Some(job) = &self.table.sorting {
            match job.rows.try_recv() {
                Ok(order) => {
                    self.table.sorting = None;
                    if order.key == key {
                        return order;
                    }
                }
                // A newer key waits for it: one sort at a time.
                Err(TryRecvError::Empty) => return RowOrder::waiting(shown, key),
                Err(TryRecvError::Disconnected) => self.table.sorting = None,
            }
        }
        let big = key.flat.is_some() && !key.live && view_node.file_count > SORT_APART;
        let Some(root) = self.root.clone().filter(|_| big) else {
            return self.row_order(view_node, key);
        };
        let (tx, rows) = channel();
        let (job_key, hidden, ctx) = (key.clone(), self.hidden.clone(), ctx.clone());
        std::thread::spawn(move || {
            let order = order_rows(get_node(&root, &job_key.view), job_key, &hidden);
            // The tree is let go before the rows are handed over (see
            // `finish_sorting`).
            drop(root);
            let _ = tx.send(order);
            ctx.request_repaint();
        });
        self.table.sorting = Some(Sorting { rows });
        RowOrder::waiting(shown, key)
    }

    /// Waits for a list being sorted on another thread, which holds the
    /// tree: before the tree is changed in place, so that copies nothing.
    pub(crate) fn finish_sorting(&mut self) {
        if let Some(job) = self.table.sorting.take() {
            let _ = job.rows.recv();
        }
    }

    /// Details panel (i), drawn over the main area `area`.
    pub(crate) fn table_overlays(&mut self, ctx: &egui::Context, area: egui::Rect) {
        if self.table.show_info && !self.scanning {
            self.info_panel(ctx, area);
        }
    }

    /// Details of the row under the cursor, in the top-right corner.
    fn info_panel(&mut self, ctx: &egui::Context, area: egui::Rect) {
        let Some(n) = self.cursor_node() else {
            return;
        };
        let h = HoverInfo {
            path: n.path(),
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
            age_range: None,
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

    /// The keyboard shortcuts ("?" or the main menu).
    pub(crate) fn help_overlay(&mut self, ctx: &egui::Context) {
        let modal = egui::Modal::new("table_help".into()).show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.heading(tr("HELP_TITLE"));
            // Scrolls when the window is too short for the whole list.
            let room = (ctx.content_rect().height() - 140.0).max(120.0);
            egui::ScrollArea::vertical()
                .max_height(room)
                .show(ui, |ui| {
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
                });
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
        if self.typing || self.delete_dialog_open() || self.show_about || self.quit_asked {
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
                'a' => self.sort_by(SortColumn::Perms),
                'S' => self.toggle_col(TableCol::Size),
                'F' => self.toggle_col(TableCol::Files),
                'M' => self.toggle_col(TableCol::Modified),
                'C' => self.toggle_col(TableCol::Changed),
                'A' => self.toggle_col(TableCol::Perms),
                '%' => self.toggle_col(TableCol::Percent),
                '<' => self.move_col(false),
                '>' => self.move_col(true),
                'G' => self.toggle_col(TableCol::Bar),
                'l' => self.set_flat(!self.table.flat),
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
                '0'..='9' => self.pick_category_key(ch as usize - '0' as usize),
                '?' => self.table.show_help = true,
                '/' => {
                    self.table.jump = Some(String::new());
                    self.table.jump_focus_pending = true;
                }
                'D' if self.transfer_busy() => self.status = tr("STATUS_TRANSFER_BUSY"),
                'D' => self.request_delete(),
                'T' if self.scanning => {}
                'T' if self.transfer_busy() => self.status = tr("STATUS_TRANSFER_BUSY"),
                'T' => {
                    self.queue_trash(self.selected_targets());
                    ctx.request_repaint();
                }
                _ => {}
            }
        }
        // Ctrl+C / Ctrl+X take the marked rows (or the cursor row); Ctrl+V
        // pastes into the folder shown. Not while scanning.
        let (copy, cut, paste) = self.clipboard_events(ctx);
        if !self.scanning {
            if copy || cut {
                let mode = if cut { ClipMode::Move } else { ClipMode::Copy };
                self.clip(ctx, self.selected_targets(), mode);
            }
            if let (Some(text), Some(root)) = (paste, self.root.clone()) {
                let dest = self.current_view_node(&root).path();
                self.paste_into(dest, &text);
            }
        }
    }

    /// The folder shown and the table's rows; None if the tree changed since
    /// the rows were computed.
    fn listed(&self) -> Option<(&Node, &Rows)> {
        let order = self.table.order.as_ref()?;
        if self.scanning {
            // The live table: rows of its snapshot of the preview tree.
            let current = order.key.live && order.key.tree_gen == self.live_gen;
            return current.then_some((&self.live_view, &order.rows));
        }
        if order.key.live
            || order.key.tree_gen != self.tree_gen
            || Some(&order.key.view) != self.view_stack.last()
        {
            return None;
        }
        Some((get_node(self.root.as_ref()?, &order.key.view), &order.rows))
    }

    fn cursor_index(&self) -> Option<usize> {
        let (view, rows) = self.listed()?;
        find_cursor(
            self.table.cursor.as_ref()?,
            &self.table.cursor_pos,
            view,
            rows,
        )
    }

    /// The cursor row's node.
    fn cursor_node(&self) -> Option<&Node> {
        let i = self.cursor_index()?;
        let (view, rows) = self.listed()?;
        Some(rows.node(view, i))
    }

    /// Path of the cursor row, and whether it's a folder.
    fn cursor_row(&self) -> Option<(PathBuf, bool)> {
        self.cursor_node().map(|n| (n.path(), n.is_dir))
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
        let Some((view, rows)) = self.listed() else {
            return;
        };
        let n = rows.len();
        if n == 0 {
            return;
        }
        let i = match from {
            None => 0,
            Some(i) => (i as isize).saturating_add(delta).clamp(0, n as isize - 1) as usize,
        };
        self.table.cursor = Some(rows.node(view, i).path());
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
            let left = get_node(&root, &view).path();
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
        if let Err(e) = open_externally(&path) {
            self.log_issue(trf("ERR_OPEN_FAILED", &[&show_path(&path), &e.to_string()]));
        }
    }

    /// s/n/f/m/c/p: sort by `col`, or flip the order if already sorted by it.
    /// A hidden column is shown again, so the sort is visible.
    fn sort_by(&mut self, col: SortColumn) {
        self.contents_sort = self.table_sort();
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

    /// Space: marks or unmarks the cursor row; the cursor stays.
    fn toggle_mark(&mut self) {
        if let Some(c) = self.table.cursor.clone() {
            self.toggle_mark_of(c);
        }
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
                Some((view, rows)) => (0..rows.len())
                    .map(|i| rows.node(view, i).path())
                    .filter(|p| self.table.marked.contains(p))
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
        if let (Some(pos), Some((view, rows))) = (self.cursor_index(), self.listed()) {
            let path = |i: usize| rows.node(view, i).path();
            let next = (pos..rows.len())
                .find(|&i| !gone.contains(&path(i)))
                .or_else(|| (0..pos).rev().find(|&i| !gone.contains(&path(i))));
            self.table.cursor = next.map(path);
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
        let target = self.current_view_node(&root).path();
        self.rescan_folder(target);
    }

    /// Rescans folder `target` of the tree; the result is spliced in when
    /// done, keeping the view where it is.
    pub(crate) fn rescan_folder(&mut self, target: PathBuf) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let graft = Graft {
            target: target.clone(),
            view_paths: self.view_paths(&root),
            cursor: self.table.cursor.clone(),
            free_space: self.free_space,
        };
        self.rescanning_part = true;
        self.start_scan(target);
        self.graft = Some(graft);
    }

    /// A folder rescan finished: splices `node` into the full tree in place
    /// of the folder's old contents and returns to where the user was.
    /// False if no folder rescan was in progress.
    pub(crate) fn finish_graft(&mut self, mut node: Node) -> bool {
        let Some(g) = self.graft.take() else {
            return false;
        };
        // Scans sort by bytes; the tree may be sorted by files.
        if self.tree_by_files {
            sort_tree_by_measure(&mut node, true);
        }
        // Nothing else holds the tree, so it's changed in place, not copied.
        self.finish_sorting();
        for tree in [self.root.take(), self.cat_base.take()]
            .into_iter()
            .flatten()
        {
            release_tree(tree);
        }
        let Some(mut full) = self.full_root.take() else {
            return false;
        };
        if full.path_is(&g.target) {
            release_tree(std::mem::replace(&mut full, Arc::new(node)));
        } else {
            let tree = Arc::make_mut(&mut full);
            replace_in_tree(tree, &g.target, node);
            resort_above(tree, std::slice::from_ref(&g.target));
        }
        self.full_root = Some(full);
        self.rebuild_view_tree();
        self.restore_view(&g.view_paths);
        self.free_space = g
            .free_space
            .and(self.root.as_ref().and_then(|r| fs_space(&r.path())));
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

    /// Timing of the flat list on 1,000,000 files in 1,000 folders (run
    /// with `cargo test --release flat_1m -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn flat_1m() {
        let folders: Vec<Node> = (0..1000)
            .map(|d| {
                let files = (0..1000)
                    .map(|f| {
                        let mut n = test_node(
                            &format!("/r/d{d}/f{f}"),
                            (d * 7919 + f * 104_729) as u64 % 1_000_003,
                            false,
                            vec![],
                        );
                        n.mtime = (f * 31 + d) as i64;
                        n
                    })
                    .collect();
                test_node(&format!("/r/d{d}"), 0, true, files)
            })
            .collect();
        let root = test_node("/r", 0, true, folders);
        for limit in [1000, usize::MAX] {
            for column in [SortColumn::Size, SortColumn::Modified, SortColumn::Name] {
                let sort = SortState {
                    column,
                    ascending: false,
                };
                let t = Instant::now();
                let (rows, count, ..) = flat_files(&root, sort, true, &HashSet::new(), limit);
                eprintln!(
                    "{column:?}: {:?} ({} of {count})",
                    t.elapsed(),
                    rows.files.len()
                );
            }
        }
    }

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
                flat: None,
            };
            let t = Instant::now();
            let order = app.row_order(&folder, key);
            eprintln!(
                "{column:?} asc={ascending}: {:?} ({} rows)",
                t.elapsed(),
                order.rows.len()
            );
        }
    }
}

/// What the tests read back from a drawn table.
#[cfg(test)]
mod tests_probe {
    thread_local! {
        /// Width of the Name column, as last drawn.
        pub(super) static NAME_WIDTH: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    }
}

#[cfg(test)]
mod flat_tests {
    use super::*;

    fn file(path: &str, size: u64) -> Node {
        test_node(path, size, false, vec![])
    }

    /// /v with files at three levels, a dot folder and a hidden folder.
    fn tree() -> Node {
        let deep = test_node("/v/a/b", 70, true, vec![file("/v/a/b/big", 70)]);
        let a = test_node("/v/a", 100, true, vec![file("/v/a/mid", 30), deep]);
        let dot = test_node("/v/.cache", 500, true, vec![file("/v/.cache/huge", 500)]);
        let gone = test_node("/v/gone", 900, true, vec![file("/v/gone/x", 900)]);
        test_node("/v", 1505, true, vec![a, dot, gone, file("/v/small", 5)])
    }

    fn names(view: &Node, rows: &FlatRows) -> Vec<String> {
        (0..rows.files.len())
            .map(|i| rows.node(view, i).name.to_string())
            .collect()
    }

    const BY_SIZE: SortState = SortState {
        column: SortColumn::Size,
        ascending: false,
    };

    /// A row limit gives exactly the first rows of the full sorted list,
    /// for every column and both directions, with many equal names, sizes
    /// and times (ties settle by where the files were found), names that
    /// differ only in case or digits, and a limit of 1 and of all but one.
    #[test]
    fn limited_lists_are_the_start_of_the_full_list() {
        let names = [
            "a", "A", "a1", "a01", "a10", "a2", "b", "B", "é", "Z", "z10", "z9",
        ];
        let folders: Vec<Node> = (0..7)
            .map(|d| {
                let files = (0..300)
                    .map(|i| {
                        let mut f = file(
                            &format!("/w/d{d}/{}", names[i % names.len()]),
                            (i % 4) as u64,
                        );
                        f.mtime = (i % 3) as i64;
                        f.ctime = (i % 5) as i64;
                        f.mode = 0o600 + (i % 2) as u32;
                        f
                    })
                    .collect();
                test_node(&format!("/w/d{d}"), 0, true, files)
            })
            .collect();
        let v = test_node("/w", 0, true, folders);
        let none = HashSet::new();
        let columns = [
            SortColumn::Size,
            SortColumn::Files,
            SortColumn::Modified,
            SortColumn::Changed,
            SortColumn::Perms,
            SortColumn::Name,
        ];
        for column in columns {
            for ascending in [true, false] {
                let sort = SortState { column, ascending };
                let (all, total, ..) = flat_files(&v, sort, true, &none, usize::MAX);
                assert_eq!(total, 2100);
                for limit in [1, 7, 500, 2099] {
                    let (some, ..) = flat_files(&v, sort, true, &none, limit);
                    assert_eq!(
                        some.files,
                        all.files[..limit],
                        "{column:?} {ascending} {limit}"
                    );
                }
            }
        }
    }

    #[test]
    fn lists_files_from_all_subfolders_in_sort_order() {
        let v = tree();
        let hidden: HashSet<PathBuf> = [PathBuf::from("/v/gone")].into();
        let (paths, count, size, dot) = flat_files(&v, BY_SIZE, true, &hidden, 1000);
        assert_eq!(names(&v, &paths), ["huge", "big", "mid", "small"]);
        assert_eq!((count, size, dot), (4, 605, 500));
        // Dotfiles hidden: the dot folder's files go, its size is reported.
        let (paths, count, size, dot) = flat_files(&v, BY_SIZE, false, &hidden, 1000);
        assert_eq!(names(&v, &paths), ["big", "mid", "small"]);
        assert_eq!((count, size, dot), (3, 105, 500));
        let by_name = SortState {
            column: SortColumn::Name,
            ascending: true,
        };
        let (paths, ..) = flat_files(&v, by_name, false, &hidden, 1000);
        assert_eq!(names(&v, &paths), ["big", "mid", "small"]);
    }

    #[test]
    fn the_limit_keeps_the_first_rows_but_counts_them_all() {
        let v = tree();
        let (paths, count, size, _) = flat_files(&v, BY_SIZE, true, &HashSet::new(), 2);
        assert_eq!(names(&v, &paths), ["x", "huge"]);
        assert_eq!((count, size), (5, 1505));
        let smallest = SortState {
            column: SortColumn::Size,
            ascending: true,
        };
        let (paths, ..) = flat_files(&v, smallest, true, &HashSet::new(), 2);
        assert_eq!(names(&v, &paths), ["small", "mid"]);
    }

    /// Draws the contents table once, headless, and returns its row names.
    fn draw(app: &mut DiskScanApp) -> Vec<String> {
        let ctx = egui::Context::default();
        let _ = headless_frame(&ctx, egui::RawInput::default(), |ui| {
            let root = app.root.clone().unwrap();
            app.table_ui(ui, &root, 400.0);
        });
        let (view, rows) = app.listed().unwrap();
        (0..rows.len())
            .map(|i| rows.node(view, i).name.to_string())
            .collect()
    }

    /// A tree with more files than are sorted at once: `SORT_APART` + 1000
    /// files in 100 folders, of many sizes.
    fn big_tree() -> Node {
        let per = (SORT_APART as usize + 1000) / 100;
        let folders: Vec<Node> = (0..100)
            .map(|d| {
                let files: Vec<Node> = (0..per)
                    .map(|f| {
                        file(
                            &format!("/b/d{d}/f{f}"),
                            ((d * 7919 + f * 104_729) % 99_991) as u64,
                        )
                    })
                    .collect();
                let mut folder = test_node(&format!("/b/d{d}"), 0, true, files);
                folder.file_count = per as u64;
                folder
            })
            .collect();
        let mut top = test_node("/b", 0, true, folders);
        top.file_count = (100 * per) as u64;
        top
    }

    /// Draws frames until the table's rows are no longer being worked out
    /// on another thread.
    fn draw_until_sorted(app: &mut DiskScanApp) {
        let start = Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(60) {
            draw(app);
            if app.table.order.as_ref().is_some_and(|o| !o.pending) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the rows were never sorted");
    }

    /// A flat list of more files than `SORT_APART` is sorted on another
    /// thread: meanwhile the rows shown so far stay (marked as waiting) and
    /// the cursor with them; then the rows are exactly those of a sort
    /// done at once.
    #[test]
    fn big_flat_lists_sort_apart() {
        let mut app = DiskScanApp {
            summary_view: true,
            ..DiskScanApp::default()
        };
        app.full_root = Some(Arc::new(big_tree()));
        app.rebuild_view_tree();
        app.table.flat = true;
        // Every file listed, so the cursor's stays in the list.
        app.settings.flat_all = true;
        draw(&mut app);
        assert!(app.table.order.as_ref().unwrap().pending, "sorted apart");
        draw_until_sorted(&mut app);
        let rows = |app: &DiskScanApp| {
            let (view, rows) = app.listed().unwrap();
            (0..rows.len())
                .map(|i| rows.node(view, i).path())
                .collect::<Vec<_>>()
        };
        let by_size = rows(&app);
        assert_eq!(by_size.len(), SORT_APART as usize + 1000);
        app.table.cursor = Some(by_size[3].clone());

        app.contents_sort = SortState {
            column: SortColumn::Name,
            ascending: true,
        };
        draw(&mut app);
        let order = app.table.order.as_ref().unwrap();
        assert!(order.pending);
        assert_eq!(rows(&app), by_size, "the rows shown so far stay");
        assert_eq!(app.table.cursor, Some(by_size[3].clone()));
        draw_until_sorted(&mut app);
        let root = app.root.clone().unwrap();
        let key = app.table.order.as_ref().unwrap().key.clone();
        let at_once = order_rows(&root, key, &HashSet::new());
        let Rows::Files(want) = at_once.rows else {
            panic!("a flat list")
        };
        let want: Vec<PathBuf> = (0..want.files.len())
            .map(|i| want.node(&root, i).path())
            .collect();
        assert_eq!(rows(&app), want);
        assert_eq!(
            app.table.cursor,
            Some(by_size[3].clone()),
            "the cursor stays"
        );
    }

    /// Deleting while a list is sorted on another thread (which holds the
    /// tree) still changes the tree in place: it waits for the sort.
    #[test]
    fn deletes_wait_for_a_sort_apart() {
        let mut app = DiskScanApp {
            summary_view: true,
            ..DiskScanApp::default()
        };
        app.full_root = Some(Arc::new(big_tree()));
        app.rebuild_view_tree();
        app.table.flat = true;
        draw(&mut app);
        assert!(app.table.sorting.is_some());
        let before = Arc::as_ptr(app.full_root.as_ref().unwrap());
        app.drop_from_tree(&[PathBuf::from("/b/d0/f0")]);
        assert_eq!(
            Arc::as_ptr(app.full_root.as_ref().unwrap()),
            before,
            "not copied"
        );
        let d0 = &app.full_root.as_ref().unwrap().children[0];
        assert!(!d0.children.iter().any(|c| &*c.name == "f0"), "deleted");
    }

    /// The marked rows' total follows every change to the marks and to the
    /// tree.
    #[test]
    fn the_marked_total_follows_marks_and_tree() {
        let mut app = DiskScanApp {
            summary_view: true,
            ..DiskScanApp::default()
        };
        app.full_root = Some(Arc::new(tree()));
        app.rebuild_view_tree();
        let total = |app: &DiskScanApp| {
            let order = app.table.order.as_ref().unwrap();
            order.marked_size.as_ref().map(|m| m.1)
        };
        draw(&mut app);
        assert_eq!(total(&app), None);
        app.table.marked.insert(PathBuf::from("/v/small"));
        draw(&mut app);
        assert_eq!(total(&app), Some(5));
        app.table.marked.insert(PathBuf::from("/v/a"));
        draw(&mut app);
        assert_eq!(total(&app), Some(105));
        app.table.marked.remove(Path::new("/v/small"));
        draw(&mut app);
        assert_eq!(total(&app), Some(100));
        // The same folder, changed: worked out from the new rows.
        app.full_root = Some(Arc::new(test_node(
            "/v",
            42,
            true,
            vec![test_node("/v/a", 42, true, vec![file("/v/a/new", 42)])],
        )));
        app.rebuild_view_tree();
        draw(&mut app);
        assert_eq!(total(&app), Some(42));
    }

    #[test]
    fn the_flat_table_follows_the_pick_and_comes_back() {
        let mut app = DiskScanApp {
            summary_view: true,
            ..DiskScanApp::default()
        };
        let sub = test_node(
            "/t/d",
            60,
            true,
            vec![file("/t/d/a.eml", 50), file("/t/d/b.mkv", 10)],
        );
        app.full_root = Some(Arc::new(test_node(
            "/t",
            70,
            true,
            vec![sub, file("/t/c.txt", 10)],
        )));
        app.rebuild_view_tree();
        assert_eq!(draw(&mut app), ["d", "c.txt"]);
        let order = app.table.order.as_ref().unwrap();
        assert_eq!(
            (order.folders, order.rows.len()),
            (1, 2),
            "1 folder, 1 file"
        );
        app.table.flat = true;
        assert_eq!(draw(&mut app), ["a.eml", "b.mkv", "c.txt"]);
        app.pick = Some(Pick::extension("eml"));
        app.rebuild_view_tree();
        assert_eq!(draw(&mut app), ["a.eml"]);
        app.table.flat = false;
        assert_eq!(draw(&mut app), ["d"]);
    }

    /// The Name column is wide enough for the longest name, past the
    /// window if need be, fills a wider window, and narrows back with it.
    #[test]
    fn the_name_column_fits_the_longest_name() {
        let mut app = DiskScanApp {
            summary_view: true,
            ..DiskScanApp::default()
        };
        let long = format!("/v/{}", "a long file name ".repeat(12));
        app.full_root = Some(Arc::new(test_node("/v", 10, true, vec![file(&long, 10)])));
        app.rebuild_view_tree();
        let ctx = egui::Context::default();
        let mut name_width = |window: f32| {
            for _ in 0..3 {
                let raw = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        Pos2::ZERO,
                        Vec2::new(window, 600.0),
                    )),
                    ..Default::default()
                };
                let _ = headless_frame(&ctx, raw, |ui| {
                    let root = app.root.clone().unwrap();
                    app.table_ui(ui, &root, 400.0);
                });
            }
            tests_probe::NAME_WIDTH.get()
        };
        let full = {
            let ctx = egui::Context::default();
            let mut w = 0.0;
            let _ = headless_frame(&ctx, egui::RawInput::default(), |ui| {
                let font = egui::TextStyle::Body.resolve(ui.style());
                w = ui
                    .painter()
                    .layout_no_wrap(file_name_of(Path::new(&long)), font, Color32::WHITE)
                    .size()
                    .x;
            });
            w
        };
        let narrow = name_width(700.0);
        assert!(narrow >= full && narrow > 700.0, "{narrow} {full}");
        let wide = name_width(4000.0);
        assert!(wide > full + 1000.0, "{wide} {full}");
        assert!((name_width(700.0) - narrow).abs() < 1.0);
    }

    /// A double-click on a column divider reaches the table.
    #[test]
    fn double_clicking_a_divider_is_seen() {
        let mut app = DiskScanApp {
            summary_view: true,
            ..DiskScanApp::default()
        };
        app.full_root = Some(Arc::new(tree()));
        app.rebuild_view_tree();
        let ctx = egui::Context::default();
        let run = |app: &mut DiskScanApp, events: Vec<egui::Event>| {
            let raw = egui::RawInput {
                events,
                ..Default::default()
            };
            let _ = headless_frame(&ctx, raw, |ui| {
                let root = app.root.clone().unwrap();
                app.table_ui(ui, &root, 400.0);
            });
        };
        // The first frame only measures the columns.
        run(&mut app, vec![]);
        run(&mut app, vec![]);
        // Columns: mark, bar (fixed width), then %: its divider is the third.
        let divider = app
            .table
            .table_id
            .get()
            .unwrap()
            .with("resize_column")
            .with(2usize);
        let at = ctx
            .read_response(divider)
            .expect("divider drawn")
            .rect
            .center();
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        run(&mut app, vec![egui::Event::PointerMoved(at)]);
        run(&mut app, vec![press(true), press(false)]);
        run(&mut app, vec![press(true), press(false)]);
        assert!(ctx.read_response(divider).unwrap().double_clicked());
    }

    #[test]
    fn keys_move_through_files_in_subfolders() {
        let mut app = DiskScanApp::default();
        let root = Arc::new(tree());
        app.root = Some(root.clone());
        app.summary_view = true;
        let key = OrderKey {
            tree_gen: app.tree_gen,
            view: vec![],
            sort: BY_SIZE,
            dirs_first: false,
            show_dotfiles: false,
            hidden: 0,
            live: false,
            flat: Some(1000),
        };
        app.table.order = Some(app.row_order(&root, key));
        app.table.cursor = Some(PathBuf::from("/v/gone/x"));
        app.move_cursor(1);
        assert_eq!(app.cursor_row(), Some((PathBuf::from("/v/a/b/big"), false)));
        app.table.marked.insert(PathBuf::from("/v/small"));
        app.table.marked.insert(PathBuf::from("/v/a/mid"));
        assert_eq!(
            app.selected_targets(),
            [PathBuf::from("/v/a/mid"), PathBuf::from("/v/small")]
        );
        app.set_flat(true);
        app.set_flat(false);
        assert!(app.table.marked.is_empty(), "switching views drops marks");

        // Sorted by files, the flat list sorts by size the same way, and
        // folder view sorts by files again.
        app.contents_sort = SortState {
            column: SortColumn::Files,
            ascending: true,
        };
        app.set_flat(true);
        let by_size_up = SortState {
            column: SortColumn::Size,
            ascending: true,
        };
        assert!(app.table_sort() == by_size_up);
        app.set_flat(false);
        assert!(app.table_sort().column == SortColumn::Files);
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    /// A small scanned-looking tree: /t with folders a (2 files) and b
    /// (1 file).
    fn small_tree() -> Node {
        let mut a = test_node(
            "/t/a",
            0,
            true,
            vec![
                test_node("/t/a/x.mkv", 1000, false, vec![]),
                test_node("/t/a/y.txt", 10, false, vec![]),
            ],
        );
        a.size = 1010;
        a.file_count = 2;
        let mut b = test_node(
            "/t/b",
            0,
            true,
            vec![test_node("/t/b/z.pdf", 500, false, vec![])],
        );
        b.size = 500;
        b.file_count = 1;
        let mut t = test_node("/t", 0, true, vec![a, b]);
        t.size = 1510;
        t.file_count = 3;
        t
    }

    fn scan(p: &Path) -> Node {
        scan_as(p, false)
    }

    fn scan_as(p: &Path, apparent: bool) -> Node {
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let ctx = ScanCtx {
            mounts: &HashSet::new(),
            progress: &tx,
            cancel: &Default::default(),
            apparent_size: apparent,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: None,
        };
        scan_dir(p, &ctx)
    }

    /// Every node's path, size, file count and place, in tree order.
    fn shape(n: &Node, out: &mut Vec<(PathBuf, u64, u64)>) {
        out.push((n.path(), n.size, n.file_count));
        for c in &n.children {
            shape(c, out);
        }
    }

    /// Switching to files re-sorts every folder by file count (ties by
    /// name) and the table's size sort to files; a delete then keeps that
    /// order, as a fresh scan sorted by files would; switching back gives
    /// exactly the order a scan gives.
    #[test]
    fn switching_the_measure_resorts_everything() {
        let dir = std::env::temp_dir().join(format!("spacescan-measure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["heavy", "crowd/inner", "tie_b", "tie_a"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("heavy/one"), vec![1u8; 900_000]).unwrap();
        for i in 0..40 {
            std::fs::write(dir.join(format!("crowd/f{i}")), b"x").unwrap();
            std::fs::write(dir.join(format!("crowd/inner/g{i}")), b"x").unwrap();
        }
        for t in ["tie_a", "tie_b"] {
            for i in 0..3 {
                std::fs::write(dir.join(t).join(format!("{i}")), vec![2u8; 5000 * (i + 1)])
                    .unwrap();
            }
        }
        let mut app = DiskScanApp {
            full_root: Some(Arc::new(scan(&dir))),
            ..DiskScanApp::default()
        };
        app.rebuild_view_tree();
        let names = |n: &Node| {
            n.children
                .iter()
                .map(|c| c.name.to_string())
                .collect::<Vec<_>>()
        };
        let (mut before, mut fresh) = (Vec::new(), Vec::new());
        shape(app.full_root.as_ref().unwrap(), &mut before);
        app.settings.measure_files = true;
        app.follow_measure();
        let full = app.full_root.as_ref().unwrap();
        assert_eq!(names(full), ["crowd", "tie_a", "tie_b", "heavy"]);
        assert_eq!(app.contents_sort.column, SortColumn::Files);
        std::fs::remove_file(dir.join("crowd/inner/g0")).unwrap();
        app.drop_from_tree(&[dir.join("crowd/inner/g0")]);
        let mut want = scan(&dir);
        sort_tree_by_measure(&mut want, true);
        let mut got = Vec::new();
        shape(app.full_root.as_ref().unwrap(), &mut got);
        shape(&want, &mut fresh);
        assert_eq!(got, fresh, "after a delete, measuring files");
        app.settings.measure_files = false;
        app.follow_measure();
        let (mut back, mut scanned) = (Vec::new(), Vec::new());
        shape(app.full_root.as_ref().unwrap(), &mut back);
        shape(&scan(&dir), &mut scanned);
        assert_eq!(back, scanned, "back to bytes: the order a scan gives");
        assert_eq!(app.contents_sort.column, SortColumn::Size);
        assert!(!before.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A scan that ends while measuring files is shown sorted by files at
    /// once; switching the measure during a folder rescan sorts the tree
    /// shown meanwhile, and the rescanned folder joins in the same order,
    /// equal to a fresh scan sorted by files.
    #[test]
    fn measure_holds_across_scan_ends_and_rescans() {
        let dir = std::env::temp_dir().join(format!("spacescan-measure2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["heavy", "crowd", "sub/a", "sub/b"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("heavy/one"), vec![1u8; 900_000]).unwrap();
        std::fs::write(dir.join("sub/a/one"), vec![1u8; 500_000]).unwrap();
        for i in 0..30 {
            std::fs::write(dir.join(format!("crowd/f{i}")), b"x").unwrap();
            std::fs::write(dir.join(format!("sub/b/f{i}")), b"x").unwrap();
        }
        let sorted = |p: &Path| {
            let mut t = scan(p);
            sort_tree_by_measure(&mut t, true);
            let mut out = Vec::new();
            shape(&t, &mut out);
            out
        };
        let mut app = DiskScanApp::default();
        app.settings.measure_files = true;
        app.follow_measure();
        // The scan ends: its message arrives.
        let (tx, rx) = channel();
        app.scan_rx = Some(rx);
        app.scanning = true;
        tx.send(ScanMsg::Done(scan(&dir), 0.0, Vec::new())).unwrap();
        app.poll_scan();
        let mut got = Vec::new();
        shape(app.full_root.as_ref().unwrap(), &mut got);
        assert_eq!(
            got,
            sorted(&dir),
            "shown sorted by files when the scan ends"
        );
        // Back to bytes, then files again while "sub" is rescanned.
        app.settings.measure_files = false;
        app.follow_measure();
        app.graft = Some(Graft {
            target: dir.join("sub"),
            view_paths: vec![dir.clone()],
            cursor: None,
            free_space: None,
        });
        app.scanning = true;
        app.settings.measure_files = true;
        app.follow_measure();
        let mut during = Vec::new();
        shape(app.full_root.as_ref().unwrap(), &mut during);
        assert_eq!(during, sorted(&dir), "sorted by files during the rescan");
        std::fs::write(dir.join("sub/a/two"), b"y").unwrap();
        assert!(app.finish_graft(scan(&dir.join("sub"))));
        let mut after = Vec::new();
        shape(app.full_root.as_ref().unwrap(), &mut after);
        assert_eq!(after, sorted(&dir), "after the rescan");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder's own size is read from the disk again when entries leave
    /// it (filesystems can free a folder's own blocks), in both ways of
    /// counting sizes, with several deletes from the same folder at once:
    /// a stale own size (here made up) is replaced, and the folders above
    /// follow, so the tree equals a fresh scan.
    #[test]
    fn folders_take_their_own_size_again_after_deletes() {
        let dir = std::env::temp_dir().join(format!("spacescan-ownsize-{}", std::process::id()));
        for apparent in [false, true] {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("logs/deeper")).unwrap();
            for i in 0..60 {
                std::fs::write(dir.join(format!("logs/f{i}.log")), vec![1u8; 100]).unwrap();
            }
            std::fs::write(dir.join("logs/deeper/keep"), vec![1u8; 5000]).unwrap();
            let mut tree = scan_as(&dir, apparent);
            // As if the folder's own blocks were 4 KiB bigger when scanned.
            let logs = tree
                .children
                .iter_mut()
                .find(|c| c.disk_name() == "logs")
                .unwrap();
            logs.size += 4096;
            tree.size += 4096;
            let mut app = DiskScanApp {
                full_root: Some(Arc::new(tree)),
                tree_apparent: apparent,
                ..DiskScanApp::default()
            };
            app.rebuild_view_tree();
            let gone: Vec<PathBuf> = (0..60)
                .map(|i| dir.join(format!("logs/f{i}.log")))
                .collect();
            for g in &gone {
                std::fs::remove_file(g).unwrap();
            }
            app.drop_from_tree(&gone);
            let (mut got, mut want) = (Vec::new(), Vec::new());
            shape(app.full_root.as_ref().unwrap(), &mut got);
            shape(&scan_as(&dir, apparent), &mut want);
            assert_eq!(got, want, "apparent {apparent}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// After a delete (with and without a category picked) and after a
    /// folder is rescanned, the tree equals a fresh scan of the same data:
    /// folders that change places, and equal sizes in folders where the
    /// disk lists names in another order than by name.
    #[test]
    fn edits_in_place_equal_a_fresh_scan() {
        let dir = std::env::temp_dir().join(format!("spacescan-inplace-{}", std::process::id()));
        for picked in [false, true] {
            let _ = std::fs::remove_dir_all(&dir);
            for d in ["big", "small", "ties", "ties/sub", "pa", "pb"] {
                std::fs::create_dir_all(dir.join(d)).unwrap();
            }
            // "pa" is bigger until a delete leaves it the size of "pb".
            std::fs::write(dir.join("pa/same"), vec![3u8; 8192]).unwrap();
            std::fs::write(dir.join("pa/extra"), vec![3u8; 4096]).unwrap();
            std::fs::write(dir.join("pb/same"), vec![3u8; 8192]).unwrap();
            std::fs::write(dir.join("big/huge.rs"), vec![1u8; 600_000]).unwrap();
            std::fs::write(dir.join("big/keep.rs"), vec![1u8; 4096]).unwrap();
            std::fs::write(dir.join("small/a.rs"), vec![1u8; 200_000]).unwrap();
            // Created out of name order: the disk lists them otherwise.
            for name in ["zeta", "alpha", "mid", "beta", "omega"] {
                std::fs::write(dir.join("ties").join(name), vec![2u8; 8192]).unwrap();
            }
            std::fs::write(dir.join("ties/mid2"), vec![2u8; 16384]).unwrap();
            let mut app = DiskScanApp {
                full_root: Some(Arc::new(scan(&dir))),
                ..DiskScanApp::default()
            };
            if picked {
                app.pick = Some(Pick::Category(app.cats.of_name("x.rs")));
            }
            app.rebuild_view_tree();
            // Delete: "big" drops below "small", and "mid2" becomes a tie.
            std::fs::remove_file(dir.join("big/huge.rs")).unwrap();
            std::fs::remove_file(dir.join("pa/extra")).unwrap();
            std::fs::write(dir.join("ties/mid2"), vec![2u8; 8192]).unwrap();
            app.drop_from_tree(&[dir.join("big/huge.rs"), dir.join("pa/extra")]);
            app.graft = Some(Graft {
                target: dir.join("ties"),
                view_paths: vec![dir.clone()],
                cursor: None,
                free_space: None,
            });
            assert!(app.finish_graft(scan(&dir.join("ties"))));
            let (mut got, mut want) = (Vec::new(), Vec::new());
            shape(app.full_root.as_ref().unwrap(), &mut got);
            shape(&scan(&dir), &mut want);
            assert_eq!(got, want, "picked {picked}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Deleting and splicing in a rescanned folder change the tree in place
    /// (never a copy of all of it), with no filter and with a category
    /// picked, and the totals come out right.
    #[test]
    fn tree_changes_happen_in_place() {
        for picked in [false, true] {
            let mut app = DiskScanApp {
                full_root: Some(Arc::new(small_tree())),
                ..DiskScanApp::default()
            };
            if picked {
                app.pick = Some(Pick::Category(app.cats.of_name("x.mkv")));
            }
            app.rebuild_view_tree();
            let before = Arc::as_ptr(app.full_root.as_ref().unwrap());
            app.drop_from_tree(&[PathBuf::from("/t/a/y.txt")]);
            let full = app.full_root.as_ref().unwrap();
            assert_eq!(
                Arc::as_ptr(full),
                before,
                "picked {picked}: copied on delete"
            );
            assert_eq!((full.size, full.file_count), (1500, 2));
            app.graft = Some(Graft {
                target: PathBuf::from("/t/b"),
                view_paths: vec![PathBuf::from("/t")],
                cursor: None,
                free_space: None,
            });
            let mut b = test_node(
                "/t/b",
                0,
                true,
                vec![test_node("/t/b/w.pdf", 70, false, vec![])],
            );
            b.size = 70;
            b.file_count = 1;
            assert!(app.finish_graft(b));
            let full = app.full_root.as_ref().unwrap();
            assert_eq!(
                Arc::as_ptr(full),
                before,
                "picked {picked}: copied on rescan"
            );
            assert_eq!((full.size, full.file_count), (1070, 2));
        }
    }

    /// Rows not laid out yet report endless or undefined positions; none of
    /// them may become scroll geometry, while real ones do, even far down a
    /// huge list.
    #[test]
    fn row_geometry_takes_only_real_positions() {
        let g = |pitch, first_top, offset, visible| RowGeometry {
            pitch,
            first_top,
            offset,
            visible,
        };
        let bad = [f32::INFINITY, f32::NEG_INFINITY, f32::NAN];
        for b in bad {
            assert!(g(29.0, b, 0.0, 1000.0).checked().is_none());
            assert!(g(29.0, 33.0, b, 1000.0).checked().is_none());
            assert!(g(29.0, 33.0, 0.0, b).checked().is_none());
            assert!(g(b, 33.0, 0.0, 1000.0).checked().is_none());
        }
        assert!(g(0.0, 33.0, 0.0, 1000.0).checked().is_none());
        assert!(g(-1.0, 33.0, 0.0, 1000.0).checked().is_none());
        let real = g(29.0, 33.0, 0.0, 1000.0).checked().unwrap();
        for i in [0, 1, 34, 35, 299, 99_999] {
            if let Some(y) = real.offset_showing(i) {
                assert!(y.is_finite() && y >= 0.0, "row {i}: {y}");
            }
        }
        assert_eq!(real.offset_showing(0), None);
        assert!(real.offset_showing(99_999).unwrap() > 2_000_000.0);
    }

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
            flat: None,
        };
        let order = app.row_order(&root, key);
        app.table.order = Some(order);
        app.table.cursor = Some(PathBuf::from("/t/a"));
        app
    }

    fn cursor(app: &DiskScanApp) -> String {
        file_name_of(app.table.cursor.as_ref().unwrap())
    }

    /// The command line's list and flat reports come in the table's order
    /// for every sort, both ways, equal ones included: equal sizes by byte
    /// order of the name, equal dates by size, and a folder's files
    /// together in the flat list even with a subfolder between them.
    #[test]
    fn command_line_orders_like_the_table() {
        let file = |name: &str, size: u64, mtime: i64, ctime: i64| {
            let mut n = test_node(&format!("/t/{name}"), size, false, vec![]);
            (n.mtime, n.ctime) = (mtime, ctime);
            n
        };
        let in_sub = |path: &str| test_node(&format!("/t/sub/{path}"), 4096, false, vec![]);
        let inner = test_node("/t/sub/inner", 4096, true, vec![in_sub("inner/x")]);
        let mut sub_files = vec![in_sub("zz"), inner, in_sub("10")];
        sort_largest_first(&mut sub_files);
        let mut sub = test_node("/t/sub", 3 * 4096, true, sub_files);
        sub.file_count = 3;
        let mut children = vec![
            file("beta", 4096, 50, 7),
            file("10", 4096, 60, 7),
            file("Alpha", 4096, 50, 9),
            file("_u", 4096, 50, 7),
            file("9", 4096, 60, 9),
            file("big", 8192, 50, 7),
            sub,
        ];
        sort_largest_first(&mut children);
        let root = Arc::new(test_node("/t", 0, true, children));
        let mut app = DiskScanApp::default();
        for flat in [false, true] {
            for column in [
                SortColumn::Size,
                SortColumn::Files,
                SortColumn::Modified,
                SortColumn::Changed,
                SortColumn::Name,
            ] {
                // The flat report sorts names by path, not by file name.
                if flat && column == SortColumn::Name {
                    continue;
                }
                for ascending in [false, true] {
                    let key = OrderKey {
                        tree_gen: 0,
                        view: vec![],
                        sort: SortState { column, ascending },
                        dirs_first: false,
                        show_dotfiles: true,
                        hidden: 0,
                        live: false,
                        flat: flat.then_some(usize::MAX),
                    };
                    let order = app.row_order(&root, key);
                    let table: Vec<PathBuf> = (0..order.rows.len())
                        .map(|i| order.rows.node(&root, i).path())
                        .collect();
                    let reverse = ascending != (column == SortColumn::Name);
                    let cli: Vec<PathBuf> = crate::cli::rows_in_order(&root, flat, column, reverse)
                        .iter()
                        .map(|n| n.path())
                        .collect();
                    assert_eq!(cli, table, "flat {flat}, {column:?}, ascending {ascending}");
                }
            }
        }
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
            flat: None,
        };
        let order = app.row_order(&partial, key);
        app.table.order = Some(order);
        app.live_view = partial;
    }

    fn names(app: &DiskScanApp) -> Vec<String> {
        let (view, rows) = app.listed().unwrap();
        (0..rows.len())
            .map(|i| rows.node(view, i).name.to_string())
            .collect()
    }

    #[test]
    fn live_rows_follow_the_scan_and_the_cursor_stays_put() {
        let mut app = DiskScanApp {
            scanning: true,
            contents_sort: SortState {
                column: SortColumn::Size,
                ascending: false,
            },
            ..DiskScanApp::default()
        };
        app.partial_root = test_node("/scan", 0, true, vec![folder("a", 30), folder("b", 20)]);
        refresh(&mut app);
        assert_eq!(names(&app), ["a", "b"]);
        app.table.cursor = Some(PathBuf::from("/scan/b"));

        // More data arrives: b grows past a, c appears.
        app.partial_root.children[1].size = 50;
        app.partial_root.children = [app.partial_root.children.to_vec(), vec![folder("c", 40)]]
            .concat()
            .into();
        refresh(&mut app);
        assert_eq!(names(&app), ["b", "c", "a"]);
        assert_eq!(
            app.cursor_row().map(|(p, _)| p),
            Some(PathBuf::from("/scan/b"))
        );
        // The preview tree re-sorts itself as data arrives; until the next
        // refresh the table keeps showing its snapshot, row for row.
        app.partial_root.children.reverse();
        app.partial_root.children = [app.partial_root.children.to_vec(), vec![folder("d", 99)]]
            .concat()
            .into();
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
