//! File categories (Video, Audio, Documents, …) by extension: the colored
//! bar beside the contents table, which filters it to one category.

use super::*;

/// High-level kind of a file, from its extension. Eight categories plus
/// "Other" (unknown or no extension): more colors than eight stop being
/// told apart reliably, so anything rarer folds into Other.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Category {
    Video,
    Audio,
    Images,
    Documents,
    Archives,
    Applications,
    Code,
    Data,
    Other,
}

pub(crate) const ALL_CATEGORIES: [Category; 9] = [
    Category::Video,
    Category::Audio,
    Category::Images,
    Category::Documents,
    Category::Archives,
    Category::Applications,
    Category::Code,
    Category::Data,
    Category::Other,
];

// Built in, so categories are the same on every system (no MIME database
// needed). Grouped like the freedesktop.org generic icons that file
// managers use. Subtitles count as Video: they belong to the videos.
const VIDEO: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "wmv", "flv", "webm", "mpg", "mpeg", "m2ts", "mts", "ts", "vob", "ogv", "3gp",
    "divx", "rm", "rmvb", "asf", "f4v", "m2v", "srt", "ass", "ssa", "sub", "idx", "vtt", "sup",
];
const AUDIO: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "oga", "opus", "m4a", "m4b", "aac", "wma", "aiff", "aif", "alac", "ape", "dsf", "dff",
    "mka", "mid", "midi", "wv", "ac3", "dts", "amr", "au", "cue", "m3u", "m3u8", "pls",
];
const IMAGES: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "avif", "jxl", "svg", "svgz",
    "ico", "icns", "psd", "xcf", "kra", "raw", "cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "raf", "srw", "pef",
    "exr", "hdr", "tga",
];
const DOCUMENTS: &[&str] = &[
    "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "epub", "mobi", "azw", "azw3", "djvu", "fb2", "cbz", "cbr", "xls",
    "xlsx", "ods", "csv", "tsv", "ppt", "pptx", "odp", "odg", "tex", "nfo", "pages", "numbers", "key", "ps", "eps",
    "xps", "oxps", "org", "rst",
];
const ARCHIVES: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst", "lz", "lz4", "lzma", "z", "cab", "arj",
    "iso", "img", "dmg", "cpio", "par2",
];
const APPLICATIONS: &[&str] = &[
    "exe", "msi", "appimage", "deb", "rpm", "apk", "flatpak", "flatpakref", "snap", "dll", "so", "run", "jar", "pkg",
    "app", "bat", "cmd", "com", "elf", "ko", "efi", "sys", "drv", "xpi", "crx",
];
const CODE: &[&str] = &[
    "rs", "py", "js", "mjs", "cjs", "jsx", "tsx", "c", "h", "cpp", "hpp", "cc", "hh", "cxx", "java", "kt", "kts", "go",
    "rb", "php", "sh", "bash", "zsh", "fish", "pl", "pm", "lua", "cs", "swift", "scala", "hs", "ml", "r", "m", "dart",
    "vue", "svelte", "html", "htm", "css", "scss", "sass", "less", "json", "yaml", "yml", "toml", "xml", "ini", "cfg",
    "conf", "sql", "ipynb", "cmake", "mk", "gradle", "patch", "diff", "o", "a", "class", "pyc", "wasm", "rlib", "rmeta",
];
const DATA: &[&str] = &[
    "db", "sqlite", "sqlite3", "mdb", "accdb", "log", "bak", "old", "tmp", "temp", "cache", "lock", "dat", "bin",
    "pak", "qcow2", "vdi", "vmdk", "vhd", "vhdx", "swp", "dump", "parquet", "pack", "torrent", "part", "crdownload",
    "ldb", "sqlite-shm", "sqlite-wal", "db-wal", "db-shm", "jsonlz4", "mozlz4", "baklz4", "idx2",
];

static BY_EXTENSION: LazyLock<HashMap<&'static str, Category>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for (list, cat) in [
        (VIDEO, Category::Video),
        (AUDIO, Category::Audio),
        (IMAGES, Category::Images),
        (DOCUMENTS, Category::Documents),
        (ARCHIVES, Category::Archives),
        (APPLICATIONS, Category::Applications),
        (CODE, Category::Code),
        (DATA, Category::Data),
    ] {
        for ext in list {
            m.insert(*ext, cat);
        }
    }
    m
});

impl Category {
    /// The category of a file called `name`, by its extension (any case).
    pub(crate) fn of_name(name: &str) -> Category {
        let Some(ext) = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()) else {
            return Category::Other;
        };
        BY_EXTENSION.get(ext.as_str()).copied().unwrap_or(Category::Other)
    }

    pub(crate) fn label(self) -> String {
        tr(match self {
            Category::Video => "CAT_VIDEO",
            Category::Audio => "CAT_AUDIO",
            Category::Images => "CAT_IMAGES",
            Category::Documents => "CAT_DOCUMENTS",
            Category::Archives => "CAT_ARCHIVES",
            Category::Applications => "CAT_APPLICATIONS",
            Category::Code => "CAT_CODE",
            Category::Data => "CAT_DATA",
            Category::Other => "CAT_OTHER",
        })
    }

    /// Fixed color per category (the same whatever the folder), from a
    /// color-blind-checked categorical palette, stepped for dark or light
    /// backgrounds. Other is a neutral gray.
    pub(crate) fn color(self, dark: bool) -> Color32 {
        let hex: u32 = match (self, dark) {
            (Category::Video, true) => 0x3987e5,
            (Category::Video, false) => 0x2a78d6,
            (Category::Audio, true) => 0xd95926,
            (Category::Audio, false) => 0xeb6834,
            (Category::Images, true) => 0x199e70,
            (Category::Images, false) => 0x1baf7a,
            (Category::Documents, true) => 0xc98500,
            (Category::Documents, false) => 0xeda100,
            (Category::Archives, true) => 0xd55181,
            (Category::Archives, false) => 0xe87ba4,
            (Category::Applications, _) => 0x008300,
            (Category::Code, true) => 0x9085e9,
            (Category::Code, false) => 0x4a3aa7,
            (Category::Data, true) => 0xe66767,
            (Category::Data, false) => 0xe34948,
            (Category::Other, true) => 0x6f6e69,
            (Category::Other, false) => 0xa8a7a2,
        };
        Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
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

/// How the files under `node` split into categories, largest first with
/// Other last; categories with no files are left out.
pub(crate) fn category_breakdown(node: &Node) -> Vec<CategoryRow> {
    fn walk(n: &Node, acc: &mut HashMap<String, (u64, u64)>) {
        if n.is_dir {
            for c in &n.children {
                deep(|| walk(c, acc));
            }
        } else {
            let ext = Path::new(&n.name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let e = acc.entry(ext).or_insert((0, 0));
            e.0 = e.0.saturating_add(n.size);
            e.1 += n.file_count.max(1);
        }
    }
    let mut by_ext = HashMap::new();
    walk(node, &mut by_ext);
    let mut rows: Vec<CategoryRow> = Vec::new();
    for (ext, (size, files)) in by_ext {
        let cat = BY_EXTENSION.get(ext.as_str()).copied().unwrap_or(Category::Other);
        let row = match rows.iter_mut().find(|r| r.cat == cat) {
            Some(r) => r,
            None => {
                rows.push(CategoryRow { cat, size: 0, files: 0, exts: Vec::new() });
                rows.last_mut().unwrap()
            }
        };
        row.size = row.size.saturating_add(size);
        row.files += files;
        row.exts.push((ext, size, files));
    }
    for r in &mut rows {
        r.exts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    }
    // Other always last; the rest by size, ties in the fixed order.
    rows.sort_by_key(|r| (r.cat == Category::Other, std::cmp::Reverse(r.size), ALL_CATEGORIES.iter().position(|x| *x == r.cat)));
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

    #[test]
    fn extensions_map_to_categories() {
        assert_eq!(Category::of_name("Star Wars (1977).mkv"), Category::Video);
        assert_eq!(Category::of_name("Movie.EN.SRT"), Category::Video);
        assert_eq!(Category::of_name("song.flac"), Category::Audio);
        assert_eq!(Category::of_name("IMG_0001.JPG"), Category::Images);
        assert_eq!(Category::of_name("report.pdf"), Category::Documents);
        assert_eq!(Category::of_name("backup.tar.gz"), Category::Archives);
        assert_eq!(Category::of_name("tool.AppImage"), Category::Applications);
        assert_eq!(Category::of_name("main.rs"), Category::Code);
        assert_eq!(Category::of_name("places.sqlite"), Category::Data);
        assert_eq!(Category::of_name("Makefile"), Category::Other);
        assert_eq!(Category::of_name(".bashrc"), Category::Other);
        assert_eq!(Category::of_name("weird.xyz123"), Category::Other);
    }

    #[test]
    fn no_extension_is_listed_twice() {
        let mut seen = HashSet::new();
        for list in [VIDEO, AUDIO, IMAGES, DOCUMENTS, ARCHIVES, APPLICATIONS, CODE, DATA] {
            for ext in list {
                assert!(seen.insert(*ext), "{ext} is in two categories");
                assert_eq!(*ext, ext.to_lowercase());
            }
        }
    }

    #[test]
    fn breakdown_sums_and_orders() {
        let tree = test_node("/r", 0, true, vec![
            test_node("/r/a.mkv", 100, false, vec![]),
            test_node("/r/d", 0, true, vec![
                test_node("/r/d/b.mp4", 50, false, vec![]),
                test_node("/r/d/c.pdf", 200, false, vec![]),
                test_node("/r/d/README", 999, false, vec![]),
            ]),
        ]);
        let b: Vec<(Category, u64, u64)> = category_breakdown(&tree).iter().map(|r| (r.cat, r.size, r.files)).collect();
        assert_eq!(b, vec![(Category::Documents, 200, 1), (Category::Video, 150, 2), (Category::Other, 999, 1)]);
        let video = &category_breakdown(&tree)[1];
        assert_eq!(video.exts, vec![("mkv".to_string(), 100, 1), ("mp4".to_string(), 50, 1)]);
        assert_eq!(find_by_path(&tree, Path::new("/r/d")).map(|n| n.children.len()), Some(3));
        assert!(find_by_path(&tree, Path::new("/r/x")).is_none());
    }

    #[test]
    fn category_filter_keeps_only_its_files() {
        let tree = test_node("/r", 0, true, vec![
            test_node("/r/a.mkv", 100, false, vec![]),
            test_node("/r/docs", 0, true, vec![test_node("/r/docs/c.pdf", 200, false, vec![])]),
            test_node("/r/d", 0, true, vec![
                test_node("/r/d/b.srt", 5, false, vec![]),
                test_node("/r/d/n.txt", 7, false, vec![]),
            ]),
        ]);
        let video = filter_tree_by(&tree, &|n: &Node| Category::of_name(&n.name) == Category::Video).unwrap();
        assert_eq!((video.size, video.file_count), (105, 2));
        let names: Vec<&str> = video.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["a.mkv", "d"]);
        assert_eq!(video.children[1].children.len(), 1);
    }
}
