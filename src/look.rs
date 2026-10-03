//! Slice colors in the finished chart. The category taking the most space
//! in a slice sets its hue (the category bar's colors). Age, by Changed
//! time (when the data arrived on this disk), sets brightness in three
//! steps: new is brightest, `age_days` and older is darkest. Each slice
//! blends along its arc from the shade of its newest file to the shade of
//! its oldest.

use super::*;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicI64, AtomicU64};

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

    fn is_empty(&self) -> bool {
        self.files.iter().all(|&f| f == 0)
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

/// One scan thread's share of a folder's counters: bytes and files per
/// category, newest and oldest Changed time. Kept apart per thread so
/// threads don't slow each other down updating the same memory.
#[repr(align(64))]
struct Shard {
    bytes: Vec<AtomicU64>,
    files: Vec<AtomicU64>,
    newest: AtomicI64,
    oldest: AtomicI64,
}

/// Running totals of one folder being scanned, updated by the scan threads
/// for every file. A thread's shard is made when it first counts something
/// there (most folders are touched by one or two threads).
pub(crate) struct Counters {
    categories: usize,
    shards: Vec<std::sync::OnceLock<Box<Shard>>>,
}

impl Counters {
    fn new(categories: usize) -> Self {
        let threads = rayon::current_num_threads() + 1;
        Counters {
            categories,
            shards: (0..threads).map(|_| std::sync::OnceLock::new()).collect(),
        }
    }

    /// This thread's shard (the last one for threads outside the pool).
    fn shard(&self) -> &Shard {
        let i = rayon::current_thread_index().unwrap_or(usize::MAX);
        self.shards[i.min(self.shards.len() - 1)].get_or_init(|| {
            Box::new(Shard {
                bytes: (0..self.categories).map(|_| AtomicU64::new(0)).collect(),
                files: (0..self.categories).map(|_| AtomicU64::new(0)).collect(),
                newest: AtomicI64::new(i64::MIN),
                oldest: AtomicI64::new(i64::MAX),
            })
        })
    }

    fn add_file(&self, cat: Category, size: u64, ctime: Option<i64>) {
        let s = self.shard();
        s.bytes[cat.0].fetch_add(size, Relaxed);
        s.files[cat.0].fetch_add(1, Relaxed);
        if let Some(t) = ctime {
            s.newest.fetch_max(t, Relaxed);
            s.oldest.fetch_min(t, Relaxed);
        }
    }

    fn add(&self, t: &Totals) {
        let s = self.shard();
        for (a, b) in s.bytes.iter().zip(&t.bytes) {
            a.fetch_add(*b, Relaxed);
        }
        for (a, b) in s.files.iter().zip(&t.files) {
            a.fetch_add(*b, Relaxed);
        }
        if let Some(n) = t.newest {
            s.newest.fetch_max(n, Relaxed);
        }
        if let Some(o) = t.oldest {
            s.oldest.fetch_min(o, Relaxed);
        }
    }

    /// All shards added up.
    fn snapshot(&self) -> Totals {
        let categories = self.categories;
        let mut t = Totals::new(categories);
        for s in self.shards.iter().filter_map(|s| s.get()) {
            for i in 0..categories {
                t.bytes[i] = t.bytes[i].saturating_add(s.bytes[i].load(Relaxed));
                t.files[i] += s.files[i].load(Relaxed);
            }
            let (newest, oldest) = (s.newest.load(Relaxed), s.oldest.load(Relaxed));
            t.add_times(
                if newest == i64::MIN { NO_TIME } else { newest },
                if oldest == i64::MAX { NO_TIME } else { oldest },
            );
        }
        t
    }
}

/// The counters of a running scan, shared by its threads and the window:
/// every folder being scanned has its own. A finished folder's totals go
/// into its parent's, so a folder's counters hold its own files read so
/// far plus its finished subfolders.
pub(crate) struct LiveCounters {
    cats: Arc<CategoryModel>,
    /// The folder being scanned.
    root: PathBuf,
    /// Counters of the folders being scanned, split by a hash of the path
    /// so threads seldom wait for each other.
    active: Vec<ActivePart>,
}

type ActivePart = std::sync::Mutex<HashMap<PathBuf, Arc<Counters>>>;

impl LiveCounters {
    pub(crate) fn new(cats: Arc<CategoryModel>, root: PathBuf) -> Self {
        LiveCounters {
            cats,
            root,
            active: (0..16).map(|_| Default::default()).collect(),
        }
    }

    /// The part of `active` that holds `path`.
    fn part(&self, path: &Path) -> &ActivePart {
        use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
        let hash = BuildHasherDefault::<DefaultHasher>::default().hash_one(path);
        &self.active[hash as usize % self.active.len()]
    }

    /// Folder `path` starts being scanned: its counters.
    pub(crate) fn open(&self, path: &Path) -> Arc<Counters> {
        let c = Arc::new(Counters::new(self.cats.other().0 + 1));
        self.part(path)
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), c.clone());
        c
    }

    /// `node` was read in a folder with counters `c`: a file counts.
    pub(crate) fn count(&self, c: &Counters, node: &Node) {
        if node.is_dir {
            return;
        }
        let ctime = (node.ctime != NO_TIME).then_some(node.ctime);
        c.add_file(self.cats.of_name(&node.name), node.size, ctime);
    }

    /// Folder `path` finished: its totals, now final, go into its parent's
    /// counters.
    pub(crate) fn close(&self, path: &Path, c: &Counters) -> Totals {
        let totals = c.snapshot();
        // Into the parent first, then off the list: the window may count
        // these files twice for a moment, but never misses them.
        if let Some(parent) = path.parent() {
            let parent = self.part(parent).lock().unwrap().get(parent).cloned();
            if let Some(parent) = parent {
                parent.add(&totals);
            }
        }
        self.part(path).lock().unwrap().remove(path);
        totals
    }

    /// The totals of every folder still being scanned, as of now.
    fn snapshot(&self) -> Vec<(PathBuf, Totals)> {
        let mut out = Vec::new();
        for part in &self.active {
            let part = part.lock().unwrap();
            out.extend(part.iter().map(|(p, c)| (p.clone(), c.snapshot())));
        }
        out
    }
}

/// Slice looks during a scan, for the live chart: final for finished
/// folders, and from the counters (refreshed every 100 ms) for folders
/// still being scanned.
#[derive(Default)]
pub(crate) struct LiveLooks {
    done: HashMap<PathBuf, Look>,
    /// Totals so far of the folders still being scanned and the folders
    /// above them.
    partial: HashMap<PathBuf, Totals>,
    /// When each folder first had files classified under it.
    since: HashMap<PathBuf, Instant>,
    refreshed: Option<Instant>,
}

impl LiveLooks {
    /// Folder `node` (in the live tree) finished with `totals`.
    pub(crate) fn folder_done(&mut self, node: &Node, totals: &Totals) {
        self.done.insert(node.path.clone(), totals.look(node.size));
        if !totals.is_empty() {
            self.since
                .entry(node.path.clone())
                .or_insert_with(Instant::now);
        }
    }

    /// Reads `counters` again if the last read is 100 ms old: the totals of
    /// every folder still being scanned, added up the folders above it.
    pub(crate) fn refresh(&mut self, counters: &LiveCounters) {
        const EVERY: std::time::Duration = std::time::Duration::from_millis(100);
        if self.refreshed.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.refresh_now(counters);
    }

    /// Reads `counters` now (see `refresh`).
    pub(crate) fn refresh_now(&mut self, counters: &LiveCounters) {
        self.refreshed = Some(Instant::now());
        self.partial.clear();
        let categories = counters.cats.other().0 + 1;
        for (path, totals) in counters.snapshot() {
            if totals.is_empty() {
                continue;
            }
            for folder in path.ancestors() {
                self.partial
                    .entry(folder.to_path_buf())
                    .or_insert_with(|| Totals::new(categories))
                    .add(&totals);
                if folder == counters.root {
                    break;
                }
            }
        }
        let now = Instant::now();
        for path in self.partial.keys() {
            self.since.entry(path.clone()).or_insert(now);
        }
    }

    /// The look of folder `node` from what's classified under it so far
    /// (final once it finished), and since when it has one.
    pub(crate) fn get(&self, node: &Node) -> Option<(Look, Instant)> {
        let since = *self.since.get(&node.path)?;
        let look = match self.done.get(&node.path) {
            Some(look) => *look,
            None => self.partial.get(&node.path)?.look(node.size),
        };
        Some((look, since))
    }

    /// Categories under `root` so far, most bytes first.
    pub(crate) fn order(&self, root: &Path) -> Vec<Category> {
        let Some(t) = self.partial.get(root) else {
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
        self.since.get(path).copied()
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

    /// Looks worked out as folders finish match the finished tree's.
    #[test]
    fn live_looks_match_finished_looks() {
        let cats = CategoryModel::defaults();
        let file = |p: &str, size: u64, ctime: i64| {
            let mut n = test_node(p, size, false, vec![]);
            n.ctime = ctime;
            n
        };
        let (mkv, txt, flac) = (
            file("/r/s/a.mkv", 2000, 100),
            file("/r/s/b.txt", 1000, 900),
            file("/r/c.flac", 500, 50),
        );
        let sub = test_node("/r/s", 3000, true, vec![mkv.clone(), txt.clone()]);
        let root = test_node("/r", 3500, true, vec![sub, flac.clone()]);
        let finished = Looks::build(&root, &cats);

        let counters = LiveCounters::new(Arc::new(cats.clone()), PathBuf::from("/r"));
        let mut live = LiveLooks::default();
        // The live tree has folders only.
        let live_sub = test_node("/r/s", 3000, true, vec![]);
        let live_root = test_node("/r", 3500, true, vec![live_sub.clone()]);
        let r = counters.open(Path::new("/r"));
        let s = counters.open(Path::new("/r/s"));
        live.refresh(&counters);
        assert!(live.get(&live_root).is_none(), "nothing classified yet");
        // One file read: /r/s and /r already show it.
        counters.count(&s, &mkv);
        live.refresh_now(&counters);
        let early = live.get(&live_root).unwrap().0;
        assert_eq!((cats.label(early.cat), early.newest), ("Video".into(), 100));
        counters.count(&s, &txt);
        let s_totals = counters.close(Path::new("/r/s"), &s);
        live.folder_done(&live_sub, &s_totals);
        counters.count(&r, &flac);
        let r_totals = counters.close(Path::new("/r"), &r);
        live.folder_done(&live_root, &r_totals);
        assert_eq!(
            live.get(&live_sub).unwrap().0,
            finished.of(&root.children[0], &cats)
        );
        assert_eq!(live.get(&live_root).unwrap().0, finished.of(&root, &cats));
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
