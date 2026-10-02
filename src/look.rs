//! Slice colors in the finished chart. The category taking the most space
//! in a slice sets its hue (the category bar's colors); the size-weighted
//! average age of its contents, by Changed time (when the data arrived on
//! this disk), sets its brightness in three steps: new is brightest,
//! `age_days` and older is darkest.

use super::*;

/// What decides a slice's color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Look {
    pub(crate) cat: Category,
    /// Size-weighted average Changed time; NO_TIME if none is known.
    pub(crate) avg_ctime: i64,
    pub(crate) size: u64,
}

/// The look of every folder in a tree, keyed by the folder node's address
/// (valid until the tree is rebuilt).
pub(crate) struct Looks {
    folders: HashMap<usize, Look>,
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

/// Number of brightness steps.
pub(crate) const AGE_STEPS: u8 = 3;

/// Brightness step of an average Changed time: the time from now back to
/// `age_days` is split evenly between the steps, and `age_days` or older
/// (or unknown) is the last.
pub(crate) fn age_step(avg_ctime: i64, now: i64, age_days: u32) -> u8 {
    const DAY: f64 = 24.0 * 3600.0;
    const LAST: u8 = AGE_STEPS - 1;
    if avg_ctime == NO_TIME {
        return LAST;
    }
    let days = (now - avg_ctime).max(0) as f64 / DAY;
    let share = days / age_days.max(1) as f64;
    ((share * LAST as f64) as u8).min(LAST)
}

/// `color` at brightness step `step`: 0 is the color as is, each step
/// darker, down to 25% brightness at the last.
pub(crate) fn shade(color: Color32, step: u8) -> Color32 {
    let last = (AGE_STEPS - 1) as f32;
    let factor = 1.0 - 0.75 * (step as f32).min(last) / last;
    let scale = |v: u8| (v as f32 * factor).round() as u8;
    Color32::from_rgb(scale(color.r()), scale(color.g()), scale(color.b()))
}

/// The color of a slice with look `look`.
pub(crate) fn look_color(
    look: &Look,
    now: i64,
    age_days: u32,
    cats: &CategoryModel,
    dark: bool,
) -> Color32 {
    shade(
        cats.color(look.cat, dark),
        age_step(look.avg_ctime, now, age_days),
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
    fn age_steps_split_the_days_evenly() {
        let now = 1_000_000_000;
        assert_eq!(age_step(now - DAY, now, 365), 0);
        assert_eq!(age_step(now - 182 * DAY, now, 365), 0);
        assert_eq!(age_step(now - 183 * DAY, now, 365), 1);
        assert_eq!(age_step(now - 364 * DAY, now, 365), 1);
        assert_eq!(age_step(now - 365 * DAY, now, 365), 2);
        assert_eq!(age_step(now - 5000 * DAY, now, 365), 2);
        assert_eq!(age_step(NO_TIME, now, 365), 2);
        // Steps never go down as files get older.
        let steps: Vec<u8> = (0..400)
            .map(|d| age_step(now - d * DAY, now, 365))
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

    #[test]
    fn shades_get_darker_with_age() {
        let c = Color32::from_rgb(200, 100, 50);
        assert_eq!(shade(c, 0), c);
        assert!(shade(c, 2).r() < shade(c, 1).r() && shade(c, 1).r() < c.r());
        assert_eq!(shade(c, 2), Color32::from_rgb(50, 25, 13));
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
