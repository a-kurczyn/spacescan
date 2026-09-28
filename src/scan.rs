//! The scanned tree (Node), scanning a folder into it, and formatting
//! its values for display.

use super::*;

// ---------------- Data model ----------------

#[derive(Clone)]
pub(crate) struct Node {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    pub(crate) file_count: u64,
    pub(crate) is_dir: bool,
    pub(crate) children: Vec<Node>,
    pub(crate) mode: u32,
    /// All free — same `stat` struct already fetched for size/mode.
    pub(crate) mtime: i64,
    pub(crate) ctime: i64,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    /// Birth (creation) time, 0 when the filesystem doesn't report one.
    pub(crate) btime: i64,
}

/// "755 (rwxr-xr-x)" style permission summary.
pub(crate) fn format_perms(mode: u32) -> String {
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

/// "drwxr-xr-x"-style mode string, as `ls -l` shows it (including the
/// setuid/setgid/sticky bits). `is_dir` covers a mode without type bits.
pub(crate) fn format_mode_ls(mode: u32, is_dir: bool) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o060000 => 'b',
        0o020000 => 'c',
        0o010000 => 'p',
        0o140000 => 's',
        _ if is_dir => 'd',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(kind);
    for (shift, special, set_x, set_no_x) in [(6, 0o4000, 's', 'S'), (3, 0o2000, 's', 'S'), (0, 0o1000, 't', 'T')] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(match (mode & special != 0, bits & 1 != 0) {
            (true, true) => set_x,
            (true, false) => set_no_x,
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// Unix epoch seconds -> local "YYYY-MM-DD HH:MM" — free from the same
/// `stat` struct already fetched, no extra syscall.
pub(crate) fn format_epoch(secs: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => "-".to_string(),
    }
}

/// uid/gid -> username/groupname, cached (uzers hits the system's NSS
/// lookup each call — cheap, but no reason to repeat it every frame while
/// the pointer sits still over the same file).
pub(crate) fn format_owner(
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

pub(crate) fn empty_node() -> Node {
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
pub(crate) fn graft_slice(
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
pub(crate) fn reposition_by_size(children: &mut [Node], idx: usize) {
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

pub(crate) const MAX_EXTENSIONS_SHOWN: usize = 40;

pub(crate) fn collect_extensions(node: &Node, map: &mut std::collections::HashMap<String, (u64, u64)>) {
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
pub(crate) fn extension_breakdown(node: &Node) -> Vec<(String, u64, u64)> {
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

pub(crate) fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string_lossy().to_string())
}

/// Turns an io::Error into the kind of plain-language line a non-technical
/// user asked for, e.g. "can't find that directory" / "don't have permission
/// to read or enter that folder", instead of a raw errno string.
pub(crate) fn friendly_io_error(path: &Path, e: &std::io::Error) -> String {
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
pub(crate) fn birth_secs(m: &std::fs::Metadata) -> i64 {
    m.created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn scan_entry(
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

pub(crate) fn scan_dir(
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

pub(crate) enum ScanMsg {
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
pub(crate) struct EntryCount {
    pub(crate) found: std::sync::atomic::AtomicU64,
    pub(crate) done: std::sync::atomic::AtomicBool,
}

/// Counts every entry under `path` that the scan will visit (same
/// filesystem, symlinks not followed), using only directory listings — no
/// per-file stat, which is most of the real scan's cost — so it normally
/// finishes well ahead of the scan and gives the progress bar a real total
/// for folders that aren't whole drives.
pub(crate) fn count_entries(path: &Path, root_dev: u64, found: &std::sync::atomic::AtomicU64, stop: &(dyn Fn() -> bool + Sync)) {
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

pub(crate) fn human_size(bytes: u64) -> String {
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
pub(crate) fn format_count(n: u64) -> String {
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
pub(crate) fn fs_space(path: &Path) -> Option<(u64, u64)> {
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
pub(crate) fn is_real_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = path.parent() else {
        return true; // "/" has no parent: trivially a mount point
    };
    let (Ok(here), Ok(up)) = (std::fs::metadata(path), std::fs::metadata(parent)) else {
        return false;
    };
    here.dev() != up.dev()
}


/// Re-finds the folder at `idx` (child indices from `old`'s root) in `new`,
/// matching by path; stops at the deepest folder that still exists.
pub(crate) fn remap_index_path(old: &Node, new: &Node, idx: &[usize]) -> Vec<usize> {
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
pub(crate) fn index_path_to(root: &Node, target: &Path) -> Option<Vec<usize>> {
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
