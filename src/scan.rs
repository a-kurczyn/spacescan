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
    /// Its name as shown (see `show_os`); also its name on disk when
    /// `raw` is None, so it's set before the node is placed and not changed
    /// after.
    pub(crate) name: String,
    /// The folder it's in, shared by everything in that folder; empty for
    /// a tree's top, whose `raw` is then its whole path.
    dir: Arc<Path>,
    /// Its name on disk, when that differs from `name` (escaped or
    /// translated); None for nearly everything.
    raw: Option<Box<std::ffi::OsStr>>,
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
            dir: self.dir.clone(),
            raw: self.raw.clone(),
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

impl Node {
    /// Its name on disk (the last part of its path).
    pub(crate) fn disk_name(&self) -> &std::ffi::OsStr {
        match &self.raw {
            // A tree's top keeps its whole path.
            Some(raw) if self.dir.as_os_str().is_empty() => Path::new(raw).file_name().unwrap_or(raw),
            Some(raw) => raw,
            None => std::ffi::OsStr::new(&self.name),
        }
    }

    /// Its full path.
    pub(crate) fn path(&self) -> PathBuf {
        self.dir.join(self.stored_name())
    }

    /// What `dir` is joined with: its name on disk, or a top's whole path.
    fn stored_name(&self) -> &std::ffi::OsStr {
        self.raw
            .as_deref()
            .unwrap_or_else(|| std::ffi::OsStr::new(&self.name))
    }

    /// True if its full path is `p`, without building it (the same as
    /// `path() == p` for the paths the app makes).
    pub(crate) fn path_is(&self, p: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let (d, n, p) = (
            self.dir.as_os_str().as_bytes(),
            self.stored_name().as_bytes(),
            p.as_os_str().as_bytes(),
        );
        // As `dir.join(name)` builds it.
        if d.is_empty() || n.first() == Some(&b'/') {
            p == n
        } else if d.last() == Some(&b'/') {
            p.len() == d.len() + n.len() && p.starts_with(d) && p.ends_with(n)
        } else {
            p.len() == d.len() + 1 + n.len()
                && p.starts_with(d)
                && p[d.len()] == b'/'
                && p.ends_with(n)
        }
    }

    /// Puts it in folder `dir` (shared with its siblings), named `disk` on
    /// disk. Set `name` first.
    pub(crate) fn place(&mut self, dir: Arc<Path>, disk: &std::ffi::OsStr) {
        self.raw = (disk != std::ffi::OsStr::new(&self.name)).then(|| disk.into());
        self.dir = dir;
    }

    /// Takes `from`'s place (a copy of it with the same name).
    pub(crate) fn copy_place(&mut self, from: &Node) {
        self.dir = from.dir.clone();
        self.raw = from.raw.clone();
    }

    /// The length in bytes of its full path, without building it.
    pub(crate) fn path_len(&self) -> usize {
        let (d, n) = (self.dir.as_os_str().len(), self.stored_name().len());
        match d {
            0 => n,
            _ if self.dir.as_os_str().as_encoded_bytes().last() == Some(&b'/') => d + n,
            _ => d + 1 + n,
        }
    }

    /// Sets its full path (a tree's top, or a node made on its own).
    pub(crate) fn set_path(&mut self, p: &Path) {
        match (p.parent(), p.file_name()) {
            (Some(parent), Some(file)) => self.place(parent.into(), file),
            _ => {
                self.dir = Path::new("").into();
                self.raw = Some(p.as_os_str().into());
            }
        }
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
    n.name = file_name_of(Path::new(path));
    n.set_path(Path::new(path));
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
    for (shift, special, set_x, set_no_x) in [
        (6, 0o4000, 's', 'S'),
        (3, 0o2000, 's', 'S'),
        (0, 0o1000, 't', 'T'),
    ] {
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
/// A fast hash for short keys hashed very often (extensions, paths during
/// a scan), the way the Rust compiler hashes internally: a multiply and a
/// rotate per word. Not resistant to crafted keys, which don't matter here.
#[derive(Default, Clone, Copy)]
pub(crate) struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(*w)).wrapping_mul(K);
        }
        for &b in rest {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(K);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type FxBuild = std::hash::BuildHasherDefault<FxHasher>;
/// A HashMap with the fast hash.
pub(crate) type FxHashMap<K, V> = HashMap<K, V, FxBuild>;

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
        dir: Path::new("").into(),
        raw: None,
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

/// `disk`, the name on disk, when it differs from `shown`.
fn raw_name(disk: &std::ffi::OsStr, shown: &str) -> Option<Box<std::ffi::OsStr>> {
    (disk != std::ffi::OsStr::new(shown)).then(|| disk.into())
}

/// Folder node `n` (scanned at `path`) placed in its parent's shared
/// `in_dir`, or on its own path at a tree's top.
fn placed(mut n: Node, path: &Path, in_dir: Option<&Arc<Path>>) -> Node {
    match (in_dir, path.file_name()) {
        (Some(dir), Some(disk)) => n.place(dir.clone(), disk),
        _ => n.set_path(path),
    }
    n
}

/// `p`'s last part as shown (see `show_os`), or the whole path for "/".
pub(crate) fn file_name_of(p: &Path) -> String {
    p.file_name().map(show_os).unwrap_or_else(|| show_path(p))
}

/// A name for display that can't be mistaken for another, like `ls -b`:
/// invalid UTF-8 bytes appear as `\xFF`, control characters as `\n`,
/// `\t` or `\x1B`, and a backslash as `\\`.
pub(crate) fn show_os(s: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    // Nearly every name is valid UTF-8 with nothing to escape: a plain copy.
    if let Ok(text) = std::str::from_utf8(s.as_bytes()) {
        let plain = |c: char| c != '\\' && !c.is_control();
        let clean = if text.is_ascii() {
            text.bytes().all(|b| b >= 0x20 && b != 0x7f && b != b'\\')
        } else {
            text.chars().all(plain)
        };
        if clean {
            return text.to_owned();
        }
    }
    let mut out = String::with_capacity(s.len());
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
        dir: c.dir.clone(),
        raw: c.raw.clone(),
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
    shallow(
        n,
        n.children.iter().map(|c| shallow(c, Vec::new())).collect(),
    )
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
                        for base in std::iter::once(l)
                            .nfd()
                            .filter(|b| !unicode_normalization::char::is_combining_mark(*b))
                        {
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

/// Every mount point, from /proc/self/mountinfo. A folder in this set has
/// another filesystem mounted on it; scans and deletes stop there.
pub(crate) fn mount_points() -> HashSet<PathBuf> {
    let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return HashSet::new();
    };
    info.lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(|field| PathBuf::from(unescape_mountinfo(field)))
        .collect()
}

/// True if `path` is on a spinning disk. Linux reports it per disk in
/// /sys/dev/block/<major>:<minor>/queue/rotational (for a partition, in its
/// disk's folder, one level up). Network shares and virtual devices have no
/// such entry and count as not rotational.
pub(crate) fn is_rotational(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let dev = meta.dev();
    let (major, minor) = (libc::major(dev), libc::minor(dev));
    let block = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
    [
        block.join("queue/rotational"),
        block.join("../queue/rotational"),
    ]
    .iter()
    .find_map(|f| std::fs::read_to_string(f).ok())
    .is_some_and(|v| v.trim() == "1")
}

/// mountinfo writes space, tab, newline and backslash as octal escapes
/// (`\040` for a space).
pub(crate) fn unescape_mountinfo(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Everything a scan's worker threads share.
pub(crate) struct ScanCtx<'a> {
    /// Folders where another filesystem is mounted (from `mount_points`):
    /// not entered. Anything else is scanned, btrfs subvolumes included.
    pub(crate) mounts: &'a HashSet<PathBuf>,
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
    /// Read each folder's entries in file-number order (for spinning disks,
    /// see `is_rotational`).
    pub(crate) in_file_order: bool,
    /// The live tree, updated for every folder and file read.
    pub(crate) live: Option<&'a LiveTree>,
}

impl ScanCtx<'_> {
    /// Size to count for an entry: disk space actually allocated (so a
    /// sparse file counts what it really uses), or its length when
    /// `apparent_size` is set. A file with several hard links counts only
    /// at the first name found.
    fn size_of(&self, m: &std::fs::Metadata) -> u64 {
        use std::os::unix::fs::MetadataExt;
        if !m.is_dir()
            && m.nlink() > 1
            && !self.hard_links.lock().unwrap().insert((m.dev(), m.ino()))
        {
            return 0;
        }
        if self.apparent_size {
            m.len()
        } else {
            m.blocks() * 512
        }
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
pub(crate) fn scan_entry(
    entry: &std::fs::DirEntry,
    here: &Arc<Path>,
    dir: &DirHandle,
    ctx: &ScanCtx,
    live_parent: Option<&LiveFolder>,
) -> Node {
    let ScanCtx {
        mounts,
        progress,
        counter,
        progress_interval,
        ..
    } = *ctx;
    use std::os::unix::fs::MetadataExt;
    // The name once, from the listing; a full path only where needed.
    let os_name = entry.file_name();
    let name = show_os(&os_name);
    let raw = raw_name(&os_name, &name);
    let full = || here.join(&os_name);
    // Korean script needs non-ASCII bytes.
    if !os_name.is_ascii()
        && !ctx.saw_hangul.load(std::sync::atomic::Ordering::Relaxed)
        && name.chars().any(is_hangul)
    {
        ctx.saw_hangul
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let ft = entry.file_type();
    let node = match ft {
        Ok(ft) if ft.is_dir() && !ft.is_symlink() => {
            // Other filesystems mounted inside aren't entered (like `du -x`).
            // The folder's details come from the listing (relative to the
            // open parent, cheaper than by full path).
            let meta = entry.metadata().ok();
            let p = full();
            if mounts.contains(&p) {
                let label = trf("SEG_OTHER_FS", &[&name]);
                Node {
                    raw: raw_name(&os_name, &label),
                    name: label,
                    dir: here.clone(),
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
                deep(|| scan_dir_in(&p, name, Some(here), meta, dir, ctx, live_parent))
            }
        }
        _ => {
            let (sz, mode, mtime, ctime, uid, gid, btime) = match entry.metadata() {
                Ok(m) => (
                    ctx.size_of(&m),
                    m.mode(),
                    m.mtime(),
                    m.ctime(),
                    m.uid(),
                    m.gid(),
                    birth_secs(&m),
                ),
                Err(e) => {
                    let _ = progress.send(ScanMsg::LogError(friendly_io_error(&full(), &e)));
                    (0, 0, NO_TIME, NO_TIME, 0, 0, 0)
                }
            };
            Node {
                name,
                dir: here.clone(),
                raw,
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

/// (extension, size, count) per extension of the files among `nodes`.
fn file_summary(nodes: &[Node]) -> Vec<(String, u64, u64)> {
    let mut exts: Vec<(String, u64, u64)> = Vec::new();
    let mut buf = [0; 16];
    for c in nodes.iter().filter(|c| !c.is_dir) {
        // A new string only for an extension not seen yet in this folder.
        let key = ext_key_in(&c.name, &mut buf);
        match exts.iter_mut().find(|e| e.0 == *key) {
            Some(e) => {
                e.1 = e.1.saturating_add(c.size);
                e.2 += c.file_count.max(1);
            }
            None => exts.push((key.into_owned(), c.size, c.file_count.max(1))),
        }
    }
    exts
}

pub(crate) fn scan_dir(path: &Path, ctx: &ScanCtx) -> Node {
    scan_dir_in(
        path,
        file_name_of(path),
        None,
        std::fs::metadata(path).ok(),
        &None,
        ctx,
        None,
    )
}

/// Scans `path`, whose parent folder is open as `parent` when the path is
/// long (see `DirHandle`).
/// `name` is its name as shown, `self_meta` its details, `live_parent` its
/// parent in the live tree (None: the scanned folder).
fn scan_dir_in(
    path: &Path,
    name: String,
    in_dir: Option<&Arc<Path>>,
    self_meta: Option<std::fs::Metadata>,
    parent: &DirHandle,
    ctx: &ScanCtx,
    live_parent: Option<&LiveFolder>,
) -> Node {
    let ScanCtx {
        progress, cancel, ..
    } = *ctx;
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::Ordering;
    if cancel.load(Ordering::Relaxed) {
        // Cancelled: stop at once; the result is discarded.
        return placed(
            Node {
                name,
                dir: Path::new("").into(),
                raw: None,
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
            },
            path,
            in_dir,
        );
    }
    // Keep this folder open when its children's paths may get too long
    // to open directly.
    let open_at = openable(path, parent);
    let handle: DirHandle = if path.as_os_str().len() + 256 > LONG_PATH {
        std::fs::File::open(&open_at).ok()
    } else {
        None
    };
    let listing = match &handle {
        Some(f) => {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", f.as_raw_fd()))
        }
        None => open_at.clone(),
    };
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

    // On a spinning disk, reading entries in file-number order (where the
    // filesystem keeps their details, e.g. NTFS's file table) saves most of
    // the seeking: about 10× faster on a big folder.
    let mut entries = entries;
    if ctx.in_file_order {
        use std::os::unix::fs::DirEntryExt;
        entries.sort_by_key(|e| e.ino());
    }
    // The folder's own entry uses space too.
    let own_size = self_meta.as_ref().map_or(0, |m| ctx.size_of(m));
    // In the live tree, every file read counts at once in its folder.
    let folder = ctx.live.map(|live| live.open(live_parent, path, own_size));
    // The path its entries share.
    let here: Arc<Path> = path.into();
    let mut children: Vec<Node> = entries
        .par_iter()
        .map(|entry| {
            let node = scan_entry(
                entry,
                &here,
                &handle,
                ctx,
                folder.as_ref().map(|o| &*o.folder),
            );
            if let (Some(live), Some(f)) = (ctx.live, &folder) {
                live.count(f, &node);
            }
            node
        })
        .collect();

    children.sort_by_key(|c| std::cmp::Reverse(c.size));
    // Saturating: apparent sizes of sparse files can add up past u64.
    let size = children
        .iter()
        .fold(own_size, |t, c| t.saturating_add(c.size));
    let file_count: u64 = children.iter().map(|c| c.file_count).sum();

    if let (Some(live), Some(open)) = (ctx.live, folder) {
        live.close(
            open, size, file_count, self_mode, self_mtime, self_ctime, self_uid, self_gid,
        );
    }
    let _ = progress.send(ScanMsg::SliceDone {
        exts: file_summary(&children),
    });

    placed(
        Node {
            name,
            dir: Path::new("").into(),
            raw: None,
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
        },
        path,
        in_dir,
    )
}

pub(crate) enum ScanMsg {
    Progress(u64),
    /// A folder (at any depth) finished scanning: (extension, size, file
    /// count) of the files directly in it, for the category bar's live
    /// totals (see `ext_key`). Sizes and colors come from the live tree.
    SliceDone {
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
pub(crate) fn count_entries(
    path: &Path,
    mounts: &HashSet<PathBuf>,
    found: &std::sync::atomic::AtomicU64,
    stop: &(dyn Fn() -> bool + Sync),
) {
    use std::sync::atomic::Ordering;
    if stop() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(path) else {
        return;
    };
    let entries: Vec<std::fs::DirEntry> = rd.filter_map(|e| e.ok()).collect();
    found.fetch_add(entries.len() as u64, Ordering::Relaxed);
    entries.par_iter().for_each(|e| {
        let is_dir = e
            .file_type()
            .is_ok_and(|ft| ft.is_dir() && !ft.is_symlink());
        if is_dir {
            let child = e.path();
            if !mounts.contains(&child) {
                deep(|| count_entries(&child, mounts, found, stop));
            }
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
    // SAFETY: `cpath` is a valid C string that outlives the call, and
    // `stat` is a plain C struct that statvfs fills in.
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
    Some(
        target
            .strip_prefix(base)
            .ok()?
            .components()
            .map(|c| c.as_os_str())
            .collect(),
    )
}

/// Index of `node`'s child named `name`.
pub(crate) fn child_named(node: &Node, name: &std::ffi::OsStr) -> Option<usize> {
    node.children
        .iter()
        .position(|c| c.disk_name() == name)
}

/// Re-finds the folder at `idx` (child indices from `old`'s root) in `new`,
/// by path; stops at the deepest folder that still exists.
pub(crate) fn remap_index_path(old: &Node, new: &Node, idx: &[usize]) -> Vec<usize> {
    let (mut o, mut n) = (old, new);
    let mut out = Vec::new();
    for &i in idx {
        let Some(oc) = o.children.get(i) else { break };
        // Same parent, so the same name means the same folder.
        let Some(j) = child_named(n, oc.disk_name()) else {
            break;
        };
        out.push(j);
        o = oc;
        n = &n.children[j];
    }
    out
}

/// Child-index path from `root` down to `target`, if it's in the tree.
pub(crate) fn index_path_to(root: &Node, target: &Path) -> Option<Vec<usize>> {
    let root_path = root.path();
    let rel = target.strip_prefix(&root_path).ok()?;
    let mut n = root;
    let mut out = Vec::new();
    for comp in rel.components() {
        let j = n
            .children
            .iter()
            .position(|c| c.is_dir && c.disk_name() == comp.as_os_str())?;
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
        assert_eq!(
            show_os(std::ffi::OsStr::new("a\nb\tc\\d")),
            "a\\nb\\tc\\\\d"
        );
        assert_eq!(
            show_os(std::ffi::OsStr::new("ünïcödé 日本語")),
            "ünïcödé 日本語"
        );
    }

    /// The quick copy for plain names gives exactly what the careful,
    /// escaping path gives, for every kind of odd name.
    #[test]
    fn plain_names_take_the_quick_path_safely() {
        // The careful path, as it was before the quick copy.
        fn careful(s: &std::ffi::OsStr) -> String {
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
        let names: &[&[u8]] = &[
            b"",
            b"plain.txt",
            b"with space",
            b"back\\slash",
            b"del\x7f",
            b"bell\x07",
            "next-line\u{85}".as_bytes(),
            "caf\u{e9}\u{9f}".as_bytes(),
            "\u{d55c}\u{ae00}.txt".as_bytes(),
            b"\xc3\xa9t\xc3\xa9\xff",
            b"\xc2",
            b"\xc3\x28",
            "emoji \u{1f600}".as_bytes(),
            "rtl \u{202e}".as_bytes(),
        ];
        for name in names {
            let os = std::ffi::OsStr::from_bytes(name);
            assert_eq!(show_os(os), careful(os), "{name:?}");
        }
    }

    #[test]
    fn natural_name_order() {
        let mut names = vec![
            "file10", "File2", "file1", ".dotfile", "Beta", "alpha", "b", "Ärger", "a007", "a7",
            "a07x", "Zed",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            vec![
                ".dotfile", "a007", "a7", "a07x", "alpha", "Ärger", "b", "Beta", "file1", "File2",
                "file10", "Zed"
            ]
        );
        assert_eq!(natural_cmp("abc", "abc"), std::cmp::Ordering::Equal);
        let mut accented = vec![
            "Zed", "émile", "Árbol", "abc", "Ñandú", "emile", "Øre", "nube", "Straße", "strasse",
        ];
        accented.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            accented,
            vec![
                "abc", "Árbol", "emile", "émile", "Ñandú", "nube", "Øre", "Straße", "strasse",
                "Zed"
            ]
        );
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
        let scan = || {
            let ctx = ScanCtx {
                mounts: &HashSet::new(),
                progress: &tx,
                counter: &counter,
                cancel: &cancel,
                progress_interval: 512,
                apparent_size: false,
                hard_links: Default::default(),
                saw_hangul: &Default::default(),
                in_file_order: false,
                live: None,
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
    // SAFETY: malloc_trim has no preconditions; it only releases free
    // heap memory.
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
        assert!(
            format_epoch(0).starts_with("1970-01-01") || format_epoch(0).starts_with("1969-12-31")
        );
    }
}

#[cfg(test)]
mod hangul_tests {
    use super::*;
    #[test]
    fn scan_notices_korean_names() {
        let dir = std::env::temp_dir().join(format!("spacemap-hangul-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("한국어.txt"), "x").unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let seen = std::sync::atomic::AtomicBool::new(false);
        let ctx = ScanCtx {
            mounts: &HashSet::new(),
            progress: &tx,
            counter: &Default::default(),
            cancel: &Default::default(),
            progress_interval: 512,
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &seen,
            in_file_order: false,
            live: None,
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
        assert_eq!(
            true_case(Path::new("/usr/share")),
            PathBuf::from("/usr/share")
        );
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
        let dir = std::env::temp_dir().join(format!("spacemap-livecat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("top.MKV"), vec![0u8; 20_000]).unwrap();
        std::fs::write(dir.join("a/doc.pdf"), vec![0u8; 5_000]).unwrap();
        std::fs::write(dir.join("a/b/noext"), vec![0u8; 9_000]).unwrap();
        std::fs::write(dir.join("a/b/clip.mp4"), vec![0u8; 7_000]).unwrap();
        std::fs::hard_link(dir.join("a/b/clip.mp4"), dir.join("a/clip-link.mp4")).unwrap();
        std::fs::create_dir(dir.join("mail")).unwrap();
        for i in 0..600 {
            std::fs::write(dir.join(format!("mail/{i}.eml")), b"x").unwrap();
        }
        let (tx, rx) = channel();
        let ctx = ScanCtx {
            mounts: &HashSet::new(),
            progress: &tx,
            counter: &Default::default(),
            cancel: &Default::default(),
            progress_interval: 512,
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: None,
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
        assert_eq!(live.get("eml").map(|e| e.1), Some(600));
        let cats = CategoryModel::defaults();
        assert_eq!(
            category_rows(&live, &cats),
            category_breakdown(&tree, &cats)
        );
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
    /// $SPACEMAP_THREADS sets the number of scan threads, and $SPACEMAP_RUNS
    /// the number of runs (default 5; 1 for a cold-cache measurement).
    #[test]
    #[ignore]
    fn scan_bench() {
        let dir = PathBuf::from(std::env::var("SPACEMAP_BENCH").expect("set SPACEMAP_BENCH"));
        let env_num = |name: &str, default: usize| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let threads = env_num("SPACEMAP_THREADS", 0);
        // $SPACEMAP_FILE_ORDER=1 reads entries in file-number order.
        let in_file_order = std::env::var_os("SPACEMAP_FILE_ORDER").is_some();
        // $SPACEMAP_LIVE=1 counts every file for the live chart, as the app
        // does; $SPACEMAP_WINDOW=1 also does the window's work meanwhile
        // (extension totals, reading the live tree every 100 ms).
        let with_live = std::env::var_os("SPACEMAP_LIVE").is_some();
        let window = std::env::var_os("SPACEMAP_WINDOW").is_some();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        for run in 0..env_num("SPACEMAP_RUNS", 5) {
            let live = with_live
                .then(|| pool.install(|| LiveTree::new(Arc::new(CategoryModel::defaults()))));
            let (tx, rx) = channel();
            let drain = std::thread::spawn(move || {
                let mut exts = ExtTotals::new();
                let mut n = 0;
                for msg in rx {
                    n += 1;
                    if let (true, ScanMsg::SliceDone { exts: e }) = (window, msg) {
                        for (ext, size, files) in e {
                            add_ext(&mut exts, ext, size, files);
                        }
                    }
                }
                n
            });
            let scanning = std::sync::atomic::AtomicBool::new(true);
            let ctx = ScanCtx {
                mounts: &HashSet::new(),
                progress: &tx,
                counter: &Default::default(),
                cancel: &Default::default(),
                progress_interval: 512,
                apparent_size: false,
                hard_links: Default::default(),
                saw_hangul: &Default::default(),
                in_file_order,
                live: live.as_ref(),
            };
            let t = Instant::now();
            let tree = std::thread::scope(|scope| {
                if let (true, Some(live)) = (window, &live) {
                    let scanning = &scanning;
                    scope.spawn(move || {
                        let mut looks = LiveLooks::default();
                        while scanning.load(std::sync::atomic::Ordering::Relaxed) {
                            std::hint::black_box(live.snapshot(7, 1.3 / 360.0, &mut looks));
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    });
                }
                let tree = pool.install(|| scan_dir(&dir, &ctx));
                scanning.store(false, std::sync::atomic::Ordering::Relaxed);
                tree
            });
            let took = t.elapsed();
            drop(tx);
            let msgs = drain.join().unwrap();
            eprintln!(
                "run {run}: {took:?}, {} threads, {} files, {msgs} messages",
                pool.current_num_threads(),
                tree.file_count
            );
        }
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    /// Reading entries in file-number order gives the same tree.
    #[test]
    fn file_order_gives_the_same_tree() {
        let dir = std::env::temp_dir().join(format!("spacemap-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for i in 0..50 {
            std::fs::write(dir.join(format!("f{i}")), vec![0u8; i * 100]).unwrap();
            std::fs::write(dir.join(format!("sub/g{i}")), vec![0u8; i * 10]).unwrap();
        }
        let scan = |in_file_order: bool| {
            let (tx, rx) = channel();
            std::thread::spawn(move || for _ in rx {});
            let ctx = ScanCtx {
                mounts: &HashSet::new(),
                progress: &tx,
                counter: &Default::default(),
                cancel: &Default::default(),
                progress_interval: 512,
                apparent_size: true,
                hard_links: Default::default(),
                saw_hangul: &Default::default(),
                in_file_order,
                live: None,
            };
            let tree = scan_dir(&dir, &ctx);
            (tree.size, tree.file_count, tree.children.len())
        };
        assert_eq!(scan(true), scan(false));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Paths without a block device (here /proc) count as not rotational.
    #[test]
    fn virtual_filesystems_are_not_rotational() {
        assert!(!is_rotational(Path::new("/proc")));
    }
}

#[cfg(test)]
mod mount_tests {
    use super::*;

    /// Only folders in the mount table are left out of a scan (shown as
    /// "[other filesystem]"); every other folder is scanned and counted,
    /// whatever device it reports (btrfs subvolumes report their own).
    #[test]
    fn scans_stop_only_at_mount_points() {
        let dir = std::env::temp_dir().join(format!("spacemap-mounts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mounted")).unwrap();
        std::fs::create_dir_all(dir.join("plain")).unwrap();
        std::fs::write(dir.join("mounted/f"), vec![0u8; 5000]).unwrap();
        std::fs::write(dir.join("plain/f"), vec![0u8; 5000]).unwrap();
        let mounts: HashSet<PathBuf> = [dir.join("mounted")].into();
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let ctx = ScanCtx {
            mounts: &mounts,
            progress: &tx,
            counter: &Default::default(),
            cancel: &Default::default(),
            progress_interval: 512,
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: None,
        };
        let tree = scan_dir(&dir, &ctx);
        let child = |name: &str| {
            tree.children
                .iter()
                .find(|c| c.path_is(&dir.join(name)))
                .unwrap()
        };
        assert_eq!((child("mounted").file_count, child("mounted").size), (0, 0));
        assert_eq!(child("plain").file_count, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod scan_dump {
    use super::*;
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt;

    /// Writes everything a scan of $SPACEMAP_BENCH produces to
    /// $SPACEMAP_DUMP, in a fixed order: every node's values, every finished
    /// folder's report, every error and unreadable folder. Two versions of
    /// the scanner must give identical files (run with RAYON_NUM_THREADS=1
    /// where hard links are shared between folders: which name counts them
    /// depends on thread timing). $SPACEMAP_APPARENT=1 counts apparent sizes.
    /// Run with `cargo test --release scan_dump -- --ignored`.
    #[test]
    #[ignore]
    fn scan_dump() {
        let dir = PathBuf::from(std::env::var("SPACEMAP_BENCH").unwrap());
        let out = std::env::var("SPACEMAP_DUMP").unwrap();
        let apparent = std::env::var_os("SPACEMAP_APPARENT").is_some();
        let (tx, rx) = channel();
        let collect = std::thread::spawn(move || {
            let mut lines = Vec::new();
            for msg in rx {
                match msg {
                    ScanMsg::SliceDone { exts } => {
                        let mut exts = exts;
                        exts.sort();
                        lines.push(format!("DONE {exts:?}"));
                    }
                    ScanMsg::LogError(e) => lines.push(format!("ERR {e}")),
                    ScanMsg::Unreadable(p) => lines.push(format!(
                        "UNREADABLE {}",
                        p.as_os_str().as_bytes().escape_ascii()
                    )),
                    _ => {}
                }
            }
            lines
        });
        let ctx = ScanCtx {
            mounts: &mount_points(),
            progress: &tx,
            counter: &Default::default(),
            cancel: &Default::default(),
            progress_interval: 512,
            apparent_size: apparent,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: None,
        };
        let tree = scan_dir(&dir, &ctx);
        drop(tx);
        let mut lines = collect.join().unwrap();
        fn walk(n: &Node, lines: &mut Vec<String>) {
            let mut s = String::new();
            let _ = write!(
                s,
                "NODE {} | {} | {} {} {} {} {} {} {} {} {}",
                n.path().as_os_str().as_bytes().escape_ascii(),
                n.name,
                n.size,
                n.file_count,
                n.is_dir,
                n.mode,
                n.mtime,
                n.ctime,
                n.uid,
                n.gid,
                n.btime
            );
            lines.push(s);
            for c in &n.children {
                walk(c, lines);
            }
        }
        walk(&tree, &mut lines);
        lines.sort();
        std::fs::write(out, lines.join("\n")).unwrap();
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    /// Every way to read a node's path agrees with `p`, and near misses
    /// (a byte off, a slash moved, the shown name) don't match.
    fn check(n: &Node, p: &Path) {
        assert_eq!(n.path(), p);
        assert!(n.path_is(p), "{}", p.display());
        assert_eq!(n.path_len(), p.as_os_str().len(), "{}", p.display());
        let disk = p.file_name().unwrap_or(p.as_os_str());
        assert_eq!(n.disk_name(), disk, "{}", p.display());
        let b = p.as_os_str().as_bytes();
        let mut longer = b.to_vec();
        longer.push(b'x');
        let mut shorter = b.to_vec();
        shorter.pop();
        let mut doubled = b"/".to_vec();
        doubled.extend_from_slice(b);
        let mut flipped = b.to_vec();
        *flipped.last_mut().unwrap() ^= 1;
        for near in [longer, shorter, doubled, flipped] {
            let near = Path::new(OsStr::from_bytes(&near));
            assert!(!n.path_is(near), "{} matched {}", p.display(), near.display());
        }
        // Moving the last slash keeps the length but isn't the same path.
        if let Some(i) = b.iter().rposition(|&c| c == b'/')
            && i > 0
            && i + 1 < b.len()
        {
            let mut moved = b.to_vec();
            moved.swap(i, i + 1);
            let moved = Path::new(OsStr::from_bytes(&moved));
            assert!(!n.path_is(moved), "{} matched {}", p.display(), moved.display());
        }
    }

    fn top(name: &str, p: &Path) -> Node {
        let mut n = empty_node();
        n.name = name.into();
        n.set_path(p);
        n
    }

    /// Tops: the root, a mount shown by its label, a relative path, a name
    /// that isn't UTF-8.
    #[test]
    fn tops_keep_their_whole_path() {
        check(&top("/", Path::new("/")), Path::new("/"));
        check(&top("Backup disk", Path::new("/run/media/u/16TB")), Path::new("/run/media/u/16TB"));
        check(&top("16TB", Path::new("/run/media/u/16TB")), Path::new("/run/media/u/16TB"));
        check(&top("rel", Path::new("rel")), Path::new("rel"));
        check(&top("a/b", Path::new("a/b")), Path::new("a/b"));
        let odd = Path::new(OsStr::from_bytes(b"/tmp/\xff\xfe\\x"));
        check(&top(&file_name_of(odd), odd), odd);
    }

    /// Children placed in a shared folder: in "/", with names shown
    /// differently from disk (bad UTF-8, a backslash, a newline, a name
    /// that looks like an escape), and a sibling whose shown name is
    /// another's disk name.
    #[test]
    fn placed_children_match_their_disk_path() {
        for dir in ["/", "/a", "/a/", "rel", "/a b/\u{1F600}"] {
            let shared: Arc<Path> = Path::new(dir).into();
            for disk in [
                &b"x"[..],
                b"\xff",
                b"a\\b",
                b"line\nbreak",
                b"\\xFF",
                b" lead and trail ",
                b"\xe2\x82",
            ] {
                let disk = OsStr::from_bytes(disk);
                let mut n = empty_node();
                n.name = show_os(disk);
                n.place(shared.clone(), disk);
                check(&n, &Path::new(dir).join(disk));
                // The shown name never matches when it differs from disk.
                if n.name.as_bytes() != disk.as_bytes() {
                    assert!(!n.path_is(&Path::new(dir).join(&n.name)));
                }
                let mut copy = empty_node();
                copy.name = n.name.clone();
                copy.copy_place(&n);
                check(&copy, &Path::new(dir).join(disk));
            }
        }
    }

    /// A real scan of names that display differently from disk: every
    /// node's path exists, and agrees with all the other readings.
    #[test]
    fn scanned_paths_exist() {
        let dir = std::env::temp_dir().join(format!("spacemap-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let names: [&[u8]; 5] = [b"\xff dir", b"back\\slash", b"new\nline", b"\\xFF", b"plain"];
        for d in names {
            let sub = dir.join(OsStr::from_bytes(d));
            std::fs::create_dir_all(&sub).unwrap();
            for f in names {
                std::fs::write(sub.join(OsStr::from_bytes(f)), b"1").unwrap();
            }
        }
        let mounts = HashSet::new();
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let ctx = ScanCtx {
            mounts: &mounts,
            progress: &tx,
            counter: &Default::default(),
            cancel: &Default::default(),
            progress_interval: 512,
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: None,
        };
        let tree = scan_dir(&dir, &ctx);
        check(&tree, &dir);
        let mut seen = 0;
        let mut stack = vec![&tree];
        while let Some(n) = stack.pop() {
            let p = n.path();
            assert!(std::fs::symlink_metadata(&p).is_ok(), "{}", p.display());
            check(n, &p);
            assert_eq!(n.disk_name(), p.file_name().unwrap());
            seen += 1;
            stack.extend(n.children.iter());
        }
        assert_eq!(seen, 1 + names.len() * (1 + names.len()));
        let flat = flat_copy(&tree);
        for f in &flat.children {
            assert!(std::fs::symlink_metadata(f.path()).is_ok(), "{}", f.path().display());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
