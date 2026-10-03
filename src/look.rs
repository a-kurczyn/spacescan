//! Slice colors in the finished chart. The category taking the most space
//! in a slice sets its hue (the category bar's colors). Age, by Changed
//! time (when the data arrived on this disk), sets brightness in three
//! steps: new is brightest, `age_days` and older is darkest. Each slice
//! blends along its arc from the shade of its newest file to the shade of
//! its oldest.

use super::*;

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
struct Totals {
    bytes: Vec<u64>,
    files: Vec<u64>,
    newest: Option<i64>,
    oldest: Option<i64>,
}

impl Totals {
    fn new(categories: usize) -> Self {
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

    fn add(&mut self, other: &Totals) {
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

    fn add_file(&mut self, cat: Category, size: u64, ctime: i64) {
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

/// Slice looks during a scan. Every folder keeps running totals of the
/// files classified under it so far: when a folder finishes, its own files
/// are added to it and to every folder above it, up to the scanned one.
/// A finished folder's look is final.
#[derive(Default)]
pub(crate) struct LiveLooks {
    /// Totals of every folder with files classified under it so far, and
    /// when the first ones arrived.
    totals: HashMap<PathBuf, (Totals, Instant)>,
    /// Finished folders' looks.
    done: HashMap<PathBuf, Look>,
}

impl LiveLooks {
    /// Folder `node` (in the live tree under `root`) finished: `exts` and
    /// `times` are its own files' (extension, size, count) and newest and
    /// oldest Changed times.
    pub(crate) fn folder_done(
        &mut self,
        node: &Node,
        root: &Path,
        exts: &[(String, u64, u64)],
        times: (i64, i64),
        cats: &CategoryModel,
    ) {
        let mut own = Totals::new(cats.other().0 + 1);
        for (ext, size, files) in exts {
            let cat = cats.of_ext(ext);
            own.bytes[cat.0] = own.bytes[cat.0].saturating_add(*size);
            own.files[cat.0] += files;
        }
        own.add_times(times.0, times.1);
        let now = Instant::now();
        for folder in node.path.ancestors() {
            self.totals
                .entry(folder.to_path_buf())
                .or_insert_with(|| (Totals::new(own.bytes.len()), now))
                .0
                .add(&own);
            if folder == root {
                break;
            }
        }
        let look = self.totals[&node.path].0.look(node.size);
        self.done.insert(node.path.clone(), look);
    }

    /// The look of folder `node` from what's classified under it so far
    /// (final once it finished), and since when it has one.
    pub(crate) fn get(&self, node: &Node) -> Option<(Look, Instant)> {
        let (totals, since) = self.totals.get(&node.path)?;
        let look = match self.done.get(&node.path) {
            Some(look) => *look,
            None => totals.look(node.size),
        };
        Some((look, *since))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.totals.is_empty()
    }

    /// When folder `path` got a color during the scan, if it did.
    pub(crate) fn colored_at(&self, path: &Path) -> Option<Instant> {
        self.totals.get(path).map(|t| t.1)
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
        let sub = test_node(
            "/r/s",
            3000,
            true,
            vec![file("/r/s/a.mkv", 2000, 100), file("/r/s/b.txt", 1000, 900)],
        );
        let root = test_node("/r", 3500, true, vec![sub, file("/r/c.flac", 500, 50)]);
        let finished = Looks::build(&root, &cats);
        // The live tree has folders only.
        let live_sub = test_node("/r/s", 3000, true, vec![]);
        let live_root = test_node("/r", 3500, true, vec![live_sub.clone()]);
        let r = Path::new("/r");
        let mut live = LiveLooks::default();
        assert!(live.get(&live_root).is_none(), "nothing classified yet");
        let mkv_txt = [("mkv".to_string(), 2000, 1), ("txt".to_string(), 1000, 1)];
        live.folder_done(&live_sub, r, &mkv_txt, (900, 100), &cats);
        // Unfinished, /r already shows what's classified under it: video.
        let early = live.get(&live_root).unwrap().0;
        assert_eq!(
            (cats.label(early.cat), early.newest, early.oldest),
            ("Video".into(), 900, 100)
        );
        live.folder_done(
            &live_root,
            r,
            &[("flac".to_string(), 500, 1)],
            (50, 50),
            &cats,
        );
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
