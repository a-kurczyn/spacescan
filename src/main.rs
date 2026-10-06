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
        /// `v` moved into `range` if it's outside it.
        fn clamp<T: PartialOrd + Copy>(v: &mut T, range: RangeInclusive<T>) {
            if *v < *range.start() {
                *v = *range.start();
            } else if *v > *range.end() {
                *v = *range.end();
            }
        }
        clamp(&mut self.max_render_depth, Self::DEPTH);
        clamp(&mut self.min_segment_angle_deg, Self::MIN_ANGLE);
        clamp(&mut self.max_children_shown, Self::MAX_CHILDREN);
        clamp(&mut self.hub_radius_frac, Self::HUB);
        clamp(&mut self.age_days, Self::AGE_DAYS);
        clamp(&mut self.age_steps, Self::AGE_STEPS);
        clamp(&mut self.age_darkest_pct, Self::AGE_DARKEST);
        clamp(&mut self.free_space_gamma, Self::FREE_GAMMA);
        clamp(&mut self.stroke_width, Self::STROKE_WIDTH);
        clamp(&mut self.tess_px_per_step, Self::TESS);
        clamp(&mut self.max_log_lines, Self::LOG_LINES);
        clamp(&mut self.flat_rows, Self::FLAT_ROWS);
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
    /// What the last read of the live tree showed: depth, smallest slice
    /// angle (as bits), picked category and measure. A change reads it again
    /// at once.
    live_read_for: Option<(usize, u32, Option<Category>, Measure)>,
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
    /// Category breakdown of the viewed folder, made from `cat_base`: for
    /// (folder, base_gen, measure in use).
    cat_breakdown: Vec<CategoryRow>,
    /// The extensions table's rows, made from `cat_breakdown`.
    ext_rows: Option<panels::ExtRows>,
    cat_breakdown_for: Option<(PathBuf, u64, Measure)>,
    /// Counts changes to `cat_base`, for what's made from it.
    base_gen: u64,
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
    /// Counts reads of the live tree into `partial_root` during a scan. The
    /// live table refreshes from each new one, bumping `live_gen`.
    partial_gen: u64,
    live_gen: u64,
    /// What the live table shows: a copy of the live tree's top level, taken
    /// at each refresh (the live tree keeps re-sorting as data arrives).
    live_view: Node,
    live_seen: u64,
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
    /// What sizes are measured in, from the settings at the start of each
    /// frame (see `follow_measure`).
    measure: Measure,
    /// A folder to scan as soon as the app starts (from the command line).
    start_path: Option<PathBuf>,
    /// Problems from before the window opened (the old settings' copy),
    /// listed again after the first scan clears the Issues log.
    startup_issues: Vec<String>,
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
    /// A change not saved yet, and since when the settings have been so.
    config_changed: Option<(Config, Instant)>,
    show_settings: bool,
    /// The About window is open.
    show_about: bool,
    /// Asking whether to quit while work is running.
    quit_asked: bool,
    /// Quitting was confirmed: the next close goes through.
    quit_confirmed: bool,
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
    /// Reads and writes the user's settings and categories (see
    /// `with_user_files`).
    user_files: bool,
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
            measure: Measure::Bytes,
            start_path: None,
            startup_issues: Vec::new(),
            sorts_switched: [false; 2],
            status: String::new(),
            free_space: None,
            log: Vec::new(),
            log_truncated: 0,
            cancel_flag: None,
            partial_root: empty_node(),
            settings: Settings::default(),
            saved_config: None,
            config_changed: None,
            show_settings: false,
            show_about: false,
            quit_asked: false,
            quit_confirmed: false,
            path_input: String::new(),
            path_input_focused: false,
            cats: Arc::new(CategoryModel::defaults()),
            looks: None,
            colored_at: None,
            live_looks: Default::default(),
            live_tree: None,
            live_read_at: None,
            live_read_for: None,
            pick: None,
            pick_pending: None,
            live_exts: ExtTotals::default(),
            cat_base: None,
            cat_breakdown: Vec::new(),
            ext_rows: None,
            cat_breakdown_for: None,
            base_gen: 0,
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
            user_files: false,
        };
        app.reload_categories();
        app
    }
}

impl DiskScanApp {
    /// The app as the user starts it: with their settings and categories,
    /// which it reads and keeps up to date. (`default` reads and writes
    /// none of the user's files: what tests use.)
    fn with_user_files() -> Self {
        let mut app = DiskScanApp {
            user_files: true,
            ..DiskScanApp::default()
        };
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
        // Without the user's files (tests): the built-in categories.
        if !self.user_files {
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
        for msg in std::mem::take(&mut self.startup_issues) {
            self.log_issue(msg);
        }
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
        // Made for the scan threads: one counter shard each.
        let live = Arc::new(scan_pool().install(|| LiveTree::new(self.cats.clone())));
        self.scan_cats = Some(self.cats.clone());
        self.live_tree = Some(live.clone());
        self.live_read_at = None;
        self.live_read_for = None;
        std::thread::spawn(move || {
            let start = Instant::now();
            match std::fs::metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let _ = tx.send(ScanMsg::Error(trf(
                        "ERR_PATH_NOT_FOUND",
                        &[&show_path(&path)],
                    )));
                    return;
                }
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
            let root = scan_pool().install(|| scan_dir(&path, &ctx));
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
        if files != self.measure.by_files() {
            self.measure = Measure::of_setting(files);
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
        release_tree(root);
        if let Some(base) = self.cat_base.take() {
            release_tree(base);
        }
        self.finish_sorting();
        if let Some(full) = &mut self.full_root {
            scan::sort_tree_by_measure(Arc::make_mut(full), files);
        }
        self.tree_by_files = files;
        self.rebuild_view_tree();
        self.restore_view(&view_paths);
    }

    /// Rebuilds the displayed tree from the last scan, the filter and the
    /// picked category, keeping each view in the zoom history on the same
    /// folder (or its nearest remaining parent). For when the tree, the
    /// filter or the measure changed; `repick` when only the pick did.
    fn rebuild_view_tree(&mut self) {
        self.tree_gen += 1;
        let Some(full) = self.full_root.clone() else {
            return;
        };
        let base = match &self.filter {
            Some(f) => {
                Arc::new(filter_tree(&full, f, self.measure).unwrap_or_else(|| empty_like(&full)))
            }
            None => full,
        };
        if let Some(old) = self.cat_base.replace(base) {
            release_tree(old);
        }
        self.base_gen += 1;
        self.show_picked();
    }

    /// The picked category changed: the displayed tree is made again from
    /// the filtered one, which stays (as does its category breakdown).
    fn repick(&mut self) {
        if self.cat_base.is_none() {
            return self.rebuild_view_tree();
        }
        self.tree_gen += 1;
        self.show_picked();
    }

    /// Shows `cat_base` with the picked category applied, keeping each view
    /// in the zoom history on the same folder (or its nearest remaining
    /// parent).
    fn show_picked(&mut self) {
        let Some(base) = self.cat_base.clone() else {
            return;
        };
        let new_root = match &self.pick {
            Some(pick) => {
                let cats = self.cats.clone();
                // A picked category: the one each file was given when it was
                // scanned, if that was with these categories (much faster
                // than reading every name again).
                let stored = self
                    .tree_cats
                    .as_ref()
                    .is_some_and(|c| Arc::ptr_eq(c, &cats));
                let kept = match pick {
                    Pick::Category(c) if stored && cat_byte(*c) != NO_CAT => {
                        let byte = cat_byte(*c);
                        filter_tree_by(&base, &|n: &Node| n.cat == byte, self.measure)
                    }
                    _ => filter_tree_by(
                        &base,
                        &|n: &Node| cats.pick_matches(pick, &n.name),
                        self.measure,
                    ),
                };
                Arc::new(kept.unwrap_or_else(|| empty_like(&base)))
            }
            None => base,
        };
        if let Some(old) = &self.root {
            self.view_stack = self
                .view_stack
                .iter()
                .map(|vp| remap_index_path(old, &new_root, vp))
                .collect();
            self.view_stack.dedup();
        }
        if let Some(old) = self.root.replace(new_root) {
            release_tree(old);
        }
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

    /// Work that quitting would cut short: a copy or move, or emptying the
    /// trash. (A scan just stops; deletes finish within a frame.)
    fn work_running(&self) -> bool {
        self.transferring() || self.emptying_trash()
    }

    /// Closing the window (✕ or Ctrl+Q) while work runs asks first; quitting
    /// anyway stops a copy or move cleanly, without leaving a half-written
    /// file.
    fn guard_quit(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && self.work_running()
            && !self.quit_confirmed
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit_asked = true;
        }
        if !self.quit_asked {
            return;
        }
        let mut quit = false;
        let modal = egui::Modal::new("quit_while_working".into()).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading(tr("QUIT_TITLE"));
            ui.add(egui::Label::new(tr("QUIT_BODY")).wrap());
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let keep = ui.button(tr("QUIT_KEEP"));
                if ui.button(tr("QUIT_ANYWAY")).clicked() {
                    quit = true;
                }
                keep.clicked()
            })
            .inner
        });
        if quit {
            self.quit_asked = false;
            self.quit_confirmed = true;
            self.stop_transfer(std::time::Duration::from_secs(5));
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if modal.inner || modal.should_close() {
            self.quit_asked = false;
        }
    }

    /// Esc cancels the scan, unless it's closing a menu or window first.
    fn esc_cancels_scan(&mut self, ctx: &egui::Context) {
        let closing_something = self.show_about
            || self.table.show_help
            || self.quit_asked
            || egui::Popup::is_any_open(ctx);
        if self.scanning && !closing_something && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.abort_scan();
        }
    }

    /// Esc while scanning: stops the scan and goes back to the previous
    /// result, if any.
    fn abort_scan(&mut self) {
        if let Some(cancel) = &self.cancel_flag {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.cancel_graft();
        self.scan_ended();
        self.status = tr("STATUS_SCAN_ABORTED");
    }

    /// A scan finished, failed or was cancelled: it's no longer running, its
    /// messages are dropped, and the live tree is freed (its memory
    /// returned).
    fn scan_ended(&mut self) {
        self.scan_ended_freeing(());
    }

    /// `scan_ended`, also freeing `retired` (the tree the scan replaced) with
    /// the live tree, before the memory is returned to the system.
    fn scan_ended_freeing<T: Send + 'static>(&mut self, retired: T) {
        self.scanning = false;
        self.scan_rx = None;
        let partial = std::mem::replace(&mut self.partial_root, empty_node());
        free_in_background((partial, self.live_tree.take(), retired));
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
        // A time budget keeps the window responsive during bursts of messages
        // (well within a frame at 60 per second); the rest wait for the next
        // frame.
        const FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(10);
        // Taken while messages are handled, and put back unless the scan ended.
        let Some(rx) = self.scan_rx.take() else {
            return false;
        };
        let drain_start = Instant::now();
        let mut processed = 0u32;
        loop {
            if processed.is_multiple_of(64)
                && processed > 0
                && drain_start.elapsed() >= FRAME_BUDGET
            {
                self.scan_rx = Some(rx);
                return true;
            }
            processed += 1;
            match rx.try_recv() {
                Ok(ScanMsg::Unreadable(p)) => self.unreadable.push(p),
                Ok(ScanMsg::LogError(msg)) => self.log_issue(msg),
                Ok(ScanMsg::SliceDone { exts }) => {
                    // Per-extension totals for the category bar; sizes come
                    // from the live tree.
                    for (ext, size, files) in exts {
                        add_ext(&mut self.live_exts, ext, size, files);
                    }
                }
                Ok(ScanMsg::Done(node, secs, counted_at)) => {
                    self.scan_done(node, secs, counted_at);
                    return false;
                }
                Ok(ScanMsg::Error(e)) => {
                    self.cancel_graft();
                    self.scan_ended();
                    self.status = e;
                    return false;
                }
                Err(TryRecvError::Empty) => {
                    self.scan_rx = Some(rx);
                    return false;
                }
                Err(TryRecvError::Disconnected) => {
                    self.cancel_graft();
                    self.scan_ended();
                    return false;
                }
            }
        }
    }

    /// The scan finished with the tree `node`, after `secs` seconds;
    /// `counted_at` is where each file with several hard links was counted.
    fn scan_done(&mut self, mut node: Node, secs: f64, counted_at: Vec<((u64, u64), PathBuf)>) {
        // Freed with the tree it replaces, when the scan has ended.
        let live = self.live_tree.take();
        let mut retired = None;
        self.colored_at = Some(Instant::now());
        let scan_cats = self.scan_cats.take();
        match &self.graft {
            Some(g) => {
                let target = g.target.clone();
                self.link_owners.retain(|_, at| !at.starts_with(&target));
                // A folder classified with other categories than the rest
                // leaves the tree mixed.
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
                // Scans sort by bytes; shown sorted by files at once when
                // that's the measure.
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
            retired = self.full_root.replace(Arc::new(node));
            self.rebuild_view_tree();
        }
        self.status = self
            .status_after_rescan
            .take()
            .unwrap_or_else(|| trf("STATUS_SCAN_COMPLETED", &[&format!("{secs:.1}")]));
        self.scan_ended_freeing((live, retired));
    }
}

/// An empty folder in `n`'s place: what a filter or pick leaves when
/// nothing in `n` matches.
fn empty_like(n: &Node) -> Node {
    let mut e = empty_node();
    e.name = n.name.clone();
    e.copy_place(n);
    e
}

impl eframe::App for DiskScanApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame_ui(ui);
    }

    fn on_exit(&mut self) {
        self.save_config_now();
    }
}

impl DiskScanApp {
    /// One frame of the whole app.
    fn frame_ui(&mut self, ui: &mut egui::Ui) {
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
        // as the chart currently shows, and at once when the depth, the
        // smallest slice, the picked category or the measure changes.
        // (A picked extension applies when the scan finishes.)
        // The smallest slice the chart can draw, as a share of the circle.
        let min_angle = if self.settings.unlimited_slices {
            0.02
        } else {
            self.settings.min_segment_angle_deg
        };
        let live_pick = match self.pick {
            Some(Pick::Category(c)) => Some(c),
            _ => None,
        };
        let live_for = (
            self.settings.max_render_depth,
            min_angle.to_bits(),
            live_pick,
            self.measure,
        );
        if self.scanning
            && let Some(tree) = self.live_tree.clone()
            && (self.live_read_for != Some(live_for)
                || self.live_read_at.is_none_or(|t| Instant::now() >= t))
        {
            let started = Instant::now();
            // The scanned folder's total is in what's picked.
            if self
                .live_read_for
                .is_some_and(|(_, _, pick, _)| pick != live_pick)
            {
                self.live_looks.reset_total();
            }
            self.live_read_for = Some(live_for);
            let min_share = f64::from(min_angle) / 360.0;
            if let Some(root) = tree.snapshot(
                self.settings.max_render_depth,
                min_share,
                live_pick,
                self.measure,
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
        // While scanning, redraw 10 times a second, as often as the live tree
        // is read; 4 times with a screen reader, whose updates take CPU from
        // the scan. At once while messages are waiting.
        if scan_backlog || !self.mime_inflight.is_empty() {
            ctx.request_repaint();
        } else if self.scanning {
            let screen_reader = ctx
                .accesskit_node_builder(egui::accesskit_root_id(), |_| ())
                .is_some();
            let every = if screen_reader { 250 } else { 100 };
            ctx.request_repaint_after(std::time::Duration::from_millis(every));
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
        self.save_config_if_changed(&ctx);
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
        self.guard_quit(&ctx);
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
            && !self.quit_asked
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
    // The user's language, for the command line's messages too.
    lang::set_language(&config::language_setting());
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
                ..DiskScanApp::with_user_files()
            };
            for n in &not_copied {
                app.log_issue(n.message());
            }
            app.startup_issues = app.log.clone();
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod pick_tests {
    use super::*;

    /// The files left by picking each category: the same whether the
    /// categories the scan stored are used or every name is read again; and
    /// the stored ones are what's used when the scan had these categories.
    #[test]
    fn stored_categories_pick_the_same_files() {
        let names = [
            "/t/a/x.mkv",
            "/t/a/y.txt",
            "/t/a/b/z.JPG",
            "/t/q.mp3",
            "/t/r.rs",
            "/t/none",
            "/t/c/d/e.eml",
            "/t/c/f.zip",
        ];
        let tree = |cats: &CategoryModel, misfiled: bool| {
            let file = |p: &str| {
                let mut n = test_node(p, 10, false, vec![]);
                n.cat = cat_byte(cats.of_name(&n.name));
                // Stored as something its name doesn't say.
                if misfiled && p.ends_with("y.txt") {
                    n.cat = cat_byte(cats.of_name("v.mkv"));
                }
                n
            };
            let d = test_node("/t/c/d", 10, true, vec![file(names[6])]);
            let c = test_node("/t/c", 20, true, vec![d, file(names[7])]);
            let b = test_node("/t/a/b", 10, true, vec![file(names[2])]);
            let a = test_node("/t/a", 30, true, vec![file(names[0]), file(names[1]), b]);
            let rest = names[3..6].iter().map(|p| file(p));
            test_node("/t", 80, true, [a, c].into_iter().chain(rest).collect())
        };
        let files = |app: &DiskScanApp| {
            fn walk(n: &Node, out: &mut Vec<PathBuf>) {
                for c in n.children.iter() {
                    if c.is_dir {
                        walk(c, out);
                    } else {
                        out.push(c.path());
                    }
                }
            }
            let mut out = Vec::new();
            walk(app.root.as_ref().unwrap(), &mut out);
            out.sort();
            out
        };
        let picked = |misfiled: bool, stored: bool, c: Category| {
            let mut app = DiskScanApp::default();
            app.full_root = Some(Arc::new(tree(&app.cats, misfiled)));
            app.tree_cats = stored.then(|| app.cats.clone());
            app.pick = Some(Pick::Category(c));
            app.rebuild_view_tree();
            files(&app)
        };
        let cats = CategoryModel::defaults();
        for c in 0..=cats.other().0 {
            assert_eq!(
                picked(false, true, Category(c)),
                picked(false, false, Category(c))
            );
        }
        let video = cats.of_name("v.mkv");
        assert_eq!(
            picked(true, true, video),
            [PathBuf::from("/t/a/x.mkv"), PathBuf::from("/t/a/y.txt")]
        );
        assert_eq!(picked(true, false, video), [PathBuf::from("/t/a/x.mkv")]);
    }

    /// Changing only the pick keeps the filtered tree and its category
    /// breakdown, and shows what a full rebuild would.
    #[test]
    fn a_new_pick_keeps_the_filtered_tree() {
        let file = |p: &str| test_node(p, 10, false, vec![]);
        let tree = || {
            let a = test_node(
                "/t/a",
                30,
                true,
                vec![file("/t/a/ax.mkv"), file("/t/a/ay.txt"), file("/t/a/b.mkv")],
            );
            test_node("/t", 50, true, vec![a, file("/t/a1.txt"), file("/t/c.mkv")])
        };
        let shown = |app: &DiskScanApp| {
            fn walk(n: &Node, out: &mut Vec<PathBuf>) {
                out.push(n.path());
                for c in n.children.iter() {
                    walk(c, out);
                }
            }
            let mut out = Vec::new();
            walk(app.root.as_ref().unwrap(), &mut out);
            out.sort();
            out
        };
        let mut app = DiskScanApp {
            full_root: Some(Arc::new(tree())),
            ..DiskScanApp::default()
        };
        app.filter_form.name = "a*".into();
        app.apply_filter_form();
        let root = app.root.clone().unwrap();
        app.refresh_cat_breakdown(&root);
        let (base, made_for) = (app.cat_base.clone().unwrap(), app.cat_breakdown_for.clone());
        for pick in [
            Some(app.cats.of_name("x.mkv")),
            Some(app.cats.of_name("x.txt")),
            None,
        ] {
            app.pick = pick.map(Pick::Category);
            app.repick();
            assert!(
                Arc::ptr_eq(&base, app.cat_base.as_ref().unwrap()),
                "base kept"
            );
            let root = app.root.clone().unwrap();
            app.refresh_cat_breakdown(&root);
            assert_eq!(app.cat_breakdown_for, made_for, "breakdown kept");

            let mut fresh = DiskScanApp {
                full_root: Some(Arc::new(tree())),
                ..DiskScanApp::default()
            };
            fresh.filter_form.name = "a*".into();
            fresh.pick = app.pick.clone();
            fresh.apply_filter_form();
            assert_eq!(shown(&app), shown(&fresh));
        }
    }
}

/// Frame times of the whole app on a big tree while the user interacts:
/// hovering, sliders, zoom, filters, picks, the table and a live scan. Run
/// with `cargo test --release frame_bench -- --ignored --nocapture`;
/// $SPACESCAN_FILES sets the tree's size (default 500,000 files), and
/// $SPACESCAN_BENCH a folder to scan live meanwhile.
#[cfg(test)]
mod frame_bench {
    use super::*;
    use std::time::Duration;

    /// A tree like a big home folder: ten top folders, each with folders of
    /// folders of 50 files of mixed kinds, sizes and ages, `files` in all,
    /// sorted the way a scan sorts them.
    fn synthetic_tree(files: usize, cats: &CategoryModel) -> Node {
        const EXTS: [&str; 10] = [
            "mkv", "jpg", "txt", "rs", "zip", "mp3", "pdf", "eml", "bin", "json",
        ];
        const PER_FOLDER: usize = 50;
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let now = now_secs();
        let folder = |path: &str, mut children: Vec<Node>| {
            scan::sort_largest_first(&mut children);
            let mut n = test_node(path, 0, true, Vec::new());
            n.size = children.iter().map(|c| c.size).sum();
            n.file_count = children.iter().map(|c| c.file_count).sum();
            n.children = children.into();
            (n.mode, n.mtime, n.ctime) = (0o40755, now, now);
            n
        };
        let leaves = files.div_ceil(PER_FOLDER);
        let subs = leaves.div_ceil(10 * 10).max(1);
        let mut tops = Vec::new();
        for t in 0..10 {
            let mut mids = Vec::new();
            for m in 0..10 {
                let mut leaf_folders = Vec::new();
                for s in 0..subs {
                    let mut kids = Vec::new();
                    for f in 0..PER_FOLDER {
                        let r = random();
                        let ext = EXTS[(r % 10) as usize];
                        let path = format!("/bench/t{t}/m{m}/s{s}/f{f}.{ext}");
                        let mut n = test_node(&path, (r >> 8) % (1 << 24), false, vec![]);
                        // Classified as a scan does.
                        n.cat = cat_byte(cats.of_name(&n.name));
                        n.ctime = now - ((r >> 40) % (3 * 365 * 86_400)) as i64;
                        (n.mtime, n.mode) = (n.ctime, 0o100644);
                        kids.push(n);
                    }
                    leaf_folders.push(folder(&format!("/bench/t{t}/m{m}/s{s}"), kids));
                }
                mids.push(folder(&format!("/bench/t{t}/m{m}"), leaf_folders));
            }
            tops.push(folder(&format!("/bench/t{t}"), mids));
        }
        folder("/bench", tops)
    }

    /// Frame times: the app's work, and turning its shapes into triangles.
    struct Times {
        run: Vec<Duration>,
        tessellate: Vec<Duration>,
    }

    /// Draws `warm` frames, then `n` timed ones, of the whole app in a
    /// 1600 × 1000 window, after `before` has changed the app (or given
    /// input events) for each.
    fn frames(
        app: &mut DiskScanApp,
        ctx: &egui::Context,
        warm: usize,
        n: usize,
        mut before: impl FnMut(&mut DiskScanApp, usize) -> Vec<egui::Event>,
    ) -> Times {
        let mut times = Times {
            run: Vec::new(),
            tessellate: Vec::new(),
        };
        for i in 0..warm + n {
            let events = before(app, i);
            // Nothing is ever saved: the settings count as saved as they are.
            app.saved_config = Some(app.current_config());
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(1600.0, 1000.0),
                )),
                events,
                ..Default::default()
            };
            let t = Instant::now();
            let out = headless_frame(ctx, input, |ui| app.frame_ui(ui));
            let run = t.elapsed();
            let t = Instant::now();
            std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
            // The first frames settle the layout; they aren't counted.
            if i >= warm {
                times.run.push(run);
                times.tessellate.push(t.elapsed());
            }
        }
        times
    }

    fn report(name: &str, t: &Times) {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let stats = |v: &[Duration]| {
            let mut v = v.to_vec();
            v.sort();
            let at = |q: f64| ms(v[((v.len() - 1) as f64 * q) as usize]);
            (at(0.5), at(0.95), ms(*v.last().unwrap()))
        };
        let total: Vec<Duration> = t
            .run
            .iter()
            .zip(&t.tessellate)
            .map(|(a, b)| *a + *b)
            .collect();
        let (med, p95, max) = stats(&total);
        let (tess, _, _) = stats(&t.tessellate);
        eprintln!(
            "{name:<34} median {med:>7.1} ms   p95 {p95:>7.1} ms   max {max:>7.1} ms   (tessellate {tess:>5.1} ms)"
        );
    }

    #[test]
    #[ignore]
    fn frame_bench() {
        let files = std::env::var("SPACESCAN_FILES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500_000);
        let t = Instant::now();
        let mut app = DiskScanApp::default();
        let tree = synthetic_tree(files, &app.cats);
        eprintln!(
            "tree: {} files in {} top folders, made in {:?}",
            tree.file_count,
            tree.children.len(),
            t.elapsed()
        );
        let ctx = egui::Context::default();
        // As after a scan with the categories in use.
        app.tree_cats = Some(app.cats.clone());
        app.full_root = Some(Arc::new(tree));
        app.rebuild_view_tree();
        let cats = app.cats.clone();
        let center = Pos2::new(800.0, 520.0);
        let circle = |i: usize| {
            let a = i as f32 * 0.21;
            vec![egui::Event::PointerMoved(
                center + Vec2::new(a.cos(), a.sin()) * (120.0 + (i % 7) as f32 * 40.0),
            )]
        };
        let n = 40;

        report("chart, still", &frames(&mut app, &ctx, 3, n, |_, _| vec![]));
        report(
            "chart, hover",
            &frames(&mut app, &ctx, 3, n, |_, i| circle(i)),
        );
        report(
            "chart, depth slider",
            &frames(&mut app, &ctx, 3, n, |a, i| {
                a.settings.max_render_depth = 3 + i % 10;
                vec![]
            }),
        );
        app.settings.max_render_depth = Settings::default().max_render_depth;
        report(
            "chart, smallest slice slider",
            &frames(&mut app, &ctx, 3, n, |a, i| {
                a.settings.min_segment_angle_deg = 0.1 + (i % 40) as f32 * 0.1;
                vec![]
            }),
        );
        app.settings.min_segment_angle_deg = Settings::default().min_segment_angle_deg;
        report(
            "chart, zoom and pan",
            &frames(&mut app, &ctx, 3, n, |a, i| {
                a.chart_scale = 1.0 + (i % 30) as f32 * 0.1;
                a.chart_offset = Vec2::new((i % 9) as f32 * 20.0, 0.0);
                vec![]
            }),
        );
        (app.chart_scale, app.chart_offset) = (1.0, Vec2::ZERO);
        report(
            "chart, filter applied",
            &frames(&mut app, &ctx, 3, n, |a, i| {
                a.filter_form.name = ["*.mkv", "*.txt", "f1*"][i % 3].to_string();
                a.apply_filter_form();
                vec![]
            }),
        );
        app.filter_form = FilterForm::default();
        app.apply_filter_form();
        report(
            "chart, category picked",
            &frames(&mut app, &ctx, 3, n, |a, i| {
                a.pick = Some(Pick::Category(Category(i % (cats.other().0 + 1))));
                a.repick();
                vec![]
            }),
        );
        app.pick = None;
        app.repick();

        app.summary_view = true;
        report("table, still", &frames(&mut app, &ctx, 3, n, |_, _| vec![]));
        let key = |k: egui::Key| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        report(
            "table, cursor moving",
            &frames(&mut app, &ctx, 3, n, |_, _| vec![key(egui::Key::ArrowDown)]),
        );
        app.table.flat = true;
        for all in [false, true] {
            app.settings.flat_all = all;
            let rows = if all { "all files" } else { "first 1,000" };
            report(
                &format!("table, flat list ({rows})"),
                &frames(&mut app, &ctx, 3, n, |_, _| vec![]),
            );
            report(
                &format!("table, flat list ({rows}) re-sorted"),
                &frames(&mut app, &ctx, 3, n, |a, i| {
                    a.contents_sort.column = [SortColumn::Size, SortColumn::Name][i % 2];
                    vec![]
                }),
            );
        }
        // How long a re-sorted list of every file takes to show, sorted on
        // another thread, with frames at 60 a second meanwhile.
        app.settings.flat_all = true;
        for column in [SortColumn::Name, SortColumn::Size] {
            app.contents_sort.column = column;
            let started = Instant::now();
            let mut slowest = Duration::ZERO;
            let mut i = 0;
            loop {
                let frame_start = Instant::now();
                let t = frames(&mut app, &ctx, 0, 1, |_, _| vec![]);
                slowest = slowest.max(t.run[0] + t.tessellate[0]);
                i += 1;
                if !app.table.sorting() || i > 10_000 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(16).saturating_sub(frame_start.elapsed()));
            }
            eprintln!(
                "table, all files re-sorted by {column:?}: shown after {:.0} ms, {i} frames, slowest {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0,
                slowest.as_secs_f64() * 1000.0
            );
        }
        app.table.flat = false;
        app.settings.flat_all = false;
        app.contents_sort.column = SortColumn::Size;

        // A live scan of a real folder, frames paced at 60 a second like
        // the window's while the pointer moves: those that read the live
        // tree (every 100 ms) are reported apart.
        if let Some(dir) = std::env::var_os("SPACESCAN_BENCH") {
            for summary in [false, true] {
                app.summary_view = summary;
                app.start_scan(PathBuf::from(&dir));
                let blank = || Times {
                    run: Vec::new(),
                    tessellate: Vec::new(),
                };
                let (mut reads, mut others) = (blank(), blank());
                let started = Instant::now();
                let mut i = 0;
                while app.scanning && started.elapsed() < Duration::from_secs(300) {
                    let frame_start = Instant::now();
                    let read_before = app.partial_gen;
                    let t = frames(&mut app, &ctx, 0, 1, |_, j| circle(i + j));
                    let to = if app.partial_gen != read_before {
                        &mut reads
                    } else {
                        &mut others
                    };
                    to.run.extend(t.run);
                    to.tessellate.extend(t.tessellate);
                    i += 1;
                    let left = Duration::from_millis(16).saturating_sub(frame_start.elapsed());
                    std::thread::sleep(left);
                }
                let view = if summary { "table" } else { "chart" };
                let secs = started.elapsed().as_secs_f64();
                eprintln!("live scan, {view}: {secs:.1} s, {i} frames");
                if !reads.run.is_empty() {
                    report(
                        &format!("  frames reading the live tree ({})", reads.run.len()),
                        &reads,
                    );
                }
                if !others.run.is_empty() {
                    report(&format!("  other frames ({})", others.run.len()), &others);
                }
            }
        }
    }
}

/// Resident memory across rescans ("r") of the scanned folder, the way the
/// app does them (SM-19). Run with `SPACESCAN_MEM_TREE=<folder> cargo test
/// --release app_rescan_memory -- --ignored --nocapture`.
#[cfg(test)]
mod rescan_memory {
    use super::*;

    fn rss_mb() -> u64 {
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
        let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
        pages * 4096 / (1 << 20)
    }

    #[test]
    #[ignore]
    fn app_rescan_memory() {
        let dir = std::env::var_os("SPACESCAN_MEM_TREE").unwrap_or_else(|| "/usr".into());
        let mut app = DiskScanApp::default();
        // Until the scan has ended and the app is idle (freeing done).
        let wait = |app: &mut DiskScanApp| {
            while app.scanning {
                app.poll_scan();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        };
        eprintln!("start: {} MB", rss_mb());
        app.start_scan(PathBuf::from(&dir));
        wait(&mut app);
        let first = rss_mb();
        let files = app.full_root.as_ref().map_or(0, |r| r.file_count);
        eprintln!("scan: {first} MB ({files} files)");
        for i in 1..=6 {
            app.rescan_current();
            wait(&mut app);
            let now = rss_mb();
            eprintln!(
                "rescan {i}: {now} MB ({:+.0}%)",
                (now as f64 / first as f64 - 1.0) * 100.0
            );
        }
    }
}
