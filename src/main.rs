// diskscan: a portable single-binary disk usage sunburst visualizer.
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

struct Node {
    name: String,
    path: PathBuf,
    size: u64,
    file_count: u64,
    is_dir: bool,
    children: Vec<Node>,
    mode: u32,
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
    format!("Perms: {:o} ({})", mode & 0o777, sym)
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

fn scan_dir(
    path: &Path,
    root_dev: u64,
    progress: &Sender<ScanMsg>,
    counter: &std::sync::atomic::AtomicU64,
) -> Node {
    use std::os::unix::fs::MetadataExt;
    let name = file_name_of(path);
    let self_mode = std::fs::metadata(path).map(|m| m.mode()).unwrap_or(0);
    let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(path) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(e) => {
            let _ = progress.send(ScanMsg::LogError(friendly_io_error(path, &e)));
            Vec::new()
        }
    };

    let children: Vec<Node> = entries
        .par_iter()
        .map(|entry| {
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
                            mode: meta.map(|m| m.mode()).unwrap_or(0),
                        }
                    } else {
                        scan_dir(&p, root_dev, progress, counter)
                    }
                }
                _ => {
                    let (sz, mode) = match entry.metadata() {
                        Ok(m) => (m.len(), m.mode()),
                        Err(e) => {
                            let _ = progress.send(ScanMsg::LogError(friendly_io_error(&p, &e)));
                            (0, 0)
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
                    }
                }
            };
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n % 512 == 0 {
                let _ = progress.send(ScanMsg::Progress(n));
            }
            node
        })
        .collect();

    let mut children = children;
    children.sort_by(|a, b| b.size.cmp(&a.size));
    let size = children.iter().map(|c| c.size).sum();
    let file_count = children.iter().map(|c| c.file_count).sum::<u64>() + if children.is_empty() { 0 } else { 0 };
    let file_count = if children.is_empty() { 0 } else { file_count };

    Node {
        name,
        path: path.to_path_buf(),
        size,
        file_count,
        is_dir: true,
        children,
        mode: self_mode,
    }
}

enum ScanMsg {
    Progress(u64),
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

// ---------------- Mount listing ----------------

fn list_mounts() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let skip_fs = [
        "proc", "sysfs", "devtmpfs", "tmpfs", "devpts", "cgroup", "cgroup2", "pstore", "bpf",
        "autofs", "mqueue", "hugetlbfs", "debugfs", "tracefs", "securityfs", "configfs",
        "fusectl", "binfmt_misc", "rpc_pipefs", "nsfs", "overlay", "efivarfs",
        "selinuxfs", "fuse.portal", "fuse.gvfsd-fuse", "fuse.gocryptfs", "ramfs",
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
    mode: Option<u32>,
}

struct ContextMenuState {
    node_path: Vec<usize>,
    screen_pos: Pos2,
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
}

const MAX_LOG_LINES: usize = 500;

impl Default for DiskScanApp {
    fn default() -> Self {
        let mounts = list_mounts();
        let mut app = Self {
            mounts,
            selected_mount: 0,
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
        };
        if let Some((_, path)) = app.mounts.first().cloned() {
            app.start_scan(path);
        }
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
        let is_mount_point = self.mounts.iter().any(|(_, p)| p == &path);
        self.free_space = if is_mount_point { fs_space(&path) } else { None };
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
            let root = scan_dir(&path, root_dev, &tx, &counter);
            let _ = tx.send(ScanMsg::Done(root, start.elapsed().as_secs_f64()));
        });
    }

    fn current_view_node<'a>(&self, root: &'a Node) -> &'a Node {
        let idx_path = self.view_stack.last().unwrap();
        get_node(root, idx_path)
    }

    fn poll_scan(&mut self) {
        if let Some(rx) = &self.scan_rx {
            loop {
                match rx.try_recv() {
                    Ok(ScanMsg::Progress(n)) => {
                        self.scanned_count = n;
                    }
                    Ok(ScanMsg::LogError(msg)) => {
                        if self.log.len() < MAX_LOG_LINES {
                            self.log.push(msg);
                        } else {
                            self.log_truncated += 1;
                        }
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

const MAX_RENDER_DEPTH: usize = 6;
const MIN_SEGMENT_ANGLE_DEG: f32 = 0.75;
const MAX_CHILDREN_SHOWN: usize = 64;

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
    out: &mut Vec<Segment>,
) {
    if ring >= MAX_RENDER_DEPTH {
        return;
    }
    let visible_children: Vec<(usize, &Node)> = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| !hidden.contains(&c.path))
        .collect();
    let real_total: u64 = visible_children.iter().map(|(_, c)| c.size).sum();
    let total: u64 = (real_total + extra_free_bytes).max(1);
    let span_abs = (end_angle - start_angle).abs().max(0.0001);
    let min_frac = (MIN_SEGMENT_ANGLE_DEG.to_radians() / span_abs).max(0.0);

    // Children are pre-sorted largest-first. Keep showing individual segments
    // only while they'd still be wide enough to render as a real slice; lump
    // the long tail of tiny ones into a single "other" bucket instead of
    // producing hundreds of sub-pixel slivers.
    let mut split = 0;
    for (_, c) in &visible_children {
        if split >= MAX_CHILDREN_SHOWN {
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

    let span = end_angle - start_angle;
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
            layout_sunburst(child, cp, a0, a1, ring + 1, hidden, 0, out);
        }
    }
    if rest_size > 0 {
        let frac = rest_size as f32 / total as f32;
        let a0 = cursor;
        let a1 = cursor + span * frac;
        cursor = a1;
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
        // Consume exactly the remaining span so there's no float-rounding gap.
        out.push(Segment {
            idx_path: idx_path.clone(),
            start_angle: cursor,
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

fn segment_color(seg: &Segment, top_branch_hue: f32) -> Color32 {
    if seg.is_other {
        // A pale tint of the *same* branch hue, so the aggregate bucket
        // reads as "more of this folder", not an unrelated color/glitch.
        return hsv_to_rgb(top_branch_hue, 0.38, 0.80);
    }
    let sat = 0.55;
    let val = (0.95 - (seg.ring as f32) * 0.08).max(0.45);
    hsv_to_rgb(top_branch_hue, sat, val)
}

/// Arc outline points, traced outer-arc-forward then inner-arc-backward,
/// suitable for both mesh fill (as a triangle strip) and a closed stroke.
fn arc_dir(t: f32) -> Vec2 {
    let (s, c) = t.sin_cos();
    Vec2::new(s, -c) // angle 0 = straight up, increasing clockwise
}

fn draw_arc_mesh(painter: &egui::Painter, center: Pos2, r0: f32, r1: f32, a0: f32, a1: f32, color: Color32) {
    let span = (a1 - a0).abs();
    // Tessellate based on actual on-screen arc length (at the outer radius)
    // so outer rings stay smooth instead of getting faceted/pixelated.
    let arc_len_px = span * r1.max(1.0);
    let steps = ((arc_len_px / 3.0).ceil() as usize).clamp(1, 256);

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

    // Crisp, consistent 1px border regardless of theme/background, instead
    // of relying on a radial gap that only sometimes shows through.
    let stroke = egui::Stroke::new(1.0, Color32::from_black_alpha(90));
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
        if self.scanning {
            ctx.request_repaint();
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("diskscan");
                ui.separator();
                let breadcrumb = if let Some(h) = &self.hovered {
                    h.path.display().to_string()
                } else if let Some(root) = &self.root {
                    let n = self.current_view_node(root);
                    n.path.display().to_string()
                } else {
                    String::new()
                };
                ui.monospace(breadcrumb);
            });
        });

        egui::Panel::left("left").min_size(220.0).show(ui, |ui| {
            ui.label("Drives / mount points:");
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
                ui.label(format_perms(n.mode));
            }
            if let Some(h) = &self.hovered {
                ui.separator();
                ui.label("Hovered:");
                ui.monospace(h.path.display().to_string());
                ui.label(format!("Size: {}", human_size(h.size)));
                ui.label(format!("Files: {}", format_count(h.file_count)));
                if let Some(m) = h.mode {
                    ui.label(format_perms(m));
                }
            }
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
            if self.scanning {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.label("Scanning...");
                        ui.spinner();
                        ui.label(format!("{} items scanned", format_count(self.scanned_count)));
                    });
                });
                return;
            }

            let root = match &self.root {
                Some(r) => r.clone(),
                None => return,
            };

            let avail = ui.available_size();
            let side = avail.x.min(avail.y);
            let (response, painter) =
                ui.allocate_painter(Vec2::new(avail.x, avail.y), egui::Sense::click());
            let center = response.rect.center();
            let max_radius = side / 2.0 - 10.0;
            let hub_radius = max_radius * 0.22;
            let ring_thickness = (max_radius - hub_radius) / MAX_RENDER_DEPTH as f32;

            let view_node = get_node(&root, self.view_stack.last().unwrap());

            if self.summary_view {
                egui::Area::new("summary_overlay".into())
                    .fixed_pos(response.rect.left_top())
                    .show(&ctx, |ui| {
                        ui.set_min_size(response.rect.size());
                        egui::Frame::default().fill(ui.visuals().panel_fill).show(ui, |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                for (i, c) in view_node.children.iter().enumerate() {
                                    if self.hidden.contains(&c.path) {
                                        continue;
                                    }
                                    ui.horizontal(|ui| {
                                        ui.label(format!("{:>10}", human_size(c.size)));
                                        if ui.link(&c.name).clicked() && c.is_dir {
                                            let mut vp = self.view_stack.last().unwrap().clone();
                                            vp.push(i);
                                            self.view_stack.push(vp);
                                        }
                                    });
                                }
                            });
                        });
                    });
                return;
            }

            let bg = ui.visuals().panel_fill;
            let free_color = gamma_lighten(bg, 0.7);

            // hub (center circle) - click navigates up
            painter.circle_filled(center, hub_radius, bg);
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                human_size(view_node.size),
                egui::FontId::proportional(18.0),
                Color32::WHITE,
            );

            let root_free_bytes = if self.view_stack.len() == 1 {
                self.free_space.map(|(_, free)| free).unwrap_or(0)
            } else {
                0
            };
            let mut segs = Vec::new();
            layout_sunburst(view_node, vec![], 0.0, std::f32::consts::TAU, 0, &self.hidden, root_free_bytes, &mut segs);

            let pointer = ctx.input(|i| i.pointer.hover_pos());
            let mut new_hover: Option<HoverInfo> = None;
            let mut hover_idx_path: Option<Vec<usize>> = None;

            for seg in &segs {
                let r0 = hub_radius + ring_thickness * seg.ring as f32;
                let r1 = r0 + ring_thickness;
                let top_hue = hue_for_branch(*seg.idx_path.first().unwrap_or(&0));
                let color = if seg.is_free { free_color } else { segment_color(seg, top_hue) };
                draw_arc_mesh(&painter, center, r0, r1, seg.start_angle, seg.end_angle, color);

                if let Some(p) = pointer {
                    let v = p - center;
                    let dist = v.length();
                    if dist >= r0 && dist <= r1 {
                        let mut ang = v.x.atan2(-v.y);
                        if ang < 0.0 {
                            ang += std::f32::consts::TAU;
                        }
                        if ang >= seg.start_angle && ang <= seg.end_angle {
                            new_hover = Some(HoverInfo {
                                path: if seg.is_other || seg.is_free {
                                    view_node.path.join(&seg.name)
                                } else {
                                    get_node(&root, &seg.idx_path).path.clone()
                                },
                                size: seg.size,
                                file_count: seg.file_count,
                                is_dir: seg.is_dir || seg.is_other,
                                mode: seg.mode,
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

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "diskscan",
        options,
        Box::new(|_cc| Ok(Box::new(DiskScanApp::default()))),
    )
}
