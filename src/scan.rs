//! Scanning a folder into a tree of Nodes, on its own thread pool.

use super::*;

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
                let mut n = Node::new(label, true, Stat::of_or_unknown(meta.as_ref()));
                n.place(here.clone(), &os_name);
                n
            } else {
                deep(|| scan_dir_in(&p, name, Some(here), meta, parent, ctx))
            }
        }
        _ => {
            let (size, stat) = match entry.metadata() {
                Ok(m) => (
                    match ctx.size_of(&m) {
                        size if m.nlink() > 1 => ctx.link_size(&m, size, here, &os_name),
                        size => size,
                    },
                    Stat::of(&m),
                ),
                Err(e) => {
                    let _ = progress.send(ScanMsg::LogError(friendly_io_error(&full(), &e)));
                    (0, Stat::UNKNOWN)
                }
            };
            let mut n = Node::new(name, false, stat);
            n.place(here.clone(), &os_name);
            n.size = size;
            n.file_count = 1;
            n
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
    use std::sync::atomic::Ordering;
    if cancel.load(Ordering::Relaxed) {
        // Cancelled: stop at once; the result is discarded.
        return placed(Node::new(name, true, Stat::UNKNOWN), path, in_dir);
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
    let stat = Stat::of_or_unknown(self_meta.as_ref());
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
        live.close(open, size, file_count, stat);
    }
    let _ = progress.send(ScanMsg::SliceDone {
        exts: file_summary(&children),
    });

    let mut node = Node::new(name, true, stat);
    node.size = size;
    node.file_count = file_count;
    node.children = children.into();
    placed(node, path, in_dir)
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
            category_rows(&live, &cats, Measure::Bytes),
            category_breakdown(&tree, &cats, Measure::Bytes)
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
            let looks = Looks::build(&tree, &cats, false, Measure::Bytes);
            let t_looks = t.elapsed();
            let t = Instant::now();
            let stored = Looks::build(&tree, &cats, true, Measure::Bytes);
            let t_stored = t.elapsed();
            let t = Instant::now();
            let rows = category_breakdown(&tree, &cats, Measure::Bytes);
            let t_cat = t.elapsed();
            let t = Instant::now();
            let filtered =
                crate::filter::filter_tree_by(&tree, &|n: &Node| n.size > 4096, Measure::Bytes);
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
                            std::hint::black_box(live.snapshot(
                                7,
                                1.3 / 360.0,
                                None,
                                Measure::Bytes,
                                &mut looks,
                            ));
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
