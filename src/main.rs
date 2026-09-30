//! spacemap: a disk usage explorer for Linux, with a sunburst chart and an
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
mod config;
mod delete;
mod filter;
mod lang;
mod panels;
mod scan;
mod table;
mod theme;
mod widgets;
use category::*;
use chart::*;
use config::Config;
use filter::*;
use lang::*;
use scan::*;
use table::{Graft, TableState};
use theme::*;
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
    ring_sat: f32,
    ring_val_base: f32,
    ring_val_falloff: f32,
    ring_val_floor: f32,
    other_sat: f32,
    other_val: f32,
    free_space_gamma: f32,
    stroke_width: f32,
    stroke_alpha: u8,
    tess_px_per_step: f32,
    max_log_lines: usize,
    /// The "N items scanned" counter updates every 2^this entries.
    progress_interval_pow2: u32,
    /// Count file lengths instead of disk space used (from the next scan).
    apparent_size: bool,
}

/// Allowed ranges, for the settings sliders and for values read from the
/// settings file.
impl Settings {
    const DEPTH: RangeInclusive<usize> = 1..=12;
    const MIN_ANGLE: RangeInclusive<f32> = 0.1..=5.0;
    const MAX_CHILDREN: RangeInclusive<usize> = 4..=360;
    const HUB: RangeInclusive<f32> = 0.05..=0.5;
    const RING_SAT: RangeInclusive<f32> = 0.0..=1.0;
    const RING_VAL_BASE: RangeInclusive<f32> = 0.3..=1.0;
    const RING_VAL_FALLOFF: RangeInclusive<f32> = 0.0..=0.3;
    const RING_VAL_FLOOR: RangeInclusive<f32> = 0.1..=0.9;
    const OTHER_SAT: RangeInclusive<f32> = 0.0..=1.0;
    const OTHER_VAL: RangeInclusive<f32> = 0.3..=1.0;
    const FREE_GAMMA: RangeInclusive<f32> = 0.2..=1.5;
    const STROKE_WIDTH: RangeInclusive<f32> = 0.0..=3.0;
    const TESS: RangeInclusive<f32> = 1.0..=10.0;
    const LOG_LINES: RangeInclusive<usize> = 50..=5000;
    const PROGRESS_POW2: RangeInclusive<u32> = 0..=16;

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
        self.ring_sat = self
            .ring_sat
            .clamp(*Self::RING_SAT.start(), *Self::RING_SAT.end());
        self.ring_val_base = self
            .ring_val_base
            .clamp(*Self::RING_VAL_BASE.start(), *Self::RING_VAL_BASE.end());
        self.ring_val_falloff = self.ring_val_falloff.clamp(
            *Self::RING_VAL_FALLOFF.start(),
            *Self::RING_VAL_FALLOFF.end(),
        );
        self.ring_val_floor = self
            .ring_val_floor
            .clamp(*Self::RING_VAL_FLOOR.start(), *Self::RING_VAL_FLOOR.end());
        self.other_sat = self
            .other_sat
            .clamp(*Self::OTHER_SAT.start(), *Self::OTHER_SAT.end());
        self.other_val = self
            .other_val
            .clamp(*Self::OTHER_VAL.start(), *Self::OTHER_VAL.end());
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
        self.progress_interval_pow2 = self
            .progress_interval_pow2
            .clamp(*Self::PROGRESS_POW2.start(), *Self::PROGRESS_POW2.end());
        self
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
            ring_sat: 0.55,
            ring_val_base: 0.95,
            ring_val_falloff: 0.08,
            ring_val_floor: 0.45,
            other_sat: 0.38,
            other_val: 0.80,
            free_space_gamma: 0.7,
            stroke_width: 1.0,
            stroke_alpha: 90,
            tess_px_per_step: 3.0,
            max_log_lines: 500,
            progress_interval_pow2: 9, // every 512 entries
            apparent_size: false,
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
    /// A click in the category bar, applied once the frame's table is drawn
    /// (the table is drawn from the tree as it was when the frame began).
    category_pending: Option<Option<Category>>,
    /// Extension totals of the files found so far in the running scan, so
    /// the category bar grows live like the table.
    live_exts: ExtTotals,
    /// Category picked in the summary view's category bar (None = all).
    /// Applied on top of `filter`.
    category: Option<Category>,
    /// The tree with `filter` applied but not `category`: what the category
    /// bar breaks down, so every category stays visible and clickable.
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
    scanned_count: u64,
    /// Counting pass for the current scan (folder scans only).
    entry_count: Option<Arc<EntryCount>>,
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
    /// Window width added for open side panels (see panels.rs).
    window_grown: panels::WindowGrown,
    /// Folders the last scan couldn't list (size unknown), for the delete
    /// dialog's warning.
    unreadable: Vec<PathBuf>,
    /// Set by the scanner on finding a Korean name; `korean_font` once the
    /// font for it is loaded.
    saw_hangul: Arc<std::sync::atomic::AtomicBool>,
    korean_font: bool,
    /// Pending deletes and their confirmation (see delete.rs).
    removal: delete::Removal,
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
    path_input: String,
    path_input_focused: bool,
    contents_sort: SortState,
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
            scanned_count: 0,
            entry_count: None,
            progress_shown: 0.0,
            scan_start: Instant::now(),
            hidden: HashSet::new(),
            hovered: None,
            context_target: None,
            removal: Default::default(),
            saw_hangul: Default::default(),
            korean_font: false,
            unreadable: Vec::new(),
            window_grown: Default::default(),
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
            status: String::new(),
            free_space: None,
            log: Vec::new(),
            log_truncated: 0,
            cancel_flag: None,
            partial_root: empty_node(),
            settings: Settings::default(),
            saved_config: None,
            show_settings: false,
            path_input: String::new(),
            path_input_focused: false,
            cats: Arc::new(CategoryModel::defaults()),
            category: None,
            category_pending: None,
            live_exts: ExtTotals::new(),
            cat_base: None,
            cat_breakdown: Vec::new(),
            cat_breakdown_for: None,
            // Largest first, like the chart.
            contents_sort: SortState {
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
            self.category = None;
            self.cat_breakdown_for = None;
        }
        if let Some(p) = problem {
            self.log_issue(p);
        }
    }

    fn start_scan(&mut self, path: PathBuf) {
        // The canonical path, so no "..", "./" or doubled slashes show up.
        let path = true_case(&std::fs::canonicalize(&path).unwrap_or(path));
        // Any new scan supersedes a pending folder rescan ("r").
        self.graft = None;
        self.scanning = true;
        // A new live table.
        self.live_gen += 1;
        self.live_view = empty_node();
        self.live_exts.clear();
        self.cat_breakdown.clear();
        self.cat_breakdown_for = None;
        self.status = tr("STATUS_SCANNING");
        // This scan reports again whatever it can't read under `path`.
        self.unreadable.retain(|u| !u.starts_with(&path));
        self.scan_start = Instant::now();
        self.scanned_count = 0;
        self.selection = None;
        self.progress_shown = 0.0;
        self.view_stack = vec![vec![]];
        self.hidden.clear();
        self.log.clear();
        self.log_truncated = 0;
        // After clearing the log, so problems in the file stay listed.
        self.reload_categories();
        self.partial_root = empty_node();
        self.partial_root.path = path.clone();
        self.partial_root.name = file_name_of(&path);
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

        let progress_interval: u64 = 1u64 << self.settings.progress_interval_pow2;
        let apparent_size = self.settings.apparent_size;
        let saw_hangul = self.saw_hangul.clone();

        let (tx, rx) = channel();
        self.scan_rx = Some(rx);
        // A whole drive's progress is measured against its used space; other
        // folders need a counted total.
        let entry_count = self
            .free_space
            .is_none()
            .then(|| Arc::new(EntryCount::default()));
        self.entry_count = entry_count.clone();
        std::thread::spawn(move || {
            let counter = std::sync::atomic::AtomicU64::new(0);
            let start = Instant::now();
            if !path.exists() {
                let _ = tx.send(ScanMsg::Error(trf(
                    "ERR_PATH_NOT_FOUND",
                    &[&show_path(&path)],
                )));
                return;
            }
            let root_dev = match std::fs::metadata(&path) {
                Ok(m) => {
                    use std::os::unix::fs::MetadataExt;
                    m.dev()
                }
                Err(e) => {
                    let _ = tx.send(ScanMsg::Error(trf(
                        "ERR_CANNOT_STAT",
                        &[&show_path(&path), &e.to_string()],
                    )));
                    return;
                }
            };
            let scan_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
            if let Some(count) = entry_count {
                let (path, cancel, scan_finished) =
                    (path.clone(), cancel.clone(), scan_finished.clone());
                std::thread::spawn(move || {
                    use std::sync::atomic::Ordering;
                    let stop =
                        || cancel.load(Ordering::Relaxed) || scan_finished.load(Ordering::Relaxed);
                    count_entries(&path, root_dev, &count.found, &stop);
                    if !stop() {
                        count.done.store(true, Ordering::Relaxed);
                    }
                });
            }
            let ctx = ScanCtx {
                root_dev,
                progress: &tx,
                counter: &counter,
                cancel: &cancel,
                progress_interval,
                apparent_size,
                hard_links: Default::default(),
                saw_hangul: &saw_hangul,
            };
            let root = scan_dir(&path, &ctx);
            scan_finished.store(true, std::sync::atomic::Ordering::Relaxed);
            if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tx.send(ScanMsg::Done(root, start.elapsed().as_secs_f64()));
            }
        });
    }

    /// Rebuilds the displayed tree from the last scan, the filter and the
    /// picked category, keeping each view in the zoom history on the same
    /// folder (or its nearest remaining parent).
    fn rebuild_view_tree(&mut self) {
        self.tree_gen += 1;
        let Some(full) = self.full_root.clone() else {
            return;
        };
        let empty = |full: &Node| Node {
            name: full.name.clone(),
            path: full.path.clone(),
            size: 0,
            file_count: 0,
            children: Vec::new(),
            ..empty_node()
        };
        let base = match &self.filter {
            Some(f) => Arc::new(filter_tree(&full, f).unwrap_or_else(|| empty(&full))),
            None => full.clone(),
        };
        let new_root = match self.category {
            Some(cat) => {
                let cats = self.cats.clone();
                Arc::new(
                    filter_tree_by(&base, &|n: &Node| cats.of_name(&n.name) == cat)
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
        try_get_node(self.current_view_node(root), rel).map(|n| n.path.clone())
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
                    Ok(ScanMsg::Progress(n)) => {
                        self.scanned_count = n;
                    }
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
                    Ok(ScanMsg::SliceDone {
                        path,
                        size,
                        file_count,
                        mode,
                        mtime,
                        ctime,
                        uid,
                        gid,
                        exts,
                    }) => {
                        graft_slice(
                            &mut self.partial_root,
                            &path,
                            size,
                            file_count,
                            mode,
                            mtime,
                            ctime,
                            uid,
                            gid,
                        );
                        for (ext, size, files) in exts {
                            add_ext(&mut self.live_exts, ext, size, files);
                        }
                        self.partial_gen += 1;
                    }
                    Ok(ScanMsg::Done(node, secs)) => {
                        if self.graft.is_some() {
                            self.finish_graft(node);
                        } else {
                            self.full_root = Some(Arc::new(node));
                            self.rebuild_view_tree();
                        }
                        self.scanning = false;
                        self.status = trf("STATUS_SCAN_COMPLETED", &[&format!("{:.1}", secs)]);
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
        self.typing = ctx.text_edit_focused();
        let scan_backlog = self.poll_scan();
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
        if self.scanning && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.abort_scan();
        }
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
                    ui.strong(short_path(&n.path));
                    folder_stats_ui(ui, n);
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
        // Not while the right-click menu is open (Esc still closes it).
        let menu_open = egui::Popup::is_any_open(&ctx);
        if self.root.is_some()
            && !self.scanning
            && !self.summary_view
            && !self.typing
            && !self.delete_dialog_open()
            && !menu_open
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

/// Scanning or deleting folder chains past the kernel's path length limit
/// keeps one folder open per level; the usual soft limit of 1,024 open
/// files would cut very deep chains short. Raise it to the hard limit (as
/// file managers and `find` effectively allow).
fn raise_open_file_limit() {
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
    quiet_accessibility_panic();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "spacemap",
        options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
            install_fallback_fonts(&cc.egui_ctx, false);
            Ok(Box::new(DiskScanApp::default()))
        }),
    )
}
