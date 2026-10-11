//! The scanned tree: Node, its order by size or file count, finding
//! nodes by path, and freeing big trees off the window's thread.

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
    // Its details, kept here one by one rather than as a `Stat`: that
    // would add padding to every node.
    pub(crate) mode: u32,
    pub(crate) mtime: i64,
    pub(crate) ctime: i64,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    /// Birth (creation) time, 0 when the filesystem doesn't report one.
    pub(crate) btime: i64,
}

/// A file's details as read from disk: permissions, owner and times.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Stat {
    pub(crate) mode: u32,
    pub(crate) mtime: i64,
    pub(crate) ctime: i64,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    /// Birth (creation) time, 0 when the filesystem doesn't report one.
    pub(crate) btime: i64,
}

impl Stat {
    /// Details not known: no permissions, owner or times.
    pub(crate) const UNKNOWN: Stat = Stat {
        mode: 0,
        mtime: NO_TIME,
        ctime: NO_TIME,
        uid: 0,
        gid: 0,
        btime: 0,
    };

    /// The details in `m`.
    pub(crate) fn of(m: &std::fs::Metadata) -> Stat {
        use std::os::unix::fs::MetadataExt;
        Stat {
            mode: m.mode(),
            mtime: m.mtime(),
            ctime: m.ctime(),
            uid: m.uid(),
            gid: m.gid(),
            btime: birth_secs(m),
        }
    }

    /// The details in `m`, or unknown ones.
    pub(crate) fn of_or_unknown(m: Option<&std::fs::Metadata>) -> Stat {
        m.map_or(Stat::UNKNOWN, Stat::of)
    }
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
    /// A node named `name` (shown name, see `show_os`), with no size, no
    /// children and no category. Place it with `place` or `set_path`.
    pub(crate) fn new(name: impl Into<Box<str>>, is_dir: bool, stat: Stat) -> Node {
        Node {
            name: name.into(),
            dir: Path::new("").into(),
            raw: None,
            size: 0,
            file_count: 0,
            is_dir,
            cat: NO_CAT,
            children: Box::default(),
            mode: 0,
            mtime: NO_TIME,
            ctime: NO_TIME,
            uid: 0,
            gid: 0,
            btime: 0,
        }
        .with_stat(stat)
    }

    /// Sets its details.
    pub(crate) fn set_stat(&mut self, s: Stat) {
        let Stat {
            mode,
            mtime,
            ctime,
            uid,
            gid,
            btime,
        } = s;
        (self.mode, self.mtime, self.ctime) = (mode, mtime, ctime);
        (self.uid, self.gid, self.btime) = (uid, gid, btime);
    }

    fn with_stat(mut self, s: Stat) -> Node {
        self.set_stat(s);
        self
    }

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

/// No category stored for this node (a folder, or not classified).
pub(crate) const NO_CAT: u8 = u8::MAX;

/// `c` as stored in `Node::cat` (`NO_CAT` for an index too big to store).
pub(crate) fn cat_byte(c: Category) -> u8 {
    u8::try_from(c.0)
        .ok()
        .filter(|&b| b != NO_CAT)
        .unwrap_or(NO_CAT)
}

/// A folder node with no name, size, children or details.
pub(crate) fn empty_node() -> Node {
    Node::new("", true, Stat::UNKNOWN)
}

/// Folder node `n` (scanned at `path`) placed in its parent's shared
/// `in_dir`, or on its own path at a tree's top.
pub(crate) fn placed(mut n: Node, path: &Path, in_dir: Option<&Arc<Path>>) -> Node {
    match (in_dir, path.file_name()) {
        (Some(dir), Some(disk)) => n.place(dir.clone(), disk),
        _ => n.set_path(path),
    }
    n
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

/// What sizes are measured in (a setting): what slices, shares, bars and
/// the folders' order follow. Passed to whatever depends on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Measure {
    #[default]
    Bytes,
    Files,
}

impl Measure {
    /// The measure of the setting "measure in files" (`files`).
    pub(crate) fn of_setting(files: bool) -> Measure {
        if files {
            Measure::Files
        } else {
            Measure::Bytes
        }
    }

    pub(crate) fn by_files(self) -> bool {
        self == Measure::Files
    }

    /// What `n` weighs: its bytes, or its files.
    pub(crate) fn of(self, n: &Node) -> u64 {
        self.pick(n.size, n.file_count)
    }

    /// What something of `size` bytes and `files` files weighs.
    pub(crate) fn pick(self, size: u64, files: u64) -> u64 {
        match self {
            Measure::Bytes => size,
            Measure::Files => files,
        }
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
/// `measure`.
pub(crate) fn resort_above(root: &mut Node, changed: &[PathBuf], measure: Measure) {
    let files = measure.by_files();
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

#[cfg(test)]
mod layout_tests {
    use super::*;

    /// Every file and folder is a Node: its size is the scan's memory.
    #[test]
    fn nodes_stay_small() {
        assert!(std::mem::size_of::<Node>() <= 120);
    }
}
