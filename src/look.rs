//! Slice colors in the finished chart. The category taking the most space
//! in a slice sets its hue (the category bar's colors); the size-weighted
//! average age of its contents, by Changed time (when the data arrived on
//! this disk), sets its brightness in ten steps: this week is brightest,
//! `age_weeks` and older is darkest.

use super::*;

/// What decides a slice's color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Look {
    pub(crate) cat: Category,
    /// Size-weighted average Changed time; NO_TIME if none is known.
    pub(crate) avg_ctime: i64,
    pub(crate) size: u64,
}

/// What a folder's own files (not its subfolders') add to its look: size
/// and file count per extension (`ext_key`), and their size-weighted
/// Changed times. The scanner sends this with each finished folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DirectFiles {
    pub(crate) exts: Vec<(String, u64, u64)>,
    pub(crate) ctime_sum: f64,
    pub(crate) ctime_weight: f64,
}

impl DirectFiles {
    /// The files among `children`.
    pub(crate) fn of_children(children: &[Node]) -> Self {
        let mut d = DirectFiles::default();
        for c in children.iter().filter(|c| !c.is_dir) {
            let key = ext_key(&c.name);
            match d.exts.iter_mut().find(|e| e.0 == key) {
                Some(e) => {
                    e.1 = e.1.saturating_add(c.size);
                    e.2 += c.file_count.max(1);
                }
                None => d.exts.push((key, c.size, c.file_count.max(1))),
            }
            if c.ctime != NO_TIME {
                let weight = c.size.max(1) as f64;
                d.ctime_sum += c.ctime as f64 * weight;
                d.ctime_weight += weight;
            }
        }
        d
    }
}

/// The look of every folder in a tree, by path.
pub(crate) struct Looks {
    folders: HashMap<PathBuf, Look>,
}

/// Running totals for one folder: bytes and files per category, and the
/// weighted sum of Changed times.
struct Totals {
    bytes: Vec<u64>,
    files: Vec<u64>,
    ctime_sum: f64,
    ctime_weight: f64,
}

impl Totals {
    fn new(categories: usize) -> Self {
        Totals {
            bytes: vec![0; categories],
            files: vec![0; categories],
            ctime_sum: 0.0,
            ctime_weight: 0.0,
        }
    }

    fn add(&mut self, other: &Totals) {
        for (a, b) in self.bytes.iter_mut().zip(&other.bytes) {
            *a = a.saturating_add(*b);
        }
        for (a, b) in self.files.iter_mut().zip(&other.files) {
            *a += b;
        }
        self.ctime_sum += other.ctime_sum;
        self.ctime_weight += other.ctime_weight;
    }

    fn add_direct(&mut self, d: &DirectFiles, cats: &CategoryModel) {
        for (ext, size, files) in &d.exts {
            let cat = cats.of_ext(ext);
            self.bytes[cat.0] = self.bytes[cat.0].saturating_add(*size);
            self.files[cat.0] += files;
        }
        self.ctime_sum += d.ctime_sum;
        self.ctime_weight += d.ctime_weight;
    }

    fn add_file(&mut self, cat: Category, size: u64, ctime: i64) {
        self.bytes[cat.0] = self.bytes[cat.0].saturating_add(size);
        self.files[cat.0] += 1;
        if ctime != NO_TIME {
            // Weighted by size; empty files still count a little.
            let weight = size.max(1) as f64;
            self.ctime_sum += ctime as f64 * weight;
            self.ctime_weight += weight;
        }
    }

    /// The category with the most bytes (most files if all are empty).
    fn look(&self, size: u64) -> Look {
        let most = |v: &[u64]| (0..v.len()).max_by_key(|&i| (v[i], std::cmp::Reverse(i)));
        let by_bytes = most(&self.bytes).filter(|&i| self.bytes[i] > 0);
        let cat = by_bytes.or_else(|| most(&self.files)).unwrap_or(0);
        let avg_ctime = if self.ctime_weight > 0.0 {
            (self.ctime_sum / self.ctime_weight) as i64
        } else {
            NO_TIME
        };
        Look {
            cat: Category(cat),
            avg_ctime,
            size,
        }
    }
}

impl Looks {
    /// No looks yet.
    pub(crate) fn empty() -> Looks {
        Looks {
            folders: HashMap::new(),
        }
    }

    /// Works out the look of every folder under `root`, in one pass.
    pub(crate) fn build(root: &Node, cats: &CategoryModel) -> Looks {
        fn walk(n: &Node, cats: &CategoryModel, out: &mut HashMap<PathBuf, Look>) -> Totals {
            let mut totals = Totals::new(cats.other().0 + 1);
            for c in &n.children {
                if c.is_dir {
                    let sub = deep(|| walk(c, cats, out));
                    totals.add(&sub);
                } else {
                    totals.add_file(cats.of_name(&c.name), c.size, c.ctime);
                }
            }
            out.insert(n.path.clone(), totals.look(n.size));
            totals
        }
        let mut folders = HashMap::new();
        walk(root, cats, &mut folders);
        Looks { folders }
    }

    /// The same for the live tree of a running scan, which has folders but
    /// no files: each folder's own files come from `direct`, by path.
    pub(crate) fn build_live(
        root: &Node,
        direct: &HashMap<PathBuf, DirectFiles>,
        cats: &CategoryModel,
    ) -> Looks {
        fn walk(
            n: &Node,
            direct: &HashMap<PathBuf, DirectFiles>,
            cats: &CategoryModel,
            out: &mut HashMap<PathBuf, Look>,
        ) -> Totals {
            let mut totals = Totals::new(cats.other().0 + 1);
            if let Some(d) = direct.get(&n.path) {
                totals.add_direct(d, cats);
            }
            for c in n.children.iter().filter(|c| c.is_dir) {
                let sub = deep(|| walk(c, direct, cats, out));
                totals.add(&sub);
            }
            out.insert(n.path.clone(), totals.look(n.size));
            totals
        }
        let mut folders = HashMap::new();
        walk(root, direct, cats, &mut folders);
        Looks { folders }
    }

    /// The look of node `n`: a file's own, or its folder's.
    pub(crate) fn of(&self, n: &Node, cats: &CategoryModel) -> Look {
        if n.is_dir {
            // A folder not worked out yet (the live tree just grew) is grey.
            return self.folders.get(&n.path).copied().unwrap_or(Look {
                cat: cats.other(),
                avg_ctime: NO_TIME,
                size: n.size,
            });
        }
        let mut totals = Totals::new(cats.other().0 + 1);
        totals.add_file(cats.of_name(&n.name), n.size, n.ctime);
        totals.look(n.size)
    }

    /// The look of several nodes together (an "other" slice): the category
    /// with the most bytes among their looks, and their weighted age.
    pub(crate) fn of_group<'a>(
        &self,
        nodes: impl Iterator<Item = &'a Node>,
        cats: &CategoryModel,
    ) -> Look {
        let mut totals = Totals::new(cats.other().0 + 1);
        let mut size = 0u64;
        for n in nodes {
            let look = self.of(n, cats);
            totals.bytes[look.cat.0] = totals.bytes[look.cat.0].saturating_add(look.size);
            totals.files[look.cat.0] += 1;
            if look.avg_ctime != NO_TIME {
                let weight = look.size.max(1) as f64;
                totals.ctime_sum += look.avg_ctime as f64 * weight;
                totals.ctime_weight += weight;
            }
            size = size.saturating_add(look.size);
        }
        totals.look(size)
    }
}

/// Brightness step of an average Changed time: 0 for this week, 9 for
/// `age_weeks` or older (or unknown), evenly spread in between.
pub(crate) fn age_step(avg_ctime: i64, now: i64, age_weeks: u32) -> u8 {
    const WEEK: f64 = 7.0 * 24.0 * 3600.0;
    if avg_ctime == NO_TIME {
        return 9;
    }
    let weeks = (now - avg_ctime).max(0) as f64 / WEEK;
    if weeks < 1.0 {
        return 0;
    }
    let oldest = age_weeks.max(2) as f64;
    if weeks >= oldest {
        return 9;
    }
    // Steps 1 to 8 share the time between one week and `age_weeks`.
    1 + ((weeks - 1.0) / (oldest - 1.0) * 8.0) as u8
}

/// `color` at brightness step `step`: 0 is the color as is, each step
/// darker, down to 18% brightness at 9.
pub(crate) fn shade(color: Color32, step: u8) -> Color32 {
    let factor = 1.0 - 0.82 * step.min(9) as f32 / 9.0;
    let scale = |v: u8| (v as f32 * factor).round() as u8;
    Color32::from_rgb(scale(color.r()), scale(color.g()), scale(color.b()))
}

/// The color of a slice with look `look`.
pub(crate) fn look_color(
    look: &Look,
    now: i64,
    age_weeks: u32,
    cats: &CategoryModel,
    dark: bool,
) -> Color32 {
    shade(
        cats.color(look.cat, dark),
        age_step(look.avg_ctime, now, age_weeks),
    )
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
    fn age_steps_span_this_week_to_the_baseline() {
        let now = 1_000_000_000;
        assert_eq!(age_step(now - DAY, now, 52), 0);
        assert_eq!(age_step(now - 8 * DAY, now, 52), 1);
        assert_eq!(age_step(now - 51 * 7 * DAY, now, 52), 8);
        assert_eq!(age_step(now - 52 * 7 * DAY, now, 52), 9);
        assert_eq!(age_step(now - 500 * 7 * DAY, now, 52), 9);
        assert_eq!(age_step(NO_TIME, now, 52), 9);
        // Steps never go down as files get older.
        let steps: Vec<u8> = (0..60)
            .map(|w| age_step(now - w * 7 * DAY, now, 52))
            .collect();
        assert!(steps.windows(2).all(|p| p[0] <= p[1]));
    }

    #[test]
    fn folders_take_the_biggest_category_and_weighted_age() {
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
        // (9000 × 1000 + 1000 × 2000) / 10000 = 1100.
        assert_eq!(look.avg_ctime, 1100);
        let file = looks.of(&root.children[0].children[0], &cats);
        assert_eq!(cats.label(file.cat), "Documents");
    }

    /// The live tree (folders plus each folder's own files) gives the same
    /// looks as the finished tree.
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
        let mut direct = HashMap::new();
        direct.insert(
            PathBuf::from("/r"),
            DirectFiles::of_children(&root.children),
        );
        direct.insert(
            PathBuf::from("/r/s"),
            DirectFiles::of_children(&root.children[0].children),
        );
        let live_root = test_node(
            "/r",
            3500,
            true,
            vec![test_node("/r/s", 3000, true, vec![])],
        );
        let live = Looks::build_live(&live_root, &direct, &cats);
        assert_eq!(live.of(&live_root, &cats), finished.of(&root, &cats));
        assert_eq!(
            live.of(&live_root.children[0], &cats),
            finished.of(&root.children[0], &cats)
        );
    }

    #[test]
    fn shades_get_darker_with_age() {
        let c = Color32::from_rgb(200, 100, 50);
        assert_eq!(shade(c, 0), c);
        assert!(shade(c, 9).r() < shade(c, 5).r() && shade(c, 5).r() < c.r());
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

    /// Time to rebuild the live looks of a 300,000-folder scan (run with
    /// `cargo test --release live_looks_300k -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn live_looks_300k() {
        let cats = CategoryModel::defaults();
        let mut direct = HashMap::new();
        let tops: Vec<Node> = (0..300)
            .map(|t| {
                let subs: Vec<Node> = (0..1000)
                    .map(|f| {
                        let path = format!("/r/t{t}/s{f}");
                        direct.insert(
                            PathBuf::from(&path),
                            DirectFiles {
                                exts: vec![("mkv".into(), 4096, 3), ("txt".into(), 100, 2)],
                                ctime_sum: 1.0,
                                ctime_weight: 1.0,
                            },
                        );
                        test_node(&path, 4196, true, vec![])
                    })
                    .collect();
                test_node(&format!("/r/t{t}"), 0, true, subs)
            })
            .collect();
        let root = test_node("/r", 0, true, tops);
        for _ in 0..3 {
            let t = Instant::now();
            let looks = Looks::build_live(&root, &direct, &cats);
            eprintln!("{:?} for {} folders", t.elapsed(), looks.folders.len());
        }
    }
}
