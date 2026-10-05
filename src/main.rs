//! spacescan: a disk usage explorer for Linux, with a sunburst chart and an
//! ncdu-style table.

use eframe::egui;
use egui::{Color32, Pos2, Vec2};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Instant;

mod category;
mod chart;
mod cli;
mod config;
mod delete;
mod filter;
mod lang;
mod look;
mod panels;
mod scan;
mod table;
mod theme;
mod transfer;
mod widgets;
mod x11clip;
use category::*;
use chart::*;
use config::Config;
use filter::*;
use lang::*;
use look::*;
use scan::*;
use table::{Graft, SidePanel, TableState};
use theme::*;
use transfer::ClipMode;
use widgets::*;

#[derive(Clone)]
#[allow(dead_code)]
struct HoverInfo {
    path: PathBuf,
    size: u64,
    file_count: u64,
    is_dir: bool,
    is_free: bool,
    is_other: bool,
    mode: Option<u32>,
    mtime: Option<i64>,
    ctime: Option<i64>,
    uid: Option<u32>,
    gid: Option<u32>,
    /// Changed times of the newest and oldest file inside (folders and
    /// "other" slices), as used for the slice's colors.
    age_range: Option<(i64, i64)>,
}

#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum SortColumn {
    Size,
    Files,
    Modified,
    /// ctime: last change to the file's contents *or* metadata
    /// (permissions, owner, renames...).
    Changed,
    /// Permission bits, then owner.
    Perms,
    Name,
}

impl SortColumn {
    /// Direction a column starts in when first sorted by: text-like
    /// columns A→Z, numbers and dates largest/newest first.
    fn default_ascending(self) -> bool {
        matches!(self, SortColumn::Name | SortColumn::Perms)
    }
}

#[derive(Clone, Copy, PartialEq)]
struct SortState {
    column: SortColumn,
    ascending: bool,
}

/// Chart and scan settings (saved in settings.json). Each has an allowed
/// range, below, so no value can break the chart.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    max_render_depth: usize,
    min_segment_angle_deg: f32,
    max_children_shown: usize,
    /// Every child gets its own slice (no "other"), however thin.
    unlimited_slices: bool,
    hub_radius_frac: f32,
    /// Days of age at which slices reach their darkest shade.
    age_days: u32,
    /// How many shades of age there are.
    age_steps: u8,
    /// Brightness of the darkest shade, in percent.
    age_darkest_pct: u8,
    free_space_gamma: f32,
    stroke_width: f32,
    stroke_alpha: u8,
    tess_px_per_step: f32,
    max_log_lines: usize,
    /// Count file lengths instead of disk space used (from the next scan).
    apparent_size: bool,
    /// Most rows in the contents table's flat list.
    flat_rows: usize,
    /// The flat list shows every file (no row limit).
    flat_all: bool,
    /// Ctrl+C / Ctrl+X also put the picked paths on the clipboard as text.
    paths_to_clipboard: bool,
    /// Category colors told apart with any kind of color blindness.
    color_blind_safe: bool,
    /// Sizes measured in files instead of bytes (see `scan::weight`).
    measure_files: bool,
}

/// Allowed ranges, for the settings sliders and for values read from the
/// settings file.
impl Settings {
    const DEPTH: RangeInclusive<usize> = 1..=12;
    const MIN_ANGLE: RangeInclusive<f32> = 0.1..=5.0;
    const MAX_CHILDREN: RangeInclusive<usize> = 4..=360;
    const HUB: RangeInclusive<f32> = 0.05..=0.5;
    const AGE_DAYS: RangeInclusive<u32> = 7..=3650;
    const AGE_STEPS: RangeInclusive<u8> = 2..=10;
    const AGE_DARKEST: RangeInclusive<u8> = 10..=90;
    const FREE_GAMMA: RangeInclusive<f32> = 0.2..=1.5;
    const STROKE_WIDTH: RangeInclusive<f32> = 0.0..=3.0;
    const TESS: RangeInclusive<f32> = 1.0..=10.0;
    const LOG_LINES: RangeInclusive<usize> = 50..=5000;
    const FLAT_ROWS: RangeInclusive<usize> = 100..=100_000;

    fn sanitized(mut self) -> Self {
        self.max_render_depth = self
            .max_render_depth
            .clamp(*Self::DEPTH.start(), *Self::DEPTH.end());
        self.min_segment_angle_deg = self
            .min_segment_angle_deg
            .clamp(*Self::MIN_ANGLE.start(), *Self::MIN_ANGLE.end());
        self.max_children_shown = self
            .max_children_shown
            .clamp(*Self::MAX_CHILDREN.start(), *Self::MAX_CHILDREN.end());
        self.hub_radius_frac = self
            .hub_radius_frac
            .clamp(*Self::HUB.start(), *Self::HUB.end());
        self.age_days = self
            .age_days
            .clamp(*Self::AGE_DAYS.start(), *Self::AGE_DAYS.end());
        self.age_steps = self
            .age_steps
            .clamp(*Self::AGE_STEPS.start(), *Self::AGE_STEPS.end());
        self.age_darkest_pct = self
            .age_darkest_pct
            .clamp(*Self::AGE_DARKEST.start(), *Self::AGE_DARKEST.end());
        self.free_space_gamma = self
            .free_space_gamma
            .clamp(*Self::FREE_GAMMA.start(), *Self::FREE_GAMMA.end());
        self.stroke_width = self
            .stroke_width
            .clamp(*Self::STROKE_WIDTH.start(), *Self::STROKE_WIDTH.end());
        self.tess_px_per_step = self
            .tess_px_per_step
            .clamp(*Self::TESS.start(), *Self::TESS.end());
        self.max_log_lines = self
            .max_log_lines
            .clamp(*Self::LOG_LINES.start(), *Self::LOG_LINES.end());
        self.flat_rows = self
            .flat_rows
            .clamp(*Self::FLAT_ROWS.start(), *Self::FLAT_ROWS.end());
        self
    }
}

impl Settings {
    /// How age turns into slice brightness.
    fn age_shades(&self) -> AgeShades {
        AgeShades {
            days: self.age_days,
            steps: self.age_steps,
            darkest: f32::from(self.age_darkest_pct) / 100.0,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_render_depth: 6,
            min_segment_angle_deg: 0.75,
            max_children_shown: 120,
            unlimited_slices: false,
            hub_radius_frac: 0.22,
            age_days: 365,
            age_steps: 3,
            age_darkest_pct: 40,
            free_space_gamma: 0.7,
            stroke_width: 1.0,
            stroke_alpha: 90,
            tess_px_per_step: 3.0,
            max_log_lines: 500,
            apparent_size: false,
            flat_rows: 1000,
            flat_all: false,
            paths_to_clipboard: true,
            color_blind_safe: false,
            measure_files: false,
        }
    }
}

struct DiskScanApp {
    /// The last completed scan, unfiltered. `root` is what's shown: this tree
    /// or a filtered copy.
    full_root: Option<Arc<Node>>,
    show_filters: bool,
    /// Filter panel fields as currently typed / as last applied.
    filter_form: FilterForm,
    filter_applied: FilterForm,
    filter: Option<Arc<CompiledFilter>>,
    filter_error: Option<String>,
    /// The categories from categories.json (reloaded at each new scan).
    cats: Arc<CategoryModel>,
    /// Slice looks of the displayed tree, for the tree_gen they were built for.
    looks: Option<(u64, Looks)>,
    /// When the last scan finished: slice colors fade in from then.
    colored_at: Option<Instant>,
    /// Slice looks of the running (or last) scan's folders as they
    /// finished, and when: the live chart's colors, and the fade-in times
    /// kept for the finished chart.
    live_looks: LiveLooks,
    /// The running scan's counters, read every 100 ms into `live_looks`.
    live_tree: Option<Arc<LiveTree>>,
    /// When the live tree is to be read next.
    live_read_at: Option<Instant>,
    /// A pick from the left panel, applied once the frame's table is drawn
    /// (the table is drawn from the tree as it was when the frame began).
    pick_pending: Option<Option<Pick>>,
    /// Extension totals of the files found so far in the running scan, so
    /// the category bar grows live like the table.
    live_exts: ExtTotals,
    /// The category or extension picked in the left panel (None = all
    /// files). Applied on top of `filter`.
    pick: Option<Pick>,
    /// The tree with `filter` applied but not `pick`: what the left panel
    /// breaks down, so every category and extension stays clickable.
    cat_base: Option<Arc<Node>>,
    /// Category breakdown of the viewed folder, cached per (folder, tree_gen).
    cat_breakdown: Vec<CategoryRow>,
    cat_breakdown_for: Option<(PathBuf, u64)>,
    /// The path bar is a text field (else clickable folder names).
    path_editing: bool,
    path_edit_focus_pending: bool,
    /// The folder dialog's result, while it's open.
    folder_pick_rx: Option<Receiver<Option<PathBuf>>>,
    root: Option<Arc<Node>>,
    /// Zoom history as child-index paths; the last is the folder shown.
    view_stack: Vec<Vec<usize>>,
    scanning: bool,
    scan_rx: Option<Receiver<ScanMsg>>,
    /// Highest progress shown this scan, so the bar never moves backwards.
    progress_shown: f32,
    scan_start: Instant,
    hidden: HashSet<PathBuf>,
    hovered: Option<HoverInfo>,
    /// Counts folders added to `partial_root` during a scan. The live table
    /// refreshes from it at most every 250 ms, bumping `live_gen`.
    partial_gen: u64,
    live_gen: u64,
    /// What the live table shows: a copy of the live tree's top level, taken
    /// at each refresh (the live tree keeps re-sorting as data arrives).
    live_view: Node,
    live_seen: u64,
    live_refreshed: Instant,
    /// Counts changes to the displayed tree, so views derived from it know to
    /// recompute.
    tree_gen: u64,
    /// Folders the last scan couldn't list (size unknown), for the delete
    /// dialog's warning.
    unreadable: Vec<PathBuf>,
    /// Set by the scanner on finding a Korean name; `korean_font` once the
    /// font for it is loaded.
    saw_hangul: Arc<std::sync::atomic::AtomicBool>,
    korean_font: bool,
    /// Pending deletes and their confirmation (see delete.rs).
    removal: delete::Removal,
    /// Copy and move (Ctrl+C / Ctrl+X, then Ctrl+V).
    transfer: transfer::Transfer,
    /// The status line to show when the folder rescan running now ends,
    /// instead of the scan time.
    status_after_rescan: Option<String>,
    /// Item the chart's right-click menu acts on, fixed when it opens.
    context_target: Option<PathBuf>,
    summary_view: bool,
    chart_order: ChartOrder,
    /// Chart size relative to fitting its area; Ctrl+mouse wheel changes it.
    chart_scale: f32,
    /// Pan of a zoomed-in chart from its centred position (drag to move).
    chart_offset: Vec2,
    /// Slice highlighted by the arrow keys.
    selection: Option<ChartSel>,
    /// Selectable slices of the chart drawn this frame: (idx_path relative
    /// to the view, start angle). Empty when no chart is on screen.
    chart_segs: Vec<(Vec<usize>, f32)>,
    /// Whether a text field had keyboard focus as this frame began, so keys
    /// typed into it (Enter in particular) aren't also taken by the chart.
    typing: bool,
    /// The Summary view's contents table (see table.rs).
    table: TableState,
    /// Folder rescan in progress ("r" in the table), to splice into the
    /// full tree when it finishes.
    graft: Option<Graft>,
    /// Where each file with several hard links in the scanned tree is
    /// counted (by device and inode).
    link_owners: FxHashMap<(u64, u64), PathBuf>,
    /// The next scan only renews a folder of the scanned tree.
    rescanning_part: bool,
    /// The categories the running scan classifies files with, and those
    /// the scanned tree's files were classified with (None: mixed).
    scan_cats: Option<Arc<CategoryModel>>,
    tree_cats: Option<Arc<CategoryModel>>,
    /// Whether the running scan, and the one that made the tree, count
    /// apparent sizes.
    scan_apparent: bool,
    tree_apparent: bool,
    /// Whether the tree's folders are sorted by files (else by bytes).
    tree_by_files: bool,
    /// A folder to scan as soon as the app starts (from the command line).
    start_path: Option<PathBuf>,
    /// The contents and extension sorts switched from size to files when
    /// the measure became files (switched back with it).
    sorts_switched: [bool; 2],
    status: String,
    /// (capacity, free bytes) of the drive when the scanned folder is a mount
    /// point, for the chart's free-space slice.
    free_space: Option<(u64, u64)>,
    log: Vec<String>,
    log_truncated: u64,
    cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The live tree of the scan in progress: every finished folder's totals.
    partial_root: Node,
    settings: Settings,
    /// The settings as last saved; None until the file is up to date.
    saved_config: Option<Config>,
    show_settings: bool,
    /// The About window is open.
    show_about: bool,
    path_input: String,
    path_input_focused: bool,
    contents_sort: SortState,
    /// Sort order of the left panel's extensions table.
    ext_sort: SortState,
    /// File types detected from content, for the hovered file only, on a
    /// background thread. `None` = looked up, but unknown.
    mime_cache: std::collections::HashMap<PathBuf, Option<String>>,
    mime_inflight: HashSet<PathBuf>,
    mime_tx: Sender<(PathBuf, Option<String>)>,
    mime_rx: Receiver<(PathBuf, Option<String>)>,
    user_cache: std::collections::HashMap<u32, String>,
    group_cache: std::collections::HashMap<u32, String>,
}

impl Default for DiskScanApp {
    fn default() -> Self {
        let (mime_tx, mime_rx) = channel();
        let mut app = Self {
            folder_pick_rx: None,
            full_root: None,
            show_filters: false,
            filter_form: FilterForm::default(),
            filter_applied: FilterForm::default(),
            filter: None,
            filter_error: None,
            path_editing: false,
            path_edit_focus_pending: false,
            root: None,
            view_stack: vec![vec![]],
            scanning: false,
            scan_rx: None,
            progress_shown: 0.0,
            scan_start: Instant::now(),
            hidden: HashSet::new(),
            hovered: None,
            context_target: None,
            removal: Default::default(),
            transfer: Default::default(),
            status_after_rescan: None,
            saw_hangul: Default::default(),
            korean_font: false,
            unreadable: Vec::new(),
            tree_gen: 0,
            partial_gen: 0,
            live_gen: 0,
            live_view: empty_node(),
            live_seen: 0,
            live_refreshed: Instant::now(),
            summary_view: false,
            chart_order: ChartOrder::Size,
            chart_scale: 1.0,
            chart_offset: Vec2::ZERO,
            selection: None,
            chart_segs: Vec::new(),
            typing: false,
            table: TableState::default(),
            graft: None,
            link_owners: Default::default(),
            rescanning_part: false,
            scan_cats: None,
            tree_cats: None,
            scan_apparent: false,
            tree_apparent: false,
            tree_by_files: false,
            start_path: None,
            sorts_switched: [false; 2],
            status: String::new(),
            free_space: None,
            log: Vec::new(),
            log_truncated: 0,
            cancel_flag: None,
            partial_root: empty_node(),
            settings: Settings::default(),
            saved_config: None,
            show_settings: false,
            show_about: false,
            path_input: String::new(),
            path_input_focused: false,
            cats: Arc::new(CategoryModel::defaults()),
            looks: None,
            colored_at: None,
            live_looks: Default::default(),
            live_tree: None,
            live_read_at: None,
            pick: None,
            pick_pending: None,
            live_exts: ExtTotals::default(),
            cat_base: None,
            cat_breakdown: Vec::new(),
            cat_breakdown_for: None,
            // Largest first, like the chart.
            contents_sort: SortState {
                column: SortColumn::Size,
                ascending: false,
            },
            ext_sort: SortState {
                column: SortColumn::Size,
                ascending: false,
            },
            mime_cache: std::collections::HashMap::new(),
            mime_inflight: HashSet::new(),
            mime_tx,
            mime_rx,
            user_cache: std::collections::HashMap::new(),
            group_cache: std::collections::HashMap::new(),
        };
        // Tests build the app too: they keep the defaults and never read or
        // write the user's settings.
        if !cfg!(test) {
            let (cfg, needs_save, problem) = Config::load();
            app.settings = cfg.chart.clone();
            app.apply_table_prefs(&cfg.table);
            app.saved_config = (!needs_save).then_some(cfg);
            if let Some(p) = problem {
                app.log_issue(p);
            }
            if let Some(p) = lang_file_problem(&current_lang_code()) {
                app.log_issue(p);
            }
        }
        app.reload_categories();
        app
    }
}

impl DiskScanApp {
    /// Child-index path of the folder shown: the last entry of the zoom
    /// history, which always has at least one.
    fn current_view(&self) -> &Vec<usize> {
        self.view_stack
            .last()
            .expect("the zoom history is never empty")
    }

    /// Replaces the folder shown (the last entry of the zoom history).
    fn set_current_view(&mut self, view: Vec<usize>) {
        *self
            .view_stack
            .last_mut()
            .expect("the zoom history is never empty") = view;
    }

    /// Rereads categories.json (edits show up at the next scan). A changed
    /// list drops the picked category: positions may mean something else.
    fn reload_categories(&mut self) {
        // Tests keep the built-in categories and never read the user's file.
        if cfg!(test) {
            return;
        }
        let (model, problem) = CategoryModel::load();
        if *self.cats != model {
            self.cats = Arc::new(model);
            self.pick = None;
            self.cat_breakdown_for = None;
            // Colors worked out with the old categories are redone.
            self.tree_gen += 1;
        }
        if let Some(p) = problem {
            self.log_issue(p);
        }
    }

    fn start_scan(&mut self, path: PathBuf) {
        self.status_after_rescan = None;
        // The canonical path, so no "..", "./" or doubled slashes show up.
        let path = true_case(&std::fs::canonicalize(&path).unwrap_or(path));
        // Any new scan supersedes a pending folder rescan ("r").
        self.graft = None;
        self.scanning = true;
        // A new live table.
        self.live_gen += 1;
        self.live_view = empty_node();
        self.live_exts.clear();
        self.live_looks = Default::default();
        self.cat_breakdown.clear();
        self.cat_breakdown_for = None;
        self.status = tr("STATUS_SCANNING");
        // This scan reports again whatever it can't read under `path`.
        self.unreadable.retain(|u| !u.starts_with(&path));
        self.scan_start = Instant::now();
        self.selection = None;
        self.progress_shown = 0.0;
        self.view_stack = vec![vec![]];
        self.hidden.clear();
        self.log.clear();
        self.log_truncated = 0;
        // After clearing the log, so problems in the file stay listed.
        self.reload_categories();
        self.partial_root = empty_node();
        self.partial_root.name = file_name_of(&path).into();
        self.partial_root.set_path(&path);
        self.free_space = if is_real_mount_point(&path) {
            fs_space(&path)
        } else {
            None
        };

        // Stop any previous scan still running.
        if let Some(prev) = &self.cancel_flag {
            prev.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.cancel_flag = Some(cancel.clone());

        let apparent_size = self.settings.apparent_size;
        self.scan_apparent = apparent_size;
        // A folder scanned again inside the tree: files counted under a name
        // outside it count nothing in it, as in the full scan.
        let elsewhere: FxHashSet<(u64, u64)> = if std::mem::take(&mut self.rescanning_part) {
            self.link_owners
                .iter()
                .filter(|(_, at)| !at.starts_with(&path))
                .map(|(key, _)| *key)
                .collect()
        } else {
            Default::default()
        };
        let saw_hangul = self.saw_hangul.clone();

        let (tx, rx) = channel();
        self.scan_rx = Some(rx);
        // Every folder and file counts in the live tree as it's read.
        let live = Arc::new(LiveTree::new(self.cats.clone()));
        self.scan_cats = Some(self.cats.clone());
        self.live_tree = Some(live.clone());
        self.live_read_at = None;
        std::thread::spawn(move || {
            let start = Instant::now();
            if !path.exists() {
                let _ = tx.send(ScanMsg::Error(trf(
                    "ERR_PATH_NOT_FOUND",
                    &[&show_path(&path)],
                )));
                return;
            }
            match std::fs::metadata(&path) {
                Err(e) => {
                    let _ = tx.send(ScanMsg::Error(trf(
                        "ERR_CANNOT_STAT",
                        &[&show_path(&path), &e.to_string()],
                    )));
                    return;
                }
                // A file given to scan (on the command line, say).
                Ok(m) if !m.is_dir() => {
                    let _ = tx.send(ScanMsg::Error(trf(
                        "ERR_NOT_A_DIRECTORY",
                        &[&show_path(&path)],
                    )));
                    return;
                }
                Ok(_) => {}
            }
            let mounts = mount_points();
            let ctx = ScanCtx {
                mounts: &mounts,
                progress: &tx,
                cancel: &cancel,
                apparent_size,
                hard_links: HardLinks::counted_elsewhere(elsewhere),
                saw_hangul: &saw_hangul,
                in_file_order: is_rotational(&path),
                live: Some(&live),
            };
            let root = scan_dir(&path, &ctx);
            if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let counted_at = std::mem::take(&mut *ctx.hard_links.counted_at.lock().unwrap());
                let secs = start.elapsed().as_secs_f64();
                let _ = tx.send(ScanMsg::Done(root, secs, counted_at));
            }
        });
    }

    /// Puts the measure from the settings in use: folders sorted by it, and
    /// a table sorted by size or files switched to it.
    fn follow_measure(&mut self) {
        let files = self.settings.measure_files;
        if files != scan::measure_files() {
            scan::set_measure_files(files);
            // A sort by size follows to files and back; one chosen by files
            // stays.
            let sorts = [&mut self.contents_sort, &mut self.ext_sort];
            for (sort, switched) in sorts.into_iter().zip(&mut self.sorts_switched) {
                if files && sort.column == SortColumn::Size {
                    sort.column = SortColumn::Files;
                    *switched = true;
                } else if !files && *switched && sort.column == SortColumn::Files {
                    sort.column = SortColumn::Size;
                }
                if !files {
                    *switched = false;
                }
            }
            self.tree_gen += 1;
            // The live chart's last total was in the other measure.
            self.live_looks.reset_total();
        }
        // A full scan replaces the tree (sorted when it ends); a folder
        // rescan keeps the tree shown meanwhile, so that's sorted now.
        if self.tree_by_files == files || (self.scanning && self.graft.is_none()) {
            return;
        }
        // Sorted in place: nothing else may hold the tree meanwhile.
        let Some(root) = self.root.take() else {
            self.tree_by_files = files;
            return;
        };
        let view_paths = self.view_paths(&root);
        drop(root);
        self.cat_base = None;
        if let Some(full) = &mut self.full_root {
            scan::sort_tree_by_measure(Arc::make_mut(full), files);
        }
        self.tree_by_files = files;
        self.rebuild_view_tree();
        self.restore_view(&view_paths);
    }

    /// Rebuilds the displayed tree from the last scan, the filter and the
    /// picked category, keeping each view in the zoom history on the same
    /// folder (or its nearest remaining parent).
    fn rebuild_view_tree(&mut self) {
        self.tree_gen += 1;
        let Some(full) = self.full_root.clone() else {
            return;
        };
        let empty = |full: &Node| {
            let mut n = empty_node();
            n.name = full.name.clone();
            n.copy_place(full);
            n
        };
        let base = match &self.filter {
            Some(f) => Arc::new(filter_tree(&full, f).unwrap_or_else(|| empty(&full))),
            None => full.clone(),
        };
        let new_root = match &self.pick {
            Some(pick) => {
                let cats = self.cats.clone();
                Arc::new(
                    filter_tree_by(&base, &|n: &Node| cats.pick_matches(pick, &n.name))
                        .unwrap_or_else(|| empty(&full)),
                )
            }
            None => base.clone(),
        };
        self.cat_base = Some(base);
        if let Some(old) = &self.root {
            self.view_stack = self
                .view_stack
                .iter()
                .map(|vp| remap_index_path(old, &new_root, vp))
                .collect();
            self.view_stack.dedup();
        }
        self.root = Some(new_root);
        self.cat_breakdown_for = None;
        self.selection = None; // child indices may have changed
    }

    fn apply_filter_form(&mut self) {
        match CompiledFilter::compile(&self.filter_form) {
            Ok(f) => {
                self.filter = f.map(Arc::new);
                self.filter_applied = self.filter_form.clone();
                self.filter_error = None;
                self.rebuild_view_tree();
            }
            Err(e) => self.filter_error = Some(e),
        }
    }

    /// Ctrl+wheel over the chart resizes it around the pointer; dragging pans
    /// it while it's bigger than its area. Returns the chart's center and
    /// outer radius for this frame.
    fn chart_view(&mut self, ctx: &egui::Context, chart: &egui::Response) -> (Pos2, f32) {
        let rect = chart.rect;
        if chart.contains_pointer() {
            let z = ctx.input(|i| i.zoom_delta());
            if z != 1.0 {
                let new_scale = (self.chart_scale * z).clamp(0.25, 4.0);
                let z = new_scale / self.chart_scale;
                if let Some(p) = ctx.input(|i| i.pointer.hover_pos()) {
                    self.chart_offset = (p - rect.center()) * (1.0 - z) + self.chart_offset * z;
                }
                self.chart_scale = new_scale;
            }
        }
        if chart.dragged() {
            self.chart_offset += chart.drag_delta();
        }
        let max_radius = (rect.width().min(rect.height()) / 2.0 - 10.0) * self.chart_scale;
        // Pan only as far as the chart overhangs its area.
        let limit = Vec2::new(
            (max_radius + 10.0 - rect.width() / 2.0).max(0.0),
            (max_radius + 10.0 - rect.height() / 2.0).max(0.0),
        );
        self.chart_offset = self.chart_offset.clamp(-limit, limit);
        if limit != Vec2::ZERO {
            if chart.dragged() {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if chart.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
        (rect.center() + self.chart_offset, max_radius)
    }

    fn current_view_node<'a>(&self, root: &'a Node) -> &'a Node {
        let idx_path = self.current_view();
        get_node(root, idx_path)
    }

    /// Esc while scanning: stops the scan and goes back to the previous result,
    /// if any.
    /// Esc cancels the scan, unless it's closing a menu or window first.
    fn esc_cancels_scan(&mut self, ctx: &egui::Context) {
        let closing_something =
            self.show_about || self.table.show_help || egui::Popup::is_any_open(ctx);
        if self.scanning && !closing_something && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.abort_scan();
        }
    }

    fn abort_scan(&mut self) {
        if let Some(cancel) = &self.cancel_flag {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.scanning = false;
        self.scan_rx = None;
        self.cancel_graft();
        self.scan_ended();
        self.status = tr("STATUS_SCAN_ABORTED");
    }

    /// A scan finished, failed or was cancelled: frees the live tree and
    /// returns the memory.
    fn scan_ended(&mut self) {
        self.partial_root = empty_node();
        self.live_tree = None;
        after_tree_dropped();
    }

    /// Drains completed on-demand MIME lookups into the cache.
    fn poll_mime(&mut self) {
        while let Ok((path, result)) = self.mime_rx.try_recv() {
            self.mime_inflight.remove(&path);
            self.mime_cache.insert(path, result);
        }
    }

    /// Starts detecting `path`'s file type on a background thread, unless it's
    /// known or already being detected. For the hovered file only.
    fn ensure_mime_lookup(&mut self, path: &Path) {
        if self.mime_cache.contains_key(path) || self.mime_inflight.contains(path) {
            return;
        }
        if self.mime_cache.len() > 500 {
            self.mime_cache.clear(); // keeps the cache small
        }
        self.mime_inflight.insert(path.to_path_buf());
        let tx = self.mime_tx.clone();
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            let result = infer::get_from_path(&p)
                .ok()
                .flatten()
                .map(|t| t.mime_type().to_string());
            let _ = tx.send((p, result));
        });
    }

    /// Adds a line to the Issues log (up to its line limit).
    fn log_issue(&mut self, msg: String) {
        if self.log.len() < self.settings.max_log_lines {
            self.log.push(msg);
        } else {
            self.log_truncated += 1;
        }
    }

    /// The highlighted slice (idx_path relative to the current view), if
    /// it belongs to the view being shown.
    fn selected_rel(&self) -> Option<&Vec<usize>> {
        self.selection
            .as_ref()
            .filter(|s| Some(&s.view) == self.view_stack.last())
            .map(|s| &s.rel)
    }

    /// Moves the slice highlight: ⬆⬇ previous/next slice in the same ring
    /// (wrapping), ➡ first slice one ring out, ⬅ the parent slice (from the
    /// inner ring: the parent folder). With nothing highlighted, any arrow
    /// starts at the first inner slice.
    fn move_selection(&mut self, dir: NavDir) {
        let view = self.current_view().clone();
        let segs = &self.chart_segs;
        let first_where = |pred: &dyn Fn(&Vec<usize>) -> bool| {
            segs.iter()
                .filter(|(p, _)| pred(p))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(p, _)| p.clone())
        };
        let new_rel = match self.selected_rel().cloned() {
            None => first_where(&|p| p.len() == 1),
            Some(cur) => match dir {
                NavDir::Prev | NavDir::Next => {
                    let parent = &cur[..cur.len() - 1];
                    let mut sibs: Vec<&(Vec<usize>, f32)> = segs
                        .iter()
                        .filter(|(p, _)| p.len() == cur.len() && p[..p.len() - 1] == *parent)
                        .collect();
                    sibs.sort_by(|a, b| a.1.total_cmp(&b.1));
                    let n = sibs.len();
                    (n > 0).then(|| match sibs.iter().position(|(p, _)| *p == cur) {
                        Some(i) if dir == NavDir::Next => sibs[(i + 1) % n].0.clone(),
                        Some(i) => sibs[(i + n - 1) % n].0.clone(),
                        None => sibs[0].0.clone(),
                    })
                }
                NavDir::In => first_where(&|p| p.len() == cur.len() + 1 && p.starts_with(&cur)),
                NavDir::Out if cur.len() > 1 => Some(cur[..cur.len() - 1].to_vec()),
                NavDir::Out => {
                    self.chart_parent_folder();
                    return;
                }
            },
        };
        if let Some(rel) = new_rel {
            self.selection = Some(ChartSel { view, rel });
        }
    }

    /// Path of the highlighted slice; None if nothing or "other" is
    /// highlighted.
    fn selected_slice_path(&self) -> Option<PathBuf> {
        let rel = self.selected_rel()?;
        if is_other_marker(rel) {
            return None;
        }
        let root = self.root.as_ref()?;
        try_get_node(self.current_view_node(root), rel).map(|n| n.path())
    }

    /// Backspace (and ⬅ from the inner ring): steps the chart out to the
    /// parent folder, highlighting the folder just left.
    fn chart_parent_folder(&mut self) {
        let view = self.current_view().clone();
        if let Some((&left, parent_view)) = view.split_last() {
            let parent_view = parent_view.to_vec();
            self.set_current_view(parent_view.clone());
            self.selection = Some(ChartSel {
                view: parent_view,
                rel: vec![left],
            });
        }
    }

    /// Enter: same as clicking the highlighted slice — opens it if it's a
    /// folder.
    fn open_selection(&mut self) {
        let Some(rel) = self.selected_rel().cloned() else {
            return;
        };
        if is_other_marker(&rel) {
            self.open_other_bucket(&rel);
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        let view = self.current_view().clone();
        if try_get_node(get_node(&root, &view), &rel).is_some_and(|n| n.is_dir) {
            let mut vp = view;
            vp.extend(rel);
            self.view_stack.push(vp);
            self.selection = None;
        }
    }

    /// Opening "other": shows the folder it belongs to in the Summary view,
    /// where every item is listed.
    fn open_other_bucket(&mut self, ip: &[usize]) {
        let owner_rel = &ip[..ip.len() - 1];
        if !owner_rel.is_empty() {
            let mut vp = self.current_view().clone();
            vp.extend(owner_rel.iter().copied());
            self.view_stack.push(vp);
        }
        self.summary_view = true;
        self.selection = None;
    }

    /// Handles waiting scan messages, up to a time budget per frame. True if
    /// messages are still waiting.
    fn poll_scan(&mut self) -> bool {
        // A time budget keeps the window responsive during bursts of messages;
        // the rest wait for the next frame.
        const FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(30);
        let drain_start = Instant::now();
        let mut processed = 0u32;
        if let Some(rx) = &self.scan_rx {
            loop {
                if processed.is_multiple_of(64)
                    && processed > 0
                    && drain_start.elapsed() >= FRAME_BUDGET
                {
                    return true;
                }
                processed += 1;
                match rx.try_recv() {
                    Ok(ScanMsg::Unreadable(p)) => {
                        self.unreadable.push(p);
                    }
                    Ok(ScanMsg::LogError(msg)) => {
                        if self.log.len() < self.settings.max_log_lines {
                            self.log.push(msg);
                        } else {
                            self.log_truncated += 1;
                        }
                    }
                    Ok(ScanMsg::SliceDone { exts }) => {
                        // Per-extension totals for the category bar; sizes
                        // come from the live tree.
                        for (ext, size, files) in exts {
                            add_ext(&mut self.live_exts, ext, size, files);
                        }
                    }
                    Ok(ScanMsg::Done(mut node, secs, counted_at)) => {
                        self.live_tree = None;
                        self.colored_at = Some(Instant::now());
                        let scan_cats = self.scan_cats.take();
                        match &self.graft {
                            Some(g) => {
                                let target = g.target.clone();
                                self.link_owners.retain(|_, at| !at.starts_with(&target));
                                // A folder classified with other categories than
                                // the rest leaves the tree mixed.
                                let same = match (&self.tree_cats, &scan_cats) {
                                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                                    _ => false,
                                };
                                if !same {
                                    self.tree_cats = None;
                                }
                            }
                            None => {
                                self.link_owners.clear();
                                self.tree_cats = scan_cats;
                                self.tree_apparent = self.scan_apparent;
                                // Scans sort by bytes; shown sorted by files
                                // at once when that's the measure.
                                self.tree_by_files = self.settings.measure_files;
                                if self.tree_by_files {
                                    scan::sort_tree_by_measure(&mut node, true);
                                }
                            }
                        }
                        self.link_owners.extend(counted_at);
                        if self.graft.is_some() {
                            self.finish_graft(node);
                        } else {
                            self.full_root = Some(Arc::new(node));
                            self.rebuild_view_tree();
                        }
                        self.scanning = false;
                        self.status = self.status_after_rescan.take().unwrap_or_else(|| {
                            trf("STATUS_SCAN_COMPLETED", &[&format!("{:.1}", secs)])
                        });
                        self.scan_rx = None;
                        self.scan_ended();
                        break;
                    }
                    Ok(ScanMsg::Error(e)) => {
                        self.cancel_graft();
                        self.scan_ended();
                        self.status = e;
                        self.scanning = false;
                        self.scan_rx = None;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.cancel_graft();
                        self.scan_ended();
                        self.scanning = false;
                        self.scan_rx = None;
                        break;
                    }
                }
            }
        }
        false
    }
}

impl eframe::App for DiskScanApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        category::COLOR_BLIND_SAFE.store(
            self.settings.color_blind_safe,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.follow_measure();
        self.typing = ctx.text_edit_focused();
        self.note_paste_key(&ctx);
        if let Some(path) = self.start_path.take() {
            self.start_scan(path);
        }
        let scan_backlog = self.poll_scan();
        // The live chart and table: the live tree, read every 100 ms as deep
        // as the chart currently shows.
        if self.scanning
            && let Some(tree) = self.live_tree.clone()
            && self.live_read_at.is_none_or(|t| Instant::now() >= t)
        {
            let started = Instant::now();
            // The smallest slice the chart can draw, as a share of the circle.
            let min_angle = if self.settings.unlimited_slices {
                0.02
            } else {
                self.settings.min_segment_angle_deg
            };
            let min_share = f64::from(min_angle) / 360.0;
            if let Some(root) = tree.snapshot(
                self.settings.max_render_depth,
                min_share,
                &mut self.live_looks,
            ) {
                self.partial_root = root;
                self.partial_gen += 1;
            }
            // The next read in 100 ms, or later if reading took long, so the
            // window always has time for everything else.
            let took = started.elapsed();
            self.live_read_at =
                Some(started + (took * 5).max(std::time::Duration::from_millis(100)));
        }
        // The live looks' fade-in times are done with once the chart has
        // faded in.
        if !self.scanning && self.color_fade() >= 1.0 && !self.live_looks.is_empty() {
            self.live_looks = Default::default();
        }
        // The Korean font: once a Korean name is found, or for a Korean UI.
        let korean_needed = self.saw_hangul.load(std::sync::atomic::Ordering::Relaxed)
            || current_lang_code() == "ko";
        if !self.korean_font && korean_needed {
            self.korean_font = true;
            install_fallback_fonts(&ctx, true);
        }
        self.poll_mime();
        self.removal_frame_start(&ctx);
        self.table_frame_start(&ctx);
        self.esc_cancels_scan(&ctx);
        // While scanning, redraw 4 times a second (each frame takes CPU from the
        // scan), or at once while messages are waiting.
        if scan_backlog || !self.mime_inflight.is_empty() {
            ctx.request_repaint();
        } else if self.scanning {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        self.toolbar_ui(ui);
        self.filter_panel_ui(ui);
        self.settings_panel_ui(ui);
        self.log_panel_ui(ui);
        let area = self.main_area_ui(ui);
        if !self.summary_view && (self.root.is_some() || self.scanning) {
            self.chart_order_buttons(&ctx, area);
        }

        // Chart view, after a scan: stats of the highlighted slice (else the
        // folder viewed) in the top-left corner.
        if !self.scanning
            && !self.summary_view
            && let Some(root) = &self.root
        {
            let view_node = self.current_view_node(root);
            let selected = self
                .selected_rel()
                .and_then(|rel| try_get_node(view_node, rel));
            egui::Area::new("folder_stats_overlay".into())
                .order(egui::Order::Foreground)
                .interactable(false)
                .fixed_pos(area.left_top() + Vec2::new(8.0, 8.0))
                .show(&ctx, |ui| {
                    let n = selected.unwrap_or(view_node);
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.strong(short_path(&n.path()));
                    folder_stats_ui(ui, n);
                });
        }

        // Chart view: the categories in the category bar's order, bottom
        // left. While scanning, those found so far, re-sorted as totals
        // grow; each row glides to its new place.
        if !self.summary_view && (self.scanning || self.root.is_some()) {
            let rows: Vec<Category> = if self.scanning {
                self.live_looks.order()
            } else {
                let root = self.root.clone().expect("checked above");
                self.refresh_cat_breakdown(self.current_view_node(&root));
                self.cat_breakdown.iter().map(|r| r.cat).collect()
            };
            let dark = ctx.global_style().visuals.dark_mode;
            egui::Area::new("category_legend".into())
                .order(egui::Order::Foreground)
                .interactable(false)
                .pivot(egui::Align2::LEFT_BOTTOM)
                .fixed_pos(area.left_bottom() + Vec2::new(8.0, -8.0))
                .show(&ctx, |ui| {
                    let font = egui::TextStyle::Small.resolve(ui.style());
                    let text_color = ui.visuals().text_color();
                    let labels: Vec<_> = rows
                        .iter()
                        .map(|&c| {
                            ui.painter().layout_no_wrap(
                                self.cats.label(c),
                                font.clone(),
                                text_color,
                            )
                        })
                        .collect();
                    const SWATCH: f32 = 10.0;
                    let gap = ui.spacing().item_spacing.x;
                    let row_h = font.size.max(SWATCH) + ui.spacing().item_spacing.y + 4.0;
                    let width =
                        labels.iter().map(|g| g.size().x).fold(0.0, f32::max) + SWATCH + gap;
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(width, row_h * rows.len() as f32),
                        egui::Sense::hover(),
                    );
                    for (i, (&cat, label)) in rows.iter().zip(labels).enumerate() {
                        // Glides to row `i` when the order changes.
                        let y = ui.ctx().animate_value_with_time(
                            egui::Id::new(("legend_row", cat.0)),
                            i as f32 * row_h,
                            0.25,
                        );
                        let top = rect.top() + y;
                        let mid = top + row_h / 2.0;
                        let swatch = egui::Rect::from_center_size(
                            Pos2::new(rect.left() + SWATCH / 2.0, mid),
                            Vec2::splat(SWATCH),
                        );
                        ui.painter()
                            .rect_filled(swatch, 2.0, self.cats.color(cat, dark));
                        let at = Pos2::new(swatch.right() + gap, mid - label.size().y / 2.0);
                        ui.painter().galley(at, label, text_color);
                    }
                });
        }

        // Chart keys: arrows move the highlight, Backspace goes up, Enter opens,
        // Esc clears, D / T delete or trash, r rescans. The table's keys are in
        // table.rs.
        self.save_config_if_changed();
        // The table also runs live during a scan (see live_table_ui).
        if self.summary_view && (self.root.is_some() || self.scanning) {
            self.table_keys(&ctx);
            self.table_overlays(&ctx, area);
        }
        // Opened from the main menu in any view, or with "?" in the table.
        if self.table.show_help {
            // "?" closes it in any view (the table checks it with its own keys).
            if !self.summary_view
                && ctx.input(|i| {
                    i.events
                        .iter()
                        .any(|e| matches!(e, egui::Event::Text(t) if t == "?"))
                })
            {
                self.table.show_help = false;
            } else {
                self.help_overlay(&ctx);
            }
        }
        if self.show_about {
            self.about_window(&ctx);
        }
        quit_on_ctrl_q(&ctx);
        // Not while the right-click menu is open (Esc still closes it).
        let menu_open = egui::Popup::is_any_open(&ctx);
        if self.root.is_some()
            && !self.scanning
            && !self.summary_view
            && !self.typing
            && !self.delete_dialog_open()
            && !menu_open
            && !self.table.show_help
            && !self.show_about
        {
            if let Some(d) = ctx.input(arrow_nav) {
                self.move_selection(d);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) {
                self.chart_parent_folder();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.open_selection();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selection = None;
            }
            let typed = ctx.input(|i| {
                let plain = !i.modifiers.command && !i.modifiers.alt;
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) if plain => Some(t.clone()),
                        _ => None,
                    })
                    .collect::<String>()
            });
            if typed.contains('r') {
                self.rescan_current();
            }
            // Ctrl+C / Ctrl+X take the selected slice; Ctrl+V pastes into the
            // folder shown.
            let (copy, cut, paste) = self.clipboard_events(&ctx);
            if let Some(target) = self.selected_slice_path().filter(|_| copy || cut) {
                let mode = if cut { ClipMode::Move } else { ClipMode::Copy };
                self.clip(&ctx, vec![target], mode);
            }
            if let (Some(text), Some(root)) = (paste, self.root.clone()) {
                let dest = self.current_view_node(&root).path();
                self.paste_into(dest, &text);
            }
            if (typed.contains('D') || typed.contains('T'))
                && let Some(target) = self.selected_slice_path()
            {
                if typed.contains('D') {
                    self.ask_delete(vec![target]);
                } else {
                    self.queue_trash(vec![target]);
                    ctx.request_repaint();
                }
            }
        }
        self.confirm_dialog(&ctx);
        self.transfer_ui(&ctx);
    }
}

/// Hides one harmless panic: with no accessibility service running, the
/// accessibility library's thread panics at start-up. Other panics are
/// reported as usual.
fn quiet_accessibility_panic() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info
            .location()
            .is_some_and(|l| l.file().contains("accesskit_unix"))
        {
            return;
        }
        default(info);
    }));
}

/// Ctrl+Q in any of the app's windows closes the app.
pub(crate) fn quit_on_ctrl_q(ctx: &egui::Context) {
    if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Q)) {
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
    }
}

/// Scanning or deleting folder chains past the kernel's path length limit
/// keeps one folder open per level; the usual soft limit of 1,024 open
/// files would cut very deep chains short. Raise it to the hard limit (as
/// file managers and `find` effectively allow).
pub(crate) fn raise_open_file_limit() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: both calls only read or write the local `lim`.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 && lim.rlim_cur < lim.rlim_max {
            lim.rlim_cur = lim.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

fn main() -> eframe::Result<()> {
    raise_open_file_limit();
    // A command runs and exits; otherwise the app starts.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let start_path = match cli::run(&args) {
        cli::Run::App(path) => path,
        cli::Run::Exit(code) => std::process::exit(code),
    };
    // Before anything reads the settings.
    let not_copied = config::copy_old_config();
    quiet_accessibility_panic();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 800.0])
            .with_title("SpaceScan")
            .with_app_id("spacescan"),
        ..Default::default()
    };
    eframe::run_native(
        "spacescan",
        options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
            install_fallback_fonts(&cc.egui_ctx, false);
            let mut app = DiskScanApp {
                start_path,
                ..DiskScanApp::default()
            };
            for n in &not_copied {
                app.log_issue(n.message());
            }
            Ok(Box::new(app))
        }),
    )
}
