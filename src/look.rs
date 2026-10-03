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

/// One scan thread's share of a folder's counters, in one block: bytes
/// per category, then files per category, then the newest and oldest
/// Changed time. Each shard has a single writer, its thread, so updates
/// are plain loads and stores (no locked instructions); other threads only
/// read it.
#[repr(align(64))]
struct Shard(Box<[AtomicU64]>);

impl Shard {
    fn new(categories: usize) -> Self {
        let mut v: Vec<AtomicU64> = (0..categories * 2).map(|_| AtomicU64::new(0)).collect();
        v.push(AtomicU64::new(i64::MIN as u64));
        v.push(AtomicU64::new(i64::MAX as u64));
        Shard(v.into_boxed_slice())
    }

    fn bytes(&self, cat: usize) -> &AtomicU64 {
        &self.0[cat]
    }

    fn files(&self, cat: usize, categories: usize) -> &AtomicU64 {
        &self.0[categories + cat]
    }

    fn newest(&self) -> &AtomicU64 {
        &self.0[self.0.len() - 2]
    }

    fn oldest(&self) -> &AtomicU64 {
        &self.0[self.0.len() - 1]
    }
}

/// Running totals of everything under one folder being scanned: each file
/// read counts in its folder's counters and those of every counted folder
/// above it. A thread's shard is made when it first counts something
/// there.
pub(crate) struct Counters {
    categories: usize,
    shards: Box<[std::sync::OnceLock<Shard>]>,
}

impl Counters {
    fn new(categories: usize) -> Self {
        let threads = rayon::current_num_threads() + 1;
        Counters {
            categories,
            shards: (0..threads).map(|_| std::sync::OnceLock::new()).collect(),
        }
    }

    /// Shard `i` (see `shard_index`).
    fn shard(&self, i: usize) -> &Shard {
        self.shards[i].get_or_init(|| Shard::new(self.categories))
    }

    /// The shard of the calling thread: its index in the thread pool, or
    /// the last shard for the one scan thread outside it.
    fn shard_index(&self) -> usize {
        let last = self.shards.len() - 1;
        rayon::current_thread_index().map_or(last, |i| i.min(last))
    }

    /// All shards added up.
    fn snapshot(&self) -> Totals {
        let n = self.categories;
        let mut t = Totals::new(n);
        for s in self.shards.iter().filter_map(|s| s.get()) {
            for i in 0..n {
                t.bytes[i] = t.bytes[i].saturating_add(s.bytes(i).load(Relaxed));
                t.files[i] += s.files(i, n).load(Relaxed);
            }
            let (newest, oldest) = (
                s.newest().load(Relaxed) as i64,
                s.oldest().load(Relaxed) as i64,
            );
            t.add_times(
                if newest == i64::MIN { NO_TIME } else { newest },
                if oldest == i64::MAX { NO_TIME } else { oldest },
            );
        }
        t
    }
}

/// The counted folders from one folder up to the scanned one: each file
/// counts in all of them.
pub(crate) struct Chain<'a> {
    pub(crate) counters: &'a Counters,
    pub(crate) up: Option<&'a Chain<'a>>,
}

/// Where a folder sits in a scan with live counters: its depth below the
/// scanned folder, and the nearest counted folders above (or at) it.
#[derive(Clone, Copy)]
pub(crate) struct LivePlace<'a> {
    pub(crate) depth: usize,
    pub(crate) chain: Option<&'a Chain<'a>>,
}

/// The counters of a running scan, shared by its threads and the window.
/// Only folders the chart can show (down to `depth` below the scanned one)
/// have counters; deeper files count in the nearest counted folder above.
pub(crate) struct LiveCounters {
    cats: Arc<CategoryModel>,
    /// How deep below the scanned folder folders get counters.
    depth: usize,
    /// Counted folders being scanned, and the final totals of those that
    /// finished since the window last looked, one part per scan thread (a
    /// folder is opened and finished on the same thread), so threads only
    /// wait for the window's brief reads.
    active: Vec<std::sync::Mutex<ActivePart>>,
}

#[derive(Default)]
struct ActivePart {
    /// Numbered slots, reused once free (see `free`).
    scanning: Vec<Option<(PathBuf, Arc<Counters>)>>,
    free: Vec<usize>,
    finished: Vec<FolderTotals>,
}

/// A counted folder being scanned: its counters, and its slot in the
/// opening thread's part of `LiveCounters::active`.
pub(crate) struct Open {
    pub(crate) counters: Arc<Counters>,
    part: usize,
    slot: usize,
}

/// A folder and its totals.
type FolderTotals = (PathBuf, Totals);

impl LiveCounters {
    /// Counters for a scan, counting folders down to `depth` below the
    /// scanned one.
    pub(crate) fn new(cats: Arc<CategoryModel>, depth: usize) -> Self {
        LiveCounters {
            cats,
            depth,
            active: (0..rayon::current_num_threads() + 1)
                .map(|_| Default::default())
                .collect(),
        }
    }

    /// The calling thread's part of `active`.
    fn part_index(&self) -> usize {
        let last = self.active.len() - 1;
        rayon::current_thread_index().map_or(last, |i| i.min(last))
    }

    /// Folder `path` at `depth` starts being scanned: its counters, if the
    /// chart can show it.
    pub(crate) fn open(&self, path: &Path, depth: usize) -> Option<Open> {
        if depth > self.depth {
            return None;
        }
        let counters = Arc::new(Counters::new(self.cats.other().0 + 1));
        let part = self.part_index();
        let mut p = self.active[part].lock().unwrap();
        let entry = Some((path.to_path_buf(), counters.clone()));
        let slot = match p.free.pop() {
            Some(slot) => {
                p.scanning[slot] = entry;
                slot
            }
            None => {
                p.scanning.push(entry);
                p.scanning.len() - 1
            }
        };
        Some(Open {
            counters,
            part,
            slot,
        })
    }

    /// `node` was read under the counted folders `chain`: a file counts in
    /// all of them.
    pub(crate) fn count(&self, chain: &Chain, node: &Node) {
        if node.is_dir {
            return;
        }
        let cat = self.cats.of_name(&node.name).0;
        let i = chain.counters.shard_index();
        let mut link = Some(chain);
        // A folder's newest and oldest also hold for every folder above it,
        // so the times stop at the first folder they don't change.
        let t = node.ctime;
        let (mut newest, mut oldest) = (t != NO_TIME, t != NO_TIME);
        // This thread is the shard's only writer.
        let bump = |a: &AtomicU64, by: u64| a.store(a.load(Relaxed).wrapping_add(by), Relaxed);
        let n = self.cats.other().0 + 1;
        while let Some(l) = link {
            let s = l.counters.shard(i);
            bump(s.bytes(cat), node.size);
            bump(s.files(cat, n), 1);
            if newest {
                newest = (s.newest().load(Relaxed) as i64) < t;
                if newest {
                    s.newest().store(t as u64, Relaxed);
                }
            }
            if oldest {
                oldest = (s.oldest().load(Relaxed) as i64) > t;
                if oldest {
                    s.oldest().store(t as u64, Relaxed);
                }
            }
            link = l.up;
        }
    }

    /// Counted folder `open` finished: its totals are final.
    pub(crate) fn close(&self, open: &Open) {
        let totals = open.counters.snapshot();
        let mut p = self.active[open.part].lock().unwrap();
        if let Some((path, _)) = p.scanning[open.slot].take() {
            p.finished.push((path, totals));
        }
        p.free.push(open.slot);
    }

    /// The totals so far of every counted folder still being scanned, and
    /// the final totals of those that finished since the last call.
    fn take_snapshot(&self) -> (Vec<FolderTotals>, Vec<FolderTotals>) {
        let (mut scanning, mut finished) = (Vec::new(), Vec::new());
        for part in &self.active {
            let mut part = part.lock().unwrap();
            scanning.extend(
                part.scanning
                    .iter()
                    .flatten()
                    .map(|(p, c)| (p.clone(), c.snapshot())),
            );
            finished.append(&mut part.finished);
        }
        (scanning, finished)
    }
}

/// Slice looks during a scan, for the live chart: final for finished
/// folders, and from the counters (read every 100 ms) for folders still
/// being scanned.
#[derive(Default)]
pub(crate) struct LiveLooks {
    /// Final totals of finished folders.
    done: HashMap<PathBuf, Totals>,
    /// Totals so far of the folders still being scanned.
    partial: HashMap<PathBuf, Totals>,
    /// When each folder first had files classified under it.
    since: HashMap<PathBuf, Instant>,
    refreshed: Option<Instant>,
}

impl LiveLooks {
    /// Reads `counters` again if the last read is 100 ms old.
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
        let (scanning, finished) = counters.take_snapshot();
        let now = Instant::now();
        self.partial.clear();
        for (path, totals) in scanning.into_iter().chain(finished.iter().cloned()) {
            if !totals.is_empty() {
                self.since.entry(path.clone()).or_insert(now);
            }
            self.partial.insert(path, totals);
        }
        for (path, totals) in finished {
            self.partial.remove(&path);
            self.done.insert(path, totals);
        }
    }

    /// The look of folder `node` from what's classified under it so far
    /// (final once it finished), and since when it has one.
    pub(crate) fn get(&self, node: &Node) -> Option<(Look, Instant)> {
        let since = *self.since.get(&node.path)?;
        let totals = match self.done.get(&node.path) {
            Some(t) => t,
            None => self.partial.get(&node.path)?,
        };
        let look = totals.look(node.size);
        Some((look, since))
    }

    /// Categories under `root` so far, most bytes first.
    pub(crate) fn order(&self, root: &Path) -> Vec<Category> {
        let Some(t) = self.done.get(root).or_else(|| self.partial.get(root)) else {
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

        let counters = LiveCounters::new(Arc::new(cats.clone()), 6);
        let mut live = LiveLooks::default();
        // The live tree has folders only.
        let live_sub = test_node("/r/s", 3000, true, vec![]);
        let live_root = test_node("/r", 3500, true, vec![live_sub.clone()]);
        let r = counters.open(Path::new("/r"), 0).unwrap();
        let s = counters.open(Path::new("/r/s"), 1).unwrap();
        assert!(
            counters.open(Path::new("/r/s/deep"), 7).is_none(),
            "too deep to show"
        );
        let at_r = Chain {
            counters: &r.counters,
            up: None,
        };
        let at_s = Chain {
            counters: &s.counters,
            up: Some(&at_r),
        };
        live.refresh_now(&counters);
        assert!(live.get(&live_root).is_none(), "nothing classified yet");
        // One file read: /r/s and /r already show it.
        counters.count(&at_s, &mkv);
        live.refresh_now(&counters);
        let early = live.get(&live_root).unwrap().0;
        assert_eq!((cats.label(early.cat), early.newest), ("Video".into(), 100));
        counters.count(&at_s, &txt);
        counters.close(&s);
        counters.count(&at_r, &flac);
        counters.close(&r);
        live.refresh_now(&counters);
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

/// Live counters under worst-case conditions, checked against the
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

    /// Every folder down to `depth` below `n`, with its depth.
    fn counted<'a>(n: &'a Node, depth: usize, at: usize, out: &mut Vec<(&'a Node, usize)>) {
        out.push((n, at));
        if at < depth {
            for c in n.children.iter().filter(|c| c.is_dir) {
                counted(c, depth, at + 1, out);
            }
        }
    }

    /// A huge flat folder that every thread counts in at once, a chain far
    /// deeper than the counted depth, many tiny and empty folders and a
    /// link to a folder: the live totals of every counted folder must match
    /// the finished tree exactly, every time.
    #[test]
    fn live_totals_are_exact_under_load() {
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

        let cats = CategoryModel::defaults();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        const DEPTH: usize = 3;
        for _ in 0..3 {
            let (tx, rx) = std::sync::mpsc::channel();
            let drain = std::thread::spawn(move || rx.into_iter().count());
            let live = pool.install(|| LiveCounters::new(Arc::new(cats.clone()), DEPTH));
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
            let tree = pool.install(|| scan_dir(&dir, &ctx));
            drop(tx);
            drain.join().unwrap();

            let (scanning, finished) = live.take_snapshot();
            assert!(scanning.is_empty(), "every counted folder was closed");
            let got: HashMap<PathBuf, Totals> = finished.into_iter().collect();
            let mut folders = Vec::new();
            counted(&tree, DEPTH, 0, &mut folders);
            assert_eq!(
                got.len(),
                folders.len(),
                "only folders down to the counted depth"
            );
            for (n, _) in folders {
                assert_eq!(
                    got.get(&n.path),
                    Some(&expected(n, &cats)),
                    "{}",
                    n.path.display()
                );
            }
            // The flat files, the one at the bottom of the chain, the small
            // folders' files and the link (listed, not followed).
            assert_eq!(
                expected(&tree, &cats).files.iter().sum::<u64>(),
                12_000 + 1 + 900 + 1
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
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
