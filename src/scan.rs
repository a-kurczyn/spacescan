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
    pub(crate) name: Box<str>,
    /// The folder it's in, shared by everything in that folder; empty for
    /// a tree's top, whose `raw` is then its whole path.
    dir: Arc<Path>,
    /// Its name on disk, when that differs from `name` (escaped or
    /// translated); None for nearly everything.
    raw: Option<Box<std::ffi::OsStr>>,
    pub(crate) size: u64,
    pub(crate) file_count: u64,
    pub(crate) is_dir: bool,
    /// A file's category (its index in the category model the scan used),
    /// or `NO_CAT`; see `Looks::build`.
    pub(crate) cat: u8,
    pub(crate) children: Box<[Node]>,
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
            cat: self.cat,
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
            Some(raw) if self.dir.as_os_str().is_empty() => {
                Path::new(raw).file_name().unwrap_or(raw)
            }
            Some(raw) => raw,
            None => std::ffi::OsStr::new(&*self.name),
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
            .unwrap_or_else(|| std::ffi::OsStr::new(&*self.name))
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
        self.raw = (disk != std::ffi::OsStr::new(&*self.name)).then(|| disk.into());
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

    /// Sets its full path (a tree's top, or a node made on its own),
    /// byte for byte as given (so "/a/./b/" stays as is, the way the
    /// paths of its children start).
    pub(crate) fn set_path(&mut self, p: &Path) {
        match (p.parent(), p.file_name()) {
            (Some(parent), Some(file)) if parent.join(file).as_os_str() == p.as_os_str() => {
                self.place(parent.into(), file)
            }
            _ => {
                self.dir = Path::new("").into();
                self.raw = Some(p.as_os_str().into());
            }
        }
    }

    /// Its full path's bytes, written into `out` (the same as `path()`).
    pub(crate) fn write_path(&self, out: &mut Vec<u8>) {
        use std::os::unix::ffi::OsStrExt;
        let (d, n) = (
            self.dir.as_os_str().as_bytes(),
            self.stored_name().as_bytes(),
        );
        out.clear();
        // As `dir.join(name)` builds it.
        if !d.is_empty() && n.first() != Some(&b'/') {
            out.extend_from_slice(d);
            if d.last() != Some(&b'/') {
                out.push(b'/');
            }
        }
        out.extend_from_slice(n);
    }

    /// True if its full path is in `set`. Sets of marked or hidden rows
    /// are nearly always small: comparing with each one is cheaper than
    /// building the path to look it up.
    pub(crate) fn is_in(&self, set: &HashSet<PathBuf>) -> bool {
        match set.len() {
            0 => false,
            1..=16 => set.iter().any(|p| self.path_is(p)),
            _ => set.contains(&self.path()),
        }
    }
}

impl Drop for Node {
    /// Frees the subtree without recursing: descendants are moved onto a
    /// work list and each is dropped once it has no children left.
    fn drop(&mut self) {
        let mut pending = std::mem::take(&mut self.children).into_vec();
        while let Some(mut n) = pending.pop() {
            pending.extend(std::mem::take(&mut n.children));
        }
    }
}

/// A node for tests: `path` (its name is the last component), size,
/// folder or file, children. (Nodes can't be built with `..empty_node()`:
/// Node has a Drop.)
#[cfg(test)]
pub(crate) fn test_node(path: &str, size: u64, is_dir: bool, children: Vec<Node>) -> Node {
    let mut n = empty_node();
    n.name = file_name_of(Path::new(path)).into();
    n.set_path(Path::new(path));
    n.size = size;
    n.file_count = u64::from(!is_dir);
    n.is_dir = is_dir;
    n.children = children.into();
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
pub(crate) type FxHashSet<K> = HashSet<K, FxBuild>;

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

/// No category stored for this node (a folder, or not classified).
pub(crate) const NO_CAT: u8 = u8::MAX;

/// `c` as stored in `Node::cat` (`NO_CAT` for an index too big to store).
pub(crate) fn cat_byte(c: Category) -> u8 {
    u8::try_from(c.0)
        .ok()
        .filter(|&b| b != NO_CAT)
        .unwrap_or(NO_CAT)
}

pub(crate) fn empty_node() -> Node {
    Node {
        name: Box::default(),
        dir: Path::new("").into(),
        raw: None,
        size: 0,
        file_count: 0,
        is_dir: true,
        cat: NO_CAT,
        children: Box::default(),
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
/// `\t` or `\x1B` (`\u{85}` above ASCII, so it can't pass for a raw
/// byte), characters that show as nothing or rearrange the text around
/// them as `\u{200B}` (see `is_invisible`), and a backslash as `\\`.
pub(crate) fn show_os(s: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    // Nearly every name is valid UTF-8 with nothing to escape: a plain copy.
    if let Ok(text) = std::str::from_utf8(s.as_bytes()) {
        let plain = |c: char| c != '\\' && !c.is_control() && !is_invisible(c);
        let clean = if text.is_ascii() {
            text.bytes().all(|b| b >= 0x20 && b != 0x7f && b != b'\\')
        } else {
            text.chars().all(plain)
        };
        if clean {
            return text.to_owned();
        }
    }
    show_os_escaped(s)
}

/// The name `shown` stands for, read back from `show_os`'s escapes; None
/// if it has none, or one `show_os` never writes.
pub(crate) fn unshow_os(shown: &str) -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;
    if !shown.contains('\\') {
        return None;
    }
    let mut out: Vec<u8> = Vec::with_capacity(shown.len());
    let mut rest = shown;
    while let Some(at) = rest.find('\\') {
        out.extend_from_slice(&rest.as_bytes()[..at]);
        let esc = &rest[at + 1..];
        let (bytes, used): (Vec<u8>, usize) = match esc.chars().next()? {
            '\\' => (vec![b'\\'], 1),
            'n' => (vec![b'\n'], 1),
            't' => (vec![b'\t'], 1),
            'r' => (vec![b'\r'], 1),
            'x' => (vec![u8::from_str_radix(esc.get(1..3)?, 16).ok()?], 3),
            'u' => {
                let hex = esc.strip_prefix("u{")?.split_once('}')?.0;
                let c = char::from_u32(u32::from_str_radix(hex, 16).ok()?)?;
                (c.to_string().into_bytes(), 3 + hex.len())
            }
            _ => return None,
        };
        out.extend_from_slice(&bytes);
        rest = &esc[used..];
    }
    out.extend_from_slice(rest.as_bytes());
    Some(std::ffi::OsString::from_vec(out))
}

/// `show_os` for a name that may need escapes.
fn show_os_escaped(s: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::with_capacity(s.len());
    for chunk in s.as_bytes().utf8_chunks() {
        let mut chars = chunk.valid().chars().peekable();
        // The character before, emoji presentation selectors skipped.
        let mut prev: Option<char> = None;
        while let Some(c) = chars.next() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                c if c.is_control() && c.is_ascii() => {
                    out.push_str(&format!("\\x{:02X}", c as u32))
                }
                c if c.is_control() || (is_invisible(c) && !in_emoji(prev, c, chars.peek())) => {
                    out.push_str(&format!("\\u{{{:X}}}", c as u32))
                }
                c => out.push(c),
            }
            if !matches!(c, '\u{FE0E}' | '\u{FE0F}') {
                prev = Some(c);
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02X}"));
        }
    }
    out
}

/// True for a character that shows as nothing, or changes how the text
/// around it shows without showing itself: Unicode's default-ignorable
/// characters (zero-width spaces and joiners, direction controls, the soft
/// hyphen, the byte order mark, variation selectors, Hangul fillers, tags)
/// and the line and paragraph separators. Two names differing only in one
/// would look the same (SM-79).
fn is_invisible(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x2028..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFFB
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

/// True for an emoji, or a symbol that can show as one.
fn is_pictographic(c: char) -> bool {
    matches!(
        c as u32,
        0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x21AA
            | 0x231A..=0x23FF
            | 0x24C2
            | 0x25AA..=0x27BF
            | 0x2934..=0x2935
            | 0x2B05..=0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1FAFF
    )
}

/// True if invisible `c`, between `prev` and `next`, is part of an emoji
/// and shows there: a joiner between two emoji (👨‍👩‍👧), or a presentation
/// selector after an emoji (❤️) or in a keycap (1️⃣).
fn in_emoji(prev: Option<char>, c: char, next: Option<&char>) -> bool {
    let emoji_before = prev.is_some_and(is_pictographic);
    match c {
        '\u{200D}' => emoji_before && next.is_some_and(|&n| is_pictographic(n)),
        '\u{FE0E}' | '\u{FE0F}' => {
            emoji_before
                || (prev.is_some_and(|p| p.is_ascii_digit() || p == '#' || p == '*')
                    && next == Some(&'\u{20E3}'))
        }
        _ => false,
    }
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
        cat: c.cat,
        children: children.into(),
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
/// `/sys/dev/block/<major>:<minor>/queue/rotational` (for a partition, in its
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
    pub(crate) cancel: &'a Arc<std::sync::atomic::AtomicBool>,
    /// Count file lengths instead of the disk space actually used.
    pub(crate) apparent_size: bool,
    /// Files with several hard links: each is counted once, like `du`,
    /// under its first name in byte order.
    pub(crate) hard_links: HardLinks,
    /// Set once any name has Korean script in it, so the app can load a
    /// font for it (see `install_fallback_fonts`).
    pub(crate) saw_hangul: &'a std::sync::atomic::AtomicBool,
    /// Read each folder's entries in file-number order (for spinning disks,
    /// see `is_rotational`).
    pub(crate) in_file_order: bool,
    /// The live tree, updated for every folder and file read.
    pub(crate) live: Option<&'a LiveTree>,
}

/// The files with several hard links met during a scan.
#[derive(Default)]
pub(crate) struct HardLinks {
    /// By (device, inode), split by inode so scan threads rarely wait for
    /// each other.
    shards: [std::sync::Mutex<FxHashMap<(u64, u64), HardLink>>; 16],
    /// Files counted under a name outside the scanned folder (when only a
    /// part of a scanned tree is scanned again): none of their names in it
    /// count.
    pub(crate) elsewhere: FxHashSet<(u64, u64)>,
    /// After the scan: where each file with several names in it was
    /// counted.
    pub(crate) counted_at: std::sync::Mutex<Vec<((u64, u64), PathBuf)>>,
}

impl HardLinks {
    /// For scanning a folder of a scanned tree again: the files in
    /// `elsewhere` are counted outside it.
    pub(crate) fn counted_elsewhere(elsewhere: FxHashSet<(u64, u64)>) -> HardLinks {
        HardLinks {
            elsewhere,
            ..Default::default()
        }
    }
}

/// A file with several hard links met during a scan.
pub(crate) struct HardLink {
    size: u64,
    /// The name its size was counted under (the first one read).
    counted: LinkName,
    /// Its first name in byte order, where the size belongs, if that's
    /// another name.
    first: Option<LinkName>,
}

/// One name of a file with several hard links.
pub(crate) struct LinkName {
    /// The folder it's in (shared with the folder's entries).
    dir: Arc<Path>,
    name: Box<std::ffi::OsStr>,
}

/// True if `dir` joined with `name` comes before `other`'s full path in
/// byte order.
fn path_before(dir: &Path, name: &std::ffi::OsStr, other: &LinkName) -> bool {
    use std::os::unix::ffi::OsStrExt;
    thread_local! {
        static BUFS: std::cell::RefCell<(Vec<u8>, Vec<u8>)> =
            const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
    }
    let join = |out: &mut Vec<u8>, dir: &Path, name: &std::ffi::OsStr| {
        let dir = dir.as_os_str().as_bytes();
        out.clear();
        out.extend_from_slice(dir);
        if !dir.is_empty() && !dir.ends_with(b"/") {
            out.push(b'/');
        }
        out.extend_from_slice(name.as_bytes());
    };
    BUFS.with_borrow_mut(|(a, b)| {
        join(a, dir, name);
        join(b, &other.dir, &other.name);
        a < b
    })
}

impl ScanCtx<'_> {
    /// Size to count for an entry: disk space actually allocated (so a
    /// sparse file counts what it really uses), or its length when
    /// `apparent_size` is set.
    fn size_of(&self, m: &std::fs::Metadata) -> u64 {
        use std::os::unix::fs::MetadataExt;
        if self.apparent_size {
            m.len()
        } else {
            m.blocks() * 512
        }
    }

    /// Size to count for file `name` in folder `here`, which has several
    /// hard links and uses `size`: all of it at the first of its names read,
    /// none at the others (see `count_links_at_first_names`).
    fn link_size(
        &self,
        m: &std::fs::Metadata,
        size: u64,
        here: &Arc<Path>,
        name: &std::ffi::OsStr,
    ) -> u64 {
        use std::os::unix::fs::MetadataExt;
        if self.hard_links.elsewhere.contains(&(m.dev(), m.ino())) {
            return 0;
        }
        let this = || LinkName {
            dir: here.clone(),
            name: name.into(),
        };
        let shard = &self.hard_links.shards[(m.ino() % 16) as usize];
        match shard.lock().unwrap().entry((m.dev(), m.ino())) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(HardLink {
                    size,
                    counted: this(),
                    first: None,
                });
                size
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let link = o.get_mut();
                let first = link.first.as_ref().unwrap_or(&link.counted);
                if path_before(here, name, first) {
                    link.first = Some(this());
                }
                0
            }
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

/// The folder an entry is scanned in.
pub(crate) struct InFolder<'a> {
    /// Its handle, open when paths below it get long (see `DirHandle`).
    handle: &'a DirHandle,
    /// Its place in the live tree (None: above the scanned folder).
    live: Option<&'a LiveFolder>,
}

/// Scans one entry of folder `here`: a folder on the same filesystem is
/// scanned into a subtree, anything else becomes a file node.
pub(crate) fn scan_entry(
    entry: &std::fs::DirEntry,
    here: &Arc<Path>,
    parent: &InFolder,
    ctx: &ScanCtx,
) -> Node {
    let ScanCtx {
        mounts, progress, ..
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
    match ft {
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
                    name: label.into(),
                    dir: here.clone(),
                    size: 0,
                    file_count: 0,
                    is_dir: true,
                    cat: NO_CAT,
                    children: Box::default(),
                    mode: meta.as_ref().map(|m| m.mode()).unwrap_or(0),
                    mtime: meta.as_ref().map(|m| m.mtime()).unwrap_or(NO_TIME),
                    ctime: meta.as_ref().map(|m| m.ctime()).unwrap_or(NO_TIME),
                    uid: meta.as_ref().map(|m| m.uid()).unwrap_or(0),
                    gid: meta.as_ref().map(|m| m.gid()).unwrap_or(0),
                    btime: meta.as_ref().map(birth_secs).unwrap_or(0),
                }
            } else {
                deep(|| scan_dir_in(&p, name, Some(here), meta, parent, ctx))
            }
        }
        _ => {
            let (sz, mode, mtime, ctime, uid, gid, btime) = match entry.metadata() {
                Ok(m) => (
                    match ctx.size_of(&m) {
                        size if m.nlink() > 1 => ctx.link_size(&m, size, here, &os_name),
                        size => size,
                    },
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
                name: name.into(),
                dir: here.clone(),
                raw,
                size: sz,
                file_count: 1,
                is_dir: false,
                cat: NO_CAT,
                children: Box::default(),
                mode,
                mtime,
                ctime,
                uid,
                gid,
                btime,
            }
        }
    }
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

/// Whether sizes are measured in files instead of bytes (a setting): what
/// slices, shares, bars and the folders' order follow.
pub(crate) static MEASURE_FILES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// True while sizes are measured in files.
pub(crate) fn measure_files() -> bool {
    #[cfg(test)]
    return TEST_MEASURE_FILES.with(|m| m.get());
    #[cfg(not(test))]
    MEASURE_FILES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Sets the measure in use (`files`, else bytes).
pub(crate) fn set_measure_files(files: bool) {
    #[cfg(test)]
    TEST_MEASURE_FILES.with(|m| m.set(files));
    MEASURE_FILES.store(files, std::sync::atomic::Ordering::Relaxed);
}

// Tests run side by side: each test thread has its own measure.
#[cfg(test)]
thread_local! {
    static TEST_MEASURE_FILES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What `n` weighs in the measure in use: its bytes, or its files.
pub(crate) fn weight(n: &Node) -> u64 {
    if measure_files() {
        n.file_count
    } else {
        n.size
    }
}

/// Sorts a folder's entries by `files` (file counts) or by bytes, largest
/// first, equal ones by name.
pub(crate) fn sort_by_measure(children: &mut [Node], files: bool) {
    if !files {
        return sort_largest_first(children);
    }
    children.sort_by_key(|c| std::cmp::Reverse(c.file_count));
    for same in children.chunk_by_mut(|a, b| a.file_count == b.file_count) {
        if same.len() > 1 {
            same.sort_unstable_by(|a, b| a.disk_name().cmp(b.disk_name()));
        }
    }
}

/// Sorts every folder under `root` by `files` or by bytes (in parallel),
/// as when the measure in use changes.
pub(crate) fn sort_tree_by_measure(root: &mut Node, files: bool) {
    root.children
        .par_iter_mut()
        .filter(|c| c.is_dir)
        .for_each(|c| deep(|| sort_tree_by_measure(c, files)));
    sort_by_measure(&mut root.children, files);
}

/// Sorts a folder's entries: largest first, equal sizes by name, so the same
/// contents always come in the same order (whatever order the disk lists
/// them in, whichever name of a hard-linked file was counted, and after
/// changes made in place).
pub(crate) fn sort_largest_first(children: &mut [Node]) {
    children.sort_by_key(|c| std::cmp::Reverse(c.size));
    for same in children.chunk_by_mut(|a, b| a.size == b.size) {
        if same.len() > 1 {
            same.sort_unstable_by(|a, b| a.disk_name().cmp(b.disk_name()));
        }
    }
}

/// Re-sorts, in the tree under `root`, every folder holding one of `changed`
/// or a folder above it, after their sizes changed in place: each once, by
/// the measure in use.
pub(crate) fn resort_above(root: &mut Node, changed: &[PathBuf]) {
    let files = measure_files();
    let mut folders: HashSet<&Path> = HashSet::new();
    for p in changed {
        folders.extend(p.ancestors().skip(1));
    }
    fn walk(n: &mut Node, at: &mut PathBuf, folders: &HashSet<&Path>, files: bool) {
        for c in n.children.iter_mut().filter(|c| c.is_dir) {
            at.push(c.disk_name());
            if folders.contains(at.as_path()) {
                deep(|| walk(c, at, folders, files));
            }
            at.pop();
        }
        sort_by_measure(&mut n.children, files);
    }
    let mut at = root.path();
    if folders.contains(at.as_path()) {
        walk(root, &mut at, &folders, files);
    }
}

/// The threads the app's scans run on, apart from the rest of its parallel
/// work: a scan keeps all of them busy, and work the window starts meanwhile
/// (sorting the live table's rows, say) would otherwise wait behind it.
pub(crate) fn scan_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .thread_name(|i| format!("scan-{i}"))
            .build()
            .expect("the scan threads start")
    })
}

pub(crate) fn scan_dir(path: &Path, ctx: &ScanCtx) -> Node {
    let mut root = scan_dir_in(
        path,
        file_name_of(path),
        None,
        std::fs::metadata(path).ok(),
        &InFolder {
            handle: &None,
            live: None,
        },
        ctx,
    );
    let links = ctx
        .hard_links
        .shards
        .iter()
        .flat_map(|s| std::mem::take(&mut *s.lock().unwrap()));
    let counted_at = count_links_at_first_names(&mut root, path, links);
    *ctx.hard_links.counted_at.lock().unwrap() = counted_at;
    root
}

/// Size changes inside one folder: bytes to add to (or remove from) files
/// in it, by name, and the changes inside its subfolders.
#[derive(Default)]
struct SizeChanges {
    files: Vec<(std::ffi::OsString, i128)>,
    folders: FxHashMap<std::ffi::OsString, SizeChanges>,
}

impl SizeChanges {
    /// The changes inside subfolder `name`.
    fn folder(&mut self, name: &std::ffi::OsStr) -> &mut SizeChanges {
        if !self.folders.contains_key(name) {
            self.folders.insert(name.to_owned(), SizeChanges::default());
        }
        self.folders.get_mut(name).expect("just inserted")
    }
}

/// Moves the size of each hard-linked file from the name it was counted
/// under to its first name in byte order, so the same files always give the
/// same folder sizes, however the scan's threads met them. `path` is the
/// path `root` was scanned at. Returns where each file is counted.
fn count_links_at_first_names(
    root: &mut Node,
    path: &Path,
    links: impl Iterator<Item = ((u64, u64), HardLink)>,
) -> Vec<((u64, u64), PathBuf)> {
    let mut changes = SizeChanges::default();
    let mut counted_at = Vec::new();
    for (key, link) in links {
        let owner = link.first.as_ref().unwrap_or(&link.counted);
        counted_at.push((key, owner.dir.join(&*owner.name)));
        if link.size == 0 {
            continue;
        }
        let Some(first) = link.first else { continue };
        let size = i128::from(link.size);
        for (at, delta) in [(link.counted, -size), (first, size)] {
            let Ok(rel) = at.dir.strip_prefix(path) else {
                continue;
            };
            let here = rel.iter().fold(&mut changes, |c, part| c.folder(part));
            here.files.push((at.name.into(), delta));
        }
    }
    if !changes.files.is_empty() || !changes.folders.is_empty() {
        apply_size_changes(root, &changes);
    }
    counted_at
}

/// Applies `changes` inside folder `n`, re-sorting each folder whose
/// contents changed size; returns how much `n` grew.
fn apply_size_changes(n: &mut Node, changes: &SizeChanges) -> i128 {
    let add = |size: u64, delta: i128| {
        u64::try_from((i128::from(size) + delta).max(0)).unwrap_or(u64::MAX)
    };
    // Children by name, looked up at once when there are many to find.
    let wanted = changes.files.len() + changes.folders.len();
    let by_name: Option<FxHashMap<&std::ffi::OsStr, usize>> = (wanted > 8).then(|| {
        n.children
            .iter()
            .enumerate()
            .map(|(i, c)| (c.disk_name(), i))
            .collect()
    });
    let find = |name: &std::ffi::OsStr| match &by_name {
        Some(m) => m.get(name).copied(),
        None => child_named(n, name),
    };
    let files: Vec<(usize, i128)> = changes
        .files
        .iter()
        .filter_map(|(name, delta)| Some((find(name)?, *delta)))
        .collect();
    let folders: Vec<(usize, &SizeChanges)> = changes
        .folders
        .iter()
        .filter_map(|(name, sub)| Some((find(name)?, sub)))
        .collect();
    drop(by_name);
    let mut total = 0;
    let mut resized = !files.is_empty();
    for (i, delta) in &files {
        n.children[*i].size = add(n.children[*i].size, *delta);
        total += delta;
    }
    // Subfolders in parallel: each is changed on its own.
    let subs: FxHashMap<usize, &SizeChanges> = folders.into_iter().collect();
    let deltas: Vec<i128> = if subs.is_empty() {
        Vec::new()
    } else {
        n.children
            .par_iter_mut()
            .enumerate()
            .filter_map(|(i, c)| Some((c, *subs.get(&i)?)))
            .filter(|(c, _)| c.is_dir)
            .map(|(c, sub)| deep(|| apply_size_changes(c, sub)))
            .collect()
    };
    for delta in deltas {
        total += delta;
        resized |= delta != 0;
    }
    n.size = add(n.size, total);
    if resized {
        sort_largest_first(&mut n.children);
    }
    total
}

/// Scans folder `path`, in folder `parent`. `name` is its name as shown,
/// `self_meta` its details.
fn scan_dir_in(
    path: &Path,
    name: String,
    in_dir: Option<&Arc<Path>>,
    self_meta: Option<std::fs::Metadata>,
    parent: &InFolder,
    ctx: &ScanCtx,
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
                name: name.into(),
                dir: Path::new("").into(),
                raw: None,
                size: 0,
                file_count: 0,
                is_dir: true,
                cat: NO_CAT,
                children: Box::default(),
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
    let open_at = openable(path, parent.handle);
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
    let folder = ctx.live.map(|live| live.open(parent.live, path, own_size));
    // The path its entries share.
    let here: Arc<Path> = path.into();
    let mut children: Vec<Node> = entries
        .par_iter()
        .map(|entry| {
            let this = InFolder {
                handle: &handle,
                live: folder.as_ref().map(|o| &*o.folder),
            };
            let mut node = scan_entry(entry, &here, &this, ctx);
            if let Some(live) = ctx.live {
                // Each file is classified once, here.
                if !node.is_dir {
                    node.cat = cat_byte(live.cats().of_name(&node.name));
                }
                if let Some(f) = &folder {
                    live.count(f, &node);
                }
            }
            node
        })
        .collect();

    sort_largest_first(&mut children);
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
            name: name.into(),
            dir: Path::new("").into(),
            raw: None,
            size,
            file_count,
            is_dir: true,
            cat: NO_CAT,
            children: children.into(),
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
    /// A folder (at any depth) finished scanning: (extension, size, file
    /// count) of the files directly in it, for the category bar's live
    /// totals (see `ext_key`). Sizes and colors come from the live tree.
    SliceDone {
        exts: Vec<(String, u64, u64)>,
    },
    /// The scan finished: the tree, how long it took in seconds, and where
    /// each file with several hard links was counted.
    Done(Node, f64, Vec<((u64, u64), PathBuf)>),
    Error(String),
    LogError(String),
    /// A folder whose contents couldn't be listed (its size is unknown).
    Unreadable(PathBuf),
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
    node.children.iter().position(|c| c.disk_name() == name)
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

/// Child-index path from `root` down to the node (file or folder) at
/// `path`, if it's in the tree. Every lookup of a node by its path goes
/// through this.
pub(crate) fn find_index_path(root: &Node, path: &Path) -> Option<Vec<usize>> {
    let mut n = root;
    let mut out = Vec::new();
    for name in rel_parts(&root.path(), path)? {
        let i = child_named(n, name)?;
        out.push(i);
        n = &n.children[i];
    }
    Some(out)
}

/// The node (file or folder) at `path`, if it's in the tree.
pub(crate) fn find_node<'a>(root: &'a Node, path: &Path) -> Option<&'a Node> {
    find_index_path(root, path).map(|ip| get_node(root, &ip))
}

/// Child-index path from `root` down to the folder `target`, if it's in the
/// tree (None for a file).
pub(crate) fn index_path_to(root: &Node, target: &Path) -> Option<Vec<usize>> {
    find_index_path(root, target).filter(|ip| get_node(root, ip).is_dir)
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
        // A control character above ASCII and the raw byte of the same
        // value look different (SM-76).
        assert_eq!(show_os(std::ffi::OsStr::new("a\u{80}b")), "a\\u{80}b");
        assert_eq!(show_os(std::ffi::OsStr::from_bytes(b"a\x80b")), "a\\x80b");
        assert_eq!(show_os(std::ffi::OsStr::new("c\u{9f}d")), "c\\u{9F}d");
        assert_eq!(show_os(std::ffi::OsStr::from_bytes(b"c\x9fd")), "c\\x9Fd");
        assert_eq!(show_os(std::ffi::OsStr::new("e\u{1b}")), "e\\x1B");
        assert_eq!(
            show_os(std::ffi::OsStr::new("ünïcödé 日本語")),
            "ünïcödé 日本語"
        );
    }

    /// The quick copy for plain names gives exactly what the careful,
    /// escaping path gives, for every kind of odd name.
    #[test]
    fn plain_names_take_the_quick_path_safely() {
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
            "zero\u{200b}width".as_bytes(),
            "family \u{1f468}\u{200d}\u{1f469}".as_bytes(),
            "heart \u{2764}\u{fe0f}".as_bytes(),
            "\u{3164}".as_bytes(),
        ];
        for name in names {
            let os = std::ffi::OsStr::from_bytes(name);
            assert_eq!(show_os(os), show_os_escaped(os), "{name:?}");
        }
    }

    /// Every shown name reads back to exactly the name it stands for, so
    /// the escapes lose nothing; text that isn't one of them reads as None.
    #[test]
    fn shown_names_read_back() {
        let names: &[&[u8]] = &[
            b"back\\slash",
            b"\\u{200B} typed literally",
            b"tab\there\nand\rthere",
            b"bell\x07 del\x7f",
            b"raw \xff\x80 bytes",
            "c1 \u{85}\u{9f}".as_bytes(),
            "zero\u{200b}width \u{202e}rtl \u{2028}".as_bytes(),
            "\u{1F468}\u{200D}x and 1\u{FE0F}".as_bytes(),
        ];
        for name in names {
            let os = std::ffi::OsStr::from_bytes(name);
            let shown = show_os(os);
            assert_eq!(unshow_os(&shown).as_deref(), Some(os), "{shown}");
        }
        for not_escapes in ["plain", "a\\q", "a\\x4", "a\\u{110000}", "a\\u{20", "end\\"] {
            assert_eq!(unshow_os(not_escapes), None, "{not_escapes}");
        }
    }

    /// Two names differing only in a character that shows as nothing, or
    /// that rearranges the text around it, never look the same (SM-79);
    /// emoji built with joiners and presentation selectors stay as they
    /// are, and so does ordinary text.
    #[test]
    fn invisible_characters_are_shown() {
        let plain = show_os(std::ffi::OsStr::new("pq"));
        for x in [
            '\u{200B}',
            '\u{200C}',
            '\u{200D}',
            '\u{FEFF}',
            '\u{200E}',
            '\u{202E}',
            '\u{00AD}',
            '\u{2060}',
            '\u{2028}',
            '\u{2029}',
            '\u{2066}',
            '\u{FE0F}',
            '\u{034F}',
            '\u{3164}',
            '\u{E0041}',
        ] {
            let name = format!("p{x}q");
            let shown = show_os(std::ffi::OsStr::new(&name));
            assert_ne!(shown, plain, "{x:?}");
            assert_eq!(shown, format!("p\\u{{{:X}}}q", x as u32));
        }
        for kept in [
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "\u{2764}\u{FE0F}",
            "\u{2764}\u{FE0F}\u{200D}\u{1F525}",
            "1\u{FE0F}\u{20E3}",
            "\u{1F44D}\u{1F3FD}",
            "caf\u{E9} na\u{EF}ve \u{65E5}\u{672C}\u{8A9E} \u{D55C}\u{AE00} \u{1F600}",
            "e\u{301}",
        ] {
            assert_eq!(show_os(std::ffi::OsStr::new(kept)), kept);
        }
        // A joiner or selector outside an emoji is shown.
        assert_eq!(
            show_os(std::ffi::OsStr::new("\u{1F468}\u{200D}x")),
            "\u{1F468}\\u{200D}x"
        );
        assert_eq!(show_os(std::ffi::OsStr::new("1\u{FE0F}")), "1\\u{FE0F}");
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
    /// `SPACESCAN_MEM_TREE=/ cargo test --release rescan_memory -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rescan_memory() {
        let root =
            PathBuf::from(std::env::var("SPACESCAN_MEM_TREE").unwrap_or_else(|_| "/".into()));
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scan = || {
            let ctx = ScanCtx {
                mounts: &HashSet::new(),
                progress: &tx,
                cancel: &cancel,
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
            return_freed_memory();
            std::thread::sleep(std::time::Duration::from_millis(500));
            eprintln!("scan {i}: {} MB", rss_mb());
        }
        drop(tree);
    }
}

/// Returns freed memory to the system after a scanned tree is dropped
/// (glibc otherwise keeps it).
fn return_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim has no preconditions; it only releases free
    // heap memory.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// How many values `drop_in_background` is still freeing.
static FREEING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Frees `value` (a tree, slice looks) on another thread, so the window
/// doesn't wait while millions of entries are freed.
pub(crate) fn drop_in_background<T: Send + 'static>(value: T) {
    use std::sync::atomic::Ordering::SeqCst;
    FREEING.fetch_add(1, SeqCst);
    std::thread::spawn(move || {
        drop(value);
        FREEING.fetch_sub(1, SeqCst);
    });
}

/// Like `drop_in_background`, then returns the freed memory to the system,
/// once every other value being freed in the background is freed too (a
/// tree replaced by a rescan, say), so none of it stays with the app.
/// Only when a scan ends: returning memory locks the allocator for a while
/// on a big heap, so it's never done while the user is changing things.
pub(crate) fn free_in_background<T: Send + 'static>(value: T) {
    use std::sync::atomic::Ordering::SeqCst;
    std::thread::spawn(move || {
        drop(value);
        let start = std::time::Instant::now();
        while FREEING.load(SeqCst) > 0 && start.elapsed() < std::time::Duration::from_secs(30) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        return_freed_memory();
    });
}

/// Lets go of `tree`: at once if something else still holds it (only a
/// count goes down), else freed on another thread. Either way this hold is
/// gone when the call returns, so an `Arc::make_mut` right after on another
/// hold of the same tree copies nothing.
pub(crate) fn release_tree(tree: Arc<Node>) {
    if let Ok(only) = Arc::try_unwrap(tree) {
        drop_in_background(only);
    }
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
        let dir = std::env::temp_dir().join(format!("spacescan-hangul-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("한국어.txt"), "x").unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let seen = std::sync::atomic::AtomicBool::new(false);
        let ctx = ScanCtx {
            mounts: &HashSet::new(),
            progress: &tx,
            cancel: &Default::default(),
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
        let dir = std::env::temp_dir().join(format!("spacescan-livecat-{}", std::process::id()));
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
            cancel: &Default::default(),
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: None,
        };
        let tree = scan_dir(&dir, &ctx);
        drop(tx);
        let mut live = ExtTotals::default();
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

    /// Timing of the work done once a scan ends, on the folder in
    /// $SPACESCAN_BENCH: slice looks, the category breakdown, a filtered
    /// copy (run with `cargo test --release after_scan -- --ignored
    /// --nocapture`).
    #[test]
    #[ignore]
    fn after_scan_bench() {
        let dir = PathBuf::from(std::env::var("SPACESCAN_BENCH").expect("set SPACESCAN_BENCH"));
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let cats = Arc::new(CategoryModel::defaults());
        // As in the app: files get their category during the scan.
        let live = LiveTree::new(cats.clone());
        let ctx = ScanCtx {
            mounts: &HashSet::new(),
            progress: &tx,
            cancel: &Default::default(),
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: Some(&live),
        };
        let tree = scan_dir(&dir, &ctx);
        for run in 0..3 {
            let t = Instant::now();
            let looks = Looks::build(&tree, &cats, false);
            let t_looks = t.elapsed();
            let t = Instant::now();
            let stored = Looks::build(&tree, &cats, true);
            let t_stored = t.elapsed();
            let t = Instant::now();
            let rows = category_breakdown(&tree, &cats);
            let t_cat = t.elapsed();
            let t = Instant::now();
            let filtered = crate::filter::filter_tree_by(&tree, &|n: &Node| n.size > 4096);
            let t_filter = t.elapsed();
            let t = Instant::now();
            let copy = tree.clone();
            let t_clone = t.elapsed();
            eprintln!(
                "run {run}: {} files; looks {t_looks:?} (stored categories {t_stored:?}), categories {t_cat:?}, filter {t_filter:?}, clone {t_clone:?}",
                tree.file_count
            );
            std::hint::black_box((looks, stored, rows, filtered, copy));
        }
    }

    /// Scan timing on the folder in $SPACESCAN_BENCH (run with
    /// `SPACESCAN_BENCH=<dir> cargo test --release scan_perf -- --ignored --nocapture`).
    /// $SPACESCAN_THREADS sets the number of scan threads, and $SPACESCAN_RUNS
    /// the number of runs (default 5; 1 for a cold-cache measurement).
    #[test]
    #[ignore]
    fn scan_bench() {
        let dir = PathBuf::from(std::env::var("SPACESCAN_BENCH").expect("set SPACESCAN_BENCH"));
        let env_num = |name: &str, default: usize| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let threads = env_num("SPACESCAN_THREADS", 0);
        // $SPACESCAN_FILE_ORDER=1 reads entries in file-number order.
        let in_file_order = std::env::var_os("SPACESCAN_FILE_ORDER").is_some();
        // $SPACESCAN_LIVE=1 counts every file for the live chart, as the app
        // does; $SPACESCAN_WINDOW=1 also does the window's work meanwhile
        // (extension totals, reading the live tree every 100 ms).
        let with_live = std::env::var_os("SPACESCAN_LIVE").is_some();
        let window = std::env::var_os("SPACESCAN_WINDOW").is_some();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        for run in 0..env_num("SPACESCAN_RUNS", 5) {
            let live = with_live
                .then(|| pool.install(|| LiveTree::new(Arc::new(CategoryModel::defaults()))));
            let (tx, rx) = channel();
            let drain = std::thread::spawn(move || {
                let mut exts = ExtTotals::default();
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
                cancel: &Default::default(),
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
                            std::hint::black_box(live.snapshot(7, 1.3 / 360.0, None, &mut looks));
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
        let dir = std::env::temp_dir().join(format!("spacescan-order-{}", std::process::id()));
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
                cancel: &Default::default(),
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
        let dir = std::env::temp_dir().join(format!("spacescan-mounts-{}", std::process::id()));
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
            cancel: &Default::default(),
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

    /// Writes everything a scan of $SPACESCAN_BENCH produces to
    /// $SPACESCAN_DUMP, in a fixed order: every node's values, every finished
    /// folder's report, every error and unreadable folder. Two versions of
    /// the scanner must give identical files (run with RAYON_NUM_THREADS=1
    /// where hard links are shared between folders: which name counts them
    /// depends on thread timing). $SPACESCAN_APPARENT=1 counts apparent sizes.
    /// Run with `cargo test --release scan_dump -- --ignored`.
    #[test]
    #[ignore]
    fn scan_dump() {
        let dir = PathBuf::from(std::env::var("SPACESCAN_BENCH").unwrap());
        let out = std::env::var("SPACESCAN_DUMP").unwrap();
        let apparent = std::env::var_os("SPACESCAN_APPARENT").is_some();
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
            cancel: &Default::default(),
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
            assert!(
                !n.path_is(near),
                "{} matched {}",
                p.display(),
                near.display()
            );
        }
        // Moving the last slash keeps the length but isn't the same path.
        if let Some(i) = b.iter().rposition(|&c| c == b'/')
            && i > 0
            && i + 1 < b.len()
        {
            let mut moved = b.to_vec();
            moved.swap(i, i + 1);
            let moved = Path::new(OsStr::from_bytes(&moved));
            assert!(
                !n.path_is(moved),
                "{} matched {}",
                p.display(),
                moved.display()
            );
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
        check(
            &top("Backup disk", Path::new("/run/media/u/16TB")),
            Path::new("/run/media/u/16TB"),
        );
        check(
            &top("16TB", Path::new("/run/media/u/16TB")),
            Path::new("/run/media/u/16TB"),
        );
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
                n.name = show_os(disk).into();
                n.place(shared.clone(), disk);
                check(&n, &Path::new(dir).join(disk));
                // The shown name never matches when it differs from disk.
                if n.name.as_bytes() != disk.as_bytes() {
                    assert!(!n.path_is(&Path::new(dir).join(&*n.name)));
                }
                let mut copy = empty_node();
                copy.name = n.name.clone();
                copy.copy_place(&n);
                check(&copy, &Path::new(dir).join(disk));
            }
        }
    }

    fn scan(p: &Path, mounts: &HashSet<PathBuf>, cancel: bool) -> Node {
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(cancel));
        let ctx = ScanCtx {
            mounts,
            progress: &tx,
            cancel: &cancel,
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: None,
        };
        scan_dir(p, &ctx)
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spacescan-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Tops without a last name ("/", "..", "/a/..") keep their whole path.
    #[test]
    fn tops_without_a_last_name() {
        for p in ["/", ".", "..", "/a/..", "a/.."] {
            let p = Path::new(p);
            let n = top(&file_name_of(p), p);
            assert_eq!(n.path().as_os_str(), p.as_os_str());
            assert!(n.path_is(p));
            assert_eq!(n.path_len(), p.as_os_str().len());
        }
    }

    /// A scan started on an untidy path ("/x/./d/", "/x/d//"): the top's
    /// path stays byte for byte what its children's paths start with, so
    /// lookups by "top path + child name" and the live colors' keys match.
    #[test]
    fn untidy_tops_match_their_children() {
        let base = temp("untidy");
        std::fs::create_dir_all(base.join("d/sub")).unwrap();
        std::fs::write(base.join("d/sub/f"), b"1").unwrap();
        std::fs::write(base.join("d/file"), b"1").unwrap();
        let b = base.display();
        for p in [
            format!("{b}/d/./"),
            format!("{b}/d/."),
            format!("{b}/d//"),
            format!("{b}/./d"),
            format!("{b}//d"),
            format!("{b}/d/"),
            format!("{b}/d/../d"),
        ] {
            let p = Path::new(&p);
            let root = scan(p, &HashSet::new(), false);
            assert_eq!(root.path().as_os_str(), p.as_os_str());
            assert!(root.path_is(p));
            assert_eq!(root.path_len(), p.as_os_str().len());
            assert_eq!(node_key(&root), path_key(p));
            assert_eq!(root.children.len(), 2, "{}", p.display());
            for c in &root.children {
                let at = root.path().join(c.disk_name());
                assert!(c.path_is(&at), "{}", at.display());
                assert_eq!(node_key(c), path_key(&at));
                assert!(find_node(&root, &at).is_some(), "{}", at.display());
                if c.is_dir {
                    assert!(find_node(&root, &at).is_some());
                    assert_eq!(index_path_to(&root, &at).map(|v| v.len()), Some(1));
                }
            }
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A cancelled scan's top, and a mount left out of a scan under a name
    /// that isn't UTF-8: both keep their path on disk, not their label.
    #[test]
    fn cancelled_tops_and_odd_mounts() {
        let base = temp("odd-mount");
        let odd = base.join(OsStr::from_bytes(b"m\xff"));
        std::fs::create_dir_all(&odd).unwrap();
        let cancelled = scan(&base, &HashSet::new(), true);
        check(&cancelled, &base);
        let mounts: HashSet<PathBuf> = [odd.clone()].into();
        let root = scan(&base, &mounts, false);
        let i = child_named(&root, odd.file_name().unwrap()).unwrap();
        let m = &root.children[i];
        check(m, &odd);
        assert!(!m.path_is(&base.join(&*m.name)));
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Siblings where one's shown name looks like the other's escape
    /// (a literal "\xFF" on disk next to the byte 0xFF): lookups pick the
    /// right one.
    #[test]
    fn lookalike_siblings_stay_apart() {
        let base = temp("lookalike");
        let names: [&[u8]; 3] = [b"\\xff", b"\\xFF", b"\\"];
        let odd: &[u8] = &[0xff];
        for n in names.iter().chain([&odd]) {
            std::fs::write(base.join(OsStr::from_bytes(n)), b"1").unwrap();
        }
        let root = scan(&base, &HashSet::new(), false);
        assert_eq!(root.children.len(), 4);
        for n in names.iter().chain([&odd]) {
            let disk = OsStr::from_bytes(n);
            let at = base.join(disk);
            let found = find_node(&root, &at).unwrap();
            assert_eq!(found.disk_name(), disk);
            let one: HashSet<PathBuf> = [at.clone()].into();
            let hits: Vec<_> = root.children.iter().filter(|c| c.is_in(&one)).collect();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].disk_name(), disk);
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// `is_in` gives the same answer for small sets (compared one by one)
    /// and big ones (looked up), with near misses in both.
    #[test]
    fn is_in_small_and_big_sets() {
        let shared: Arc<Path> = Path::new("/a/b").into();
        let mut n = empty_node();
        n.name = show_os(OsStr::from_bytes(b"c\xff")).into();
        n.place(shared, OsStr::from_bytes(b"c\xff"));
        let me = n.path();
        let near = |k: usize| PathBuf::from(format!("/a/b/c{k}"));
        for size in [0, 1, 2, 16, 17, 100] {
            let mut set: HashSet<PathBuf> = (0..size).map(near).collect();
            set.insert(PathBuf::from("/a/b"));
            set.insert(PathBuf::from("/a/bc\u{ff}"));
            set.insert(PathBuf::from("/a/b/c\\xFF"));
            assert!(!n.is_in(&set), "{size}");
            set.insert(me.clone());
            assert!(n.is_in(&set), "{size}");
        }
    }

    /// The live colors' key of a node is the key of its path, for every kind
    /// of place.
    #[test]
    fn node_keys_match_path_keys() {
        for p in ["/", "/a", "/a/", "rel", "/a//b/./c/", ".."] {
            let p = Path::new(p);
            let n = top(&file_name_of(p), p);
            assert_eq!(node_key(&n), path_key(&n.path()));
            assert_eq!(node_key(&n), path_key(p));
        }
        for dir in ["/", "/a", "/a/", "", "rel"] {
            let mut n = empty_node();
            n.name = show_os(OsStr::from_bytes(b"x\xff")).into();
            n.place(Path::new(dir).into(), OsStr::from_bytes(b"x\xff"));
            assert_eq!(node_key(&n), path_key(&n.path()), "{dir}");
        }
    }

    /// A real scan of names that display differently from disk: every
    /// node's path exists, and agrees with all the other readings.
    #[test]
    fn scanned_paths_exist() {
        let dir = std::env::temp_dir().join(format!("spacescan-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let names: [&[u8]; 5] = [
            b"\xff dir",
            b"back\\slash",
            b"new\nline",
            b"\\xFF",
            b"plain",
        ];
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
            cancel: &Default::default(),
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
            assert!(
                std::fs::symlink_metadata(f.path()).is_ok(),
                "{}",
                f.path().display()
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod hard_link_tests {
    use super::*;

    fn scan(p: &Path, apparent: bool, threads: usize) -> Node {
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
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| scan_dir(p, &ctx))
    }

    /// Every folder's (path, size, files), and every file's size, in a
    /// fixed order.
    fn sizes(n: &Node, out: &mut Vec<(PathBuf, u64, u64)>) {
        out.push((n.path(), n.size, n.file_count));
        for c in &n.children {
            sizes(c, out);
        }
        assert!(n.children.is_sorted_by_key(|c| std::cmp::Reverse(c.size)));
        if n.is_dir {
            let own = std::fs::symlink_metadata(n.path()).map_or(0, |m| {
                use std::os::unix::fs::MetadataExt;
                m.blocks() * 512
            });
            let sum = n.children.iter().map(|c| c.size).sum::<u64>();
            assert!(n.size == sum + own || n.size == sum + n.size.saturating_sub(sum));
        }
    }

    /// Scanning one folder of a scanned tree again gives exactly the sizes
    /// the full scan gave it: a file counted under a name outside the folder
    /// counts nothing in it (three names, two of them inside, one deeper),
    /// one counted inside still counts there.
    #[test]
    fn rescanned_folders_match_the_full_scan() {
        let dir = std::env::temp_dir().join(format!("spacescan-relinks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["a", "b/c", "z"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        let data = vec![9u8; 300_000];
        std::fs::write(dir.join("a/x"), &data).unwrap();
        std::fs::hard_link(dir.join("a/x"), dir.join("b/x")).unwrap();
        std::fs::hard_link(dir.join("a/x"), dir.join("b/c/x")).unwrap();
        std::fs::write(dir.join("b/y"), &data[..100_000]).unwrap();
        std::fs::hard_link(dir.join("b/y"), dir.join("z/y")).unwrap();
        let scan_with = |p: &Path, elsewhere: FxHashSet<(u64, u64)>| {
            let (tx, rx) = channel();
            std::thread::spawn(move || for _ in rx {});
            let ctx = ScanCtx {
                mounts: &HashSet::new(),
                progress: &tx,
                cancel: &Default::default(),
                apparent_size: false,
                hard_links: HardLinks::counted_elsewhere(elsewhere),
                saw_hangul: &Default::default(),
                in_file_order: false,
                live: None,
            };
            let node = scan_dir(p, &ctx);
            let counted_at = std::mem::take(&mut *ctx.hard_links.counted_at.lock().unwrap());
            (node, counted_at)
        };
        let (full, counted_at) = scan_with(&dir, Default::default());
        let owners: HashMap<(u64, u64), PathBuf> = counted_at.into_iter().collect();
        assert_eq!(owners.len(), 2);
        let mut seen = Vec::new();
        sizes(&full, &mut seen);
        for part in ["a", "b", "b/c", "z"] {
            let at = dir.join(part);
            let elsewhere = owners
                .iter()
                .filter(|(_, o)| !o.starts_with(&at))
                .map(|(k, _)| *k)
                .collect();
            let (again, _) = scan_with(&at, elsewhere);
            let mut got = Vec::new();
            sizes(&again, &mut got);
            let want: Vec<_> = seen
                .iter()
                .filter(|(p, ..)| p.starts_with(&at))
                .cloned()
                .collect();
            assert_eq!(got, want, "{part}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Hard links across folders at different depths, in one folder, three
    /// names for one file, names where byte order isn't folder order
    /// ("a-c/…" before "a/b/…"), and thousands of linked files: every scan,
    /// with any number of threads, gives the same sizes and order
    /// everywhere, with each file's size at its first name in byte order and
    /// counted once.
    #[test]
    fn hard_links_count_at_their_first_name_every_time() {
        let dir = std::env::temp_dir().join(format!("spacescan-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let deep_dir = (0..30).fold(dir.join("z"), |p, i| p.join(format!("d{i}")));
        for d in ["a/b", "a-c", "m", "same", "many1", "many2"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::create_dir_all(&deep_dir).unwrap();
        let mb = vec![7u8; 1 << 20];
        std::fs::write(dir.join("m/three"), &mb).unwrap();
        std::fs::hard_link(dir.join("m/three"), deep_dir.join("three")).unwrap();
        std::fs::hard_link(dir.join("m/three"), dir.join("a-c/three")).unwrap();
        std::fs::write(dir.join("a-c/order"), &mb[..300_000]).unwrap();
        std::fs::hard_link(dir.join("a-c/order"), dir.join("a/b/order")).unwrap();
        std::fs::write(dir.join("same/y"), &mb[..200_000]).unwrap();
        std::fs::hard_link(dir.join("same/y"), dir.join("same/x")).unwrap();
        // Equal-size sibling folders, with links inside one and across two:
        // folders above the links keep one order, whatever was counted first.
        for t in 0..6 {
            let at = dir.join(format!("ties/t{t}"));
            std::fs::create_dir_all(&at).unwrap();
            for f in 0..4 {
                std::fs::write(at.join(format!("f{f}")), [3u8; 9000]).unwrap();
            }
        }
        std::fs::hard_link(dir.join("ties/t3/f0"), dir.join("ties/t3/g0")).unwrap();
        std::fs::hard_link(dir.join("ties/t1/f1"), dir.join("ties/t4/g1")).unwrap();
        for i in 0..2000 {
            let f = dir.join(format!("many2/f{i}"));
            std::fs::write(&f, [1u8; 5000]).unwrap();
            std::fs::hard_link(&f, dir.join(format!("many1/f{i}"))).unwrap();
        }
        for apparent in [false, true] {
            let first = {
                let mut v = Vec::new();
                sizes(&scan(&dir, apparent, 8), &mut v);
                v
            };
            let size_at = |rel: &str| {
                first
                    .iter()
                    .find(|(p, ..)| *p == dir.join(rel))
                    .map(|e| e.1)
                    .unwrap()
            };
            assert!(size_at("a-c/three") > 0, "a-c/three is first in path order");
            assert_eq!(size_at("m/three"), 0);
            assert_eq!(
                size_at(
                    &deep_dir
                        .strip_prefix(&dir)
                        .unwrap()
                        .join("three")
                        .to_string_lossy()
                ),
                0
            );
            assert!(size_at("a-c/order") > 0, "'-' comes before '/'");
            assert_eq!(size_at("a/b/order"), 0);
            assert!(size_at("same/x") > 0);
            assert_eq!(size_at("same/y"), 0);
            assert!(size_at("many1") > size_at("many2"));
            assert!((0..2000).all(|i| size_at(&format!("many2/f{i}")) == 0));
            for threads in [1, 2, 3, 8, 16, 1, 16] {
                for _ in 0..3 {
                    let mut again = Vec::new();
                    sizes(&scan(&dir, apparent, threads), &mut again);
                    if let Some(d) = again.iter().zip(&first).find(|(a, b)| a != b) {
                        panic!("{threads} threads, apparent {apparent}: {d:?}");
                    }
                    assert_eq!(again.len(), first.len());
                }
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
