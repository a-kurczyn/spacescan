// SpaceMap: a portable single-binary disk usage sunburst visualizer.
// Clone of the "Scanner" Windows utility (sunburst chart of drive/folder usage).

use eframe::egui;
use egui::{Color32, Pos2, Vec2};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Instant;

// ---------------- Localization ----------------
//
// Every user-facing string lives in a `KEY=value` text file (lang/en.lang,
// bundled into the binary as the guaranteed fallback) instead of as Rust
// string literals, so a translation is just a text file: copy en.lang,
// translate the right-hand side, drop it in ~/.config/spacemap/lang/ as
// <code>.lang — no rebuild. A key missing from a translation falls back
// to English automatically. Where a template needs a runtime value, it
// contains a literal `%s` (printf-style, filled in order — see `Lang::t`);
// a translation can move `%s` elsewhere in the sentence but shouldn't
// remove, duplicate or relabel it.

static DEFAULT_LANG: &str = include_str!("../lang/en.lang");

fn config_dir() -> PathBuf {
    home_dir().join(".config/spacemap")
}

fn lang_dir() -> PathBuf {
    config_dir().join("lang")
}

fn config_file() -> PathBuf {
    config_dir().join("config.txt")
}

/// Parses `KEY=value` text: blank lines and lines starting with `#` are
/// skipped. `\n` and `\\` in a value are unescaped, so a translation can
/// still contain a literal newline (e.g. a multi-line tooltip).
fn parse_kv_file(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim_start().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let val = val.replace("\\n", "\n").replace("\\\\", "\\");
            map.insert(key.trim().to_string(), val);
        }
    }
    map
}

struct Lang {
    code: String,
    map: HashMap<String, String>,
}

impl Lang {
    /// Loads `code` with English merged in underneath it, so an
    /// incomplete translation still shows English for whatever key it's
    /// missing rather than a blank. "en" itself is just the bundled
    /// default, with nothing to merge.
    fn load(code: &str) -> Lang {
        let mut map = parse_kv_file(DEFAULT_LANG);
        if code != "en" {
            if let Ok(text) = std::fs::read_to_string(lang_dir().join(format!("{code}.lang"))) {
                map.extend(parse_kv_file(&text));
            }
        }
        Lang { code: code.to_string(), map }
    }

    /// Raw lookup; the key itself if even the English default doesn't
    /// have it, so a missing translation reads as an obviously-wrong key
    /// rather than silently vanishing.
    fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.map.get(key).map(|s| s.as_str()).unwrap_or(key)
    }

    /// Like `get`, but every `%s` in the template is replaced in order by
    /// one of `args`.
    fn t(&self, key: &str, args: &[&str]) -> String {
        let mut out = String::new();
        let mut rest = self.get(key);
        let mut args = args.iter();
        while let Some(pos) = rest.find("%s") {
            out.push_str(&rest[..pos]);
            out.push_str(args.next().copied().unwrap_or("%s"));
            rest = &rest[pos + 2..];
        }
        out.push_str(rest);
        out
    }
}

static LANG: LazyLock<RwLock<Lang>> = LazyLock::new(|| RwLock::new(Lang::load(&load_language_setting())));

/// A plain translated string with no placeholders.
fn tr(key: &str) -> String {
    LANG.read().unwrap().get(key).to_string()
}

/// A translated string with `%s` placeholders filled in order.
fn trf(key: &str, args: &[&str]) -> String {
    LANG.read().unwrap().t(key, args)
}

fn current_lang_code() -> String {
    LANG.read().unwrap().code.clone()
}

/// Switches the active language for every `tr`/`trf` call from the next
/// frame on (egui redraws continuously, so nothing else needs to react to
/// this explicitly) and remembers the choice for next launch.
fn set_language(code: &str) {
    *LANG.write().unwrap() = Lang::load(code);
    save_language_setting(code);
}

fn load_language_setting() -> String {
    std::fs::read_to_string(config_file())
        .ok()
        .and_then(|text| parse_kv_file(&text).get("language").cloned())
        .unwrap_or_else(|| "en".to_string())
}

fn save_language_setting(code: &str) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(config_file(), format!("language={code}\n"));
}

/// Every `<code>.lang` file in the user's lang directory, plus the bundled
/// "en" — (code, display name for the dropdown). Scanned fresh each time
/// the settings panel is open, so a file dropped in while spacemap is
/// running shows up without a restart.
fn available_languages() -> Vec<(String, String)> {
    let mut out = vec![("en".to_string(), "English".to_string())];
    if let Ok(rd) = std::fs::read_dir(lang_dir()) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "lang") {
                continue;
            }
            let Some(code) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else { continue };
            if code == "en" {
                continue;
            }
            let name = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| text.lines().next().map(str::to_string))
                .and_then(|first| first.strip_prefix("# name:").map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| code.clone());
            out.push((code, name));
        }
    }
    out
}

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
    /// Birth (creation) time, 0 when the filesystem doesn't report one.
    btime: i64,
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
        btime: 0,
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
    let rel = match target_path.strip_prefix(&node.path) {
        Ok(r) => r,
        Err(_) => return, // not actually a descendant; ignore
    };
    let Some(first) = rel.components().next() else {
        return;
    };
    let first = first.as_os_str();
    // Match on the final path component (a plain byte compare) rather than
    // Path::starts_with, which re-parses both paths component by component
    // for every sibling — that dominated frame time on wide directories.
    let idx = match node.children.iter().position(|c| c.path.file_name() == Some(first)) {
        Some(i) => i,
        None => {
            let child_path = node.path.join(first);
            node.children.push(Node {
                name: first.to_string_lossy().to_string(),
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
                btime: 0,
            });
            node.children.len() - 1
        }
    };
    graft_slice(&mut node.children[idx], target_path, size, file_count, mode, mtime, ctime, uid, gid);
    node.size = node.children.iter().map(|c| c.size).sum();
    node.file_count = node.children.iter().map(|c| c.file_count).sum();
    // Children stay sorted by size (descending) and only children[idx]
    // changed, so move just that one into place instead of re-sorting (a
    // full stable sort allocates a scratch buffer on every call).
    reposition_by_size(&mut node.children, idx);
}

/// Restores descending-by-size order after only `children[idx]` changed,
/// giving the same result as a stable sort: equal-sized siblings keep their
/// original order relative to the moved child.
fn reposition_by_size(children: &mut [Node], idx: usize) {
    let size = children[idx].size;
    // Moving up: pass earlier siblings that are smaller; equal ones stay ahead.
    let dest = children[..idx].partition_point(|c| c.size >= size);
    if dest < idx {
        children[dest..=idx].rotate_right(1);
        return;
    }
    // Moving down: pass later siblings that are larger; equal ones stay behind.
    let after = &children[idx + 1..];
    let dest = idx + after.partition_point(|c| c.size > size);
    if dest > idx {
        children[idx..=dest].rotate_left(1);
    }
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
            .unwrap_or_else(|| tr("EXT_NO_EXTENSION"));
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
        v.push((trf("EXT_OTHER_COUNT", &[&rest.len().to_string()]), size, count));
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
        NotFound => tr("ERR_IO_NOT_FOUND"),
        PermissionDenied => tr("ERR_IO_PERMISSION"),
        _ => trf("ERR_IO_OTHER", &[&e.to_string()]),
    };
    trf("ERR_IO_LINE", &[&path.display().to_string(), &what])
}

/// Scans a single directory entry: recurses if it's a (same-filesystem)
/// directory, otherwise builds a leaf file Node. Shared by `scan_dir`'s
/// normal recursion and the top-level streaming scan in `start_scan`.
/// Birth (creation) time in Unix seconds via statx; 0 if unavailable.
fn birth_secs(m: &std::fs::Metadata) -> i64 {
    m.created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

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
                    name: trf("SEG_OTHER_FS", &[&file_name_of(&p)]),
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
                    btime: meta.as_ref().map(birth_secs).unwrap_or(0),
                }
            } else {
                scan_dir(&p, root_dev, progress, counter, cancel, progress_interval)
            }
        }
        _ => {
            let (sz, mode, mtime, ctime, uid, gid, btime) = match entry.metadata() {
                Ok(m) => (m.len(), m.mode(), m.mtime(), m.ctime(), m.uid(), m.gid(), birth_secs(&m)),
                Err(e) => {
                    let _ = progress.send(ScanMsg::LogError(friendly_io_error(&p, &e)));
                    (0, 0, 0, 0, 0, 0, 0)
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
                btime,
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
            btime: 0,
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
        btime: self_meta.as_ref().map(birth_secs).unwrap_or(0),
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

/// Live result of the counting pass. Shared directly rather than sent as
/// ScanMsgs: the scan channel can hold a long backlog of SliceDones the UI
/// grafts a few milliseconds' worth at a time, and a total that only arrives
/// after that backlog is useless to the progress bar.
#[derive(Default)]
struct EntryCount {
    found: std::sync::atomic::AtomicU64,
    done: std::sync::atomic::AtomicBool,
}

/// Counts every entry under `path` that the scan will visit (same
/// filesystem, symlinks not followed), using only directory listings — no
/// per-file stat, which is most of the real scan's cost — so it normally
/// finishes well ahead of the scan and gives the progress bar a real total
/// for folders that aren't whole drives.
fn count_entries(path: &Path, root_dev: u64, found: &std::sync::atomic::AtomicU64, stop: &(dyn Fn() -> bool + Sync)) {
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::Ordering;
    if stop() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(path) else { return };
    let entries: Vec<std::fs::DirEntry> = rd.filter_map(|e| e.ok()).collect();
    found.fetch_add(entries.len() as u64, Ordering::Relaxed);
    entries.par_iter().for_each(|e| {
        let is_dir = e.file_type().is_ok_and(|ft| ft.is_dir() && !ft.is_symlink());
        // Only directories need a stat, to stay on the same filesystem.
        if is_dir && e.metadata().is_ok_and(|m| m.dev() == root_dev) {
            count_entries(&e.path(), root_dev, found, stop);
        }
    });
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
/// its parent's) — i.e. a genuine mount point. Works for any starting point
/// (Root/Home buttons, the folder picker, or a path typed into the path
/// bar), including network shares and just-plugged-in drives.
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

/// Opens the desktop's native folder chooser (XDG portal — the Plasma dialog
/// on KDE, which lists every drive, mount point and folder) on a background
/// thread so the UI keeps repainting while it's open. The result (None if
/// cancelled) arrives on the returned channel.
fn pick_folder_async(start_dir: Option<PathBuf>) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new().set_title(tr("DIALOG_PICK_FOLDER_TITLE"));
        if let Some(dir) = start_dir {
            dialog = dialog.set_directory(dir);
        }
        let _ = tx.send(dialog.pick_folder());
    });
    rx
}

/// Size / file count / permissions of the folder being viewed.
fn folder_stats_ui(ui: &mut egui::Ui, n: &Node) {
    // One line per stat: never wrap (the corner overlay's area is narrow).
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    ui.label(trf("STATS_SIZE", &[&human_size(n.size)]));
    ui.label(trf("STATS_FILES", &[&format_count(n.file_count)]));
    ui.label(trf("STATS_PERMS", &[&format_perms(n.mode)]));
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

// ---------------- Filters ----------------

/// The filter panel's fields exactly as typed. An empty field means "no limit".
#[derive(Clone, Default, PartialEq)]
struct FilterForm {
    name: String,
    /// Off by default (name patterns match regardless of case) — the "Aa"
    /// toggle next to the Name field flips this.
    case_sensitive: bool,
    min_size: String,
    max_size: String,
    min_created: String,
    max_created: String,
    min_modified: String,
    max_modified: String,
}

/// FilterForm parsed into something cheap to test every file against.
struct CompiledFilter {
    /// Name patterns, lowercased unless `case_sensitive` — a file matches
    /// if any one does. Patterns with `*`/`?` must match the whole name,
    /// others match as a substring.
    names: Vec<String>,
    case_sensitive: bool,
    min_size: Option<u64>,
    max_size: Option<u64>,
    min_created: Option<i64>,
    max_created: Option<i64>,
    min_modified: Option<i64>,
    max_modified: Option<i64>,
}

impl CompiledFilter {
    /// Ok(None) when every field is empty (nothing to filter).
    fn compile(f: &FilterForm) -> Result<Option<Self>, String> {
        let size = |s: &str, what: &str| -> Result<Option<u64>, String> {
            if s.trim().is_empty() { Ok(None) } else { parse_size(s).map(Some).map_err(|e| trf("ERR_FIELD_PREFIX", &[what, &e])) }
        };
        let date = |s: &str, what: &str, end_of_day: bool| -> Result<Option<i64>, String> {
            if s.trim().is_empty() { Ok(None) } else { parse_date(s, end_of_day).map(Some).map_err(|e| trf("ERR_FIELD_PREFIX", &[what, &e])) }
        };
        let c = CompiledFilter {
            names: split_name_patterns(&f.name)
                .iter()
                .flat_map(|p| expand_alternatives(p))
                .map(|p| if f.case_sensitive { p } else { p.to_lowercase() })
                .collect(),
            case_sensitive: f.case_sensitive,
            min_size: size(&f.min_size, &tr("FILTER_ERR_MIN_SIZE"))?,
            max_size: size(&f.max_size, &tr("FILTER_ERR_MAX_SIZE"))?,
            min_created: date(&f.min_created, &tr("FILTER_ERR_CREATED_FROM"), false)?,
            max_created: date(&f.max_created, &tr("FILTER_ERR_CREATED_TO"), true)?,
            min_modified: date(&f.min_modified, &tr("FILTER_ERR_MODIFIED_FROM"), false)?,
            max_modified: date(&f.max_modified, &tr("FILTER_ERR_MODIFIED_TO"), true)?,
        };
        let empty = c.names.is_empty()
            && c.min_size.is_none() && c.max_size.is_none()
            && c.min_created.is_none() && c.max_created.is_none()
            && c.min_modified.is_none() && c.max_modified.is_none();
        Ok(if empty { None } else { Some(c) })
    }

    fn matches_file(&self, n: &Node) -> bool {
        let in_range = |v: i64, lo: Option<i64>, hi: Option<i64>| lo.is_none_or(|lo| v >= lo) && hi.is_none_or(|hi| v <= hi);
        if self.min_size.is_some_and(|m| n.size < m) || self.max_size.is_some_and(|m| n.size > m) {
            return false;
        }
        if !in_range(n.mtime, self.min_modified, self.max_modified) {
            return false;
        }
        if self.min_created.is_some() || self.max_created.is_some() {
            // Unknown creation time can't satisfy a creation-date limit.
            if n.btime == 0 || !in_range(n.btime, self.min_created, self.max_created) {
                return false;
            }
        }
        if !self.names.is_empty() {
            let name = if self.case_sensitive { n.name.clone() } else { n.name.to_lowercase() };
            let hit = self.names.iter().any(|p| {
                if p.contains(['*', '?']) { glob_match(p, &name) } else { name.contains(p.as_str()) }
            });
            if !hit {
                return false;
            }
        }
        true
    }
}

/// Splits "*.iso, *.[mkv,mp4] backup" into patterns: whitespace, `,` and
/// `;` separate patterns, except inside `[...]`/`{...}` lists.
fn split_name_patterns(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '[' | '{' => { depth += 1; cur.push(ch); }
            ']' | '}' => { depth -= 1; cur.push(ch); }
            c if depth <= 0 && (c.is_whitespace() || c == ',' || c == ';') => {
                if !cur.is_empty() { out.push(std::mem::take(&mut cur)); }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

/// Expands `*.[mkv,mp4]` (or `*.{mkv,mp4}`) into `*.mkv`, `*.mp4`. Brackets
/// here are comma-separated alternatives, not regex-style character classes.
fn expand_alternatives(p: &str) -> Vec<String> {
    let Some(open) = p.find(['[', '{']) else { return vec![p.to_string()] };
    let close_ch = if p.as_bytes()[open] == b'[' { ']' } else { '}' };
    let Some(close) = p[open..].find(close_ch).map(|i| open + i) else { return vec![p.to_string()] };
    let (head, inner, tail) = (&p[..open], &p[open + 1..close], &p[close + 1..]);
    inner
        .split(',')
        .map(str::trim)
        .flat_map(|alt| expand_alternatives(&format!("{head}{alt}{tail}")))
        .collect()
}

/// Whole-string wildcard match: `*` = any run of characters, `?` = one.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// "1.5G", "500 MB", "100k", "4096" (plain bytes). Units are powers of 1024.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim().to_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = (s[..split].trim(), s[split..].trim());
    let n: f64 = num.parse().map_err(|_| trf("ERR_SIZE_FORMAT", &[s.as_str()]))?;
    let mult: u64 = match unit.trim_end_matches("ib").trim_end_matches('b') {
        "" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        _ => return Err(trf("ERR_SIZE_UNIT", &[unit])),
    };
    Ok((n * mult as f64) as u64)
}

/// "2026-09-01" or "2026-09-01 14:30" in local time, as Unix seconds. A bare
/// date means the start of that day, or its last second for an upper limit.
fn parse_date(s: &str, end_of_day: bool) -> Result<i64, String> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
    let s = s.trim();
    let dt = if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        dt
    } else {
        let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| trf("ERR_DATE_FORMAT", &[s]))?;
        if end_of_day { d.and_hms_opt(23, 59, 59).unwrap() } else { d.and_hms_opt(0, 0, 0).unwrap() }
    };
    Local
        .from_local_datetime(&dt)
        .earliest()
        .map(|d| d.timestamp())
        .ok_or_else(|| trf("ERR_DATE_TZ", &[s]))
}

/// Copy of `n` keeping only files that match `f`, with folder sizes and file
/// counts recomputed from what's left. Folders with no matches are dropped.
fn filter_tree(n: &Node, f: &CompiledFilter) -> Option<Node> {
    if !n.is_dir {
        return f.matches_file(n).then(|| n.clone());
    }
    let mut children: Vec<Node> = n.children.par_iter().filter_map(|c| filter_tree(c, f)).collect();
    if children.is_empty() {
        return None;
    }
    children.sort_by(|a, b| b.size.cmp(&a.size));
    Some(Node {
        name: n.name.clone(),
        path: n.path.clone(),
        size: children.iter().map(|c| c.size).sum(),
        file_count: children.iter().map(|c| c.file_count).sum(),
        is_dir: true,
        children,
        mode: n.mode,
        mtime: n.mtime,
        ctime: n.ctime,
        uid: n.uid,
        gid: n.gid,
        btime: n.btime,
    })
}

/// Re-finds the folder at `idx` (child indices from `old`'s root) in `new`,
/// matching by path; stops at the deepest folder that still exists.
fn remap_index_path(old: &Node, new: &Node, idx: &[usize]) -> Vec<usize> {
    let (mut o, mut n) = (old, new);
    let mut out = Vec::new();
    for &i in idx {
        let Some(oc) = o.children.get(i) else { break };
        let Some(j) = n.children.iter().position(|c| c.path == oc.path) else { break };
        out.push(j);
        o = oc;
        n = &n.children[j];
    }
    out
}

/// Child-index path from `root` down to `target`, if it's in the tree.
fn index_path_to(root: &Node, target: &Path) -> Option<Vec<usize>> {
    let rel = target.strip_prefix(&root.path).ok()?;
    let mut n = root;
    let mut out = Vec::new();
    for comp in rel.components() {
        let j = n.children.iter().position(|c| c.is_dir && c.path.file_name() == Some(comp.as_os_str()))?;
        out.push(j);
        n = &n.children[j];
    }
    Some(out)
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
    Modified,
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
    /// Bypasses both `min_segment_angle_deg` and `max_children_shown`,
    /// showing every child as its own slice with no "(N other items)"
    /// bucket — a ring can end up with thousands of sliver-thin slices on
    /// a folder with that many entries, relying on zoom (Ctrl+wheel) to
    /// make them individually clickable rather than any aggregation.
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
            progress_interval_pow2: 9, // 1 << 9 == 512, the original hardcoded value
        }
    }
}

struct DiskScanApp {
    /// Last completed scan, unfiltered. `root` is what's displayed: this
    /// same tree, or a filtered copy of it while a filter is applied.
    full_root: Option<Arc<Node>>,
    show_filters: bool,
    /// Filter panel fields as currently typed / as last applied.
    filter_form: FilterForm,
    filter_applied: FilterForm,
    filter: Option<Arc<CompiledFilter>>,
    filter_error: Option<String>,
    /// Path bar shows clickable folder segments unless this is set, in
    /// which case it's a text field for typing a path.
    path_editing: bool,
    path_edit_focus_pending: bool,
    /// Pending result of the Search button's folder dialog, while it's open.
    folder_pick_rx: Option<Receiver<Option<PathBuf>>>,
    root: Option<Arc<Node>>,
    view_stack: Vec<Vec<usize>>, // stack of index-paths; last = current view root
    scanning: bool,
    scan_rx: Option<Receiver<ScanMsg>>,
    scanned_count: u64,
    /// Counting pass for the current scan (folder scans only).
    entry_count: Option<Arc<EntryCount>>,
    /// Highest progress fraction shown this scan, so the bar never moves
    /// backwards while the counting pass is still raising the total.
    progress_shown: f32,
    scan_start: Instant,
    hidden: HashSet<PathBuf>,
    hovered: Option<HoverInfo>,
    context_menu: Option<ContextMenuState>,
    summary_view: bool,
    chart_order: ChartOrder,
    /// Chart size relative to fitting its area; Ctrl+mouse wheel changes it.
    chart_scale: f32,
    /// Pan of a zoomed-in chart from its centred position (drag to move).
    chart_offset: Vec2,
    /// Slice highlighted by the arrow keys / navigation cross.
    selection: Option<ChartSel>,
    /// Selectable slices of the chart drawn this frame: (idx_path relative
    /// to the view, start angle). Empty when no chart is on screen.
    chart_segs: Vec<(Vec<usize>, f32)>,
    /// Whether a text field had keyboard focus as this frame began, so keys
    /// typed into it (Enter in particular) aren't also taken by the chart.
    typing: bool,
    /// Arrow key just pressed, lit on the navigation cross for a moment.
    nav_flash: Option<(NavDir, Instant)>,
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
        let (mime_tx, mime_rx) = channel();
        let app = Self {
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
            context_menu: None,
            summary_view: false,
            chart_order: ChartOrder::Size,
            chart_scale: 1.0,
            chart_offset: Vec2::ZERO,
            selection: None,
            chart_segs: Vec::new(),
            typing: false,
            nav_flash: None,
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
        self.selection = None;
        self.progress_shown = 0.0;
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
        // Whole drives measure progress against the filesystem's used bytes
        // (free_space); anything else needs a counted total instead.
        let entry_count = self.free_space.is_none().then(|| Arc::new(EntryCount::default()));
        self.entry_count = entry_count.clone();
        std::thread::spawn(move || {
            let counter = std::sync::atomic::AtomicU64::new(0);
            let start = Instant::now();
            if !path.exists() {
                let _ = tx.send(ScanMsg::Error(trf("ERR_PATH_NOT_FOUND", &[&path.display().to_string()])));
                return;
            }
            let root_dev = match std::fs::metadata(&path) {
                Ok(m) => {
                    use std::os::unix::fs::MetadataExt;
                    m.dev()
                }
                Err(e) => {
                    let _ = tx.send(ScanMsg::Error(trf("ERR_CANNOT_STAT", &[&path.display().to_string(), &e.to_string()])));
                    return;
                }
            };
            let scan_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
            if let Some(count) = entry_count {
                let (path, cancel, scan_finished) = (path.clone(), cancel.clone(), scan_finished.clone());
                std::thread::spawn(move || {
                    use std::sync::atomic::Ordering;
                    let stop = || cancel.load(Ordering::Relaxed) || scan_finished.load(Ordering::Relaxed);
                    count_entries(&path, root_dev, &count.found, &stop);
                    if !stop() {
                        count.done.store(true, Ordering::Relaxed);
                    }
                });
            }
            // scan_dir streams a SliceDone for every directory as it
            // finishes (any depth), so the sunburst blossoms slice by slice
            // throughout the scan — see SliceDone's doc comment.
            let root = scan_dir(&path, root_dev, &tx, &counter, &cancel, progress_interval);
            scan_finished.store(true, std::sync::atomic::Ordering::Relaxed);
            if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tx.send(ScanMsg::Done(root, start.elapsed().as_secs_f64()));
            }
        });
    }

    /// Recomputes the displayed tree from the last full scan and the applied
    /// filter, keeping each view in the zoom history pointed at the same
    /// folder (or its deepest surviving ancestor).
    fn rebuild_view_tree(&mut self) {
        let Some(full) = self.full_root.clone() else { return };
        let new_root = match &self.filter {
            Some(f) => Arc::new(filter_tree(&full, f).unwrap_or_else(|| Node {
                name: full.name.clone(),
                path: full.path.clone(),
                size: 0,
                file_count: 0,
                children: Vec::new(),
                ..empty_node()
            })),
            None => full,
        };
        if let Some(old) = &self.root {
            self.view_stack = self.view_stack.iter().map(|vp| remap_index_path(old, &new_root, vp)).collect();
            self.view_stack.dedup();
        }
        self.root = Some(new_root);
        self.ext_breakdown_for = None;
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

    /// Ctrl+mouse wheel over the chart resizes it (egui reports Ctrl+wheel
    /// as a zoom delta, not a scroll), keeping the point under the pointer
    /// fixed; dragging pans it while it's bigger than its area. Returns the
    /// chart's centre and outer radius for this frame.
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
        // Pan only as far as the chart overhangs its area, so it can't be
        // dragged away; at 100% or less that's zero and it stays centred.
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
        self.status = tr("STATUS_SCAN_ABORTED");
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

    /// The highlighted slice (idx_path relative to the current view), if
    /// it belongs to the view being shown.
    fn selected_rel(&self) -> Option<&Vec<usize>> {
        self.selection.as_ref().filter(|s| Some(&s.view) == self.view_stack.last()).map(|s| &s.rel)
    }

    /// Moves the slice highlight: ←/→ = previous/next slice sharing the same
    /// parent (clockwise chart order, wrapping), ↓ = first slice on the next
    /// ring out, ↑ = parent slice — or, from the inner ring, the parent
    /// folder's chart with the folder just left highlighted. With nothing
    /// highlighted yet, any arrow starts at the first inner-ring slice.
    fn move_selection(&mut self, dir: NavDir) {
        let view = self.view_stack.last().unwrap().clone();
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
                NavDir::Down => first_where(&|p| p.len() == cur.len() + 1 && p.starts_with(&cur)),
                NavDir::Up if cur.len() > 1 => Some(cur[..cur.len() - 1].to_vec()),
                NavDir::Up => {
                    // Inner ring: step the view out to the parent folder and
                    // highlight the folder we were in.
                    if let Some((&left, parent_view)) = view.split_last() {
                        let parent_view = parent_view.to_vec();
                        *self.view_stack.last_mut().unwrap() = parent_view.clone();
                        self.selection = Some(ChartSel { view: parent_view, rel: vec![left] });
                    }
                    return;
                }
            },
        };
        if let Some(rel) = new_rel {
            self.selection = Some(ChartSel { view, rel });
        }
    }

    /// Enter: same as clicking the highlighted slice — opens it if it's a
    /// folder.
    fn open_selection(&mut self) {
        let Some(rel) = self.selected_rel().cloned() else { return };
        if is_other_marker(&rel) {
            self.open_other_bucket(&rel);
            return;
        }
        let Some(root) = self.root.clone() else { return };
        let view = self.view_stack.last().unwrap().clone();
        if try_get_node(get_node(&root, &view), &rel).is_some_and(|n| n.is_dir) {
            let mut vp = view;
            vp.extend(rel);
            self.view_stack.push(vp);
            self.selection = None;
        }
    }

    /// "Other" isn't one navigable node, so instead of zooming into it,
    /// this zooms into the folder that *owns* it (its idx_path minus the
    /// trailing OTHER_MARKER — a no-op if that's the folder already being
    /// viewed) and switches to Summary view, landing on a table of exactly
    /// the items that were grouped away, whichever ring they were in.
    fn open_other_bucket(&mut self, ip: &[usize]) {
        let owner_rel = &ip[..ip.len() - 1];
        if !owner_rel.is_empty() {
            let mut vp = self.view_stack.last().unwrap().clone();
            vp.extend(owner_rel.iter().copied());
            self.view_stack.push(vp);
        }
        self.summary_view = true;
        self.selection = None;
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
        //
        // The cap is a time budget, not a message count: grafting gets more
        // expensive as the tree grows, so a fixed count that's fine early in
        // a scan could still stall a frame for seconds late in a big one.
        const FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(12);
        let drain_start = Instant::now();
        let mut processed = 0u32;
        if let Some(rx) = &self.scan_rx {
            loop {
                if processed % 64 == 0 && processed > 0 && drain_start.elapsed() >= FRAME_BUDGET {
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
                        self.full_root = Some(Arc::new(node));
                        self.rebuild_view_tree();
                        self.scanning = false;
                        self.status = trf("STATUS_SCAN_COMPLETED", &[&format!("{:.1}", secs)]);
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

/// Slice highlighted in the chart: `rel` is relative to the view `view`.
struct ChartSel {
    view: Vec<usize>,
    rel: Vec<usize>,
}

/// Trailing "index" tagging an "other"-bucket idx_path — see where it's
/// pushed in `layout_sunburst`.
const OTHER_MARKER: usize = usize::MAX;

fn is_other_marker(idx_path: &[usize]) -> bool {
    idx_path.last() == Some(&OTHER_MARKER)
}

/// Like get_node, but None instead of panicking on a stale index path.
fn try_get_node<'a>(root: &'a Node, idx_path: &[usize]) -> Option<&'a Node> {
    idx_path.iter().try_fold(root, |n, &i| n.children.get(i))
}

/// A step of the navigation cross / arrow keys.
#[derive(Clone, Copy, PartialEq)]
enum NavDir {
    Up,
    Prev,
    Next,
    Down,
}

/// Clockwise order of slices around each ring of the sunburst.
#[derive(Clone, Copy, PartialEq)]
enum ChartOrder {
    /// Largest first (how children are stored).
    Size,
    /// Alphabetical, case-insensitive.
    Name,
}

fn cmp_names(a: &Node, b: &Node) -> std::cmp::Ordering {
    a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name))
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
    order: ChartOrder,
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
    // producing hundreds of sub-pixel slivers. Unless `unlimited_slices` is
    // on, in which case every child gets its own slice regardless — zoom
    // (Ctrl+wheel) is the only way to make sliver-thin ones clickable then.
    let mut split = 0;
    if settings.unlimited_slices {
        split = visible_children.len();
    } else {
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
    }
    let mut shown: Vec<(usize, &Node)> = visible_children.iter().take(split).cloned().collect();
    // Which children get their own slice is always decided by size (above),
    // so A–Z order never pushes a big folder into "other"; only the drawing
    // order of the shown slices changes. "Other" stays last either way.
    if order == ChartOrder::Name {
        shown.sort_by(|(_, a), (_, b)| cmp_names(a, b));
    }
    let rest: Vec<(usize, &Node)> = visible_children.iter().skip(split).cloned().collect();
    let rest_size: u64 = rest.iter().map(|(_, c)| c.size).sum();

    // "Other" is a "there's more, but no room to show it individually"
    // marker, not a value-proportional bucket: it gets a fixed minimum
    // width (the same min-slice-angle threshold that decided those items
    // were too small/numerous to show on their own), and every shown
    // slice stretches to fill whatever space that leaves — sized against
    // just the shown total, not the grand total that included what got
    // grouped away. Otherwise "other" could end up as the single biggest
    // wedge in the ring purely because a lot of mid-sized items landed
    // past the count cap, which is what made it read as disproportionate.
    let other_frac = if rest_size > 0 { min_frac.min(0.5) } else { 0.0 };
    let available_frac = (1.0 - other_frac).max(0.0);
    let shown_total = shown.iter().map(|(_, c)| c.size).sum::<u64>().max(1) as f32;

    let span = content_end_angle - start_angle;
    let mut cursor = start_angle;

    for (i, child) in &shown {
        let frac = (child.size as f32 / shown_total) * available_frac;
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
            layout_sunburst(child, cp, a0, a1, ring + 1, hidden, 0, 0, settings, order, out);
        }
    }
    if rest_size > 0 {
        // content_end_angle, not cursor + span*other_frac: the exact
        // remaining boundary, so there's no float-drift gap between the
        // last shown slice and "other".
        let a0 = cursor;
        let a1 = content_end_angle;
        // Tagged with OTHER_MARKER as a trailing "index" so its idx_path is
        // the same length as its visual sibling slices (real children get
        // idx_path + their own index appended) — that's what lets arrow-key
        // navigation treat it as a normal sibling to move between. It's not
        // a real child index (get_node silently no-ops on the out-of-range
        // lookup and resolves to the parent, which is what the hover
        // tooltip wants anyway); is_other_marker() is the strict check used
        // wherever code needs to tell it apart from an actual node.
        let mut other_path = idx_path.clone();
        other_path.push(OTHER_MARKER);
        out.push(Segment {
            idx_path: other_path,
            start_angle: a0,
            end_angle: a1,
            ring,
            name: trf("SEG_OTHER_ITEMS", &[&rest.len().to_string()]),
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
            name: tr("SEG_FREE_SPACE"),
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

/// Table/grid icon for the Summary view toggle — a bordered rect with a
/// header divider and two column dividers, in the same flat single-color
/// style as the file/folder icons above (chosen over the previous 📊 emoji,
/// which rendered in full color via the system emoji font).
fn draw_table_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    painter.rect_stroke(rect, egui::CornerRadius::from(1u8), stroke, egui::StrokeKind::Outside);
    let header_y = rect.top() + rect.height() * 0.32;
    painter.line_segment([Pos2::new(rect.left(), header_y), Pos2::new(rect.right(), header_y)], stroke);
    let col1_x = rect.left() + rect.width() * 0.38;
    let col2_x = rect.left() + rect.width() * 0.69;
    painter.line_segment([Pos2::new(col1_x, header_y), Pos2::new(col1_x, rect.bottom())], stroke);
    painter.line_segment([Pos2::new(col2_x, header_y), Pos2::new(col2_x, rect.bottom())], stroke);
}

/// Funnel icon for the Filters toggle — same flat single-color style,
/// chosen over the previous ▽ glyph.
fn draw_filter_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    let stem_half = rect.width() * 0.12;
    let cx = rect.center().x;
    let neck_y = rect.top() + rect.height() * 0.55;
    let points = vec![
        rect.left_top(),
        rect.right_top(),
        Pos2::new(cx + stem_half, neck_y),
        Pos2::new(cx + stem_half, rect.bottom()),
        Pos2::new(cx - stem_half, rect.bottom()),
        Pos2::new(cx - stem_half, neck_y),
    ];
    painter.add(egui::Shape::closed_line(points, stroke));
}

/// A toolbar button whose face is a hand-drawn flat icon (via `draw`)
/// instead of text/emoji — an empty-label `Button` for correct
/// hit-testing/hover/selected styling, with the icon painted over it
/// afterward using the same resolved color the button would have used for
/// text in that state (idle/hovered/selected), so it blends in exactly
/// like a normal labeled button would.
fn icon_toolbar_button(
    ui: &mut egui::Ui,
    selected: bool,
    enabled: bool,
    draw: impl FnOnce(&egui::Painter, egui::Rect, Color32),
) -> egui::Response {
    let size = ui.spacing().interact_size.y;
    let resp = ui.add_enabled(enabled, egui::Button::new("").selected(selected).min_size(Vec2::splat(size)));
    let color = ui.style().interact_selectable(&resp, selected).text_color();
    let icon_rect = resp.rect.shrink(resp.rect.width() * 0.24);
    draw(ui.painter(), icon_rect, color);
    resp
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

/// Outline of one sunburst slice (same geometry as draw_arc_mesh).
fn draw_arc_outline(painter: &egui::Painter, center: Pos2, r0: f32, r1: f32, a0: f32, a1: f32, stroke: egui::Stroke) {
    let steps = (((a1 - a0).abs() * r1.max(1.0) / 3.0).ceil() as usize).clamp(1, 512);
    let arc = |r: f32| (0..=steps).map(move |i| center + arc_dir(a0 + (a1 - a0) * (i as f32 / steps as f32)) * r);
    let mut pts: Vec<Pos2> = arc(r1).collect();
    let mut inner: Vec<Pos2> = arc(r0).collect();
    inner.reverse();
    pts.extend(inner);
    painter.add(egui::Shape::closed_line(pts, stroke));
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
        self.typing = ctx.text_edit_focused();
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
        let mut toggle_summary = false;
        let mut toggle_order = false;
        let chart_order = self.chart_order;
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
                        let label = a.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| a.display().to_string());
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
                        .on_hover_text(trf("TOOLBAR_SCAN_HOME", &[&home.display().to_string()]))
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
                            .map(|p| p.display().to_string())
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
                                        if ui.add(btn).on_hover_text(path.display().to_string()).clicked() && i != last {
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
                |ui| {
                    if ui
                        .add(egui::Button::new("⚙").selected(settings_open))
                        .on_hover_text(tr("SETTINGS_TITLE"))
                        .clicked()
                    {
                        settings_toggled = true;
                    }
                    // Right-to-left layout: each further item added sits to
                    // the left of the previous one, so this whole group
                    // lands between the path field and the settings button,
                    // divided from both by a separator.
                    ui.separator();

                    // View toggle, filters + trash (formerly the left
                    // sidebar, now placed after the path field instead).
                    if ui.button("🗑").on_hover_text(tr("TOOLBAR_EMPTY_TRASH")).clicked() {
                        empty_bin = true;
                    }
                    if icon_toolbar_button(ui, filter_active, true, draw_filter_icon)
                        .on_hover_text(if filter_active { tr("TOOLBAR_FILTERS_ACTIVE") } else { tr("FILTER_TITLE") })
                        .clicked()
                    {
                        filters_toggled = true;
                    }
                    // Shows the current order; click to switch. The Summary
                    // view's tables have their own sortable headers. Plain
                    // text rather than an emoji/custom icon: the two states
                    // read clearly as "9-1" (largest first) vs "A-Z" — a
                    // plain hyphen rather than a "→" arrow, which egui's
                    // bundled font doesn't have a glyph for and rendered as
                    // a stray dash instead.
                    let (order_icon, order_tip) = match chart_order {
                        ChartOrder::Size => ("9-1", tr("TOOLBAR_SORT_BY_SIZE")),
                        ChartOrder::Name => ("A-Z", tr("TOOLBAR_SORT_BY_NAME")),
                    };
                    if ui
                        .add_enabled(!summary_on, egui::Button::new(order_icon))
                        .on_hover_text(order_tip)
                        .clicked()
                    {
                        toggle_order = true;
                    }
                    if icon_toolbar_button(ui, summary_on, true, draw_table_icon)
                        .on_hover_text(tr("TOOLBAR_SUMMARY_VIEW"))
                        .clicked()
                    {
                        toggle_summary = true;
                    }
                    ui.separator();
                },
            );
        });

        self.path_input = path_input;
        self.path_input_focused = path_input_focused;
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
        if toggle_summary {
            self.summary_view = !self.summary_view;
        }
        if toggle_order {
            self.chart_order = match self.chart_order {
                ChartOrder::Size => ChartOrder::Name,
                ChartOrder::Name => ChartOrder::Size,
            };
        }
        if empty_bin {
            let _ = empty_trash();
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
                let p = PathBuf::from(trimmed);
                if p.is_dir() {
                    self.start_scan(p);
                } else {
                    self.log_issue(trf("ERR_NOT_A_DIRECTORY", &[&p.display().to_string()]));
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

        // Filters: narrow what the chart/summary show to matching files.
        // Applied on demand (Enter or Apply) since re-filtering a big scan
        // isn't free; folders are re-totalled from the files that match.
        if self.show_filters {
            egui::Panel::right("filter_panel")
                .resizable(true)
                .default_size(340.0)
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
                    let field = |ui: &mut egui::Ui, value: &mut String, hint: &str| -> bool {
                        let size = Vec2::new(130.0, ui.spacing().interact_size.y);
                        let r = ui.add_sized(size, egui::TextEdit::singleline(value).hint_text(hint));
                        r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    };
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
                        enter |= field(ui, &mut f.min_size, &tr("FILTER_HINT_SIZE_MIN"));
                        enter |= field(ui, &mut f.max_size, &tr("FILTER_HINT_SIZE_MAX"));
                        ui.end_row();
                        ui.label(tr("FILTER_ROW_CREATED"));
                        enter |= field(ui, &mut f.min_created, &tr("FILTER_HINT_DATE"));
                        enter |= field(ui, &mut f.max_created, &tr("FILTER_HINT_DATE"));
                        ui.end_row();
                        ui.label(tr("FILTER_ROW_MODIFIED"));
                        enter |= field(ui, &mut f.min_modified, &tr("FILTER_HINT_DATE"));
                        enter |= field(ui, &mut f.max_modified, &tr("FILTER_HINT_DATE"));
                        ui.end_row();
                        enter
                    });
                    submitted |= grid_enter.inner;
                    ui.add_space(6.0);

                    let dirty = self.filter_form != self.filter_applied;
                    let mut clear = false;
                    ui.horizontal(|ui| {
                        if ui.add_enabled(dirty, egui::Button::new(tr("FILTER_APPLY"))).clicked() {
                            submitted = true;
                        }
                        if ui.add_enabled(self.filter.is_some() || self.filter_form != FilterForm::default(), egui::Button::new(tr("FILTER_CLEAR"))).clicked() {
                            clear = true;
                        }
                    });
                    if clear {
                        self.filter_form = FilterForm::default();
                        submitted = true;
                    }
                    if submitted {
                        self.apply_filter_form();
                    }

                    if let Some(e) = &self.filter_error {
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

        // Chart settings live in a docked panel on the right side of the main
        // window (toggled by the ⚙ button) instead of a floating popup.
        if self.show_settings {
            egui::Panel::right("settings_panel")
                .resizable(true)
                .default_size(320.0)
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
                            egui::Slider::new(&mut s.max_render_depth, 1..=12)
                                .text(tr("SETTINGS_DEPTH_LEVELS")),
                        );
                        ui.add_enabled(
                            !s.unlimited_slices,
                            egui::Slider::new(&mut s.min_segment_angle_deg, 0.1..=5.0)
                                .text(tr("SETTINGS_MIN_SLICE_ANGLE")),
                        );
                        ui.add_enabled(
                            !s.unlimited_slices,
                            egui::Slider::new(&mut s.max_children_shown, 4..=360)
                                .text(tr("SETTINGS_MAX_SLICES")),
                        );
                        ui.checkbox(&mut s.unlimited_slices, tr("SETTINGS_UNLIMITED_SLICES"))
                            .on_hover_text(tr("SETTINGS_UNLIMITED_SLICES_HOVER"));
                        ui.add(
                            egui::Slider::new(&mut s.hub_radius_frac, 0.05..=0.5)
                                .text(tr("SETTINGS_HUB_SIZE")),
                        );

                        ui.separator();
                        ui.label(tr("SETTINGS_COLORS"));
                        ui.add(egui::Slider::new(&mut s.ring_sat, 0.0..=1.0).text(tr("SETTINGS_RING_SAT")));
                        ui.add(
                            egui::Slider::new(&mut s.ring_val_base, 0.3..=1.0)
                                .text(tr("SETTINGS_RING_BRIGHT_OUTER")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.ring_val_falloff, 0.0..=0.3)
                                .text(tr("SETTINGS_BRIGHT_FALLOFF")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.ring_val_floor, 0.1..=0.9)
                                .text(tr("SETTINGS_BRIGHT_FLOOR")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.other_sat, 0.0..=1.0)
                                .text(tr("SETTINGS_OTHER_SAT")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.other_val, 0.3..=1.0)
                                .text(tr("SETTINGS_OTHER_BRIGHT")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.free_space_gamma, 0.2..=1.5)
                                .text(tr("SETTINGS_FREE_GAMMA")),
                        );

                        ui.separator();
                        ui.label(tr("SETTINGS_LINE_RENDERING"));
                        ui.add(
                            egui::Slider::new(&mut s.stroke_width, 0.0..=3.0)
                                .text(tr("SETTINGS_BORDER_THICKNESS")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.stroke_alpha, 0..=255)
                                .text(tr("SETTINGS_BORDER_DARKNESS")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.tess_px_per_step, 1.0..=10.0)
                                .text(tr("SETTINGS_CURVE_SMOOTH")),
                        );

                        ui.separator();
                        ui.label(tr("SETTINGS_LOG"));
                        ui.add(
                            egui::Slider::new(&mut s.max_log_lines, 50..=5000)
                                .text(tr("SETTINGS_MAX_LOG_LINES")),
                        );

                        ui.separator();
                        ui.label(tr("SETTINGS_SCANNING"));
                        ui.add(
                            egui::Slider::new(&mut s.progress_interval_pow2, 0..=16)
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
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(false)
                        .show(ui, |ui| {
                            for line in self.log.iter().rev() {
                                ui.small(line);
                            }
                            if self.log_truncated > 0 {
                                ui.small(trf(
                                    "LOG_MORE_NOT_SHOWN",
                                    &[&format_count(self.log_truncated)],
                                ));
                            }
                        });
                }
            });

        let central = egui::CentralPanel::default().show(ui, |ui| {
            self.chart_segs.clear();
            if !self.scanning && self.root.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label(tr("CENTRAL_EMPTY_STATE"));
                });
                return;
            }
            if self.scanning {
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
                return;
            }

            let root = match &self.root {
                Some(r) => r.clone(),
                None => return,
            };

            // Room above the chart for the path line of the top-left info
            // (size/files/perms may overlap the chart's corner).
            if !self.summary_view {
                ui.add_space(ui.text_style_height(&egui::TextStyle::Body) + 12.0);
            }
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
                ui.allocate_painter(Vec2::new(avail.x, content_height), egui::Sense::click_and_drag());
            let (center, max_radius) = self.chart_view(&ctx, &response);
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
                        SortColumn::Modified => a.mtime.cmp(&b.mtime),
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
                        // Extensions have no modification date; the header
                        // isn't offered for this table.
                        SortColumn::Modified => std::cmp::Ordering::Equal,
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
                                folder_stats_ui(ui, view_node);
                                ui.add_space(8.0);
                                ui.heading(tr("SUMMARY_CONTENTS"));
                                egui::Grid::new("summary_contents_grid")
                                    .num_columns(4)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        sortable_header(ui, &tr("COL_SIZE"), SortColumn::Size, &mut self.contents_sort);
                                        sortable_header(ui, &tr("COL_FILES"), SortColumn::Files, &mut self.contents_sort);
                                        sortable_header(ui, &tr("COL_MODIFIED"), SortColumn::Modified, &mut self.contents_sort);
                                        sortable_header(ui, &tr("COL_NAME"), SortColumn::Name, &mut self.contents_sort);
                                        ui.end_row();
                                        for (i, c) in &contents_rows {
                                            ui.label(human_size(c.size));
                                            ui.label(format_count(c.file_count.max(1)));
                                            ui.label(if c.mtime == 0 { "-".to_string() } else { format_epoch(c.mtime) });
                                            // Only folders navigate, so only they look clickable.
                                            if c.is_dir {
                                                if ui.link(&c.name).clicked() {
                                                    let mut vp = self.view_stack.last().unwrap().clone();
                                                    vp.push(*i);
                                                    self.view_stack.push(vp);
                                                }
                                            } else {
                                                ui.label(&c.name);
                                            }
                                            ui.end_row();
                                        }
                                    });

                                ui.add_space(12.0);
                                ui.separator();

                                ui.heading(tr("SUMMARY_BY_EXTENSION"));
                                egui::Grid::new("summary_ext_grid")
                                    .num_columns(3)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        sortable_header(ui, &tr("COL_SIZE"), SortColumn::Size, &mut self.ext_sort);
                                        sortable_header(ui, &tr("COL_FILES"), SortColumn::Files, &mut self.ext_sort);
                                        sortable_header(ui, &tr("COL_EXTENSION"), SortColumn::Name, &mut self.ext_sort);
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

            // No menu on an "other" bucket: its idx_path is its *parent*
            // folder's, so Trash/Delete there would hit that whole folder.
            if response.secondary_clicked() && !hover_is_other {
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
                            if ui.button(tr("MENU_ZOOM")).clicked() {
                                let mut vp = self.view_stack.last().unwrap().clone();
                                vp.extend(menu_node_path.iter());
                                self.view_stack.push(vp);
                                close = true;
                            }
                            if ui.button(tr("MENU_RESCAN")).clicked() {
                                self.start_scan(target.clone());
                                close = true;
                            }
                            if ui.button(tr("MENU_OPEN")).clicked() {
                                let dir = if target.is_dir() { target.clone() } else { target.parent().map(|p| p.to_path_buf()).unwrap_or(target.clone()) };
                                let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                                close = true;
                            }
                            if ui.button(tr("MENU_HIDE")).clicked() {
                                self.hidden.insert(target.clone());
                                close = true;
                            }
                            if ui.button(tr("MENU_TRASH")).clicked() {
                                let _ = trash::delete(&target);
                                if let Some(root) = &self.root {
                                    let n = self.current_view_node(root);
                                    let p = n.path.clone();
                                    self.start_scan(p);
                                }
                                close = true;
                            }
                            if ui.button(tr("MENU_DELETE")).clicked() {
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
                            if ui.button(tr("MENU_CANCEL")).clicked() {
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
                                    ui.label(if h.is_free { tr("HOVER_AVAILABLE") } else { tr("HOVER_SIZE") });
                                    ui.label(human_size(h.size));
                                    ui.end_row();

                                    // Always 1 for a real file, always 0 for
                                    // free space — neither is informative,
                                    // so only show it for folders and the
                                    // aggregate "other" bucket.
                                    if h.is_dir {
                                        ui.label(tr("HOVER_FILES"));
                                        ui.label(format_count(h.file_count));
                                        ui.end_row();
                                    }

                                    if let Some(m) = h.mode {
                                        ui.label(tr("HOVER_PERMS"));
                                        ui.label(format_perms(m));
                                        ui.end_row();
                                    }
                                    if let Some(mt) = h.mtime {
                                        ui.label(tr("HOVER_MODIFIED"));
                                        ui.label(format_epoch(mt));
                                        ui.end_row();
                                    }
                                    if let Some(ct) = h.ctime {
                                        ui.label(tr("HOVER_CHANGED"));
                                        ui.label(format_epoch(ct));
                                        ui.end_row();
                                    }
                                    if let (Some(uid), Some(gid)) = (h.uid, h.gid) {
                                        ui.label(tr("HOVER_OWNER"));
                                        ui.label(format_owner(uid, gid, &mut self.user_cache, &mut self.group_cache));
                                        ui.end_row();
                                    }
                                    if let Some(mime) = &mime {
                                        ui.label(tr("HOVER_TYPE"));
                                        ui.label(mime);
                                        ui.end_row();
                                    }
                                });
                        });
                    });
            }
        });

        // Folder stats (formerly the left sidebar) float in the chart's
        // top-left corner — for the highlighted slice when there is one,
        // otherwise for the folder being viewed. Summary view shows them
        // inline instead, since its tables start in that corner. Only once a
        // scan has finished: while scanning, self.root still holds the
        // previous result.
        if !self.scanning && !self.summary_view {
            if let Some(root) = &self.root {
                let view_node = self.current_view_node(root);
                let selected = self.selected_rel().and_then(|rel| try_get_node(view_node, rel));
                egui::Area::new("folder_stats_overlay".into())
                    .order(egui::Order::Foreground)
                    .interactable(false)
                    .fixed_pos(central.response.rect.left_top() + Vec2::new(8.0, 8.0))
                    .show(&ctx, |ui| {
                        let n = selected.unwrap_or(view_node);
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        ui.strong(n.path.display().to_string());
                        folder_stats_ui(ui, n);
                    });
            }
        }

        // Navigation cross in the main area's top-right corner, moving the
        // slice highlight (see move_selection):
        //        ⬆            ⬆ parent slice / parent folder
        //     ⬅     ➡         ⬅ ➡ previous / next slice in the ring
        //        ⬇            ⬇ first slice on the next ring out
        // The arrow keys do the same and light up the matching button;
        // Enter opens the highlighted slice (like clicking it), Esc clears it.
        if self.root.is_some() && !self.scanning && !self.summary_view {
            const FLASH: std::time::Duration = std::time::Duration::from_millis(180);
            if !self.typing {
                let pressed = ctx.input(|i| {
                    [
                        (egui::Key::ArrowUp, NavDir::Up),
                        (egui::Key::ArrowLeft, NavDir::Prev),
                        (egui::Key::ArrowRight, NavDir::Next),
                        (egui::Key::ArrowDown, NavDir::Down),
                    ]
                    .into_iter()
                    .find(|(k, _)| i.key_pressed(*k))
                    .map(|(_, d)| d)
                });
                if let Some(d) = pressed {
                    self.nav_flash = Some((d, Instant::now()));
                    self.move_selection(d);
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.open_selection();
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.selection = None;
                }
            }
            let lit = match self.nav_flash {
                Some((d, at)) if at.elapsed() < FLASH => {
                    ctx.request_repaint_after(FLASH.saturating_sub(at.elapsed()));
                    Some(d)
                }
                _ => None,
            };

            let has_slices = !self.chart_segs.is_empty();
            let can_leave_view = !self.view_stack.last().unwrap().is_empty();
            let buttons = [
                (NavDir::Up, "⬆", tr("NAV_UP"), has_slices || can_leave_view, 1.0, 0.0),
                (NavDir::Prev, "⬅", tr("NAV_PREV"), has_slices, 0.0, 1.0),
                (NavDir::Next, "➡", tr("NAV_NEXT"), has_slices, 2.0, 1.0),
                (NavDir::Down, "⬇", tr("NAV_DOWN"), has_slices, 1.0, 2.0),
            ];
            let cell = 30.0;
            let gap = 2.0;
            let clicked = egui::Area::new("nav_cross".into())
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::RIGHT_TOP)
                .fixed_pos(central.response.rect.right_top() + Vec2::new(-8.0, 8.0))
                .show(&ctx, |ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(3.0 * cell + 2.0 * gap), egui::Sense::hover());
                    let mut clicked = None;
                    for (dir, icon, tip, enabled, col, row) in buttons {
                        let min = rect.min + Vec2::new(col * (cell + gap), row * (cell + gap));
                        let button = egui::Button::new(icon).selected(lit == Some(dir)).min_size(Vec2::splat(cell));
                        let r = ui
                            .put(egui::Rect::from_min_size(min, Vec2::splat(cell)), |ui: &mut egui::Ui| {
                                ui.add_enabled(enabled, button)
                            })
                            .on_hover_text(tip);
                        if r.clicked() {
                            clicked = Some(dir);
                        }
                    }
                    clicked
                })
                .inner;
            if let Some(d) = clicked {
                self.move_selection(d);
            }
        }
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
