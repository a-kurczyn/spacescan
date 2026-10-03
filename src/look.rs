//! Slice colors in the finished chart. The category taking the most space
//! in a slice sets its hue (the category bar's colors). Age, by Changed
//! time (when the data arrived on this disk), sets brightness in three
//! steps: new is brightest, `age_days` and older is darkest. Each slice
//! blends along its arc from the shade of its newest file to the shade of
//! its oldest.

use super::*;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::Relaxed;

/// What decides a slice's color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Look {
    pub(crate) cat: Category,
    /// Changed times of the newest and oldest file; NO_TIME if none is
    /// known.
    pub(crate) newest: i64,
    pub(crate) oldest: i64,
    pub(crate) size: u64,
}

/// The look of every folder in a tree, keyed by the folder node's address
/// (valid until the tree is rebuilt).
pub(crate) struct Looks {
    folders: HashMap<usize, Look>,
}

/// Running totals for one folder: bytes and files per category, and the
/// newest and oldest Changed times.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Totals {
    bytes: Vec<u64>,
    files: Vec<u64>,
    newest: Option<i64>,
    oldest: Option<i64>,
}

impl Totals {
    pub(crate) fn new(categories: usize) -> Self {
        Totals {
            bytes: vec![0; categories],
            files: vec![0; categories],
            newest: None,
            oldest: None,
        }
    }

    /// Takes the Changed times `newest` and `oldest` into account.
    fn add_times(&mut self, newest: i64, oldest: i64) {
        if newest != NO_TIME {
            self.newest = Some(self.newest.map_or(newest, |t| t.max(newest)));
        }
        if oldest != NO_TIME {
            self.oldest = Some(self.oldest.map_or(oldest, |t| t.min(oldest)));
        }
    }

    pub(crate) fn add(&mut self, other: &Totals) {
        for (a, b) in self.bytes.iter_mut().zip(&other.bytes) {
            *a = a.saturating_add(*b);
        }
        for (a, b) in self.files.iter_mut().zip(&other.files) {
            *a += b;
        }
        if let Some(t) = other.newest {
            self.add_times(t, NO_TIME);
        }
        if let Some(t) = other.oldest {
            self.add_times(NO_TIME, t);
        }
    }

    pub(crate) fn add_file(&mut self, cat: Category, size: u64, ctime: i64) {
        self.bytes[cat.0] = self.bytes[cat.0].saturating_add(size);
        self.files[cat.0] += 1;
        self.add_times(ctime, ctime);
    }

    /// The category with the most bytes (most files if all are empty).
    fn look(&self, size: u64) -> Look {
        let most = |v: &[u64]| (0..v.len()).max_by_key(|&i| (v[i], std::cmp::Reverse(i)));
        let by_bytes = most(&self.bytes).filter(|&i| self.bytes[i] > 0);
        let cat = by_bytes.or_else(|| most(&self.files)).unwrap_or(0);
        Look {
            cat: Category(cat),
            newest: self.newest.unwrap_or(NO_TIME),
            oldest: self.oldest.unwrap_or(NO_TIME),
            size,
        }
    }
}

impl Looks {
    /// Works out the look of every folder under `root`, in one pass.
    pub(crate) fn build(root: &Node, cats: &CategoryModel) -> Looks {
        fn walk(n: &Node, cats: &CategoryModel, out: &mut HashMap<usize, Look>) -> Totals {
            let mut totals = Totals::new(cats.other().0 + 1);
            for c in &n.children {
                if c.is_dir {
                    let sub = deep(|| walk(c, cats, out));
                    totals.add(&sub);
                } else {
                    totals.add_file(cats.of_name(&c.name), c.size, c.ctime);
                }
            }
            out.insert(n as *const Node as usize, totals.look(n.size));
            totals
        }
        let mut folders = HashMap::new();
        walk(root, cats, &mut folders);
        Looks { folders }
    }

    /// The look of node `n`: a file's own, or its folder's.
    pub(crate) fn of(&self, n: &Node, cats: &CategoryModel) -> Look {
        if n.is_dir
            && let Some(look) = self.folders.get(&(n as *const Node as usize))
        {
            return *look;
        }
        let mut totals = Totals::new(cats.other().0 + 1);
        totals.add_file(cats.of_name(&n.name), n.size, n.ctime);
        totals.look(n.size)
    }

    /// The look of several nodes together (an "other" slice): the category
    /// with the most bytes among their looks, and the newest and oldest of
    /// their files.
    pub(crate) fn of_group<'a>(
        &self,
        nodes: impl Iterator<Item = &'a Node>,
        cats: &CategoryModel,
    ) -> Look {
        group_look(nodes.map(|n| self.of(n, cats)), cats)
    }
}

/// The look of several looks together: the category with the most bytes,
/// and the newest and oldest of their files.
pub(crate) fn group_look(looks: impl Iterator<Item = Look>, cats: &CategoryModel) -> Look {
    let mut totals = Totals::new(cats.other().0 + 1);
    let mut size = 0u64;
    for look in looks {
        totals.bytes[look.cat.0] = totals.bytes[look.cat.0].saturating_add(look.size);
        totals.files[look.cat.0] += 1;
        totals.add_times(look.newest, look.oldest);
        size = size.saturating_add(look.size);
    }
    totals.look(size)
}

/// One scan thread's share of a folder's counters, in one block: bytes
/// per category, then files per category, then the newest and oldest
/// Changed time (as `time_bits`).
struct Shard(Box<[AtomicU64]>);

/// A time stored so that unsigned order is time order (the sign bit
/// flipped), and back.
fn time_bits(t: i64) -> u64 {
    (t as u64) ^ (1 << 63)
}

fn bits_time(b: u64) -> i64 {
    (b ^ (1 << 63)) as i64
}

impl Shard {
    fn new(categories: usize) -> Self {
        let mut v: Vec<AtomicU64> = (0..categories * 2).map(|_| AtomicU64::new(0)).collect();
        v.push(AtomicU64::new(time_bits(i64::MIN)));
        v.push(AtomicU64::new(time_bits(i64::MAX)));
        Shard(v.into_boxed_slice())
    }

    /// Newest and oldest time, NO_TIME if none.
    fn times(&self) -> (i64, i64) {
        let (n, o) = (
            bits_time(self.newest().load(Relaxed)),
            bits_time(self.oldest().load(Relaxed)),
        );
        (
            if n == i64::MIN { NO_TIME } else { n },
            if o == i64::MAX { NO_TIME } else { o },
        )
    }

    fn newest(&self) -> &AtomicU64 {
        &self.0[self.0.len() - 2]
    }

    fn oldest(&self) -> &AtomicU64 {
        &self.0[self.0.len() - 1]
    }
}

/// Running totals of the files directly in one folder being scanned. One
/// shard per scan thread, made when the thread first counts
/// something there; each has a single writer, so updates are plain loads
/// and stores (no locked instructions). Threads beyond the expected number
/// share the last shard with locked updates, so no count is ever lost.
pub(crate) struct Counters {
    categories: usize,
    shards: Box<[std::sync::OnceLock<Shard>]>,
}

/// What decides a finished folder's slice color, kept small and without
/// heap memory: the category with the most bytes, the newest and oldest
/// Changed time, and whether any file was counted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Summary {
    cat: Category,
    newest: i64,
    oldest: i64,
    any: bool,
}

impl Summary {
    fn look(&self, size: u64) -> Look {
        Look {
            cat: self.cat,
            newest: self.newest,
            oldest: self.oldest,
            size,
        }
    }
}

impl Totals {
    fn summary(&self) -> Summary {
        let look = self.look(0);
        Summary {
            cat: look.cat,
            newest: look.newest,
            oldest: look.oldest,
            any: self.files.iter().any(|&f| f > 0),
        }
    }
}

impl Counters {
    /// Counters with one shard per scan thread (`threads`), plus the shared
    /// one.
    fn new(categories: usize, threads: usize) -> Self {
        Counters {
            categories,
            shards: (0..threads + 1)
                .map(|_| std::sync::OnceLock::new())
                .collect(),
        }
    }

    fn shard(&self, i: usize) -> &Shard {
        self.shards[i].get_or_init(|| Shard::new(self.categories))
    }

    /// All shards added up.
    fn snapshot(&self) -> Totals {
        let n = self.categories;
        let mut t = Totals::new(n);
        for s in self.shards.iter().filter_map(|s| s.get()) {
            for i in 0..n {
                t.bytes[i] = t.bytes[i].saturating_add(s.0[i].load(Relaxed));
                t.files[i] += s.0[n + i].load(Relaxed);
            }
            let (newest, oldest) = s.times();
            t.add_times(newest, oldest);
        }
        t
    }
}

/// A folder's path as a number, for quick lookups. Two paths giving the
/// same number is vanishingly unlikely, and would only mix two slices'
/// live colors.
pub(crate) fn path_key(path: &Path) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    use std::os::unix::ffi::OsStrExt;
    let mut h = FxBuild::default().build_hasher();
    h.write(path.as_os_str().as_bytes());
    h.finish()
}

/// A finished folder's exact values.
struct Finished {
    size: u64,
    file_count: u64,
    mode: u32,
    mtime: i64,
    ctime: i64,
    uid: u32,
    gid: u32,
    summary: Summary,
}

/// One folder of a running scan. The scan counts every file read in its
/// folder's counters, adds subfolders as they're opened, and stores the
/// exact values when the folder finishes; the window reads all of it.
pub(crate) struct LiveFolder {
    path: PathBuf,
    /// The folder entry's own size.
    own_size: u64,
    /// The files directly in it, while it's being scanned.
    files: std::sync::RwLock<Option<Arc<Counters>>>,
    children: std::sync::Mutex<Vec<Arc<LiveFolder>>>,
    done: std::sync::OnceLock<Finished>,
    /// Totals of everything under it once finished, until its parent
    /// finishes and has added them up.
    totals: std::sync::Mutex<Option<Totals>>,
}

// Freed without recursion: a folder chain can be tens of thousands of
// levels deep.
impl Drop for LiveFolder {
    fn drop(&mut self) {
        let mut pending =
            std::mem::take(self.children.get_mut().unwrap_or_else(|e| e.into_inner()));
        while let Some(child) = pending.pop() {
            // Only the last holder takes its subfolders apart.
            if let Ok(mut c) = Arc::try_unwrap(child) {
                pending.append(c.children.get_mut().unwrap_or_else(|e| e.into_inner()));
            }
        }
    }
}

/// The folders of a running scan, shared by its threads and the window.
/// Every folder is counted, at every depth; what to show is the window's
/// choice.
pub(crate) struct LiveTree {
    cats: Arc<CategoryModel>,
    /// Threads of the pool that scans; each gets its own counter shard.
    threads: usize,
    root: std::sync::OnceLock<Arc<LiveFolder>>,
}

impl LiveTree {
    /// A tree for a scan run on the current rayon pool.
    pub(crate) fn new(cats: Arc<CategoryModel>) -> Self {
        LiveTree {
            cats,
            threads: rayon::current_num_threads(),
            root: Default::default(),
        }
    }

    /// Folder `path` (its entry `own_size` bytes) starts being scanned,
    /// inside `parent` (None: the scanned folder).
    pub(crate) fn open(&self, parent: Option<&LiveFolder>, path: &Path, own_size: u64) -> LiveOpen {
        let counters = Arc::new(Counters::new(self.cats.other().0 + 1, self.threads));
        let f = Arc::new(LiveFolder {
            path: path.to_path_buf(),
            own_size,
            files: std::sync::RwLock::new(Some(counters.clone())),
            children: Default::default(),
            done: Default::default(),
            totals: Default::default(),
        });
        match parent {
            Some(p) => p.children.lock().unwrap().push(f.clone()),
            None => {
                let _ = self.root.set(f.clone());
            }
        }
        LiveOpen {
            folder: f,
            counters,
        }
    }

    /// The calling thread's counter shard: its index in the pool, or the
    /// last one (shared, with locked updates) for any other thread.
    fn thread_index(&self) -> usize {
        rayon::current_thread_index()
            .filter(|&i| i < self.threads)
            .unwrap_or(self.threads)
    }

    /// `node` was read in folder `open`: a file counts there.
    pub(crate) fn count(&self, open: &LiveOpen, node: &Node) {
        if node.is_dir {
            return;
        }
        let n = self.cats.other().0 + 1;
        let cat = self.cats.of_name(&node.name).0;
        let i = self.thread_index();
        let s = open.counters.shard(i);
        // Only the shared shard (threads outside the pool) has several
        // writers and needs locked updates.
        let alone = i < self.threads;
        let add = |a: &AtomicU64, by: u64| {
            if alone {
                a.store(a.load(Relaxed).saturating_add(by), Relaxed);
            } else {
                let _ = a.fetch_update(Relaxed, Relaxed, |v| Some(v.saturating_add(by)));
            }
        };
        add(&s.0[cat], node.size);
        add(&s.0[n + cat], 1);
        if node.ctime != NO_TIME {
            let t = time_bits(node.ctime);
            if alone {
                if s.newest().load(Relaxed) < t {
                    s.newest().store(t, Relaxed);
                }
                if s.oldest().load(Relaxed) > t {
                    s.oldest().store(t, Relaxed);
                }
            } else {
                s.newest().fetch_max(t, Relaxed);
                s.oldest().fetch_min(t, Relaxed);
            }
        }
    }

    /// Folder `open` finished with these exact values; every subfolder has
    /// finished before it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn close(
        &self,
        open: LiveOpen,
        size: u64,
        file_count: u64,
        mode: u32,
        mtime: i64,
        ctime: i64,
        uid: u32,
        gid: u32,
    ) {
        let f = &open.folder;
        let mut totals = open.counters.snapshot();
        for c in f.children.lock().unwrap().iter() {
            if let Some(t) = c.totals.lock().unwrap().as_ref() {
                totals.add(t);
            }
        }
        let summary = totals.summary();
        *f.totals.lock().unwrap() = Some(totals);
        let _ = f.done.set(Finished {
            size,
            file_count,
            mode,
            mtime,
            ctime,
            uid,
            gid,
            summary,
        });
        // Only now that its values are stored: its counters go, and its
        // subfolders' totals are in its own.
        *f.files.write().unwrap() = None;
        for c in f.children.lock().unwrap().iter() {
            c.totals.lock().unwrap().take();
        }
    }

    /// Totals of the files directly in `f` so far.
    /// None if it has finished (its counters are gone).
    fn own_totals(&self, f: &LiveFolder) -> Option<Totals> {
        f.files.read().unwrap().as_ref().map(|c| c.snapshot())
    }

    /// The scan so far, as the window shows it: a tree of folders down to
    /// `depth` below the scanned one, with sizes, file counts and (in
    /// `looks`) colors of everything under each, at every depth. Finished
    /// folders give their exact values. Folders smaller than `min_share` of
    /// the scanned folder can't get a slice of their own: below the
    /// scanned folder's own children, they're grouped per folder into one
    /// "(N other items)" entry, so a read costs what the chart can show,
    /// not what the drive holds. None before the scan has begun.
    pub(crate) fn snapshot(
        &self,
        depth: usize,
        min_share: f64,
        looks: &mut LiveLooks,
    ) -> Option<Node> {
        let root = self.root.get()?;
        looks.summaries.clear();
        // The smallest slice, from the scanned folder's size at the last read
        // (which only grows); the first read works it out.
        let total = match looks.last_total {
            0 => self.read(root, None, 0, false, looks, Instant::now()).size,
            known => known,
        };
        let min_size = (total as f64 * min_share) as u64;
        // The scanned folder's own children are all kept: the table lists
        // them.
        let read = self.read(root, Some(depth), min_size, true, looks, Instant::now());
        looks.root = read.totals;
        looks.last_total = read.size;
        read.node
    }

    /// One folder for `snapshot`: its size, the totals of everything under
    /// it (while its parent may still need them), and its node with
    /// subfolders down to `depth` (None: below what's shown, no node).
    /// Subfolders under `min_size` are grouped into one entry, unless
    /// `keep_all`.
    fn read(
        &self,
        f: &LiveFolder,
        depth: Option<usize>,
        min_size: u64,
        keep_all: bool,
        looks: &mut LiveLooks,
        now: Instant,
    ) -> Read {
        let below = depth.and_then(|d| d.checked_sub(1));
        let children = f.children.lock().unwrap().clone();
        let finished = f.done.get();
        let mut totals = match finished {
            // Kept until its parent has added it up.
            Some(_) => f.totals.lock().unwrap().clone(),
            None => match self.own_totals(f) {
                Some(t) => Some(t),
                // It has just finished.
                None => return self.read(f, depth, min_size, keep_all, looks, now),
            },
        };
        let mut size = match finished {
            Some(d) => d.size,
            None => totals.as_ref().map_or(0, |t| {
                t.bytes.iter().fold(f.own_size, |a, b| a.saturating_add(*b))
            }),
        };
        // Below what's shown, a finished folder is done: its stored values.
        if finished.is_some() && depth.is_none() {
            return Read {
                size,
                totals,
                node: None,
            };
        }
        let mut kids = Vec::new();
        let mut grouped = Grouped::default();
        for c in &children {
            let small = |size: u64| !keep_all && size < min_size;
            let r = match c.done.get() {
                // Small and finished: its stored values, no node.
                Some(d) if small(d.size) => Read {
                    size: d.size,
                    // Only an unfinished folder still adds them up.
                    totals: match finished {
                        None => c.totals.lock().unwrap().clone(),
                        Some(_) => None,
                    },
                    node: None,
                },
                _ => deep(|| self.read(c, below, min_size, false, looks, now)),
            };
            if finished.is_none() {
                match &r.totals {
                    Some(t) => {
                        if let Some(all) = &mut totals {
                            all.add(t);
                        }
                    }
                    // A finished subfolder whose totals were already added up
                    // in this folder: it has just finished too.
                    None if f.done.get().is_some() => {
                        return self.read(f, depth, min_size, keep_all, looks, now);
                    }
                    None => debug_assert!(false, "a subfolder's totals went missing"),
                }
                size = size.saturating_add(r.size);
            }
            // Subfolders only where they're shown.
            if below.is_some() {
                match r.node {
                    Some(n) if !small(r.size) => kids.push(n),
                    _ => grouped.add(c, &r),
                }
            }
        }
        let node = depth.map(|_| {
            let (file_count, summary) = match (finished, &totals) {
                (Some(d), _) => (d.file_count, d.summary),
                (None, Some(t)) => (t.files.iter().sum(), t.summary()),
                (None, None) => (0, Totals::new(self.cats.other().0 + 1).summary()),
            };
            let mut n = looks.node(f, size, file_count, summary, finished, now);
            if below.is_some() {
                if let Some(other) = grouped.node(&f.path, looks, &self.cats, now) {
                    kids.push(other);
                }
                kids.sort_by_key(|c| std::cmp::Reverse(c.size));
                n.children = kids;
            }
            n
        });
        Read { size, totals, node }
    }
}

/// Subfolders too small for a slice of their own, as one entry.
#[derive(Default)]
struct Grouped {
    count: usize,
    size: u64,
    file_count: u64,
    looks: Vec<Look>,
}

impl Grouped {
    /// Adds subfolder `c`, read as `r`.
    fn add(&mut self, c: &LiveFolder, r: &Read) {
        self.count += 1;
        self.size = self.size.saturating_add(r.size);
        if let Some(d) = c.done.get() {
            self.file_count += d.file_count;
            self.looks.push(d.summary.look(r.size));
        } else if let Some(t) = &r.totals {
            self.file_count += t.files.iter().sum::<u64>();
            self.looks.push(t.summary().look(r.size));
        }
    }

    /// The entry, colored like the chart's "other" slices; None if empty.
    fn node(
        &self,
        parent: &Path,
        looks: &mut LiveLooks,
        cats: &CategoryModel,
        now: Instant,
    ) -> Option<Node> {
        if self.count == 0 {
            return None;
        }
        // A path no real entry can have, for its look.
        let path = parent.join("\u{1}grouped");
        let look = group_look(self.looks.iter().copied(), cats);
        let key = path_key(&path);
        looks.since.entry(key).or_insert(now);
        looks.summaries.insert(
            key,
            Summary {
                cat: look.cat,
                newest: look.newest,
                oldest: look.oldest,
                any: true,
            },
        );
        Some(Node {
            name: trf("SEG_OTHER_ITEMS", &[&self.count.to_string()]),
            path,
            size: self.size,
            file_count: self.file_count,
            is_dir: true,
            children: Vec::new(),
            mode: 0,
            mtime: NO_TIME,
            ctime: NO_TIME,
            uid: 0,
            gid: 0,
            btime: 0,
        })
    }
}

/// A folder being scanned in the live tree, and its counters (held by the
/// scan thread, so counting needs no lock).
pub(crate) struct LiveOpen {
    pub(crate) folder: Arc<LiveFolder>,
    counters: Arc<Counters>,
}

/// What `LiveTree::read` gives for one folder.
struct Read {
    size: u64,
    totals: Option<Totals>,
    node: Option<Node>,
}

/// Slice looks during a scan, for the live chart, from the latest
/// `LiveTree::snapshot`. Folders are known by `path_key`.
#[derive(Default)]
pub(crate) struct LiveLooks {
    summaries: FxHashMap<u64, Summary>,
    /// When each folder first had files classified under it.
    since: FxHashMap<u64, Instant>,
    /// The scanned folder's totals so far, for the category order.
    root: Option<Totals>,
    /// The scanned folder's size at the last read.
    last_total: u64,
}

impl LiveLooks {
    /// The snapshot node of folder `f`, recording its look.
    fn node(
        &mut self,
        f: &LiveFolder,
        size: u64,
        file_count: u64,
        summary: Summary,
        fin: Option<&Finished>,
        now: Instant,
    ) -> Node {
        let key = path_key(&f.path);
        if summary.any {
            self.since.entry(key).or_insert(now);
        }
        self.summaries.insert(key, summary);
        Node {
            name: file_name_of(&f.path),
            path: f.path.clone(),
            size,
            file_count,
            is_dir: true,
            children: Vec::new(),
            // Unfinished folders' details stay unknown ("…" in the table).
            mode: fin.map_or(0, |d| d.mode),
            mtime: fin.map_or(NO_TIME, |d| d.mtime),
            ctime: fin.map_or(NO_TIME, |d| d.ctime),
            uid: fin.map_or(0, |d| d.uid),
            gid: fin.map_or(0, |d| d.gid),
            btime: 0,
        }
    }

    /// The look of folder `node` from what's classified under it so far
    /// (final once it finished), and since when it has one.
    pub(crate) fn get(&self, node: &Node) -> Option<(Look, Instant)> {
        let key = path_key(&node.path);
        let since = *self.since.get(&key)?;
        let summary = self.summaries.get(&key)?;
        Some((summary.look(node.size), since))
    }

    /// Categories under the scanned folder so far, most bytes first.
    pub(crate) fn order(&self) -> Vec<Category> {
        let Some(t) = &self.root else {
            return Vec::new();
        };
        let mut cats: Vec<usize> = (0..t.bytes.len()).filter(|&i| t.files[i] > 0).collect();
        cats.sort_by_key(|&i| (std::cmp::Reverse(t.bytes[i]), i));
        cats.into_iter().map(Category).collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.since.is_empty()
    }

    /// When folder `path` got a color during the scan, if it did.
    pub(crate) fn colored_at(&self, path: &Path) -> Option<Instant> {
        self.since.get(&path_key(path)).copied()
    }
}

/// How age turns into brightness: `steps` shades from the color as is
/// (new) down to `darkest` brightness (`days` old or more).
#[derive(Clone, Copy)]
pub(crate) struct AgeShades {
    pub(crate) days: u32,
    pub(crate) steps: u8,
    /// Brightness of the last step, 0 to 1.
    pub(crate) darkest: f32,
}

impl AgeShades {
    fn last(&self) -> u8 {
        self.steps.max(2) - 1
    }

    /// Brightness step of a Changed time: the time from now back to
    /// `days` is split evenly between the steps, and `days` or older (or
    /// unknown) is the last.
    pub(crate) fn step(&self, ctime: i64, now: i64) -> u8 {
        const DAY: f64 = 24.0 * 3600.0;
        let last = self.last();
        if ctime == NO_TIME {
            return last;
        }
        let days = (now - ctime).max(0) as f64 / DAY;
        let share = days / self.days.max(1) as f64;
        ((share * f64::from(last)) as u8).min(last)
    }

    /// `color` at brightness step `step`: 0 is the color as is, each step
    /// evenly darker, down to `darkest` at the last.
    pub(crate) fn shade(&self, color: Color32, step: u8) -> Color32 {
        let last = f32::from(self.last());
        let factor = 1.0 - (1.0 - self.darkest) * f32::from(step).min(last) / last;
        let scale = |v: u8| (v as f32 * factor).round() as u8;
        Color32::from_rgb(scale(color.r()), scale(color.g()), scale(color.b()))
    }

    /// The colors a slice with look `look` blends between: the shade of its
    /// newest file, then of its oldest.
    pub(crate) fn look_colors(
        &self,
        look: &Look,
        now: i64,
        cats: &CategoryModel,
        dark: bool,
    ) -> (Color32, Color32) {
        let base = cats.color(look.cat, dark);
        (
            self.shade(base, self.step(look.newest, now)),
            self.shade(base, self.step(look.oldest, now)),
        )
    }
}

/// The current time in Unix seconds.
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 3600;

    #[test]
    fn age_steps_split_the_days_evenly() {
        let now = 1_000_000_000;
        let shades = AgeShades {
            days: 365,
            steps: 3,
            darkest: 0.4,
        };
        let step = |d: i64| shades.step(now - d * DAY, now);
        assert_eq!([step(1), step(182), step(183), step(364)], [0, 0, 1, 1]);
        assert_eq!([step(365), step(5000)], [2, 2]);
        assert_eq!(shades.step(NO_TIME, now), 2);
        let five = AgeShades { steps: 5, ..shades };
        assert_eq!(five.step(now - 100 * DAY, now), 1);
        // Steps never go down as files get older.
        let steps: Vec<u8> = (0..400).map(step).collect();
        assert!(steps.windows(2).all(|p| p[0] <= p[1]));
    }

    #[test]
    fn folders_take_the_biggest_category_and_their_age_range() {
        let cats = CategoryModel::defaults();
        let mut video = test_node("/r/d/a.mkv", 9000, false, vec![]);
        video.ctime = 1000;
        let mut docs: Vec<Node> = (0..5)
            .map(|i| {
                let mut n = test_node(&format!("/r/d/{i}.pdf"), 200, false, vec![]);
                n.ctime = 2000;
                n
            })
            .collect();
        docs.push(video);
        let folder = test_node("/r/d", 10000, true, docs);
        let root = test_node("/r", 10000, true, vec![folder]);
        let looks = Looks::build(&root, &cats);
        let look = looks.of(&root.children[0], &cats);
        // 9000 bytes of video beat 1000 bytes of documents.
        assert_eq!(cats.label(look.cat), "Video");
        assert_eq!((look.newest, look.oldest), (2000, 1000));
        let root_look = looks.of(&root, &cats);
        assert_eq!((root_look.newest, root_look.oldest), (2000, 1000));
        let file = looks.of(&root.children[0].children[0], &cats);
        assert_eq!(cats.label(file.cat), "Documents");
    }

    #[test]
    fn shades_get_darker_with_age() {
        let c = Color32::from_rgb(200, 100, 50);
        let shades = AgeShades {
            days: 365,
            steps: 3,
            darkest: 0.4,
        };
        let shade = |s| shades.shade(c, s);
        assert_eq!(shade(0), c);
        assert!(shade(2).r() < shade(1).r() && shade(1).r() < c.r());
        assert_eq!(shade(2), Color32::from_rgb(80, 40, 20));
    }
}

/// The live tree under worst-case conditions, checked against the
/// finished tree.
#[cfg(test)]
mod live_stress {
    use super::*;

    /// Totals of everything under `n`, from the finished tree.
    fn expected(n: &Node, cats: &CategoryModel) -> Totals {
        let mut t = Totals::new(cats.other().0 + 1);
        for c in &n.children {
            if c.is_dir {
                t.add(&expected(c, cats));
            } else {
                t.add_file(cats.of_name(&c.name), c.size, c.ctime);
            }
        }
        t
    }

    /// Every folder of the live snapshot `live` matches its folder in the
    /// finished tree `fin`: size, file count, details and colors. Returns
    /// how many folders were checked.
    fn same(live: &Node, fin: &Node, looks: &LiveLooks, cats: &CategoryModel) -> usize {
        let key = |n: &Node| {
            (
                n.name.clone(),
                n.size,
                n.file_count,
                n.mode,
                n.mtime,
                n.ctime,
                n.uid,
                n.gid,
            )
        };
        assert_eq!(key(live), key(fin), "{}", fin.path.display());
        let want = expected(fin, cats).summary();
        assert_eq!(
            looks.summaries.get(&path_key(&fin.path)),
            Some(&want),
            "{}",
            fin.path.display()
        );
        let mut checked = 1;
        for l in &live.children {
            let f = fin
                .children
                .iter()
                .find(|c| c.path == l.path)
                .unwrap_or_else(|| panic!("{} not in the finished tree", l.path.display()));
            checked += same(l, f, looks, cats);
        }
        checked
    }

    /// Scans `dir` with a live tree made in `made_in` and scanned in
    /// `pool`, while a reader like the window's reads it all along: the
    /// scanned folder's size and file count never go down. Then every
    /// folder of a full snapshot matches the finished tree.
    fn scan_and_check(dir: &Path, made_in: &rayon::ThreadPool, pool: &rayon::ThreadPool) -> usize {
        let cats = CategoryModel::defaults();
        let (tx, rx) = std::sync::mpsc::channel();
        let drain = std::thread::spawn(move || rx.into_iter().count());
        let live = made_in.install(|| LiveTree::new(Arc::new(cats.clone())));
        let reading = std::sync::atomic::AtomicBool::new(true);
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
            live: Some(&live),
        };
        let tree = std::thread::scope(|scope| {
            scope.spawn(|| {
                // Every folder's size and file count at the last read: none
                // may ever go down.
                fn check(n: &Node, seen: &mut HashMap<PathBuf, (u64, u64)>) {
                    let now = (n.size, n.file_count);
                    if let Some(&(size, files)) = seen.get(&n.path) {
                        assert!(now.0 >= size, "{} size went down", n.path.display());
                        assert!(now.1 >= files, "{} files went down", n.path.display());
                    }
                    seen.insert(n.path.clone(), now);
                    for c in &n.children {
                        check(c, seen);
                    }
                }
                let mut looks = LiveLooks::default();
                let mut seen = HashMap::new();
                while reading.load(Relaxed) {
                    if let Some(root) = live.snapshot(usize::MAX, 0.0, &mut looks) {
                        check(&root, &mut seen);
                    }
                    std::thread::yield_now();
                }
            });
            let tree = pool.install(|| scan_dir(dir, &ctx));
            reading.store(false, Relaxed);
            tree
        });
        drop(tx);
        drain.join().unwrap();
        let mut looks = LiveLooks::default();
        let snap = live.snapshot(usize::MAX, 0.0, &mut looks).unwrap();
        same(&snap, &tree, &looks, &cats)
    }

    /// A huge flat folder that every thread counts in at once, a chain far
    /// deeper than the chart ever shows, many tiny and empty folders and a
    /// link to a folder; also with the tree made for a smaller pool than
    /// the one scanning (extra threads share a counter shard).
    #[test]
    fn live_tree_is_exact_under_load() {
        let dir = std::env::temp_dir().join(format!("spacemap-live-stress-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let exts = ["eml", "JPG", "mkv", "txt", "x", ""];
        std::fs::create_dir_all(dir.join("flat")).unwrap();
        for i in 0..12_000 {
            let ext = exts[i % exts.len()];
            let name = if ext.is_empty() {
                format!("f{i}")
            } else {
                format!("f{i}.{ext}")
            };
            std::fs::write(dir.join("flat").join(name), vec![0u8; i % 5000]).unwrap();
        }
        let mut deep = dir.join("chain");
        for level in 0..40 {
            deep = deep.join(format!("l{level}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("bottom.mkv"), vec![0u8; 70_000]).unwrap();
        for d in 0..300 {
            let small = dir.join("small").join(format!("d{d}"));
            std::fs::create_dir_all(&small).unwrap();
            for f in 0..3 {
                std::fs::write(
                    small.join(format!("{f}.{}", exts[(d + f) % 4])),
                    vec![1u8; d],
                )
                .unwrap();
            }
        }
        std::fs::create_dir_all(dir.join("empty/a/b/c")).unwrap();
        std::os::unix::fs::symlink(dir.join("flat"), dir.join("link-to-flat")).unwrap();

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        let small = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        for round in 0..4 {
            let made_in = if round == 3 { &small } else { &pool };
            // Every folder: the root, flat, the 41 of the chain, small and its
            // 300, and empty with its 3.
            assert_eq!(scan_and_check(&dir, made_in, &pool), 1 + 1 + 41 + 301 + 4);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Thousands of folders right below the scanned one.
    #[test]
    fn wide_shallow_tree() {
        let dir = std::env::temp_dir().join(format!("spacemap-live-wide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in 0..5000 {
            let sub = dir.join(format!("d{d}"));
            std::fs::create_dir_all(&sub).unwrap();
            if d % 3 != 0 {
                let ext = ["mkv", "eml", "png"][d % 3];
                std::fs::write(sub.join(format!("f.{ext}")), vec![0u8; d]).unwrap();
            }
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        assert_eq!(scan_and_check(&dir, &pool, &pool), 5001);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Folders too small for a slice are grouped per folder into one
    /// entry with exact totals; the scanned folder's own children are all
    /// kept.
    #[test]
    fn small_folders_are_grouped_exactly() {
        let dir = std::env::temp_dir().join(format!("spacemap-live-group-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("big")).unwrap();
        std::fs::write(dir.join("big/huge.mkv"), vec![0u8; 50_000_000]).unwrap();
        for d in 0..3000 {
            let sub = dir.join("many").join(format!("d{d}"));
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("f.txt"), vec![1u8; 10]).unwrap();
        }
        for d in 0..50 {
            std::fs::create_dir_all(dir.join(format!("top{d}"))).unwrap();
        }
        let cats = CategoryModel::defaults();
        let (tx, rx) = std::sync::mpsc::channel();
        let drain = std::thread::spawn(move || rx.into_iter().count());
        let live = LiveTree::new(Arc::new(cats.clone()));
        let ctx = ScanCtx {
            mounts: &HashSet::new(),
            progress: &tx,
            counter: &Default::default(),
            cancel: &Default::default(),
            progress_interval: 512,
            apparent_size: true,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: Some(&live),
        };
        let tree = scan_dir(&dir, &ctx);
        drop(tx);
        drain.join().unwrap();
        let mut looks = LiveLooks::default();
        let snap = live.snapshot(12, 0.01, &mut looks).unwrap();
        assert_eq!((snap.size, snap.file_count), (tree.size, tree.file_count));
        // All 52 children of the scanned folder, however small.
        assert_eq!(snap.children.len(), 52);
        let many = snap.children.iter().find(|c| c.name == "many").unwrap();
        let fin = tree.children.iter().find(|c| c.name == "many").unwrap();
        assert_eq!((many.size, many.file_count), (fin.size, fin.file_count));
        // Its 3,000 tiny folders as one entry, adding up exactly.
        assert_eq!(many.children.len(), 1);
        let grouped = &many.children[0];
        assert_eq!(grouped.name, trf("SEG_OTHER_ITEMS", &["3000"]));
        let subs: u64 = fin.children.iter().map(|c| c.size).sum();
        assert_eq!((grouped.size, grouped.file_count), (subs, 3000));
        assert!(looks.get(grouped).is_some(), "the entry has a color");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A group of tiny folders, some finished and some still being
    /// scanned, adds up all their files and bytes.
    #[test]
    fn groups_count_unfinished_members() {
        let cats = CategoryModel::defaults();
        let live = LiveTree::new(Arc::new(cats.clone()));
        let root = live.open(None, Path::new("/r"), 0);
        let big = live.open(Some(&root.folder), Path::new("/r/big"), 0);
        live.count(
            &big,
            &test_node("/r/big/huge.mkv", 1_000_000, false, vec![]),
        );
        let p = live.open(Some(&root.folder), Path::new("/r/p"), 0);
        let mut open = Vec::new();
        for i in 0..20 {
            let t = live.open(Some(&p.folder), Path::new(&format!("/r/p/t{i}")), 0);
            for f in 0..3 {
                live.count(
                    &t,
                    &test_node(&format!("/r/p/t{i}/{f}.txt"), 10, false, vec![]),
                );
            }
            if i % 2 == 0 {
                live.close(t, 30, 3, 0, 0, 0, 0, 0);
            } else {
                open.push(t);
            }
        }
        let mut looks = LiveLooks::default();
        let snap = live.snapshot(5, 0.1, &mut looks).unwrap();
        let p_node = snap.children.iter().find(|c| c.name == "p").unwrap();
        assert_eq!((p_node.size, p_node.file_count), (600, 60));
        assert_eq!(p_node.children.len(), 1, "all 20 grouped");
        let g = &p_node.children[0];
        assert_eq!(
            (g.size, g.file_count),
            (600, 60),
            "finished and unfinished members"
        );
        drop(open);
    }

    /// Times before 1970, unknown times and sizes that add up past the
    /// largest number: the live summary matches the finished tree's.
    #[test]
    fn odd_times_and_huge_sizes() {
        let cats = CategoryModel::defaults();
        let file = |name: &str, size: u64, ctime: i64| {
            let mut n = test_node(&format!("/r/{name}"), size, false, vec![]);
            n.ctime = ctime;
            n
        };
        let files = [
            file("a.mkv", u64::MAX / 2 + 1, -86_400),
            file("b.mkv", u64::MAX / 2 + 1, NO_TIME),
            file("c.txt", 10, 0),
            file("d.txt", 10, -5),
        ];
        let live = LiveTree::new(Arc::new(cats.clone()));
        let r = live.open(None, Path::new("/r"), 0);
        let mut looks = LiveLooks::default();
        assert_eq!(live.snapshot(3, 0.0, &mut looks).unwrap().file_count, 0);
        for f in &files {
            live.count(&r, f);
        }
        let mut want = Totals::new(cats.other().0 + 1);
        for f in &files {
            want.add_file(cats.of_name(&f.name), f.size, f.ctime);
        }
        let snap = live.snapshot(3, 0.0, &mut looks).unwrap();
        assert_eq!(looks.root.as_ref(), Some(&want));
        assert_eq!(snap.size, u64::MAX, "sizes cap, never wrap");
        let s = looks.summaries[&path_key(Path::new("/r"))];
        assert_eq!((s.newest, s.oldest), (0, -86_400));
        assert_eq!(cats.label(s.cat), "Video");
    }
}

#[cfg(test)]
mod perf {
    use super::*;

    /// Time to work out the looks of a 1,000,000-file tree (run with
    /// `cargo test --release looks_1m -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn looks_1m() {
        let cats = CategoryModel::defaults();
        let exts = ["mkv", "jpg", "txt", "rs", "pdf", "flac", "zip", "dat", "x"];
        let folders: Vec<Node> = (0..1000)
            .map(|d| {
                let files = (0..1000)
                    .map(|f| {
                        test_node(
                            &format!("/r/d{d}/f{f}.{}", exts[f % exts.len()]),
                            4096,
                            false,
                            vec![],
                        )
                    })
                    .collect();
                test_node(&format!("/r/d{d}"), 4096 * 1000, true, files)
            })
            .collect();
        let root = test_node("/r", 0, true, folders);
        for _ in 0..3 {
            let t = Instant::now();
            let looks = Looks::build(&root, &cats);
            eprintln!("{:?} for {} folders", t.elapsed(), looks.folders.len());
        }
    }
}
