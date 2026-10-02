//! File categories (Video, Audio, Documents, …) for the colored bar beside
//! the contents table, which filters it to one category.
//!
//! The categories come from ~/.config/spacemap/categories.json, written from
//! the built-in defaults (src/categories.json) when missing. Each category
//! has a name and a list of file extensions. The name is a language-file
//! token ("CAT_VIDEO"); one without a translation is shown as written. Files
//! with an unlisted extension, or none, are "Other". Colors follow the
//! order of the list.

use super::*;
use serde_json::Value;

/// The built-in categories, written out as categories.json when there's none.
const DEFAULT_CATEGORIES: &str = include_str!("categories.json");

/// categories.json bigger than this isn't read (see MAX_SETTINGS_BYTES).
const MAX_CATEGORIES_BYTES: u64 = 1 << 20;

/// Category colors in list order, as (dark, light) steps: a categorical
/// palette checked for color-blind separation. Past eight, colors stop
/// being told apart reliably.
const PALETTE: [(u32, u32); 8] = [
    (0x3987e5, 0x2a78d6),
    (0xd95926, 0xeb6834),
    (0x199e70, 0x1baf7a),
    (0xc98500, 0xeda100),
    (0xd55181, 0xe87ba4),
    (0x008300, 0x008300),
    (0x9085e9, 0x4a3aa7),
    (0xe66767, 0xe34948),
];
const OTHER_COLOR: (u32, u32) = (0x6f6e69, 0xa8a7a2);

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// A category, as its position in the list; `CategoryModel::other()` (one
/// past the end) is Other.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Category(pub(crate) usize);

/// The categories in use and how files map to them.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct CategoryModel {
    /// Name tokens, translated when shown.
    names: Vec<String>,
    by_ext: HashMap<String, usize>,
}

impl CategoryModel {
    pub(crate) fn other(&self) -> Category {
        Category(self.names.len())
    }

    /// The category of a file called `name`, by its extension (any case);
    /// Other if it has none or it isn't listed.
    pub(crate) fn of_name(&self, name: &str) -> Category {
        self.of_ext(&ext_key(name))
    }

    /// The category of an `ext_key`.
    pub(crate) fn of_ext(&self, ext: &str) -> Category {
        self.by_ext.get(ext).map_or(self.other(), |i| Category(*i))
    }

    pub(crate) fn label(&self, c: Category) -> String {
        tr(self.names.get(c.0).map_or("CAT_OTHER", String::as_str))
    }

    /// The category's color on a dark or light background: palette colors
    /// in list order, then evenly spread hues; Other is gray.
    pub(crate) fn color(&self, c: Category, dark: bool) -> Color32 {
        if c == self.other() {
            return rgb(if dark { OTHER_COLOR.0 } else { OTHER_COLOR.1 });
        }
        match PALETTE.get(c.0) {
            Some(&(d, l)) => rgb(if dark { d } else { l }),
            None => hsv_to_rgb(hue_for_branch(c.0), 0.6, if dark { 0.8 } else { 0.7 }),
        }
    }

    pub(crate) fn defaults() -> Self {
        Self::parse(DEFAULT_CATEGORIES)
            .map(|(m, _)| m)
            .expect("built-in categories.json is valid")
    }

    /// Reads categories.json text. Err if it isn't a JSON object with a
    /// "categories" list; otherwise each problem (a missing name, an
    /// extension listed twice…) is skipped and described in the returned
    /// list.
    pub(crate) fn parse(text: &str) -> Result<(Self, Vec<String>), String> {
        let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let list = value
            .get("categories")
            .and_then(Value::as_array)
            .ok_or_else(|| tr("ERR_CATEGORIES_NO_LIST"))?;
        let mut model = CategoryModel {
            names: Vec::new(),
            by_ext: HashMap::new(),
        };
        let mut problems = Vec::new();
        for (n, entry) in list.iter().enumerate() {
            let at = format!("categories[{n}]");
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string());
            let Some(name) = name.filter(|s| !s.is_empty()) else {
                problems.push(trf("ERR_CATEGORIES_NO_NAME", &[&at]));
                continue;
            };
            let at = format!("\"{}\"", tr(&name));
            // Other is built in: a category of that name would look like a
            // second one.
            if name == "CAT_OTHER" || tr(&name).to_lowercase() == tr("CAT_OTHER").to_lowercase() {
                problems.push(trf("ERR_CATEGORIES_OTHER", &[&at]));
                continue;
            }
            let idx = model.names.len();
            model.names.push(name);

            let strings = |key: &str, problems: &mut Vec<String>| -> Vec<String> {
                match entry.get(key) {
                    None => Vec::new(),
                    Some(Value::Array(items)) => items
                        .iter()
                        .filter_map(|v| {
                            let s = v
                                .as_str()
                                .map(|s| s.trim().trim_start_matches('.').to_lowercase())
                                .filter(|s| !s.is_empty());
                            if s.is_none() {
                                problems
                                    .push(trf("ERR_CATEGORIES_ENTRY", &[&at, key, &v.to_string()]));
                            }
                            s
                        })
                        .collect(),
                    Some(v) => {
                        problems.push(trf("ERR_CATEGORIES_ENTRY", &[&at, key, &v.to_string()]));
                        Vec::new()
                    }
                }
            };
            for ext in strings("extensions", &mut problems) {
                match model.by_ext.get(&ext) {
                    Some(&first) => problems.push(trf(
                        "ERR_CATEGORIES_TWICE",
                        &[&format!(".{ext}"), &tr(&model.names[first])],
                    )),
                    None => {
                        model.by_ext.insert(ext, idx);
                    }
                }
            }
        }
        Ok((model, problems))
    }

    /// The user's categories.json, written from the defaults first if it's
    /// missing. A file that can't be used gives the defaults and a problem
    /// for the Issues log, and is left as it is.
    pub(crate) fn load() -> (Self, Option<String>) {
        let path = config_dir().join("categories.json");
        let text = match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let written = std::fs::create_dir_all(config_dir())
                    .and_then(|_| std::fs::write(&path, DEFAULT_CATEGORIES));
                let problem = written
                    .err()
                    .map(|e| trf("ERR_CATEGORIES_SAVE", &[&show_path(&path), &e.to_string()]));
                return (Self::defaults(), problem);
            }
            Err(e) => Err(e.to_string()),
            Ok(m) if !m.is_file() => Err(tr("ERR_SETTINGS_NOT_FILE")),
            Ok(m) if m.len() > MAX_CATEGORIES_BYTES => Err(tr("ERR_SETTINGS_TOO_BIG")),
            Ok(_) => std::fs::read_to_string(&path).map_err(|e| e.to_string()),
        };
        match text.and_then(|t| Self::parse(&t)) {
            Ok((model, problems)) if problems.is_empty() => (model, None),
            Ok((model, problems)) => (
                model,
                Some(trf(
                    "ERR_CATEGORIES_PROBLEMS",
                    &[&show_path(&path), &problems.join("; ")],
                )),
            ),
            Err(e) => (
                Self::defaults(),
                Some(trf("ERR_CATEGORIES_FILE", &[&show_path(&path), &e])),
            ),
        }
    }
}

/// What the files are narrowed down to: one category, or one or more
/// extensions (`ext_key`s; never none).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Pick {
    Category(Category),
    Extensions(std::collections::BTreeSet<String>),
}

impl Pick {
    /// A pick of the one extension `ext`.
    #[cfg(test)]
    pub(crate) fn extension(ext: &str) -> Pick {
        Pick::Extensions([ext.to_string()].into())
    }

    /// True if `ext` is one of the picked extensions.
    pub(crate) fn has_extension(&self, ext: &str) -> bool {
        matches!(self, Pick::Extensions(set) if set.contains(ext))
    }
}

impl CategoryModel {
    /// True if a file called `name` is part of `pick`.
    pub(crate) fn pick_matches(&self, pick: &Pick, name: &str) -> bool {
        match pick {
            Pick::Category(c) => self.of_name(name) == *c,
            Pick::Extensions(set) => set.contains(&ext_key(name)),
        }
    }

    /// True if `pick` has files of category `cat`.
    pub(crate) fn pick_in_category(&self, pick: &Pick, cat: Category) -> bool {
        match pick {
            Pick::Category(c) => *c == cat,
            Pick::Extensions(set) => set.iter().any(|e| self.of_ext(e) == cat),
        }
    }

    /// `pick` as shown: the category's name, or the extensions (the first
    /// three, then how many more).
    pub(crate) fn pick_label(&self, pick: &Pick) -> String {
        const SHOWN: usize = 3;
        match pick {
            Pick::Category(c) => self.label(*c),
            Pick::Extensions(set) => {
                let mut label = set
                    .iter()
                    .take(SHOWN)
                    .map(|e| ext_label(e))
                    .collect::<Vec<_>>()
                    .join(", ");
                if set.len() > SHOWN {
                    label.push_str(&trf("EXT_MORE", &[&(set.len() - SHOWN).to_string()]));
                }
                label
            }
        }
    }
}

/// An extension as shown: ".mkv", quoted if it has spaces or control
/// characters (so a trailing space shows), or "(no extension)".
pub(crate) fn ext_label(ext: &str) -> String {
    if ext.is_empty() {
        tr("EXT_NO_EXTENSION")
    } else if ext.chars().any(|c| c.is_whitespace() || c.is_control()) {
        format!("\".{}\"", ext.escape_debug())
    } else {
        format!(".{ext}")
    }
}

/// One category's share of a folder.
#[derive(Debug, PartialEq)]
pub(crate) struct CategoryRow {
    pub(crate) cat: Category,
    pub(crate) size: u64,
    pub(crate) files: u64,
    /// (extension, size, file count) within the category, largest first;
    /// "" for files without an extension.
    pub(crate) exts: Vec<(String, u64, u64)>,
}

/// A file's extension as the categories see it: lowercased, "" if none.
pub(crate) fn ext_key(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Extension → (total size, file count).
pub(crate) type ExtTotals = HashMap<String, (u64, u64)>;

/// Adds `size` and `files` to extension `ext`'s totals.
pub(crate) fn add_ext(totals: &mut ExtTotals, ext: String, size: u64, files: u64) {
    let e = totals.entry(ext).or_insert((0, 0));
    e.0 = e.0.saturating_add(size);
    e.1 += files;
}

/// How the files under `node` split into categories, largest first with
/// Other last; categories with no files are left out.
pub(crate) fn category_breakdown(node: &Node, model: &CategoryModel) -> Vec<CategoryRow> {
    fn walk(n: &Node, acc: &mut ExtTotals) {
        if n.is_dir {
            for c in &n.children {
                deep(|| walk(c, acc));
            }
        } else {
            add_ext(acc, ext_key(&n.name), n.size, n.file_count.max(1));
        }
    }
    let mut acc = ExtTotals::new();
    walk(node, &mut acc);
    category_rows(&acc, model)
}

/// Extension totals grouped into categories, largest first with Other last.
pub(crate) fn category_rows(totals: &ExtTotals, model: &CategoryModel) -> Vec<CategoryRow> {
    let mut rows: Vec<CategoryRow> = Vec::new();
    for (ext, &(size, files)) in totals {
        let cat = model.of_ext(ext);
        let i = match rows.iter().position(|r| r.cat == cat) {
            Some(i) => i,
            None => {
                rows.push(CategoryRow {
                    cat,
                    size: 0,
                    files: 0,
                    exts: Vec::new(),
                });
                rows.len() - 1
            }
        };
        let row = &mut rows[i];
        row.size = row.size.saturating_add(size);
        row.files += files;
        row.exts.push((ext.clone(), size, files));
    }
    for r in &mut rows {
        r.exts
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    }
    // Other always last; the rest by size, ties in list order.
    let other = model.other();
    rows.sort_by_key(|r| (r.cat == other, std::cmp::Reverse(r.size), r.cat.0));
    rows
}

/// The folder at `path` inside `root`, if it's there.
pub(crate) fn find_by_path<'a>(root: &'a Node, path: &Path) -> Option<&'a Node> {
    let rel = path.strip_prefix(&root.path).ok()?;
    let mut n = root;
    let mut cur = root.path.clone();
    for comp in rel.components() {
        cur.push(comp);
        n = n.children.iter().find(|c| c.path == cur)?;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(m: &CategoryModel, file: &str) -> String {
        m.label(m.of_name(file))
    }

    #[test]
    fn defaults_map_extensions() {
        let m = CategoryModel::defaults();
        assert_eq!(named(&m, "Star Wars (1977).mkv"), "Video");
        assert_eq!(named(&m, "Movie.EN.SRT"), "Video");
        assert_eq!(named(&m, "song.flac"), "Audio");
        assert_eq!(named(&m, "IMG_0001.JPG"), "Images");
        assert_eq!(named(&m, "report.pdf"), "Documents");
        assert_eq!(named(&m, "backup.tar.gz"), "Archives");
        assert_eq!(named(&m, "tool.AppImage"), "Applications");
        assert_eq!(named(&m, "main.rs"), "Code");
        assert_eq!(named(&m, "places.sqlite"), "Data");
        for other in ["Makefile", ".bashrc", "weird.xyz123"] {
            assert_eq!(m.of_name(other), m.other(), "{other}");
        }
    }

    #[test]
    fn defaults_have_no_problems() {
        let (_, problems) = CategoryModel::parse(DEFAULT_CATEGORIES).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn custom_file_defines_everything() {
        let text = r#"{"categories": [
            {"name": "Mail", "extensions": ["eml", ".MBOX"]},
            {"name": "Video", "extensions": ["mkv", "eml"]},
            {"name": "", "extensions": ["x"]},
            {"name": "Odd", "extensions": [3]}
        ]}"#;
        let (m, problems) = CategoryModel::parse(text).unwrap();
        assert_eq!(named(&m, "a.eml"), "Mail");
        assert_eq!(named(&m, "box.mbox"), "Mail");
        assert_eq!(named(&m, "a.mkv"), "Video");
        // Not listed any more: falls to Other.
        assert_eq!(m.of_name("a.pdf"), m.other());
        // Colors follow the list order.
        assert_eq!(m.color(m.of_name("a.eml"), true), rgb(PALETTE[0].0));
        assert_eq!(m.color(m.of_name("a.mkv"), true), rgb(PALETTE[1].0));
        // .eml twice, a nameless entry, a non-string extension.
        assert_eq!(problems.len(), 3, "{problems:?}");
    }

    /// A category named like the built-in Other is skipped, with a problem.
    #[test]
    fn a_category_named_other_is_skipped() {
        let text = r#"{"categories": [{"name": "other", "extensions": ["x"]}, {"name": "CAT_VIDEO", "extensions": ["mkv"]}]}"#;
        let (m, problems) = CategoryModel::parse(text).unwrap();
        assert_eq!(problems.len(), 1);
        assert_eq!(m.of_name("a.x"), m.other());
        assert_eq!(named(&m, "a.mkv"), "Video");
    }

    #[test]
    fn unusable_file_is_an_error() {
        assert!(CategoryModel::parse("not json").is_err());
        assert!(CategoryModel::parse(r#"{"cats": []}"#).is_err());
        let (m, _) = CategoryModel::parse(r#"{"categories": []}"#).unwrap();
        assert_eq!(m.of_name("a.mkv"), m.other());
    }

    #[test]
    fn breakdown_sums_and_orders() {
        let m = CategoryModel::defaults();
        let tree = test_node(
            "/r",
            0,
            true,
            vec![
                test_node("/r/a.mkv", 100, false, vec![]),
                test_node(
                    "/r/d",
                    0,
                    true,
                    vec![
                        test_node("/r/d/b.mp4", 50, false, vec![]),
                        test_node("/r/d/c.pdf", 200, false, vec![]),
                        test_node("/r/d/README", 999, false, vec![]),
                    ],
                ),
            ],
        );
        let rows = category_breakdown(&tree, &m);
        let b: Vec<(String, u64, u64)> = rows
            .iter()
            .map(|r| (m.label(r.cat), r.size, r.files))
            .collect();
        assert_eq!(
            b,
            vec![
                ("Documents".into(), 200, 1),
                ("Video".into(), 150, 2),
                ("Other".into(), 999, 1)
            ]
        );
        assert_eq!(
            rows[1].exts,
            vec![("mkv".to_string(), 100, 1), ("mp4".to_string(), 50, 1)]
        );
        assert_eq!(
            find_by_path(&tree, Path::new("/r/d")).map(|n| n.children.len()),
            Some(3)
        );
        assert!(find_by_path(&tree, Path::new("/r/x")).is_none());
    }

    #[test]
    fn category_filter_keeps_only_its_files() {
        let m = CategoryModel::defaults();
        let video = m.of_name("x.mkv");
        let tree = test_node(
            "/r",
            0,
            true,
            vec![
                test_node("/r/a.mkv", 100, false, vec![]),
                test_node(
                    "/r/docs",
                    0,
                    true,
                    vec![test_node("/r/docs/c.pdf", 200, false, vec![])],
                ),
                test_node(
                    "/r/d",
                    0,
                    true,
                    vec![
                        test_node("/r/d/b.srt", 5, false, vec![]),
                        test_node("/r/d/n.txt", 7, false, vec![]),
                    ],
                ),
            ],
        );
        let only = filter_tree_by(&tree, &|n: &Node| m.of_name(&n.name) == video).unwrap();
        assert_eq!((only.size, only.file_count), (105, 2));
        let names: Vec<&str> = only.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["a.mkv", "d"]);
    }
}
