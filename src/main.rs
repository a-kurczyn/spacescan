// SpaceMap: a portable single-binary disk usage sunburst visualizer.
// Clone of the "Scanner" Windows utility (sunburst chart of drive/folder usage).

use eframe::egui;
use egui::{Color32, Pos2, Vec2};
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

// ---------------- Data model ----------------

#[derive(Clone)]
struct Node {
    name: String,
    path: PathBuf,
    size: u64,
    file_count: u64,
    is_dir: bool,
    children: Vec<Node>,
    mode: u32,
    /// All free — same `stat` struct already fetched for size/mode.
    mtime: i64,
    ctime: i64,
    uid: u32,
    gid: u32,
}

/// "755 (rwxr-xr-x)" style permission summary.
fn format_perms(mode: u32) -> String {
    let bit = |m: u32, r: u32, w: u32, x: u32| {
        format!(
            "{}{}{}",
            if m & r != 0 { "r" } else { "-" },
            if m & w != 0 { "w" } else { "-" },
            if m & x != 0 { "x" } else { "-" },
        )
    };
    let sym = format!(
        "{}{}{}",
        bit(mode, 0o400, 0o200, 0o100),
        bit(mode, 0o040, 0o020, 0o010),
        bit(mode, 0o004, 0o002, 0o001),
    );
    format!("{:o} ({})", mode & 0o777, sym)
}

/// Unix epoch seconds -> local "YYYY-MM-DD HH:MM" — free from the same
/// `stat` struct already fetched, no extra syscall.
fn format_epoch(secs: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => "-".to_string(),
    }
}

/// uid/gid -> username/groupname, cached (uzers hits the system's NSS
/// lookup each call — cheap, but no reason to repeat it every frame while
/// the pointer sits still over the same file).
fn format_owner(
    uid: u32,
    gid: u32,
    user_cache: &mut std::collections::HashMap<u32, String>,
    group_cache: &mut std::collections::HashMap<u32, String>,
) -> String {
    let user = user_cache
        .entry(uid)
        .or_insert_with(|| {
            uzers::get_user_by_uid(uid)
                .map(|u| u.name().to_string_lossy().into_owned())
                .unwrap_or_else(|| uid.to_string())
        })
        .clone();
    let group = group_cache
        .entry(gid)
        .or_insert_with(|| {
            uzers::get_group_by_gid(gid)
                .map(|g| g.name().to_string_lossy().into_owned())
                .unwrap_or_else(|| gid.to_string())
        })
        .clone();
    format!("{user}:{group}")
}

fn empty_node() -> Node {
    Node {
        name: String::new(),
        path: PathBuf::new(),
        size: 0,
        file_count: 0,
        is_dir: true,
        children: Vec::new(),
        mode: 0,
        mtime: 0,
        ctime: 0,
        uid: 0,
        gid: 0,
    }
}

/// Inserts a completed directory's final stats into the growing live-scan
/// tree, by path, creating placeholder ancestors on the way down as needed.
///
/// `node` starts as the tree root and target_path is always node.path or a
/// descendant of it. When we reach the exact target, its value is
/// authoritative (that directory is done) — set directly, don't derive from
/// children, since files aren't individually streamed and would make a
/// sum-of-children an undercount. Every *ancestor* on the way back up gets
/// its aggregate recomputed as "sum of what's known so far", which is
/// naturally just an interim lower bound until that ancestor's own
/// SliceDone arrives and overwrites it with the real value the same way.
#[allow(clippy::too_many_arguments)]
fn graft_slice(
    node: &mut Node,
    target_path: &Path,
    size: u64,
    file_count: u64,
    mode: u32,
    mtime: i64,
    ctime: i64,
    uid: u32,
    gid: u32,
) {
    if node.path == target_path {
        node.size = size;
        node.file_count = file_count;
        node.mode = mode;
        node.mtime = mtime;
        node.ctime = ctime;
        node.uid = uid;
        node.gid = gid;
        return;
    }
    let idx = match node.children.iter().position(|c| target_path.starts_with(&c.path)) {
        Some(i) => i,
        None => {
            let rel = match target_path.strip_prefix(&node.path) {
                Ok(r) => r,
                Err(_) => return, // not actually a descendant; ignore
            };
            let Some(first) = rel.components().next() else {
                return;
            };
            let child_path = node.path.join(first);
            node.children.push(Node {
                name: first.as_os_str().to_string_lossy().to_string(),
                path: child_path,
                size: 0,
                file_count: 0,
                is_dir: true,
                children: Vec::new(),
                mode: 0,
                mtime: 0,
                ctime: 0,
                uid: 0,
                gid: 0,
            });
            node.children.len() - 1
        }
    };
    graft_slice(&mut node.children[idx], target_path, size, file_count, mode, mtime, ctime, uid, gid);
    node.size = node.children.iter().map(|c| c.size).sum();
    node.file_count = node.children.iter().map(|c| c.file_count).sum();
    node.children.sort_by(|a, b| b.size.cmp(&a.size));
}

const MAX_EXTENSIONS_SHOWN: usize = 40;

fn collect_extensions(node: &Node, map: &mut std::collections::HashMap<String, (u64, u64)>) {
    if node.is_dir {
        for c in &node.children {
            collect_extensions(c, map);
        }
    } else {
        let ext = Path::new(&node.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| "(no extension)".to_string());
        let entry = map.entry(ext).or_insert((0, 0));
        entry.0 += node.size;
        entry.1 += node.file_count.max(1);
    }
}

/// (extension, total size, file count), largest-first, with a long tail of
/// rare extensions folded into a single "(other)" row.
fn extension_breakdown(node: &Node) -> Vec<(String, u64, u64)> {
    let mut map = std::collections::HashMap::new();
    collect_extensions(node, &mut map);
    let mut v: Vec<(String, u64, u64)> = map.into_iter().map(|(k, (s, c))| (k, s, c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    if v.len() > MAX_EXTENSIONS_SHOWN {
        let rest = v.split_off(MAX_EXTENSIONS_SHOWN);
        let size: u64 = rest.iter().map(|(_, s, _)| s).sum();
        let count: u64 = rest.iter().map(|(_, _, c)| c).sum();
        v.push((format!("({} other extensions)", rest.len()), size, count));
    }
    v
}

fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string_lossy().to_string())
}

/// Turns an io::Error into the kind of plain-language line a non-technical
/// user asked for, e.g. "can't find that directory" / "don't have permission
/// to read or enter that folder", instead of a raw errno string.
fn friendly_io_error(path: &Path, e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    let what = match e.kind() {
        NotFound => "can't find that directory".to_string(),
        PermissionDenied => "don't have permission to read or enter that folder".to_string(),
        _ => format!("couldn't be read ({e})"),
    };
    format!("{} — {}", path.display(), what)
}

/// Scans a single directory entry: recurses if it's a (same-filesystem)
/// directory, otherwise builds a leaf file Node. Shared by `scan_dir`'s
/// normal recursion and the top-level streaming scan in `start_scan`.
fn scan_entry(
    entry: &std::fs::DirEntry,
    root_dev: u64,
    progress: &Sender<ScanMsg>,
    counter: &std::sync::atomic::AtomicU64,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    progress_interval: u64,
) -> Node {
    use std::os::unix::fs::MetadataExt;
    let p = entry.path();
    let ft = entry.file_type();
    let node = match ft {
        Ok(ft) if ft.is_dir() && !ft.is_symlink() => {
            // Don't cross filesystem/mount boundaries (matches `du -x`):
            // a drive/mount-point scan should not silently absorb other
            // mounted filesystems nested under it.
            let meta = entry.metadata().ok();
            let dev = meta.as_ref().map(|m| m.dev()).unwrap_or(root_dev);
            if dev != root_dev {
                Node {
                    name: format!("{} [other filesystem]", file_name_of(&p)),
                    path: p,
                    size: 0,
                    file_count: 0,
                    is_dir: true,
                    children: Vec::new(),
                    mode: meta.as_ref().map(|m| m.mode()).unwrap_or(0),
                    mtime: meta.as_ref().map(|m| m.mtime()).unwrap_or(0),
                    ctime: meta.as_ref().map(|m| m.ctime()).unwrap_or(0),
                    uid: meta.as_ref().map(|m| m.uid()).unwrap_or(0),
                    gid: meta.as_ref().map(|m| m.gid()).unwrap_or(0),
                }
            } else {
                scan_dir(&p, root_dev, progress, counter, cancel, progress_interval)
            }
        }
        _ => {
            let (sz, mode, mtime, ctime, uid, gid) = match entry.metadata() {
                Ok(m) => (m.len(), m.mode(), m.mtime(), m.ctime(), m.uid(), m.gid()),
                Err(e) => {
                    let _ = progress.send(ScanMsg::LogError(friendly_io_error(&p, &e)));
                    (0, 0, 0, 0, 0, 0)
                }
            };
            Node {
                name: file_name_of(&p),
                path: p,
                size: sz,
                file_count: 1,
                is_dir: false,
                children: Vec::new(),
                mode,
                mtime,
                ctime,
                uid,
                gid,
            }
        }
    };
    let n = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n % progress_interval.max(1) == 0 {
        let _ = progress.send(ScanMsg::Progress(n));
    }
    node
}

fn scan_dir(
    path: &Path,
    root_dev: u64,
    progress: &Sender<ScanMsg>,
    counter: &std::sync::atomic::AtomicU64,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    progress_interval: u64,
) -> Node {
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::Ordering;
    let name = file_name_of(path);
    if cancel.load(Ordering::Relaxed) {
        // A newer scan superseded this one: stop doing work immediately.
        // The result is discarded by the caller either way.
        return Node {
            name,
            path: path.to_path_buf(),
            size: 0,
            file_count: 0,
            is_dir: true,
            children: Vec::new(),
            mode: 0,
            mtime: 0,
            ctime: 0,
            uid: 0,
            gid: 0,
        };
    }
    let self_meta = std::fs::metadata(path).ok();
    let self_mode = self_meta.as_ref().map(|m| m.mode()).unwrap_or(0);
    let self_mtime = self_meta.as_ref().map(|m| m.mtime()).unwrap_or(0);
    let self_ctime = self_meta.as_ref().map(|m| m.ctime()).unwrap_or(0);
    let self_uid = self_meta.as_ref().map(|m| m.uid()).unwrap_or(0);
    let self_gid = self_meta.as_ref().map(|m| m.gid()).unwrap_or(0);
    let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(path) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(e) => {
            let _ = progress.send(ScanMsg::LogError(friendly_io_error(path, &e)));
            Vec::new()
        }
    };

    let children: Vec<Node> = entries
        .par_iter()
        .map(|entry| scan_entry(entry, root_dev, progress, counter, cancel, progress_interval))
        .collect();

    let mut children = children;
    children.sort_by(|a, b| b.size.cmp(&a.size));
    let size = children.iter().map(|c| c.size).sum();
    let file_count = children.iter().map(|c| c.file_count).sum::<u64>() + if children.is_empty() { 0 } else { 0 };
    let file_count = if children.is_empty() { 0 } else { file_count };

    let _ = progress.send(ScanMsg::SliceDone {
        path: path.to_path_buf(),
        size,
        file_count,
        mode: self_mode,
        mtime: self_mtime,
        ctime: self_ctime,
        uid: self_uid,
        gid: self_gid,
    });

    Node {
        name,
        path: path.to_path_buf(),
        size,
        file_count,
        is_dir: true,
        children,
        mode: self_mode,
        mtime: self_mtime,
        ctime: self_ctime,
        uid: self_uid,
        gid: self_gid,
    }
}

enum ScanMsg {
    Progress(u64),
    /// A directory (any depth) just finished — its final size/count, not the
    /// subtree itself (cheap: no cloning). The UI grafts it into the growing
    /// partial tree by path, so the sunburst blossoms slice by slice at
    /// every level, not just the top one.
    SliceDone {
        path: PathBuf,
        size: u64,
        file_count: u64,
        mode: u32,
        mtime: i64,
        ctime: i64,
        uid: u32,
        gid: u32,
    },
    Done(Node, f64),
    Error(String),
    LogError(String),
}

fn human_size(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", bytes, units[u])
    } else {
        format!("{:.2} {}", v, units[u])
    }
}

/// Thousands-separated integer, e.g. 1050824 -> "1,050,824".
fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

// ---------------- Filesystem capacity ----------------

/// Returns (total_bytes, free_bytes) for the filesystem containing `path`.
fn fs_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(cpath.as_ptr(), &mut stat) != 0 {
            return None;
        }
        let frsize = stat.f_frsize as u64;
        let total = stat.f_blocks as u64 * frsize;
        let free = stat.f_bavail as u64 * frsize;
        Some((total, free))
    }
}

/// True if `path` is itself a filesystem root (its device ID differs from
/// its parent's) — i.e. a genuine mount point. This is independent of
/// `list_mounts()`'s auto-detected list, so it correctly recognizes a
/// network share, a just-plugged-in drive not yet refreshed, or any path
/// typed directly into the path bar, rather than requiring an exact match
/// against a possibly-stale list.
fn is_real_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = path.parent() else {
        return true; // "/" has no parent: trivially a mount point
    };
    let (Ok(here), Ok(up)) = (std::fs::metadata(path), std::fs::metadata(parent)) else {
        return false;
    };
    here.dev() != up.dev()
}

// ---------------- Mount listing ----------------

fn list_mounts() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let skip_fs = [
        "proc", "sysfs", "devtmpfs", "tmpfs", "devpts", "cgroup", "cgroup2", "pstore", "bpf",
        "autofs", "mqueue", "hugetlbfs", "debugfs", "tracefs", "securityfs", "configfs",
        "fusectl", "binfmt_misc", "rpc_pipefs", "nsfs", "overlay", "efivarfs",
        "selinuxfs", "fuse.portal", "fuse.gocryptfs", "ramfs",
        // Note: fuse.gvfsd-fuse (GVFS network shares — SMB/SFTP/etc.) is
        // deliberately NOT skipped: those are real, browsable filesystems
        // users legitimately want to scan.
    ];
    if let Ok(content) = std::fs::read_to_string("/proc/mounts") {
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                continue;
            }
            let mountpoint = parts[1];
            let fstype = parts[2];
            if skip_fs.contains(&fstype) {
                continue;
            }
            if mountpoint.starts_with("/snap") || mountpoint.starts_with("/var/lib/docker") {
                continue;
            }
            let label = if mountpoint == "/" {
                format!("/ ({})", fstype)
            } else {
                format!("{} ({})", mountpoint, fstype)
            };
            out.push((label, PathBuf::from(mountpoint)));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out.dedup_by(|a, b| a.1 == b.1);
    if out.is_empty() {
        out.push(("/".to_string(), PathBuf::from("/")));
    }
    out
}

// ---------------- App ----------------

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

struct ContextMenuState {
    node_path: Vec<usize>,
    screen_pos: Pos2,
}

#[derive(Clone, Copy, PartialEq)]
enum SortColumn {
    Size,
    Files,
    Name,
}

#[derive(Clone, Copy)]
struct SortState {
    column: SortColumn,
    ascending: bool,
}

/// Draws one clickable, sortable column header. Clicking the currently
/// active column flips its direction; clicking a different column switches
/// to it with a sensible default direction (descending for numeric columns,
/// so "biggest first" without an extra click — ascending for the name
/// column, so alphabetical order reads naturally).
fn sortable_header(ui: &mut egui::Ui, label: &str, column: SortColumn, state: &mut SortState) {
    let is_active = state.column == column;
    let arrow = if is_active {
        if state.ascending { " ▲" } else { " ▼" }
    } else {
        ""
    };
    if ui.button(egui::RichText::new(format!("{label}{arrow}")).strong()).clicked() {
        if is_active {
            state.ascending = !state.ascending;
        } else {
            state.column = column;
            state.ascending = column == SortColumn::Name;
        }
    }
}

/// Every tunable knob for the sunburst, in one place with safe slider
/// ranges so nothing the user can dial in from the UI can crash the app
/// (divide-by-zero, degenerate geometry, etc.) — see `Settings::default()`
/// for the factory values and `ui_settings_window` for the bounds.
#[derive(Clone, PartialEq)]
struct Settings {
    max_render_depth: usize,
    min_segment_angle_deg: f32,
    max_children_shown: usize,
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
    /// Exponent, not the value itself: the "N items scanned" counter is
    /// pushed to the UI every `1 << progress_interval_pow2` filesystem
    /// entries. Stored as a power-of-two exponent (0..=16) so the slider
    /// can only ever select a power of two — never an arbitrary interval
    /// that could over- or under-report.
    progress_interval_pow2: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_render_depth: 6,
            min_segment_angle_deg: 0.75,
            max_children_shown: 64,
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
            progress_interval_pow2: 9, // 1 << 9 == 512, the original hardcoded value
        }
    }
}

struct DiskScanApp {
    mounts: Vec<(String, PathBuf)>,
    selected_mount: usize,
    root: Option<Arc<Node>>,
    view_stack: Vec<Vec<usize>>, // stack of index-paths; last = current view root
    scanning: bool,
    scan_rx: Option<Receiver<ScanMsg>>,
    scanned_count: u64,
    scan_start: Instant,
    hidden: HashSet<PathBuf>,
    hovered: Option<HoverInfo>,
    context_menu: Option<ContextMenuState>,
    summary_view: bool,
    status: String,
    /// (total_capacity, free_bytes) of the filesystem, when the current scan
    /// target is a real mount point (so we can draw an "unused space" slice).
    free_space: Option<(u64, u64)>,
    log: Vec<String>,
    log_truncated: u64,
    cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Top-level children streamed in so far by the scan in progress, so the
    /// sunburst can render (read-only) while scanning instead of just a
    /// spinner.
    partial_root: Node,
    settings: Settings,
    show_settings: bool,
    path_input: String,
    path_input_focused: bool,
    /// Per-extension size/count breakdown for the summary view, cached and
    /// only recomputed when the viewed folder changes (it's an O(subtree)
    /// walk, too costly to redo every frame).
    ext_breakdown: Vec<(String, u64, u64)>,
    ext_breakdown_for: Option<PathBuf>,
    contents_sort: SortState,
    ext_sort: SortState,
    /// On-demand MIME sniffing for the single file currently hovered in the
    /// tooltip — never done during the bulk scan (reading file content for
    /// every file would meaningfully slow it down), only for one file at a
    /// time on hover, and off the UI thread so a slow/spun-down drive can't
    /// cause a hitch. `None` cached = looked up, but undetermined.
    mime_cache: std::collections::HashMap<PathBuf, Option<String>>,
    mime_inflight: HashSet<PathBuf>,
    mime_tx: Sender<(PathBuf, Option<String>)>,
    mime_rx: Receiver<(PathBuf, Option<String>)>,
    user_cache: std::collections::HashMap<u32, String>,
    group_cache: std::collections::HashMap<u32, String>,
}

impl Default for DiskScanApp {
    fn default() -> Self {
        let mounts = list_mounts();
        let (mime_tx, mime_rx) = channel();
        let app = Self {
            mounts,
            selected_mount: usize::MAX, // nothing scanned yet, so nothing should read as "selected"
            root: None,
            view_stack: vec![vec![]],
            scanning: false,
            scan_rx: None,
            scanned_count: 0,
            scan_start: Instant::now(),
            hidden: HashSet::new(),
            hovered: None,
            context_menu: None,
            summary_view: false,
            status: String::new(),
            free_space: None,
            log: Vec::new(),
            log_truncated: 0,
            cancel_flag: None,
            partial_root: empty_node(),
            settings: Settings::default(),
            show_settings: false,
            path_input: String::new(),
            path_input_focused: false,
            ext_breakdown: Vec::new(),
            ext_breakdown_for: None,
            // Both tables start sorted by size, descending — matches the
            // order the sunburst itself already uses (largest slice first).
            contents_sort: SortState { column: SortColumn::Size, ascending: false },
            ext_sort: SortState { column: SortColumn::Size, ascending: false },
            mime_cache: std::collections::HashMap::new(),
            mime_inflight: HashSet::new(),
            mime_tx,
            mime_rx,
            user_cache: std::collections::HashMap::new(),
            group_cache: std::collections::HashMap::new(),
        };
        app
    }
}

impl DiskScanApp {
    fn start_scan(&mut self, path: PathBuf) {
        self.scanning = true;
        self.scan_start = Instant::now();
        self.scanned_count = 0;
        self.view_stack = vec![vec![]];
        self.hidden.clear();
        self.log.clear();
        self.log_truncated = 0;
        self.partial_root = empty_node();
        self.partial_root.path = path.clone();
        self.partial_root.name = file_name_of(&path);
        self.free_space = if is_real_mount_point(&path) { fs_space(&path) } else { None };

        // Tell any still-running previous scan to stop wasting CPU/IO: its
        // result would just be thrown away once superseded anyway.
        if let Some(prev) = &self.cancel_flag {
            prev.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.cancel_flag = Some(cancel.clone());

        let progress_interval: u64 = 1u64 << self.settings.progress_interval_pow2;

        let (tx, rx) = channel();
        self.scan_rx = Some(rx);
        std::thread::spawn(move || {
            let counter = std::sync::atomic::AtomicU64::new(0);
            let start = Instant::now();
            if !path.exists() {
                let _ = tx.send(ScanMsg::Error(format!("Path not found: {}", path.display())));
                return;
            }
            let root_dev = match std::fs::metadata(&path) {
                Ok(m) => {
                    use std::os::unix::fs::MetadataExt;
                    m.dev()
                }
                Err(e) => {
                    let _ = tx.send(ScanMsg::Error(format!("Cannot stat {}: {e}", path.display())));
                    return;
                }
            };
            // scan_dir streams a SliceDone for every directory as it
            // finishes (any depth), so the sunburst blossoms slice by slice
            // throughout the scan — see SliceDone's doc comment.
            let root = scan_dir(&path, root_dev, &tx, &counter, &cancel, progress_interval);
            if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tx.send(ScanMsg::Done(root, start.elapsed().as_secs_f64()));
            }
        });
    }

    fn current_view_node<'a>(&self, root: &'a Node) -> &'a Node {
        let idx_path = self.view_stack.last().unwrap();
        get_node(root, idx_path)
    }

    /// User-requested abort (Esc while scanning). Falls back to whatever
    /// was scanned previously, if anything — same cooperative cancellation
    /// used when a new scan supersedes an old one.
    fn abort_scan(&mut self) {
        if let Some(cancel) = &self.cancel_flag {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.scanning = false;
        self.scan_rx = None;
        self.status = "Scan aborted".to_string();
    }

    /// Drains completed on-demand MIME lookups into the cache.
    fn poll_mime(&mut self) {
        while let Ok((path, result)) = self.mime_rx.try_recv() {
            self.mime_inflight.remove(&path);
            self.mime_cache.insert(path, result);
        }
    }

    /// Kicks off a background MIME sniff for `path` if it hasn't already
    /// been resolved (or isn't already in flight) — never on the UI thread,
    /// so a slow/spun-down drive can't cause a hitch. Only meant to be
    /// called for a single hovered *file*, never during the bulk scan.
    fn ensure_mime_lookup(&mut self, path: &Path) {
        if self.mime_cache.contains_key(path) || self.mime_inflight.contains(path) {
            return;
        }
        if self.mime_cache.len() > 500 {
            self.mime_cache.clear(); // simple bound for a long-running session
        }
        self.mime_inflight.insert(path.to_path_buf());
        let tx = self.mime_tx.clone();
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            let result = infer::get_from_path(&p).ok().flatten().map(|t| t.mime_type().to_string());
            let _ = tx.send((p, result));
        });
    }

    /// Pushes a message to the bottom "Issues" log — the one consistent
    /// place scan/input problems are reported, respecting the same cap as
    /// scan-time LogError messages.
    fn log_issue(&mut self, msg: String) {
        if self.log.len() < self.settings.max_log_lines {
            self.log.push(msg);
        } else {
            self.log_truncated += 1;
        }
    }

    /// Jumps sideways to the next sibling directory in alphabetical order
    /// (wrapping past the last back to the first). A no-op at the drive
    /// root, where "sibling" isn't a meaningful concept.
    fn goto_next_sibling(&mut self, root: &Node) {
        let cur_path = self.view_stack.last().unwrap().clone();
        let Some((&cur_idx, parent_path)) = cur_path.split_last() else {
            return; // at the drive root: no parent, no siblings
        };
        let parent = get_node(root, parent_path);

        // Chart order, not alphabetical: children are already stored
        // largest-first, which is the same order the sunburst draws them
        // in, so "next" here means "next clockwise slice", matching what
        // the user actually sees.
        let siblings: Vec<usize> = parent
            .children
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_dir && !self.hidden.contains(&c.path))
            .map(|(i, _)| i)
            .collect();
        if siblings.is_empty() {
            return;
        }

        let next_idx = match siblings.iter().position(|&i| i == cur_idx) {
            Some(p) => siblings[(p + 1) % siblings.len()],
            None => siblings[0],
        };
        let mut new_path = parent_path.to_vec();
        new_path.push(next_idx);
        *self.view_stack.last_mut().unwrap() = new_path;
    }

    fn poll_scan(&mut self) {
        // Cap how much work one frame can do. A directory tree with a huge
        // number of directories (not just files) can produce a very large
        // burst of SliceDone messages; without a cap, draining "everything
        // currently queued" in one frame could take long enough that the
        // window stops responding to the compositor's ping and gets flagged
        // as hung. Any leftover messages just get processed on the next
        // frame(s) instead — request_repaint() during scanning means those
        // follow immediately.
        const MAX_MESSAGES_PER_FRAME: u32 = 4000;
        let mut processed = 0u32;
        if let Some(rx) = &self.scan_rx {
            loop {
                if processed >= MAX_MESSAGES_PER_FRAME {
                    break;
                }
                processed += 1;
                match rx.try_recv() {
                    Ok(ScanMsg::Progress(n)) => {
                        self.scanned_count = n;
                    }
                    Ok(ScanMsg::LogError(msg)) => {
                        if self.log.len() < self.settings.max_log_lines {
                            self.log.push(msg);
                        } else {
                            self.log_truncated += 1;
                        }
                    }
                    Ok(ScanMsg::SliceDone { path, size, file_count, mode, mtime, ctime, uid, gid }) => {
                        graft_slice(&mut self.partial_root, &path, size, file_count, mode, mtime, ctime, uid, gid);
                    }
                    Ok(ScanMsg::Done(node, secs)) => {
                        self.root = Some(Arc::new(node));
                        self.scanning = false;
                        self.status = format!("Scan completed in {:.1}s", secs);
                        self.scan_rx = None;
                        break;
                    }
                    Ok(ScanMsg::Error(e)) => {
                        self.status = e;
                        self.scanning = false;
                        self.scan_rx = None;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.scanning = false;
                        self.scan_rx = None;
                        break;
                    }
                }
            }
        }
    }
}

fn get_node<'a>(root: &'a Node, idx_path: &[usize]) -> &'a Node {
    let mut n = root;
    for &i in idx_path {
        if i < n.children.len() {
            n = &n.children[i];
        }
    }
    n
}

struct Segment {
    idx_path: Vec<usize>,
    start_angle: f32,
    end_angle: f32,
    ring: usize,
    name: String,
    size: u64,
    file_count: u64,
    is_dir: bool,
    is_other: bool,
    is_free: bool,
    mode: Option<u32>,
}

fn layout_sunburst(
    node: &Node,
    idx_path: Vec<usize>,
    start_angle: f32,
    end_angle: f32,
    ring: usize,
    hidden: &HashSet<PathBuf>,
    extra_free_bytes: u64,
    total_capacity: u64,
    settings: &Settings,
    out: &mut Vec<Segment>,
) {
    if ring >= settings.max_render_depth {
        return;
    }
    let visible_children: Vec<(usize, &Node)> = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| !hidden.contains(&c.path))
        .collect();
    let real_total: u64 = visible_children.iter().map(|(_, c)| c.size).sum();

    // Free space gets a *fixed* share of the span, from known filesystem
    // capacity — it doesn't grow or shrink as the scan progresses. Whatever
    // has been discovered so far always divides up the *entire* remaining
    // "content" span among itself (proportional to each other, not to some
    // eventual/unknown final total): otherwise there'd be an unrendered gap
    // between "what's mapped" and "known free space" while a scan is still
    // in progress, since discovered-so-far starts small and grows.
    let full_span = end_angle - start_angle;
    let content_end_angle = if extra_free_bytes > 0 && total_capacity > 0 {
        let free_frac = extra_free_bytes as f32 / total_capacity as f32;
        start_angle + full_span * (1.0 - free_frac)
    } else {
        end_angle
    };

    let total: u64 = real_total.max(1);
    let span_abs = (content_end_angle - start_angle).abs().max(0.0001);
    let min_frac = (settings.min_segment_angle_deg.to_radians() / span_abs).max(0.0);

    // Children are pre-sorted largest-first. Keep showing individual segments
    // only while they'd still be wide enough to render as a real slice; lump
    // the long tail of tiny ones into a single "other" bucket instead of
    // producing hundreds of sub-pixel slivers.
    let mut split = 0;
    for (_, c) in &visible_children {
        if split >= settings.max_children_shown {
            break;
        }
        let frac = c.size as f32 / total as f32;
        if frac < min_frac {
            break;
        }
        split += 1;
    }
    let shown: Vec<(usize, &Node)> = visible_children.iter().take(split).cloned().collect();
    let rest: Vec<(usize, &Node)> = visible_children.iter().skip(split).cloned().collect();
    let rest_size: u64 = rest.iter().map(|(_, c)| c.size).sum();

    let span = content_end_angle - start_angle;
    let mut cursor = start_angle;

    for (i, child) in &shown {
        let frac = child.size as f32 / total as f32;
        let a0 = cursor;
        let a1 = cursor + span * frac;
        cursor = a1;
        let mut cp = idx_path.clone();
        cp.push(*i);
        out.push(Segment {
            idx_path: cp.clone(),
            start_angle: a0,
            end_angle: a1,
            ring,
            name: child.name.clone(),
            size: child.size,
            file_count: child.file_count.max(1),
            is_dir: child.is_dir,
            is_other: false,
            is_free: false,
            mode: Some(child.mode),
        });
        if child.is_dir && !child.children.is_empty() {
            layout_sunburst(child, cp, a0, a1, ring + 1, hidden, 0, 0, settings, out);
        }
    }
    if rest_size > 0 {
        let frac = rest_size as f32 / total as f32;
        let a0 = cursor;
        let a1 = cursor + span * frac;
        out.push(Segment {
            idx_path: idx_path.clone(),
            start_angle: a0,
            end_angle: a1,
            ring,
            name: format!("({} other items)", rest.len()),
            size: rest_size,
            file_count: rest.iter().map(|(_, c)| c.file_count).sum(),
            is_dir: false,
            is_other: true,
            is_free: false,
            mode: None,
        });
    }
    if extra_free_bytes > 0 {
        // Use the precomputed fixed boundary, not `cursor`, so there's no
        // float-drift gap between mapped content and the free-space slice.
        out.push(Segment {
            idx_path: idx_path.clone(),
            start_angle: content_end_angle,
            end_angle,
            ring,
            name: "Free space".to_string(),
            size: extra_free_bytes,
            file_count: 0,
            is_dir: false,
            is_other: false,
            is_free: true,
            mode: None,
        });
    }
}

fn hue_for_branch(i: usize) -> f32 {
    // evenly distributed hues using the golden angle
    ((i as f32) * 137.50776_f32) % 360.0
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color32 {
    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r1, g1, b1) = if hp < 1.0 {
        (c, x, 0.0)
    } else if hp < 2.0 {
        (x, c, 0.0)
    } else if hp < 3.0 {
        (0.0, c, x)
    } else if hp < 4.0 {
        (0.0, x, c)
    } else if hp < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = v - c;
    Color32::from_rgb(
        (((r1 + m) * 255.0) as i32).clamp(0, 255) as u8,
        (((g1 + m) * 255.0) as i32).clamp(0, 255) as u8,
        (((b1 + m) * 255.0) as i32).clamp(0, 255) as u8,
    )
}

/// Brightens `c` with a gamma curve (gamma < 1 lightens) while keeping its
/// character, instead of flatly blending toward white.
fn gamma_lighten(c: Color32, gamma: f32) -> Color32 {
    let f = |v: u8| ((v as f32 / 255.0).powf(gamma) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

fn segment_color(seg: &Segment, top_branch_hue: f32, settings: &Settings) -> Color32 {
    if seg.is_other {
        // A pale tint of the *same* branch hue, so the aggregate bucket
        // reads as "more of this folder", not an unrelated color/glitch.
        return hsv_to_rgb(top_branch_hue, settings.other_sat, settings.other_val);
    }
    let val = (settings.ring_val_base - (seg.ring as f32) * settings.ring_val_falloff)
        .max(settings.ring_val_floor);
    hsv_to_rgb(top_branch_hue, settings.ring_sat, val)
}

/// Draws the hub's two-line label — folder name, then size occupied —
/// wrapped to fit inside the hub circle so a long folder name folds onto
/// multiple lines instead of spilling out past the circle's edge.
/// Flat, single-color vector icons drawn with the painter — deliberately
/// not Unicode/emoji glyphs (📄/📁), since those render in full color via
/// the system's emoji font on most Linux setups, clashing with the app's
/// flat, theme-matched look. `color` should track the current theme's text
/// color so the icon stays flat and readable in both light and dark modes.
fn draw_file_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
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

fn draw_folder_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
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

fn draw_hub_text(painter: &egui::Painter, center: Pos2, hub_radius: f32, name: &str, size: u64) {
    // Width of a rectangle comfortably inscribed in the circle, with a
    // little margin so wrapped lines don't touch the ring.
    let wrap_width = (hub_radius * 1.3).max(24.0);

    let display_name = if name.is_empty() { "/" } else { name };
    let name_job = egui::text::LayoutJob::simple(
        display_name.to_string(),
        egui::FontId::proportional(14.0),
        Color32::WHITE,
        wrap_width,
    );
    let name_galley = painter.layout_job(name_job);

    let size_job = egui::text::LayoutJob::simple(
        human_size(size),
        egui::FontId::proportional(18.0),
        Color32::WHITE,
        wrap_width,
    );
    let size_galley = painter.layout_job(size_job);

    let gap = 4.0;
    let total_height = name_galley.size().y + gap + size_galley.size().y;
    let top = center.y - total_height / 2.0;

    let name_pos = Pos2::new(center.x - name_galley.size().x / 2.0, top);
    painter.galley(name_pos, name_galley.clone(), Color32::WHITE);

    let size_pos = Pos2::new(
        center.x - size_galley.size().x / 2.0,
        top + name_galley.size().y + gap,
    );
    painter.galley(size_pos, size_galley.clone(), Color32::WHITE);
}

/// Arc outline points, traced outer-arc-forward then inner-arc-backward,
/// suitable for both mesh fill (as a triangle strip) and a closed stroke.
fn arc_dir(t: f32) -> Vec2 {
    let (s, c) = t.sin_cos();
    Vec2::new(s, -c) // angle 0 = straight up, increasing clockwise
}

fn draw_arc_mesh(
    painter: &egui::Painter,
    center: Pos2,
    r0: f32,
    r1: f32,
    a0: f32,
    a1: f32,
    color: Color32,
    settings: &Settings,
) {
    let span = (a1 - a0).abs();
    // Tessellate based on actual on-screen arc length (at the outer radius)
    // so outer rings stay smooth instead of getting faceted/pixelated.
    let arc_len_px = span * r1.max(1.0);
    let px_per_step = settings.tess_px_per_step.max(0.5);
    let steps = ((arc_len_px / px_per_step).ceil() as usize).clamp(1, 512);

    let mut mesh = egui::epaint::Mesh::default();
    let base = mesh.vertices.len() as u32;
    for i in 0..=steps {
        let t = a0 + (a1 - a0) * (i as f32 / steps as f32);
        let dir = arc_dir(t);
        mesh.vertices.push(egui::epaint::Vertex {
            pos: center + dir * r0,
            uv: egui::epaint::WHITE_UV,
            color,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: center + dir * r1,
            uv: egui::epaint::WHITE_UV,
            color,
        });
    }
    for i in 0..steps as u32 {
        let i0 = base + i * 2;
        let i1 = i0 + 1;
        let i2 = i0 + 2;
        let i3 = i0 + 3;
        mesh.indices.extend_from_slice(&[i0, i1, i2, i1, i3, i2]);
    }
    painter.add(egui::Shape::mesh(mesh));

    // Crisp, consistent border regardless of theme/background, instead
    // of relying on a radial gap that only sometimes shows through.
    let stroke = egui::Stroke::new(
        settings.stroke_width,
        Color32::from_black_alpha(settings.stroke_alpha),
    );
    let mut outline = Vec::with_capacity(2 * steps + 2);
    for i in 0..=steps {
        let t = a0 + (a1 - a0) * (i as f32 / steps as f32);
        outline.push(center + arc_dir(t) * r1);
    }
    for i in (0..=steps).rev() {
        let t = a0 + (a1 - a0) * (i as f32 / steps as f32);
        outline.push(center + arc_dir(t) * r0);
    }
    painter.add(egui::Shape::closed_line(outline, stroke));
}

impl eframe::App for DiskScanApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_scan();
        self.poll_mime();
        if self.scanning && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.abort_scan();
        }
        if self.scanning || !self.mime_inflight.is_empty() {
            ctx.request_repaint();
        }

        // `Sides` measures the right-hand content first and gives the
        // left-hand content whatever room remains — unlike a manual
        // "reserve N pixels" guess, the nav buttons can never end up
        // clipped regardless of window width, font, or theme. Both
        // closures below only read pre-cloned local state and report what
        // happened; every actual `self` mutation happens afterward, since
        // Sides::show can't hand out two simultaneous `&mut self` closures.
        let root_arc = self.root.clone();
        let cur_view_idx = self.view_stack.last().unwrap().clone();
        let can_go_up = self.view_stack.len() > 1;
        let can_go_sibling = root_arc.is_some() && !cur_view_idx.is_empty();
        let can_reload = root_arc.is_some();
        let mut path_input = self.path_input.clone();
        let path_input_was_focused = self.path_input_focused;

        enum NavAction {
            None,
            Up,
            Sibling,
            Reload,
        }
        let mut nav_action = NavAction::None;
        let mut settings_toggled = false;
        let mut submit: Option<String> = None;
        let mut path_input_focused = path_input_was_focused;

        egui::Panel::top("top").show(ui, |ui| {
            egui::Sides::new().shrink_left().show(
                ui,
                |ui| {
                    ui.heading("SpaceMap");
                    if ui.button("⚙").on_hover_text("Chart settings").clicked() {
                        settings_toggled = true;
                    }
                    ui.separator();

                    // Keep the path bar synced to navigation (mount switches,
                    // zooming into the chart) as long as the user isn't
                    // currently typing in it — otherwise we'd clobber their
                    // in-progress edit every frame.
                    if !path_input_was_focused {
                        let current = match &root_arc {
                            Some(root) => get_node(root, &cur_view_idx).path.display().to_string(),
                            None => String::new(),
                        };
                        if path_input != current {
                            path_input = current;
                        }
                    }

                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut path_input)
                            .desired_width(ui.available_width())
                            .hint_text("Type a path and press Enter — e.g. a USB drive or network mount"),
                    );
                    path_input_focused = resp.has_focus();
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = Some(path_input.clone());
                    }
                },
                |ui| {
                    // Nav buttons: not overlaid on the chart, so they also
                    // work in Summary view, which has no chart to overlay
                    // them on. `Sides`' right side lays out right-to-left,
                    // so list them in mirrored (visually: ⬆ ➡ ⟳) order.
                    if ui
                        .add_enabled(can_reload, egui::Button::new("⟳"))
                        .on_hover_text("Reload this folder")
                        .clicked()
                    {
                        nav_action = NavAction::Reload;
                    }
                    if ui
                        .add_enabled(can_go_sibling, egui::Button::new("➡"))
                        .on_hover_text("Next sibling folder (chart order)")
                        .clicked()
                    {
                        nav_action = NavAction::Sibling;
                    }
                    if ui
                        .add_enabled(can_go_up, egui::Button::new("⬆"))
                        .on_hover_text("Up to parent folder")
                        .clicked()
                    {
                        nav_action = NavAction::Up;
                    }
                    // Right side lays out right-to-left, so adding this
                    // last places it leftmost of the group — i.e. right
                    // next to the path bar, visually separating it from
                    // the buttons the same way the left separator sets
                    // the gear icon apart from the heading.
                    ui.separator();
                },
            );
        });

        self.path_input = path_input;
        self.path_input_focused = path_input_focused;
        if settings_toggled {
            self.show_settings = !self.show_settings;
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
                self.log_issue(format!(
                    "'{scheme}://' is a network URL, not a mounted path — mount it first \
                     (e.g. via your file manager's Network browser), then type the real \
                     local path it mounts to (often under /run/user/<uid>/gvfs/...)."
                ));
            } else {
                let p = PathBuf::from(trimmed);
                if p.is_dir() {
                    self.selected_mount = usize::MAX; // no radio matches a custom path
                    self.start_scan(p);
                } else {
                    self.log_issue(format!("Not a directory: {}", p.display()));
                }
            }
        }
        match nav_action {
            NavAction::None => {}
            NavAction::Up => {
                self.view_stack.pop();
            }
            NavAction::Sibling => {
                if let Some(root) = root_arc.clone() {
                    self.goto_next_sibling(&root);
                }
            }
            NavAction::Reload => {
                if let Some(root) = &root_arc {
                    let p = get_node(root, &cur_view_idx).path.clone();
                    self.start_scan(p);
                }
            }
        }

        {
            let mut show = self.show_settings;
            egui::Window::new("Chart settings")
                .open(&mut show)
                .resizable(false)
                .collapsible(false)
                .show(&ctx, |ui| {
                    let s = &mut self.settings;

                    ui.label("Depth & grouping");
                    ui.add(
                        egui::Slider::new(&mut s.max_render_depth, 1..=12)
                            .text("Depth levels shown"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.min_segment_angle_deg, 0.1..=5.0)
                            .text("Min slice angle (°)"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.max_children_shown, 4..=256)
                            .text("Max slices per ring"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.hub_radius_frac, 0.05..=0.5)
                            .text("Center hub size"),
                    );

                    ui.separator();
                    ui.label("Colors");
                    ui.add(egui::Slider::new(&mut s.ring_sat, 0.0..=1.0).text("Ring saturation"));
                    ui.add(
                        egui::Slider::new(&mut s.ring_val_base, 0.3..=1.0)
                            .text("Ring brightness (outer)"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.ring_val_falloff, 0.0..=0.3)
                            .text("Brightness falloff per ring"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.ring_val_floor, 0.1..=0.9)
                            .text("Brightness floor"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.other_sat, 0.0..=1.0)
                            .text("\"Other\" bucket saturation"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.other_val, 0.3..=1.0)
                            .text("\"Other\" bucket brightness"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.free_space_gamma, 0.2..=1.5)
                            .text("Free-space gamma (<1 lightens)"),
                    );

                    ui.separator();
                    ui.label("Line rendering");
                    ui.add(
                        egui::Slider::new(&mut s.stroke_width, 0.0..=3.0)
                            .text("Border thickness"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.stroke_alpha, 0..=255)
                            .text("Border darkness"),
                    );
                    ui.add(
                        egui::Slider::new(&mut s.tess_px_per_step, 1.0..=10.0)
                            .text("Curve smoothness (px/step, lower = smoother)"),
                    );

                    ui.separator();
                    ui.label("Log");
                    ui.add(
                        egui::Slider::new(&mut s.max_log_lines, 50..=5000)
                            .text("Max stored issue lines"),
                    );

                    ui.separator();
                    ui.label("Scanning");
                    ui.add(
                        egui::Slider::new(&mut s.progress_interval_pow2, 0..=16)
                            .custom_formatter(|v, _| format!("{}", 1u64 << (v as u32)))
                            .custom_parser(|s| {
                                s.parse::<u64>()
                                    .ok()
                                    .map(|v| v.max(1).next_power_of_two().trailing_zeros().min(16) as f64)
                            })
                            .text("Progress report interval (items, always a power of 2)"),
                    );

                    ui.separator();
                    if ui.button("Defaults").clicked() {
                        *s = Settings::default();
                    }
                });
            self.show_settings = show;
        }

        egui::Panel::left("left").min_size(220.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Drives / mount points:");
                if ui.small_button("⟳").on_hover_text("Refresh drive list").clicked() {
                    // Preserve the current selection across the refresh by
                    // matching on path, since a re-scan can reorder or
                    // add/remove entries (newly mounted drives, etc.).
                    let current_path = self.mounts.get(self.selected_mount).map(|(_, p)| p.clone());
                    self.mounts = list_mounts();
                    self.selected_mount = current_path
                        .and_then(|cp| self.mounts.iter().position(|(_, p)| *p == cp))
                        .unwrap_or(usize::MAX);
                }
            });
            let mut changed = None;
            for (i, (label, _path)) in self.mounts.iter().enumerate() {
                if ui.radio(self.selected_mount == i, label).clicked() {
                    changed = Some(i);
                }
            }
            if let Some(i) = changed {
                self.selected_mount = i;
                let p = self.mounts[i].1.clone();
                self.start_scan(p);
            }
            ui.separator();

            if let Some(root) = &self.root {
                let n = self.current_view_node(root);
                ui.label(format!("Size: {}", human_size(n.size)));
                ui.label(format!("Files: {}", format_count(n.file_count)));
                ui.label(format!("Perms: {}", format_perms(n.mode)));
            }
            // "Hovered" details now render as an overlay in the chart's
            // bottom-left corner (see the chart-drawing code below), next
            // to what they actually describe, instead of in this sidebar.
            ui.separator();
            if ui.selectable_label(self.summary_view, "Summary view").clicked() {
                self.summary_view = !self.summary_view;
            }
            ui.separator();
            if ui.button("Rescan").clicked() {
                if let Some(root) = &self.root {
                    let n = self.current_view_node(root);
                    let p = n.path.clone();
                    self.start_scan(p);
                }
            }
            if ui.button("Empty Recycle Bin").clicked() {
                let _ = empty_trash();
            }
            if self.view_stack.len() > 1 {
                if ui.button("<- Back").clicked() {
                    self.view_stack.pop();
                }
            }
            if !self.status.is_empty() {
                ui.separator();
                ui.small(&self.status);
            }
        });

        egui::Panel::bottom("log_panel")
            .resizable(true)
            .default_size(110.0)
            .size_range(egui::Rangef::new(28.0, 600.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(if self.log.is_empty() {
                        "Issues: none".to_string()
                    } else {
                        format!("Issues: {}", format_count(self.log.len() as u64))
                    });
                    if !self.log.is_empty() && ui.small_button("Clear").clicked() {
                        self.log.clear();
                        self.log_truncated = 0;
                    }
                });
                if !self.log.is_empty() {
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(false)
                        .show(ui, |ui| {
                            for line in self.log.iter().rev() {
                                ui.small(line);
                            }
                            if self.log_truncated > 0 {
                                ui.small(format!(
                                    "...and {} more issue(s) not shown",
                                    format_count(self.log_truncated)
                                ));
                            }
                        });
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if !self.scanning && self.root.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("Pick a drive/mount point on the left, or type a path above and press Enter, to scan it.");
                });
                return;
            }
            if self.scanning {
                // Read-only live preview: draw whatever top-level children
                // have streamed in so far, so the sunburst blossoms one
                // petal at a time instead of staying blank until the whole
                // drive finishes. No hover/click/context-menu here — the
                // data is still changing underneath every frame.
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
                    ui.allocate_painter(Vec2::new(avail.x, content_height), egui::Sense::hover());
                let side = response.rect.width().min(response.rect.height());
                let center = response.rect.center();
                let max_radius = side / 2.0 - 10.0;
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
                let used_target = self.free_space.map(|(total, free)| total.saturating_sub(free));
                let progress = used_target
                    .filter(|&u| u > 0)
                    .map(|u| (self.partial_root.size as f64 / u as f64).clamp(0.0, 1.0) as f32);

                // Rendered directly into the reserved strip below the
                // chart (ui's cursor sits there now, since the painter
                // above only consumed content_height, not the full avail)
                // — full width, and structurally unable to overlap the
                // chart regardless of how full the drive is.
                ui.add_space(4.0);
                let fraction = progress.unwrap_or(0.0);
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
                            egui::ProgressBar::new(fraction)
                                .desired_width(side)
                                .animate(progress.is_none()), // no known capacity: pulse instead of claiming 0%
                        )
                    })
                    .inner;

                // Centered within the *filled* portion specifically (not
                // the whole bar), so it visibly moves along as the fill
                // grows — egui's built-in ProgressBar text is fixed at the
                // left edge, which doesn't do that. Only clamped against
                // the outer panel's clip bounds (not the bar's own,
                // narrower bounds) — that keeps it truly centered on the
                // fill boundary in the normal case, and only nudges it in
                // the rare case of a window so narrow there's no margin
                // left to spill into.
                let text_color = ui.visuals().selection.stroke.color;
                let galley = ui.painter().layout_no_wrap(
                    format!("{} items scanned", format_count(self.scanned_count)),
                    egui::FontId::default(),
                    text_color,
                );
                let half_w = galley.size().x / 2.0 + 4.0;
                let ideal_x = bar_resp.rect.left() + bar_resp.rect.width() * fraction;
                let clip = ui.clip_rect();
                let lo = clip.left() + half_w;
                let hi = (clip.right() - half_w).max(lo);
                let center_x = ideal_x.clamp(lo, hi);
                let filled_center = Pos2::new(center_x, bar_resp.rect.center().y);
                ui.painter()
                    .galley(filled_center - galley.size() / 2.0, galley, text_color);
                return;
            }

            let root = match &self.root {
                Some(r) => r.clone(),
                None => return,
            };

            let avail = ui.available_size();

            // Reserve a fixed-height strip below the chart for hover
            // details, full width (so long paths never wrap) — fixed
            // regardless of whether anything is currently hovered, so the
            // chart's own size never jumps when hovering starts/stops.
            // Not reserved in Summary view, which has no chart to give
            // margin to and no hover state of its own.
            let hover_strip_height = ui.text_style_height(&egui::TextStyle::Body) * 3.0 + 12.0;
            let content_height = if self.summary_view {
                avail.y
            } else {
                (avail.y - hover_strip_height).max(50.0)
            };

            let (response, painter) =
                ui.allocate_painter(Vec2::new(avail.x, content_height), egui::Sense::click());
            let side = response.rect.width().min(response.rect.height());
            let center = response.rect.center();
            let max_radius = side / 2.0 - 10.0;
            let hub_radius = max_radius * self.settings.hub_radius_frac;
            let ring_thickness = (max_radius - hub_radius) / self.settings.max_render_depth as f32;

            let view_node = get_node(&root, self.view_stack.last().unwrap());

            if self.summary_view {
                if self.ext_breakdown_for.as_deref() != Some(view_node.path.as_path()) {
                    self.ext_breakdown = extension_breakdown(view_node);
                    self.ext_breakdown_for = Some(view_node.path.clone());
                }

                // Sort a (index, node) view of the children rather than the
                // children themselves, so clicking a row can still push the
                // correct original index onto view_stack for navigation.
                let mut contents_rows: Vec<(usize, &Node)> = view_node
                    .children
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| !self.hidden.contains(&c.path))
                    .collect();
                let cs = self.contents_sort;
                contents_rows.sort_by(|(_, a), (_, b)| {
                    let ord = match cs.column {
                        SortColumn::Size => a.size.cmp(&b.size),
                        SortColumn::Files => a.file_count.max(1).cmp(&b.file_count.max(1)),
                        SortColumn::Name => a.name.cmp(&b.name),
                    };
                    if cs.ascending { ord } else { ord.reverse() }
                });

                let mut ext_rows = self.ext_breakdown.clone();
                let es = self.ext_sort;
                ext_rows.sort_by(|a, b| {
                    let ord = match es.column {
                        SortColumn::Size => a.1.cmp(&b.1),
                        SortColumn::Files => a.2.cmp(&b.2),
                        SortColumn::Name => a.0.cmp(&b.0),
                    };
                    if es.ascending { ord } else { ord.reverse() }
                });

                egui::Area::new("summary_overlay".into())
                    .fixed_pos(response.rect.left_top())
                    .show(&ctx, |ui| {
                        ui.set_min_size(response.rect.size());
                        egui::Frame::default().fill(ui.visuals().panel_fill).show(ui, |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                ui.heading("Contents");
                                egui::Grid::new("summary_contents_grid")
                                    .num_columns(3)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        sortable_header(ui, "Size", SortColumn::Size, &mut self.contents_sort);
                                        sortable_header(ui, "Files", SortColumn::Files, &mut self.contents_sort);
                                        sortable_header(ui, "Name", SortColumn::Name, &mut self.contents_sort);
                                        ui.end_row();
                                        for (i, c) in &contents_rows {
                                            ui.label(human_size(c.size));
                                            ui.label(format_count(c.file_count.max(1)));
                                            if ui.link(&c.name).clicked() && c.is_dir {
                                                let mut vp = self.view_stack.last().unwrap().clone();
                                                vp.push(*i);
                                                self.view_stack.push(vp);
                                            }
                                            ui.end_row();
                                        }
                                    });

                                ui.add_space(12.0);
                                ui.separator();
                                ui.heading("By file extension");
                                egui::Grid::new("summary_ext_grid")
                                    .num_columns(3)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        sortable_header(ui, "Size", SortColumn::Size, &mut self.ext_sort);
                                        sortable_header(ui, "Files", SortColumn::Files, &mut self.ext_sort);
                                        sortable_header(ui, "Extension", SortColumn::Name, &mut self.ext_sort);
                                        ui.end_row();
                                        for (ext, size, count) in &ext_rows {
                                            ui.label(human_size(*size));
                                            ui.label(format_count(*count));
                                            ui.label(ext);
                                            ui.end_row();
                                        }
                                    });
                            });
                        });
                    });
                return;
            }

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
                &mut segs,
            );

            let pointer = ctx.input(|i| i.pointer.hover_pos());
            let mut new_hover: Option<HoverInfo> = None;
            let mut hover_idx_path: Option<Vec<usize>> = None;

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
                                Some(get_node(&root, &seg.idx_path))
                            };
                            new_hover = Some(HoverInfo {
                                path: if seg.is_free {
                                    PathBuf::from(&seg.name) // "Free space" — not a real path under view_node
                                } else if seg.is_other {
                                    view_node.path.join(&seg.name)
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
                        }
                    }
                }
            }
            self.hovered = new_hover;

            if response.clicked() {
                if let Some(p) = pointer {
                    let dist = (p - center).length();
                    if dist <= hub_radius {
                        if self.view_stack.len() > 1 {
                            self.view_stack.pop();
                        }
                    } else if let Some(ip) = &hover_idx_path {
                        let node = get_node(&root, ip);
                        if node.is_dir {
                            let mut vp = self.view_stack.last().unwrap().clone();
                            vp.extend(ip.iter());
                            self.view_stack.push(vp);
                        }
                    }
                }
            }

            if response.secondary_clicked() {
                if let (Some(ip), Some(p)) = (hover_idx_path.clone(), pointer) {
                    self.context_menu = Some(ContextMenuState { node_path: ip, screen_pos: p });
                }
            }

            if let Some(cm) = self.context_menu.as_ref().map(|c| (c.node_path.clone(), c.screen_pos)) {
                let (menu_node_path, menu_screen_pos) = cm;
                let mut close = false;
                let abs_path = {
                    let mut vp = self.view_stack.last().unwrap().clone();
                    vp.extend(menu_node_path.iter());
                    vp
                };
                let target = get_node(&root, &abs_path).path.clone();
                egui::Area::new("ctx_menu".into())
                    .fixed_pos(menu_screen_pos)
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            if ui.button("Zoom").clicked() {
                                let mut vp = self.view_stack.last().unwrap().clone();
                                vp.extend(menu_node_path.iter());
                                self.view_stack.push(vp);
                                close = true;
                            }
                            if ui.button("Rescan").clicked() {
                                self.start_scan(target.clone());
                                close = true;
                            }
                            if ui.button("Open").clicked() {
                                let dir = if target.is_dir() { target.clone() } else { target.parent().map(|p| p.to_path_buf()).unwrap_or(target.clone()) };
                                let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                                close = true;
                            }
                            if ui.button("Hide").clicked() {
                                self.hidden.insert(target.clone());
                                close = true;
                            }
                            if ui.button("Move to Trash").clicked() {
                                let _ = trash::delete(&target);
                                if let Some(root) = &self.root {
                                    let n = self.current_view_node(root);
                                    let p = n.path.clone();
                                    self.start_scan(p);
                                }
                                close = true;
                            }
                            if ui.button("Delete permanently").clicked() {
                                if target.is_dir() {
                                    let _ = std::fs::remove_dir_all(&target);
                                } else {
                                    let _ = std::fs::remove_file(&target);
                                }
                                if let Some(root) = &self.root {
                                    let n = self.current_view_node(root);
                                    let p = n.path.clone();
                                    self.start_scan(p);
                                }
                                close = true;
                            }
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                        });
                    });
                if close || ctx.input(|i| i.pointer.any_click() && i.pointer.hover_pos().map_or(false, |_| false)) {
                    // menu closes on any explicit action; also allow click-away close next frame
                }
                if close {
                    self.context_menu = None;
                }
            }

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
                let path_str = h.path.display().to_string();

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
                            egui::Grid::new(egui::Id::new("hover_tooltip_grid").with(&h.path))
                                .num_columns(2)
                                .spacing([12.0, 4.0])
                                .show(ui, |ui| {
                                    ui.label(if h.is_free { "Available:" } else { "Size:" });
                                    ui.label(human_size(h.size));
                                    ui.end_row();

                                    // Always 1 for a real file, always 0 for
                                    // free space — neither is informative,
                                    // so only show it for folders and the
                                    // aggregate "other" bucket.
                                    if h.is_dir {
                                        ui.label("Files:");
                                        ui.label(format_count(h.file_count));
                                        ui.end_row();
                                    }

                                    if let Some(m) = h.mode {
                                        ui.label("Permissions:");
                                        ui.label(format_perms(m));
                                        ui.end_row();
                                    }
                                    if let Some(mt) = h.mtime {
                                        ui.label("Modified:");
                                        ui.label(format_epoch(mt));
                                        ui.end_row();
                                    }
                                    if let Some(ct) = h.ctime {
                                        ui.label("Changed:");
                                        ui.label(format_epoch(ct));
                                        ui.end_row();
                                    }
                                    if let (Some(uid), Some(gid)) = (h.uid, h.gid) {
                                        ui.label("Owner:");
                                        ui.label(format_owner(uid, gid, &mut self.user_cache, &mut self.group_cache));
                                        ui.end_row();
                                    }
                                    if let Some(mime) = &mime {
                                        ui.label("Type:");
                                        ui.label(mime);
                                        ui.end_row();
                                    }
                                });
                        });
                    });
            }
        });
    }
}

fn empty_trash() -> std::io::Result<()> {
    if let Some(home) = std::env::var_os("HOME") {
        let trash_dir = PathBuf::from(home).join(".local/share/Trash");
        for sub in ["files", "info"] {
            let d = trash_dir.join(sub);
            if d.exists() {
                for entry in std::fs::read_dir(&d)? {
                    let entry = entry?;
                    let p = entry.path();
                    if p.is_dir() {
                        std::fs::remove_dir_all(&p)?;
                    } else {
                        std::fs::remove_file(&p)?;
                    }
                }
            }
        }
    }
    Ok(())
}

// ---------------- Theming ----------------
//
// Colors are never hardcoded: they're read from the user's actual desktop
// color scheme (KDE Plasma's kdeglobals) so the app matches whatever theme
// and accent color the user picked in System Settings, light or dark,
// rather than imposing a fixed palette. Only *structural* polish (corner
// rounding, spacing) is applied on top — that's theme-agnostic by nature.

struct KdeColors {
    window_bg: Color32,
    view_bg: Color32,
    text: Color32,
    accent: Color32,
    button_bg: Color32,
}

fn parse_rgb(s: &str) -> Option<Color32> {
    let mut parts = s.trim().split(',');
    let r: u8 = parts.next()?.trim().parse().ok()?;
    let g: u8 = parts.next()?.trim().parse().ok()?;
    let b: u8 = parts.next()?.trim().parse().ok()?;
    Some(Color32::from_rgb(r, g, b))
}

fn read_kde_colors() -> Option<KdeColors> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".config/kdeglobals");
    let content = std::fs::read_to_string(path).ok()?;

    let mut section = String::new();
    let (mut window_bg, mut view_bg, mut text, mut accent, mut button_bg) =
        (None, None, None, None, None);

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line.to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (section.as_str(), key) {
            ("[Colors:Window]", "BackgroundNormal") => window_bg = parse_rgb(value),
            ("[Colors:Window]", "ForegroundNormal") => text = parse_rgb(value),
            ("[Colors:View]", "BackgroundNormal") => view_bg = parse_rgb(value),
            ("[Colors:Selection]", "BackgroundNormal") => accent = parse_rgb(value),
            ("[Colors:Button]", "BackgroundNormal") => button_bg = parse_rgb(value),
            _ => {}
        }
    }

    let window_bg = window_bg?;
    Some(KdeColors {
        view_bg: view_bg.unwrap_or(window_bg),
        text: text.unwrap_or(Color32::WHITE),
        accent: accent?,
        button_bg: button_bg.unwrap_or(window_bg),
        window_bg,
    })
}

/// Structural-only style refinements — corner rounding and spacing — that
/// read as "designed" regardless of which color scheme is active.
fn apply_structural_style(ctx: &egui::Context) {
    let radius = egui::CornerRadius::from(6u8);
    ctx.all_styles_mut(|style| {
        style.visuals.window_corner_radius = radius;
        style.visuals.menu_corner_radius = radius;
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.open.corner_radius = radius;
        style.spacing.item_spacing = egui::Vec2::new(8.0, 8.0);
        style.spacing.button_padding = egui::Vec2::new(10.0, 5.0);
        style.spacing.window_margin = egui::Margin::same(10);
    });
}

fn apply_theme(ctx: &egui::Context) {
    apply_structural_style(ctx);
    let Some(kde) = read_kde_colors() else {
        return; // not on KDE (or couldn't read it): keep egui's own default
    };
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(kde.text);
    visuals.panel_fill = kde.view_bg;
    visuals.window_fill = kde.window_bg;
    // Used as the empty "trough" for progress bars, sliders, text-edit
    // backgrounds, etc. Needs to read as visibly *lighter* than the panel
    // behind it on a dark theme — darker (as `gamma_multiply(0.85)` gave)
    // was nearly invisible, leaving no visible container/boundary for
    // things like the scan progress bar to fill up against.
    visuals.extreme_bg_color = gamma_lighten(kde.view_bg, 0.55);
    visuals.faint_bg_color = kde.window_bg.gamma_multiply(1.1);
    visuals.hyperlink_color = kde.accent;
    visuals.selection.bg_fill = kde.accent;
    visuals.selection.stroke.color = kde.text;
    visuals.widgets.inactive.bg_fill = kde.button_bg;
    visuals.widgets.inactive.weak_bg_fill = kde.button_bg;
    visuals.widgets.hovered.bg_fill = kde.button_bg.gamma_multiply(1.25);
    visuals.widgets.active.bg_fill = kde.accent;
    visuals.widgets.noninteractive.bg_fill = kde.window_bg;
    ctx.set_visuals(visuals);
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "spacemap",
        options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
            Ok(Box::new(DiskScanApp::default()))
        }),
    )
}
