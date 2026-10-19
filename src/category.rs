//! File categories (Video, Audio, Documents, …) for the colored bar beside
//! the contents table, which filters it to one category.
//!
//! The categories come from ~/.config/spacescan/categories.json, written from
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

/// The color-blind-safe palette (Paul Tol's): his "light" colors on a dark
/// background, his "muted" ones on a light background, as (dark, light).
const SAFE_PALETTE: [(u32, u32); 8] = [
    (0x77aadd, 0x332288),
    (0xee8866, 0xcc6677),
    (0x44bb99, 0x117733),
    (0xeedd88, 0xddcc77),
    (0xffaabb, 0xaa4499),
    (0x99ddff, 0x88ccee),
    (0xbbcc33, 0x882255),
    (0xaaaa00, 0x999933),
];

/// Whether categories use the color-blind-safe palette (a setting).
pub(crate) static COLOR_BLIND_SAFE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// A category, as its position in the list; `CategoryModel::other()` (one
/// past the end) is Other.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Category(pub(crate) usize);

/// The categories in use and how files map to them.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct CategoryModel {
    /// Name tokens, translated when shown.
    names: Vec<String>,
    /// Each category's extensions, in the file's order.
    extensions: Vec<Vec<String>>,
    /// Each category's picked color; None: from the palette, by position.
    colors: Vec<Option<CatColor>>,
    by_ext: FxHashMap<String, usize>,
}

impl CategoryModel {
    pub(crate) fn other(&self) -> Category {
        Category(self.names.len())
    }

    /// The category of a file called `name`, by its extension (any case);
    /// Other if it has none or it isn't listed.
    pub(crate) fn of_name(&self, name: &str) -> Category {
        // Runs for every file during a scan, so without heap memory.
        self.of_ext(&ext_key_in(name, &mut [0; 16]))
    }

    /// The category of an `ext_key`.
    pub(crate) fn of_ext(&self, ext: &str) -> Category {
        self.by_ext.get(ext).map_or(self.other(), |i| Category(*i))
    }

    pub(crate) fn label(&self, c: Category) -> String {
        tr(self.names.get(c.0).map_or("CAT_OTHER", String::as_str))
    }

    /// Every category, Other last.
    pub(crate) fn all(&self) -> impl Iterator<Item = Category> {
        (0..=self.other().0).map(Category)
    }

    /// The categories named in `list`: names as shown in the current
    /// language, separated by commas, in any case. An unknown or missing
    /// name is an error that lists the names there are.
    pub(crate) fn named(&self, list: &str) -> Result<CatSet, String> {
        let mut set = CatSet::default();
        for name in list.split(',').map(str::trim) {
            let wanted = name.to_lowercase();
            match self.all().find(|&c| self.label(c).to_lowercase() == wanted) {
                Some(c) if !name.is_empty() => set.insert(c),
                _ => {
                    let names: Vec<String> = self.all().map(|c| self.label(c)).collect();
                    let what = if name.is_empty() {
                        "a category name is missing".to_string()
                    } else {
                        format!("unknown category {name}")
                    };
                    return Err(format!("{what} (categories: {})", names.join(", ")));
                }
            }
        }
        Ok(set)
    }

    /// The category's color on a dark or light background, in the palette
    /// chosen in the settings (see `color_in`).
    pub(crate) fn color(&self, c: Category, dark: bool) -> Color32 {
        let safe = COLOR_BLIND_SAFE.load(std::sync::atomic::Ordering::Relaxed);
        self.color_in(c, dark, safe)
    }

    /// The category's color on a dark or light background: the one picked
    /// for it, else palette colors (`safe`: the color-blind-safe ones) in
    /// list order, then evenly spread hues; Other is gray.
    pub(crate) fn color_in(&self, c: Category, dark: bool, safe: bool) -> Color32 {
        if c == self.other() {
            return Self::other_color(dark);
        }
        match self.colors.get(c.0) {
            Some(Some(picked)) => picked.on(dark),
            _ => position_color(c.0, dark, safe),
        }
    }

    /// Other's gray, on a dark or light background.
    pub(crate) fn other_color(dark: bool) -> Color32 {
        rgb(if dark { OTHER_COLOR.0 } else { OTHER_COLOR.1 })
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
            extensions: Vec::new(),
            colors: Vec::new(),
            by_ext: FxHashMap::default(),
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
            if is_other_name(&name) {
                problems.push(trf("ERR_CATEGORIES_OTHER", &[&at]));
                continue;
            }
            let idx = model.names.len();
            model.names.push(name);
            model.extensions.push(Vec::new());
            let color = match entry.get("color") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let color = v.as_str().and_then(CatColor::parse);
                    if color.is_none() {
                        problems.push(trf("ERR_CATEGORIES_ENTRY", &[&at, "color", &v.to_string()]));
                    }
                    color
                }
            };
            model.colors.push(color);

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
                        model.by_ext.insert(ext.clone(), idx);
                        model.extensions[idx].push(ext);
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
        Self::load_from_file(true)
    }

    /// Like `load`, but never writes: a missing file means the built-in
    /// categories (for the command line, which leaves the config alone).
    pub(crate) fn load_read_only() -> (Self, Option<String>) {
        Self::load_from_file(false)
    }

    fn load_from_file(write_missing: bool) -> (Self, Option<String>) {
        // Read-only, before the app first ran under its new name: the old
        // settings folder's file.
        let new_missing = std::fs::symlink_metadata(config_dir())
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
        let dir = if !write_missing && new_missing {
            config::old_config_dir()
        } else {
            config_dir()
        };
        let path = dir.join("categories.json");
        let text = match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !write_missing => {
                return (Self::defaults(), None);
            }
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

/// The color of the category at `index` when none is picked: palette colors
/// (`safe`: the color-blind-safe ones) in list order, then evenly spread
/// hues.
pub(crate) fn position_color(index: usize, dark: bool, safe: bool) -> Color32 {
    let palette = if safe { &SAFE_PALETTE } else { &PALETTE };
    match palette.get(index) {
        Some(&(d, l)) => rgb(if dark { d } else { l }),
        None => hsv_to_rgb(hue_for_branch(index), 0.6, if dark { 0.8 } else { 0.7 }),
    }
}

/// True if `name` (a token or plain text) is Other's, which is built in.
pub(crate) fn is_other_name(name: &str) -> bool {
    let bare = |n: String| n.trim_matches(['(', ')', '（', '）']).to_lowercase();
    name == "CAT_OTHER" || bare(tr(name)) == bare(tr("CAT_OTHER"))
}

/// A category as categories.json holds it: what the categories editor
/// changes.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct CategoryEntry {
    /// A language token ("CAT_VIDEO"), shown translated, or a plain name.
    pub(crate) name: String,
    /// Lowercase, without the dot.
    pub(crate) extensions: Vec<String>,
    /// None: a palette color, by position.
    pub(crate) color: Option<CatColor>,
}

impl CategoryModel {
    /// The categories, as categories.json would list them.
    pub(crate) fn entries(&self) -> Vec<CategoryEntry> {
        (0..self.names.len())
            .map(|i| CategoryEntry {
                name: self.names[i].clone(),
                extensions: self.extensions[i].clone(),
                color: self.colors[i],
            })
            .collect()
    }

    /// True if both put every extension in the same category (by position),
    /// so files classified with one are classified right for the other.
    pub(crate) fn same_classes(&self, other: &CategoryModel) -> bool {
        self.names.len() == other.names.len() && self.by_ext == other.by_ext
    }
}

/// categories.json text for `entries`, laid out like the built-in file.
pub(crate) fn categories_json(entries: &[CategoryEntry]) -> String {
    let quoted = |s: &str| serde_json::to_string(s).expect("a string is valid JSON");
    let mut out = String::from("{\n  \"categories\": [\n");
    for (i, entry) in entries.iter().enumerate() {
        out += &format!("    {{\n      \"name\": {},\n", quoted(&entry.name));
        if let Some(color) = entry.color {
            out += &format!("      \"color\": {},\n", quoted(&color.to_text()));
        }
        if entry.extensions.is_empty() {
            out += "      \"extensions\": []\n";
        } else {
            out += "      \"extensions\": [\n";
            let mut line = String::new();
            for (k, ext) in entry.extensions.iter().enumerate() {
                let item = quoted(ext)
                    + if k + 1 < entry.extensions.len() {
                        ","
                    } else {
                        ""
                    };
                if !line.is_empty() && 8 + line.len() + 1 + item.len() > 100 {
                    out += &format!("        {}\n", line.trim_end());
                    line.clear();
                }
                line += &item;
                line.push(' ');
            }
            out += &format!("        {}\n      ]\n", line.trim_end());
        }
        out += if i + 1 < entries.len() {
            "    },\n"
        } else {
            "    }\n"
        };
    }
    out + "  ]\n}\n"
}

/// Saves `entries` as the user's categories.json. A file that couldn't be
/// used as it was is first copied to categories.json.bad.
pub(crate) fn save_categories(entries: &[CategoryEntry]) -> std::io::Result<()> {
    let path = config_dir().join("categories.json");
    if let Ok(text) = std::fs::read_to_string(&path)
        && !CategoryModel::parse(&text).is_ok_and(|(_, problems)| problems.is_empty())
    {
        std::fs::copy(&path, path.with_extension("json.bad"))?;
    }
    config::write_whole(&path, categories_json(entries).as_bytes())
}

/// A set of categories: those with an index below 256 (what a file's
/// stored category byte can hold).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) struct CatSet([u64; 4]);

impl CatSet {
    /// The set holding only `c`.
    pub(crate) fn of(c: Category) -> CatSet {
        let mut set = CatSet::default();
        set.insert(c);
        set
    }

    pub(crate) fn insert(&mut self, c: Category) {
        if c.0 < 256 {
            self.0[c.0 / 64] |= 1 << (c.0 % 64);
        }
    }

    pub(crate) fn remove(&mut self, c: Category) {
        if c.0 < 256 {
            self.0[c.0 / 64] &= !(1 << (c.0 % 64));
        }
    }

    /// Adds `c` if it's missing, removes it if it's there.
    pub(crate) fn toggle(&mut self, c: Category) {
        if self.contains(c) {
            self.remove(c);
        } else {
            self.insert(c);
        }
    }

    pub(crate) fn contains(&self, c: Category) -> bool {
        c.0 < 256 && self.0[c.0 / 64] & (1 << (c.0 % 64)) != 0
    }

    /// True if a file whose stored category byte is `b` is in the set
    /// (never for `NO_CAT`).
    pub(crate) fn has_byte(&self, b: u8) -> bool {
        b != NO_CAT && self.contains(Category(usize::from(b)))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0 == [0; 4]
    }

    pub(crate) fn len(&self) -> usize {
        self.0.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// The categories in the set, in index order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = Category> + '_ {
        (0..256).map(Category).filter(|&c| self.contains(c))
    }
}

/// What the files are narrowed down to: one or more categories, or one or
/// more extensions (`ext_key`s). Never an empty set.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Pick {
    Categories(CatSet),
    Extensions(std::collections::BTreeSet<String>),
}

impl Pick {
    /// A pick of the one category `c`.
    pub(crate) fn category(c: Category) -> Pick {
        Pick::Categories(CatSet::of(c))
    }

    /// The picked categories, for a pick of categories.
    pub(crate) fn categories(&self) -> Option<CatSet> {
        match self {
            Pick::Categories(set) => Some(*set),
            Pick::Extensions(_) => None,
        }
    }

    /// True if category `c` is one of the picked categories.
    pub(crate) fn has_category(&self, c: Category) -> bool {
        matches!(self, Pick::Categories(set) if set.contains(c))
    }

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
            Pick::Categories(set) => set.contains(self.of_name(name)),
            Pick::Extensions(set) => set.contains(&ext_key(name)),
        }
    }

    /// True if `pick` has files of category `cat`.
    pub(crate) fn pick_in_category(&self, pick: &Pick, cat: Category) -> bool {
        match pick {
            Pick::Categories(set) => set.contains(cat),
            Pick::Extensions(set) => set.iter().any(|e| self.of_ext(e) == cat),
        }
    }

    /// `pick` as shown: the categories' names or the extensions (the first
    /// three, then how many more).
    pub(crate) fn pick_label(&self, pick: &Pick) -> String {
        const SHOWN: usize = 3;
        let (names, count): (Vec<String>, usize) = match pick {
            Pick::Categories(set) => (
                set.iter().take(SHOWN).map(|c| self.label(c)).collect(),
                set.len(),
            ),
            Pick::Extensions(set) => (
                set.iter().take(SHOWN).map(|e| ext_label(e)).collect(),
                set.len(),
            ),
        };
        let mut label = names.join(", ");
        if count > SHOWN {
            label.push_str(&trf("EXT_MORE", &[&(count - SHOWN).to_string()]));
        }
        label
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
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CategoryRow {
    pub(crate) cat: Category,
    pub(crate) size: u64,
    pub(crate) files: u64,
    /// (extension, size, file count) within the category, largest first;
    /// "" for files without an extension.
    pub(crate) exts: Vec<(String, u64, u64)>,
}

impl CategoryRow {
    /// Its weight in `measure`: bytes, or files.
    pub(crate) fn weight(&self, measure: Measure) -> u64 {
        measure.pick(self.size, self.files)
    }
}

/// `ext_key`, without heap memory when the extension is ASCII and fits
/// in `buf` (nearly always).
pub(crate) fn ext_key_in<'a>(name: &'a str, buf: &'a mut [u8; 16]) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    let ext = match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => return Cow::Borrowed(""),
    };
    if ext.len() <= buf.len() && ext.is_ascii() {
        let lower = &mut buf[..ext.len()];
        lower.copy_from_slice(ext.as_bytes());
        lower.make_ascii_lowercase();
        return Cow::Borrowed(std::str::from_utf8(lower).expect("ASCII is UTF-8"));
    }
    Cow::Owned(ext_key(name))
}

/// A file's extension as the categories see it: lowercased, "" if none.
pub(crate) fn ext_key(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Extension → (total size, file count).
pub(crate) type ExtTotals = FxHashMap<String, (u64, u64)>;

/// Adds `size` and `files` to extension `ext`'s totals.
pub(crate) fn add_ext(totals: &mut ExtTotals, ext: String, size: u64, files: u64) {
    let e = totals.entry(ext).or_insert((0, 0));
    e.0 = e.0.saturating_add(size);
    e.1 += files;
}

/// How the files under `node` split into categories, largest first;
/// categories with no files are left out.
pub(crate) fn category_breakdown(
    node: &Node,
    model: &CategoryModel,
    measure: Measure,
) -> Vec<CategoryRow> {
    /// Subtrees with fewer files than this are added up on one thread.
    const SPLIT: u64 = 2_000;
    fn walk(n: &Node, acc: &mut ExtTotals, buf: &mut [u8; 16]) {
        if !n.is_dir {
            // A new string only for an extension not seen yet.
            let ext = ext_key_in(&n.name, buf);
            let (size, files) = (n.size, n.file_count.max(1));
            match acc.get_mut(ext.as_ref()) {
                Some(e) => {
                    e.0 = e.0.saturating_add(size);
                    e.1 += files;
                }
                None => {
                    acc.insert(ext.into_owned(), (size, files));
                }
            }
            return;
        }
        // Big subfolders at the same time, each into its own totals.
        let big: Vec<&Node> = n
            .children
            .iter()
            .filter(|c| c.is_dir && c.file_count >= SPLIT)
            .collect();
        if !big.is_empty() {
            let parts: Vec<ExtTotals> = big
                .par_iter()
                .map(|c| {
                    let mut part = ExtTotals::default();
                    deep(|| walk(c, &mut part, &mut [0; 16]));
                    part
                })
                .collect();
            for part in parts {
                for (ext, (size, files)) in part {
                    add_ext(acc, ext, size, files);
                }
            }
        }
        for c in n
            .children
            .iter()
            .filter(|c| !c.is_dir || c.file_count < SPLIT)
        {
            deep(|| walk(c, acc, buf));
        }
    }
    let mut acc = ExtTotals::default();
    walk(node, &mut acc, &mut [0; 16]);
    category_rows(&acc, model, measure)
}

/// Extension totals grouped into categories, largest first in `measure`.
pub(crate) fn category_rows(
    totals: &ExtTotals,
    model: &CategoryModel,
    measure: Measure,
) -> Vec<CategoryRow> {
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
    // By `measure` (bytes or files), ties by extension; rows ties in list
    // order (Other last among equals).
    let files = measure.by_files();
    for r in &mut rows {
        r.exts.sort_by(|a, b| {
            let (x, y) = if files { (a.2, b.2) } else { (a.1, b.1) };
            y.cmp(&x).then_with(|| a.0.cmp(&b.0))
        });
    }
    rows.sort_by_key(|r| {
        let w = if files { r.files } else { r.size };
        (std::cmp::Reverse(w), r.cat.0)
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written back and read again, categories stay the same, picked
    /// colors and empty lists included.
    #[test]
    fn categories_are_written_as_read() {
        let mut entries = CategoryModel::defaults().entries();
        let text = categories_json(&entries);
        assert!(text.lines().all(|l| l.len() <= 100));
        assert_eq!(
            CategoryModel::parse(&text).unwrap().0,
            CategoryModel::defaults()
        );
        entries[1].color = Some(CatColor::parse("#a1b2c3").unwrap());
        entries[2].color = Some(CatColor::Listed(5));
        entries.push(CategoryEntry {
            name: "Mine \"quoted\"".to_string(),
            extensions: Vec::new(),
            color: None,
        });
        let (model, problems) = CategoryModel::parse(&categories_json(&entries)).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(model.entries(), entries);
        assert!(model.same_classes(&CategoryModel::parse(&categories_json(&entries)).unwrap().0));
        assert!(!model.same_classes(&CategoryModel::defaults()));
        let (_, problems) =
            CategoryModel::parse(r#"{"categories":[{"name":"A","color":"red-2"}]}"#).unwrap();
        assert_eq!(problems.len(), 1);
    }

    /// Sets of categories: every index that a stored byte can hold, the
    /// edges of each word, and indexes past what the set can hold.
    #[test]
    fn category_sets() {
        let mut set = CatSet::default();
        assert!(set.is_empty());
        for c in [0, 63, 64, 127, 128, 191, 192, 254, 255] {
            set.insert(Category(c));
            assert!(set.contains(Category(c)), "{c}");
        }
        assert_eq!(set.len(), 9);
        assert_eq!(
            set.iter().map(|c| c.0).collect::<Vec<_>>(),
            [0, 63, 64, 127, 128, 191, 192, 254, 255]
        );
        assert!(set.has_byte(254));
        assert!(!set.has_byte(NO_CAT), "no stored category is never picked");
        assert!(!set.has_byte(1));
        set.insert(Category(256));
        set.insert(Category(usize::MAX));
        assert!(!set.contains(Category(256)));
        assert_eq!(set.len(), 9);
        set.toggle(Category(63));
        assert!(!set.contains(Category(63)) && set.contains(Category(64)));
        set.toggle(Category(63));
        assert!(set.contains(Category(63)));
        for c in set.clone().iter() {
            set.remove(c);
        }
        assert!(set.is_empty());
        assert_eq!(
            CatSet::of(Category(5)).iter().collect::<Vec<_>>(),
            [Category(5)]
        );
    }

    /// A pick of several categories matches files of any of them.
    #[test]
    fn several_categories_pick_all_their_files() {
        let cats = CategoryModel::defaults();
        let (video, docs) = (cats.of_name("a.mkv"), cats.of_name("a.txt"));
        let mut set = CatSet::of(video);
        set.insert(docs);
        let pick = Pick::Categories(set);
        assert!(cats.pick_matches(&pick, "x.mkv"));
        assert!(cats.pick_matches(&pick, "x.TXT"));
        assert!(!cats.pick_matches(&pick, "x.mp3"));
        assert!(cats.pick_in_category(&pick, docs));
        assert!(!cats.pick_in_category(&pick, cats.of_name("x.mp3")));
        assert_eq!(pick.categories(), Some(set));
        assert!(pick.has_category(video) && !pick.has_category(cats.other()));
        let label = cats.pick_label(&pick);
        assert!(label.contains(&cats.label(video)) && label.contains(&cats.label(docs)));
    }

    /// The quick path of `of_name` agrees with `ext_key`.
    #[test]
    fn of_name_matches_ext_key() {
        let m = CategoryModel::defaults();
        for name in [
            "a.MKV",
            "b.pdf",
            ".bashrc",
            "noext",
            "x.",
            "tar.gz",
            "Ü.JPG",
            "f.ÉML",
            "long.abcdefghijklmnopqrstuvwxyz",
            "a.b.Mp4",
            "",
            ".",
            "..",
            "...x",
            "a..",
            ".a.b",
            "x.1234567890123456",
            "x.12345678901234567",
            "ß.ẞ",
            "\\x01.Txt",
            "a.K\u{212a}",
            "a.İ",
            "日本.テキスト",
            "a b.C D",
        ] {
            assert_eq!(m.of_name(name), m.of_ext(&ext_key(name)), "{name}");
            assert_eq!(ext_key_in(name, &mut [0; 16]), ext_key(name), "{name}");
        }
    }

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
        assert_eq!(
            m.color_in(m.of_name("a.eml"), true, false),
            rgb(PALETTE[0].0)
        );
        assert_eq!(
            m.color_in(m.of_name("a.mkv"), true, false),
            rgb(PALETTE[1].0)
        );
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
        let rows = category_breakdown(&tree, &m, Measure::Bytes);
        let b: Vec<(String, u64, u64)> = rows
            .iter()
            .map(|r| (m.label(r.cat), r.size, r.files))
            .collect();
        assert_eq!(
            b,
            vec![
                ("(other)".into(), 999, 1),
                ("Documents".into(), 200, 1),
                ("Video".into(), 150, 2)
            ]
        );
        assert_eq!(
            rows[2].exts,
            vec![("mkv".to_string(), 100, 1), ("mp4".to_string(), 50, 1)]
        );
        assert_eq!(
            find_node(&tree, Path::new("/r/d")).map(|n| n.children.len()),
            Some(3)
        );
        assert!(find_node(&tree, Path::new("/r/x")).is_none());
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
        let only = filter_tree_by(
            &tree,
            &|n: &Node| m.of_name(&n.name) == video,
            Measure::Bytes,
        )
        .unwrap();
        assert_eq!((only.size, only.file_count), (105, 2));
        let names: Vec<&str> = only.children.iter().map(|c| &*c.name).collect();
        assert_eq!(names, vec!["a.mkv", "d"]);
    }

    /// The breakdown equals a plain one-thread count by `ext_key`, on names
    /// that test the extension rules (no dot, a leading dot, a trailing
    /// dot, several dots, upper case, non-ASCII, longer than 16 bytes) and
    /// on subfolders big enough to be added up in parallel.
    #[test]
    fn breakdown_matches_a_plain_count() {
        let names = [
            "noext",
            ".hidden",
            "trail.",
            "a.b.c.TXT",
            "x.MKV",
            "y.mkv",
            "z.Jpeg",
            "ñ.ÑOÑO",
            "émoji.😀",
            "long.abcdefghijklmnopqrstuvwxyz",
            "Upper.ABCDEFGHIJKLMNOPQ",
            "a.tar.gz",
            "..",
            "plain.pdf",
        ];
        let big = |dir: &str, n: usize| {
            let children: Vec<Node> = (0..n)
                .map(|i| {
                    let name = names[i % names.len()];
                    test_node(&format!("{dir}/{i}{name}"), (i % 977) as u64, false, vec![])
                })
                .collect();
            let mut d = test_node(dir, 0, true, children);
            d.size = d.children.iter().map(|c| c.size).sum();
            d.file_count = n as u64;
            d
        };
        let mut root = test_node(
            "/r",
            0,
            true,
            vec![
                big("/r/one", 25_000),
                big("/r/two", 30_000),
                big("/r/small", 300),
            ],
        );
        root.file_count = 55_300;
        let model = CategoryModel::defaults();
        let mut plain: HashMap<String, (u64, u64)> = HashMap::new();
        fn count(n: &Node, acc: &mut HashMap<String, (u64, u64)>) {
            if n.is_dir {
                n.children.iter().for_each(|c| count(c, acc));
            } else {
                let e = acc.entry(ext_key(&n.name)).or_insert((0, 0));
                e.0 += n.size;
                e.1 += n.file_count.max(1);
            }
        }
        count(&root, &mut plain);
        let want = category_rows(&plain.into_iter().collect(), &model, Measure::Bytes);
        assert_eq!(category_breakdown(&root, &model, Measure::Bytes), want);
        let mut small: HashMap<String, (u64, u64)> = HashMap::new();
        count(&root.children[2], &mut small);
        let want_small = category_rows(&small.into_iter().collect(), &model, Measure::Bytes);
        assert_eq!(
            category_breakdown(&root.children[2], &model, Measure::Bytes),
            want_small
        );
    }

    /// Every pair of colors in the color-blind-safe palette, and each color
    /// against Other's gray, stays clearly apart for normal vision and as
    /// seen with protanopia, deuteranopia and tritanopia (full strength,
    /// Machado 2009), on both backgrounds. Prints the closest pair.
    #[test]
    fn color_blind_palette_stays_apart() {
        type Matrix = [[f64; 3]; 3];
        const SEEN: [(&str, Matrix); 4] = [
            (
                "normal",
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            ),
            (
                "protanopia",
                [
                    [0.152286, 1.052583, -0.204868],
                    [0.114503, 0.786281, 0.099216],
                    [-0.003882, -0.048116, 1.051998],
                ],
            ),
            (
                "deuteranopia",
                [
                    [0.367322, 0.860646, -0.227968],
                    [0.280085, 0.672501, 0.047413],
                    [-0.011820, 0.042940, 0.968881],
                ],
            ),
            (
                "tritanopia",
                [
                    [1.255528, -0.076749, -0.178779],
                    [-0.078411, 0.930809, 0.147602],
                    [0.004733, 0.691367, 0.303900],
                ],
            ),
        ];
        let linear = |c: u8| {
            let v = f64::from(c) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        // CIELAB of a color as seen through `m`.
        let lab = |c: Color32, m: &Matrix| {
            let rgb = [linear(c.r()), linear(c.g()), linear(c.b())];
            let s: Vec<f64> = (0..3)
                .map(|i| {
                    (0..3)
                        .map(|j| m[i][j] * rgb[j])
                        .sum::<f64>()
                        .clamp(0.0, 1.0)
                })
                .collect();
            let xyz = [
                0.4124 * s[0] + 0.3576 * s[1] + 0.1805 * s[2],
                0.2126 * s[0] + 0.7152 * s[1] + 0.0722 * s[2],
                0.0193 * s[0] + 0.1192 * s[1] + 0.9505 * s[2],
            ];
            let white = [0.95047, 1.0, 1.08883];
            let f = |t: f64| {
                if t > 0.008856 {
                    t.cbrt()
                } else {
                    7.787 * t + 16.0 / 116.0
                }
            };
            let (x, y, z) = (
                f(xyz[0] / white[0]),
                f(xyz[1] / white[1]),
                f(xyz[2] / white[2]),
            );
            [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
        };
        let m = CategoryModel::defaults();
        let n = m.other().0;
        assert!(
            n <= SAFE_PALETTE.len(),
            "every built-in category has a safe color"
        );
        for dark in [true, false] {
            let colors: Vec<Color32> = (0..=n)
                .map(|i| m.color_in(Category(i), dark, true))
                .collect();
            let mut closest = (f64::MAX, String::new());
            for (name, matrix) in &SEEN {
                for a in 0..colors.len() {
                    for b in a + 1..colors.len() {
                        let (p, q) = (lab(colors[a], matrix), lab(colors[b], matrix));
                        let d =
                            ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2))
                                .sqrt();
                        if d < closest.0 {
                            closest = (d, format!("{name}: {a} vs {b}"));
                        }
                    }
                }
            }
            eprintln!("dark {dark}: closest pair {:.1} ({})", closest.0, closest.1);
            assert!(
                closest.0 >= 8.0,
                "dark {dark}: {} only {:.1} apart",
                closest.1,
                closest.0
            );
        }
    }
}
