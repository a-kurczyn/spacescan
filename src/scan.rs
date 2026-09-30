//! The scanned tree (Node), scanning a folder into it, and formatting
//! its values for display.

use super::*;

/// Runs `f`, the next level of a recursion over folders, with enough stack:
/// more is allocated when a recursion runs deep, since folders can be
/// nested deeper than any fixed stack allows. Every recursion over folders
/// or the tree calls its next level through this.
#[inline]
pub(crate) fn deep<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(256 * 1024, 8 * 1024 * 1024, f)
}

pub(crate) struct Node {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    pub(crate) file_count: u64,
    pub(crate) is_dir: bool,
    pub(crate) children: Vec<Node>,
    pub(crate) mode: u32,
    pub(crate) mtime: i64,
    pub(crate) ctime: i64,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    /// Birth (creation) time, 0 when the filesystem doesn't report one.
    pub(crate) btime: i64,
}

// Clone and drop are written by hand: the derived versions recurse once per
// level and overflow the stack on very deep folder chains.
impl Clone for Node {
    fn clone(&self) -> Self {
        deep(|| Node {
            name: self.name.clone(),
            path: self.path.clone(),
            size: self.size,
            file_count: self.file_count,
            is_dir: self.is_dir,
            children: self.children.clone(),
            mode: self.mode,
            mtime: self.mtime,
            ctime: self.ctime,
            uid: self.uid,
            gid: self.gid,
            btime: self.btime,
        })
    }
}

impl Drop for Node {
    /// Frees the subtree without recursing: descendants are moved onto a
    /// work list and each is dropped once it has no children left.
    fn drop(&mut self) {
        let mut pending = std::mem::take(&mut self.children);
        while let Some(mut n) = pending.pop() {
            pending.append(&mut n.children);
        }
    }
}

/// A node for tests: `path` (its name is the last component), size,
/// folder or file, children. (Nodes can't be built with `..empty_node()`:
/// Node has a Drop.)
#[cfg(test)]
pub(crate) fn test_node(path: &str, size: u64, is_dir: bool, children: Vec<Node>) -> Node {
    let mut n = empty_node();
    n.path = PathBuf::from(path);
    n.name = file_name_of(&n.path);
    n.size = size;
    n.file_count = u64::from(!is_dir);
    n.is_dir = is_dir;
    n.children = children;
    n
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

/// A time that isn't known (0 would be a real date: 1970-01-01).
pub(crate) const NO_TIME: i64 = i64::MIN;

/// Unix seconds as local "YYYY-MM-DD HH:MM", or "-" if unknown.
pub(crate) fn format_epoch(secs: i64) -> String {
    use chrono::TimeZone;
    if secs == NO_TIME {
        return "-".to_string();
    }
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => "-".to_string(),
    }
}

/// User and group names for uids and gids, cached.
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
        mtime: NO_TIME,
        ctime: NO_TIME,
        uid: 0,
        gid: 0,
        btime: 0,
    }
}

/// Puts a finished folder's totals into the live scan tree at
/// `target_path`, creating the folders above it as needed. Each folder
/// above gets the sum of what's known so far, until its own totals arrive.
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
    let Ok(rel) = target_path.strip_prefix(&node.path) else {
        return; // not inside `node`
    };
    // Split once, so each level compares just one name.
    let parts: Vec<&std::ffi::OsStr> = rel.components().map(|c| c.as_os_str()).collect();
    graft_at(node, &parts, &|n: &mut Node| {
        n.size = size;
        n.file_count = file_count;
        n.mode = mode;
        n.mtime = mtime;
        n.ctime = ctime;
        n.uid = uid;
        n.gid = gid;
    });
}

/// `graft_slice` one level down: `rest` are the remaining path components
/// to the target folder, `set` fills in its values.
fn graft_at(node: &mut Node, rest: &[&std::ffi::OsStr], set: &dyn Fn(&mut Node)) {
    let Some((first, rest)) = rest.split_first() else {
        set(node);
        return;
    };
    // Compare last path components as bytes: fast on wide folders.
    let idx = match node.children.iter().position(|c| c.path.file_name() == Some(*first)) {
        Some(i) => i,
        None => {
            let mut child = empty_node();
            child.name = show_os(first);
            child.path = node.path.join(first);
            child.is_dir = true;
            node.children.push(child);
            node.children.len() - 1
        }
    };
    deep(|| graft_at(&mut node.children[idx], rest, set));
    node.size = node.children.iter().fold(0u64, |t, c| t.saturating_add(c.size));
    node.file_count = node.children.iter().fold(0u64, |t, c| t.saturating_add(c.file_count));
    // Only children[idx] changed: move it into place instead of re-sorting.
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

pub(crate) fn file_name_of(p: &Path) -> String {
    p.file_name().map(show_os).unwrap_or_else(|| show_path(p))
}

/// A name for display that can't be mistaken for another, like `ls -b`:
/// invalid UTF-8 bytes appear as `\xFF`, control characters as `\n`,
/// `\t` or `\x1B`, and a backslash as `\\`.
pub(crate) fn show_os(s: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for chunk in s.as_bytes().utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                c if c.is_control() => out.push_str(&format!("\\x{:02X}", c as u32)),
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02X}"));
        }
    }
    out
}

/// `n` with its children but not theirs: what the live table shows.
pub(crate) fn flat_copy(n: &Node) -> Node {
    let shallow = |c: &Node, children: Vec<Node>| Node {
        name: c.name.clone(),
        path: c.path.clone(),
        size: c.size,
        file_count: c.file_count,
        is_dir: c.is_dir,
        children,
        mode: c.mode,
        mtime: c.mtime,
        ctime: c.ctime,
        uid: c.uid,
        gid: c.gid,
        btime: c.btime,
    };
    shallow(n, n.children.iter().map(|c| shallow(c, Vec::new())).collect())
}

/// Sort key for file-manager name order: case-insensitive, numbers by value
/// ("file2" before "file10", "007" = "7"). Keys compare as plain bytes.
///
/// A run of digits becomes '0', its length without leading zeros (two
/// bytes), then the digits, so shorter numbers sort first.
pub(crate) fn natural_key(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let mut key = Vec::with_capacity(bytes.len() + 4);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = &bytes[start..i];
            let digits = &run[run.iter().take_while(|&&c| c == b'0').count()..];
            let len = digits.len().min(u16::MAX as usize) as u16;
            key.push(b'0');
            key.extend_from_slice(&len.to_be_bytes());
            key.extend_from_slice(&digits[..len as usize]);
            continue;
        }
        let c = name[i..].chars().next().unwrap();
        if c.is_ascii() {
            key.push(c.to_ascii_lowercase() as u8);
        } else {
            // é sorts as e (just after plain e).
            let mut buf = [0u8; 4];
            for l in c.to_lowercase() {
                match l {
                    'ß' => key.extend_from_slice(b"ss"),
                    'æ' => key.extend_from_slice(b"ae"),
                    'œ' => key.extend_from_slice(b"oe"),
                    'ø' => key.push(b'o'),
                    'ł' => key.push(b'l'),
                    'đ' => key.push(b'd'),
                    'þ' => key.extend_from_slice(b"th"),
                    _ => {
                        use unicode_normalization::UnicodeNormalization;
                        for base in std::iter::once(l).nfd().filter(|b| !unicode_normalization::char::is_combining_mark(*b)) {
                            key.extend_from_slice(base.encode_utf8(&mut buf).as_bytes());
                        }
                    }
                }
            }
        }
        i += c.len_utf8();
    }
    key
}

/// Natural, case-insensitive name order (see `natural_key`), then byte
/// order for names that tie.
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    natural_key(a).cmp(&natural_key(b)).then_with(|| a.cmp(b))
}

/// `s` cut to at most `max` characters by replacing its middle with "…"
/// (keeping the end, where a path's name is).
pub(crate) fn shorten_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head = max / 3;
    let tail = max - head - 1;
    let mut out: String = s.chars().take(head).collect();
    out.push('…');
    out.extend(s.chars().skip(n - tail));
    out
}

/// A path for labels and dialogs: escaped like `show_path` and shortened to
/// about 240 characters.
pub(crate) fn short_path(p: &Path) -> String {
    shorten_middle(&show_path(p), 240)
}

/// A whole path for display, escaped like `show_os`.
pub(crate) fn show_path(p: &Path) -> String {
    show_os(p.as_os_str())
}

/// An I/O error as a plain-language line naming `path`.
pub(crate) fn friendly_io_error(path: &Path, e: &std::io::Error) -> String {
    trf("ERR_IO_LINE", &[&show_path(path), &io_reason(e)])
}

/// Plain-language reason for an I/O error ("not found", "don't have
/// permission…"), for error lines that name their subject themselves.
pub(crate) fn io_reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => tr("ERR_IO_NOT_FOUND"),
        PermissionDenied => tr("ERR_IO_PERMISSION"),
        _ => trf("ERR_IO_OTHER", &[&e.to_string()]),
    }
}

/// Birth (creation) time in Unix seconds; 0 if unavailable.
pub(crate) fn birth_secs(m: &std::fs::Metadata) -> i64 {
    m.created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Everything a scan's worker threads share.
pub(crate) struct ScanCtx<'a> {
    /// Device of the scanned folder: other filesystems aren't entered.
    pub(crate) root_dev: u64,
    pub(crate) progress: &'a Sender<ScanMsg>,
    pub(crate) counter: &'a std::sync::atomic::AtomicU64,
    pub(crate) cancel: &'a Arc<std::sync::atomic::AtomicBool>,
    pub(crate) progress_interval: u64,
    /// Count file lengths instead of the disk space actually used.
    pub(crate) apparent_size: bool,
    /// (device, inode) of files with several hard links already counted,
    /// so each is counted once, like `du`.
    pub(crate) hard_links: std::sync::Mutex<HashSet<(u64, u64)>>,
    /// Set once any name has Korean script in it, so the app can load a
    /// font for it (see `install_fallback_fonts`).
    pub(crate) saw_hangul: &'a std::sync::atomic::AtomicBool,
}

impl ScanCtx<'_> {
    /// Size to count for an entry: disk space actually allocated (so a
    /// sparse file counts what it really uses), or its length when
    /// `apparent_size` is set. A file with several hard links counts only
    /// at the first name found.
    fn size_of(&self, m: &std::fs::Metadata) -> u64 {
        use std::os::unix::fs::MetadataExt;
        if !m.is_dir() && m.nlink() > 1 && !self.hard_links.lock().unwrap().insert((m.dev(), m.ino())) {
            return 0;
        }
        if self.apparent_size { m.len() } else { m.blocks() * 512 }
    }
}

/// Paths longer than this are opened relative to their parent folder
/// (see `DirHandle`), safely under the kernel's 4096-byte PATH_MAX.
pub(crate) const LONG_PATH: usize = 3800;

/// An open folder, kept while scanning a deep subtree so that folders
/// whose full path is too long for the kernel can still be opened, relative
/// to it, via the short `/proc/self/fd/<fd>/<name>`.
pub(crate) type DirHandle = Option<std::fs::File>;

/// A path the kernel will accept for `path`: itself, or — when it's too
/// long — `/proc/self/fd/<parent fd>/<name>` relative to its open parent.
pub(crate) fn openable(path: &Path, parent: &DirHandle) -> PathBuf {
    use std::os::fd::AsRawFd;
    match parent {
        Some(f) if path.as_os_str().len() > LONG_PATH => {
            let mut p = PathBuf::from(format!("/proc/self/fd/{}", f.as_raw_fd()));
            if let Some(name) = path.file_name() {
                p.push(name);
            }
            p
        }
        _ => path.to_path_buf(),
    }
}

/// Scans one folder entry: a folder on the same filesystem is scanned into
/// a subtree, anything else becomes a file node. `path` is the entry's full
/// path; `dir` is its folder's handle.
pub(crate) fn scan_entry(entry: &std::fs::DirEntry, path: PathBuf, dir: &DirHandle, ctx: &ScanCtx) -> Node {
    let ScanCtx { root_dev, progress, counter, progress_interval, .. } = *ctx;
    use std::os::unix::fs::MetadataExt;
    let p = path;
    if !ctx.saw_hangul.load(std::sync::atomic::Ordering::Relaxed)
        && p.file_name().is_some_and(|n| n.to_string_lossy().chars().any(is_hangul))
    {
        ctx.saw_hangul.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let ft = entry.file_type();
    let node = match ft {
        Ok(ft) if ft.is_dir() && !ft.is_symlink() => {
            // Other filesystems mounted inside aren't entered (like `du -x`).
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
                    mtime: meta.as_ref().map(|m| m.mtime()).unwrap_or(NO_TIME),
                    ctime: meta.as_ref().map(|m| m.ctime()).unwrap_or(NO_TIME),
                    uid: meta.as_ref().map(|m| m.uid()).unwrap_or(0),
                    gid: meta.as_ref().map(|m| m.gid()).unwrap_or(0),
                    btime: meta.as_ref().map(birth_secs).unwrap_or(0),
                }
            } else {
                deep(|| scan_dir_in(&p, dir, ctx))
            }
        }
        _ => {
            let (sz, mode, mtime, ctime, uid, gid, btime) = match entry.metadata() {
                Ok(m) => (ctx.size_of(&m), m.mode(), m.mtime(), m.ctime(), m.uid(), m.gid(), birth_secs(&m)),
                Err(e) => {
                    let _ = progress.send(ScanMsg::LogError(friendly_io_error(&p, &e)));
                    (0, 0, NO_TIME, NO_TIME, 0, 0, 0)
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

pub(crate) fn scan_dir(path: &Path, ctx: &ScanCtx) -> Node {
    scan_dir_in(path, &None, ctx)
}

/// Scans `path`, whose parent folder is open as `parent` when the path is
/// long (see `DirHandle`).
fn scan_dir_in(path: &Path, parent: &DirHandle, ctx: &ScanCtx) -> Node {
    let ScanCtx { progress, cancel, .. } = *ctx;
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::Ordering;
    let name = file_name_of(path);
    if cancel.load(Ordering::Relaxed) {
        // Cancelled: stop at once; the result is discarded.
        return Node {
            name,
            path: path.to_path_buf(),
            size: 0,
            file_count: 0,
            is_dir: true,
            children: Vec::new(),
            mode: 0,
            mtime: NO_TIME,
            ctime: NO_TIME,
            uid: 0,
            gid: 0,
            btime: 0,
        };
    }
    // Keep this folder open when its children's paths may get too long
    // to open directly.
    let open_at = openable(path, parent);
    let handle: DirHandle =
        if path.as_os_str().len() + 256 > LONG_PATH { std::fs::File::open(&open_at).ok() } else { None };
    let listing = match &handle {
        Some(f) => {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", f.as_raw_fd()))
        }
        None => open_at.clone(),
    };
    let self_meta = std::fs::metadata(&listing).ok();
    let self_mode = self_meta.as_ref().map(|m| m.mode()).unwrap_or(0);
    let self_mtime = self_meta.as_ref().map(|m| m.mtime()).unwrap_or(NO_TIME);
    let self_ctime = self_meta.as_ref().map(|m| m.ctime()).unwrap_or(NO_TIME);
    let self_uid = self_meta.as_ref().map(|m| m.uid()).unwrap_or(0);
    let self_gid = self_meta.as_ref().map(|m| m.gid()).unwrap_or(0);
    let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(&listing) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(e) => {
            let _ = progress.send(ScanMsg::LogError(friendly_io_error(path, &e)));
            let _ = progress.send(ScanMsg::Unreadable(path.to_path_buf()));
            Vec::new()
        }
    };

    let children: Vec<Node> = entries
        .par_iter()
        .map(|entry| scan_entry(entry, path.join(entry.file_name()), &handle, ctx))
        .collect();

    let mut children = children;
    children.sort_by_key(|c| std::cmp::Reverse(c.size));
    // The folder's own entry uses space too. Saturating: apparent sizes of
    // sparse files can add up past u64.
    let own_size = self_meta.as_ref().map_or(0, |m| ctx.size_of(m));
    let size = children.iter().fold(own_size, |t, c| t.saturating_add(c.size));
    let file_count: u64 = children.iter().map(|c| c.file_count).sum();

    let mut exts: Vec<(String, u64, u64)> = Vec::new();
    for c in children.iter().filter(|c| !c.is_dir) {
        let key = ext_key(&c.name);
        match exts.iter_mut().find(|e| e.0 == key) {
            Some(e) => {
                e.1 = e.1.saturating_add(c.size);
                e.2 += c.file_count.max(1);
            }
            None => exts.push((key, c.size, c.file_count.max(1))),
        }
    }
    let _ = progress.send(ScanMsg::SliceDone {
        path: path.to_path_buf(),
        size,
        file_count,
        mode: self_mode,
        mtime: self_mtime,
        ctime: self_ctime,
        uid: self_uid,
        gid: self_gid,
        exts,
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
    /// A folder (at any depth) finished scanning: its totals, for the live
    /// chart and table.
    SliceDone {
        path: PathBuf,
        size: u64,
        file_count: u64,
        mode: u32,
        mtime: i64,
        ctime: i64,
        uid: u32,
        gid: u32,
        /// (extension, size, file count) of the files directly in it, for
        /// the category bar's live totals (see `ext_key`).
        exts: Vec<(String, u64, u64)>,
    },
    Done(Node, f64),
    Error(String),
    LogError(String),
    /// A folder whose contents couldn't be listed (its size is unknown).
    Unreadable(PathBuf),
}

/// The running result of the counting pass, for the progress bar. Shared
/// directly rather than sent as a message, so a backlog of scan messages
/// can't delay it.
#[derive(Default)]
pub(crate) struct EntryCount {
    pub(crate) found: std::sync::atomic::AtomicU64,
    pub(crate) done: std::sync::atomic::AtomicBool,
}

/// Counts the entries under `path` that the scan will visit, from folder
/// listings alone (no per-file stat), so it finishes well ahead of the scan
/// and gives the progress bar its total.
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
            deep(|| count_entries(&e.path(), root_dev, found, stop));
        }
    });
}

pub(crate) fn human_size(bytes: u64) -> String {
    // A total that hit the u64 ceiling (sums saturate) is only a lower bound.
    if bytes == u64::MAX {
        return "≥ 16 EiB".to_string();
    }
    // Powers of 1024, so binary (IEC) unit names.
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    // At most one decimal, and none when it would be ".0": "4 KiB",
    // "1.5 GiB", "95.4 MiB". Rounding can reach 1024 ("1024 KiB"): move up
    // a unit then.
    let mut rounded = (v * 10.0).round() / 10.0;
    if u > 0 && rounded >= 1024.0 && u < units.len() - 1 {
        rounded = (rounded / 1024.0 * 10.0).round() / 10.0;
        u += 1;
    }
    if u == 0 || rounded.fract() == 0.0 {
        format!("{} {}", rounded as u64, units[u])
    } else {
        format!("{:.1} {}", rounded, units[u])
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

/// True if `path` is a mount point (on another device than its parent).
pub(crate) fn is_real_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = path.parent() else {
        return true; // "/"
    };
    let (Ok(here), Ok(up)) = (std::fs::metadata(path), std::fs::metadata(parent)) else {
        return false;
    };
    here.dev() != up.dev()
}

/// `p` spelled as on disk: on a case-insensitive filesystem, a component
/// typed in another case ("PHOTOS") becomes the real name ("Photos").
pub(crate) fn true_case(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        let Component::Normal(name) = comp else {
            out.push(comp.as_os_str());
            continue;
        };
        let folded = |s: &std::ffi::OsStr| s.to_string_lossy().to_lowercase();
        let actual = std::fs::read_dir(&out).ok().and_then(|entries| {
            let mut other_case = None;
            for e in entries.flatten() {
                let n = e.file_name();
                if n == name {
                    return None; // spelled right already
                }
                if other_case.is_none() && folded(&n) == folded(name) {
                    other_case = Some(n);
                }
            }
            other_case
        });
        out.push(actual.as_deref().unwrap_or(name));
    }
    out
}

/// `target`'s path components below `base`, or None if it isn't below it.
/// Tree lookups compare one name per level (whole paths would be slow on
/// deep chains).
pub(crate) fn rel_parts<'a>(base: &Path, target: &'a Path) -> Option<Vec<&'a std::ffi::OsStr>> {
    Some(target.strip_prefix(base).ok()?.components().map(|c| c.as_os_str()).collect())
}

/// Index of `node`'s child named `name`.
pub(crate) fn child_named(node: &Node, name: &std::ffi::OsStr) -> Option<usize> {
    node.children.iter().position(|c| c.path.file_name() == Some(name))
}

/// Re-finds the folder at `idx` (child indices from `old`'s root) in `new`,
/// by path; stops at the deepest folder that still exists.
pub(crate) fn remap_index_path(old: &Node, new: &Node, idx: &[usize]) -> Vec<usize> {
    let (mut o, mut n) = (old, new);
    let mut out = Vec::new();
    for &i in idx {
        let Some(oc) = o.children.get(i) else { break };
        // Same parent, so the same name means the same folder.
        let Some(j) = oc.path.file_name().and_then(|name| child_named(n, name)) else { break };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn names_are_unambiguous() {
        let bad = std::ffi::OsStr::from_bytes(b"x\xffy");
        let lookalike = std::ffi::OsStr::new("x\u{FFFD}y");
        assert_eq!(show_os(bad), "x\\xFFy");
        assert_eq!(show_os(lookalike), "x\u{FFFD}y");
        assert_eq!(show_os(std::ffi::OsStr::new("a\nb\tc\\d")), "a\\nb\\tc\\\\d");
        assert_eq!(show_os(std::ffi::OsStr::new("ünïcödé 日本語")), "ünïcödé 日本語");
    }

    #[test]
    fn natural_name_order() {
        let mut names = vec!["file10", "File2", "file1", ".dotfile", "Beta", "alpha", "b", "Ärger", "a007", "a7", "a07x", "Zed"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec![".dotfile", "a007", "a7", "a07x", "alpha", "Ärger", "b", "Beta", "file1", "File2", "file10", "Zed"]);
        assert_eq!(natural_cmp("abc", "abc"), std::cmp::Ordering::Equal);
        let mut accented = vec!["Zed", "émile", "Árbol", "abc", "Ñandú", "emile", "Øre", "nube", "Straße", "strasse"];
        accented.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(accented, vec!["abc", "Árbol", "emile", "émile", "Ñandú", "nube", "Øre", "Straße", "strasse", "Zed"]);
        assert_eq!(natural_cmp("x9", "x10"), std::cmp::Ordering::Less);
    }
}

#[cfg(test)]
mod memory {
    use super::*;

    fn rss_mb() -> u64 {
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
        let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
        pages * 4096 / (1 << 20)
    }

    /// Resident memory across rescans of a big tree, as the app does them:
    /// the new scan is built while the old tree is still held, then
    /// replaces it. Run with
    /// `SPACEMAP_MEM_TREE=/ cargo test --release rescan_memory -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rescan_memory() {
        let root = PathBuf::from(std::env::var("SPACEMAP_MEM_TREE").unwrap_or_else(|_| "/".into()));
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let counter = std::sync::atomic::AtomicU64::new(0);
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(&root).unwrap().dev();
        let scan = || {
            let ctx = ScanCtx {
                root_dev: dev,
                progress: &tx,
                counter: &counter,
                cancel: &cancel,
                progress_interval: 512,
                apparent_size: false,
                hard_links: Default::default(),
                saw_hangul: &Default::default(),
            };
            scan_dir(&root, &ctx)
        };
        eprintln!("start: {} MB", rss_mb());
        let mut tree = Arc::new(scan());
        eprintln!("scan 1: {} MB ({} files)", rss_mb(), tree.file_count);
        for i in 2..=6 {
            let new = Arc::new(scan());
            tree = new; // the old tree is dropped here
            after_tree_dropped();
            std::thread::sleep(std::time::Duration::from_millis(500));
            eprintln!("scan {i}: {} MB", rss_mb());
        }
        drop(tree);
    }
}

/// Returns freed memory to the system after a scanned tree is dropped
/// (glibc otherwise keeps it). Runs on a background thread.
pub(crate) fn after_tree_dropped() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    std::thread::spawn(|| unsafe {
        libc::malloc_trim(0);
    });
}

#[cfg(test)]
mod format_tests {
    use super::*;
    #[test]
    fn huge_sizes_and_epoch_dates() {
        assert_eq!(human_size(u64::MAX), "≥ 16 EiB");
        assert_eq!(human_size(3 << 60), "3 EiB");
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1000), "1000 B");
        assert_eq!(human_size(4096), "4 KiB");
        assert_eq!(human_size(10 << 20), "10 MiB");
        assert_eq!(human_size(1536 << 20), "1.5 GiB");
        assert_eq!(human_size(100_033_331), "95.4 MiB");
        assert_eq!(human_size(759_069_900), "723.9 MiB");
        assert_eq!(human_size((1 << 20) - 1), "1 MiB"); // rounds up across the unit
        assert_eq!(human_size(12_000), "11.7 KiB");
        assert_eq!(u64::MAX.saturating_add(5), u64::MAX);
        assert_eq!(format_epoch(NO_TIME), "-");
        assert!(format_epoch(0).starts_with("1970-01-01") || format_epoch(0).starts_with("1969-12-31"));
    }
}

#[cfg(test)]
mod hangul_tests {
    use super::*;
    #[test]
    fn scan_notices_korean_names() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("spacemap-hangul-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("한국어.txt"), "x").unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let seen = std::sync::atomic::AtomicBool::new(false);
        let ctx = ScanCtx {
            root_dev: std::fs::metadata(&dir).unwrap().dev(),
            progress: &tx,
            counter: &Default::default(),
            cancel: &Default::default(),
            progress_interval: 512,
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &seen,
        };
        let _ = scan_dir(&dir, &ctx);
        assert!(seen.load(std::sync::atomic::Ordering::Relaxed));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod case_tests {
    use super::*;
    #[test]
    fn typed_case_becomes_disk_case() {
        // Case-sensitive filesystems are left alone.
        assert_eq!(true_case(Path::new("/usr/share")), PathBuf::from("/usr/share"));
        // On the dev machine /mnt/DATA is exFAT (case-insensitive).
        let real = PathBuf::from("/mnt/DATA/System Volume Information");
        let typed = PathBuf::from("/mnt/DATA/SYSTEM VOLUME INFORMATION");
        if typed.is_dir() && real.is_dir() && typed != real {
            assert_eq!(true_case(&typed), real);
        }
    }
}

#[cfg(test)]
mod live_category_tests {
    use super::*;

    /// The per-folder extension totals streamed during a scan add up to
    /// exactly what the finished tree's category bar shows.
    #[test]
    fn streamed_extension_totals_match_the_finished_tree() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("spacemap-livecat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("top.MKV"), vec![0u8; 20_000]).unwrap();
        std::fs::write(dir.join("a/doc.pdf"), vec![0u8; 5_000]).unwrap();
        std::fs::write(dir.join("a/b/noext"), vec![0u8; 9_000]).unwrap();
        std::fs::write(dir.join("a/b/clip.mp4"), vec![0u8; 7_000]).unwrap();
        std::fs::hard_link(dir.join("a/b/clip.mp4"), dir.join("a/clip-link.mp4")).unwrap();
        let (tx, rx) = channel();
        let ctx = ScanCtx {
            root_dev: std::fs::metadata(&dir).unwrap().dev(),
            progress: &tx,
            counter: &Default::default(),
            cancel: &Default::default(),
            progress_interval: 512,
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
        };
        let tree = scan_dir(&dir, &ctx);
        drop(tx);
        let mut live = ExtTotals::new();
        for msg in rx {
            if let ScanMsg::SliceDone { exts, .. } = msg {
                for (ext, size, files) in exts {
                    add_ext(&mut live, ext, size, files);
                }
            }
        }
        let cats = CategoryModel::defaults();
        assert_eq!(category_rows(&live, &cats), category_breakdown(&tree, &cats));
        assert_eq!(live.get("mkv").map(|e| e.1), Some(1));
        assert_eq!(live.get("mp4").map(|e| e.1), Some(2));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod scan_perf {
    use super::*;

    /// Scan timing on the folder in $SPACEMAP_BENCH (run with
    /// `SPACEMAP_BENCH=<dir> cargo test --release scan_perf -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn scan_bench() {
        use std::os::unix::fs::MetadataExt;
        let dir = PathBuf::from(std::env::var("SPACEMAP_BENCH").expect("set SPACEMAP_BENCH"));
        for run in 0..5 {
            let (tx, rx) = channel();
            let drain = std::thread::spawn(move || rx.into_iter().count());
            let ctx = ScanCtx {
                root_dev: std::fs::metadata(&dir).unwrap().dev(),
                progress: &tx,
                counter: &Default::default(),
                cancel: &Default::default(),
                progress_interval: 512,
                apparent_size: false,
                hard_links: Default::default(),
                saw_hangul: &Default::default(),
            };
            let t = Instant::now();
            let tree = scan_dir(&dir, &ctx);
            let took = t.elapsed();
            drop(tx);
            let msgs = drain.join().unwrap();
            eprintln!("run {run}: {took:?}, {} files, {msgs} messages", tree.file_count);
        }
    }
}
