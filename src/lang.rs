//! Translations. Every user-facing string is a `KEY=value` line in a
//! language file (lang/<code>.lang, built into the binary). English is the
//! base: a key missing from a language shows in English. A file
//! ~/.config/spacemap/lang/<code>.lang overrides lines of a built-in
//! language, or adds a new language. Values may contain `%s` placeholders,
//! filled in order by `trf`.

use super::*;

/// The languages built into the binary: (code, file contents), in the order
/// the settings list shows them. English comes first and is the base.
const BUILT_IN: &[(&str, &str)] = &[
    ("en", include_str!("../lang/en.lang")),
    ("es", include_str!("../lang/es.lang")),
    ("fr", include_str!("../lang/fr.lang")),
    ("de", include_str!("../lang/de.lang")),
    ("it", include_str!("../lang/it.lang")),
    ("pt", include_str!("../lang/pt.lang")),
    ("ru", include_str!("../lang/ru.lang")),
    ("ja", include_str!("../lang/ja.lang")),
    ("zh", include_str!("../lang/zh.lang")),
    ("ko", include_str!("../lang/ko.lang")),
];

/// The built-in file for language `code`, if there is one.
fn built_in(code: &str) -> Option<&'static str> {
    BUILT_IN
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, text)| *text)
}

/// A language file's display name: its `# name:` first line.
fn display_name(text: &str) -> Option<String> {
    let name = text.lines().next()?.strip_prefix("# name:")?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

pub(crate) fn config_dir() -> PathBuf {
    home_dir().join(".config/spacemap")
}

pub(crate) fn lang_dir() -> PathBuf {
    config_dir().join("lang")
}

/// Parses `KEY=value` lines, skipping blank lines and `#` comments.
/// `\n` in a value is a newline and `\\` a backslash.
pub(crate) fn parse_kv_file(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim_start().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let val = val.replace("\\n", "\n").replace("\\\\", "\\");
            map.insert(key.trim().to_string(), val);
        }
    }
    map
}

/// A loaded language: its code and its strings.
pub(crate) struct Lang {
    pub(crate) code: String,
    pub(crate) map: HashMap<String, String>,
    /// Keys given by the language itself (not filled in from English).
    own: HashSet<String>,
}

impl Lang {
    /// Loads language `code`: English, then the built-in file for `code`,
    /// then the user's file for it, each overriding the one before.
    pub(crate) fn load(code: &str) -> Lang {
        let mut map = parse_kv_file(built_in("en").unwrap_or_default());
        let mut own = HashSet::new();
        let mut add = |lines: HashMap<String, String>| {
            own.extend(lines.keys().cloned());
            map.extend(lines);
        };
        if code != "en"
            && let Some(text) = built_in(code)
        {
            add(parse_kv_file(text));
        }
        if let Some(text) = read_small_file(&lang_dir().join(format!("{code}.lang"))) {
            add(parse_kv_file(&text));
        }
        Lang {
            code: code.to_string(),
            map,
            own,
        }
    }

    /// The string for `key`, or the key itself if there is none.
    pub(crate) fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.map.get(key).map(|s| s.as_str()).unwrap_or(key)
    }

    /// Like `t`, for a phrase about `n` things: uses the language's form for
    /// that number (`KEY_ONE`, `KEY_FEW`) when it has one, else `KEY`.
    pub(crate) fn tn(&self, key: &str, n: u64, args: &[&str]) -> String {
        // A form only counts if it comes from the same language as `key`, so
        // an English "1 file" never shows inside another language.
        let same_source = |k: &str| self.own.contains(k) == self.own.contains(key);
        match plural_form(&self.code, n).map(|f| format!("{key}_{f}")) {
            Some(k) if self.map.contains_key(&k) && same_source(&k) => self.t(&k, args),
            _ => self.t(key, args),
        }
    }

    /// The string for `key` with each `%s` replaced by the next of `args`.
    pub(crate) fn t(&self, key: &str, args: &[&str]) -> String {
        let mut out = String::new();
        let mut rest = self.get(key);
        let mut args = args.iter();
        while let Some(pos) = rest.find("%s") {
            out.push_str(&rest[..pos]);
            out.push_str(args.next().copied().unwrap_or("%s"));
            rest = &rest[pos + 2..];
        }
        out.push_str(rest);
        out
    }
}

/// The active language (English in tests, which don't read the user's
/// settings).
pub(crate) static LANG: LazyLock<RwLock<Lang>> = LazyLock::new(|| {
    let code = if cfg!(test) {
        "en".to_string()
    } else {
        config::language_setting()
    };
    RwLock::new(Lang::load(&code))
});

/// The translated string for `key`.
pub(crate) fn tr(key: &str) -> String {
    LANG.read().unwrap().get(key).to_string()
}

/// The translated string for `key` with its `%s` placeholders filled in.
pub(crate) fn trf(key: &str, args: &[&str]) -> String {
    LANG.read().unwrap().t(key, args)
}

/// Like `trf`, for a phrase about `n` things (see `Lang::tn`).
pub(crate) fn trn(key: &str, n: u64, args: &[&str]) -> String {
    LANG.read().unwrap().tn(key, n, args)
}

/// Which special plural form language `code` uses for the number `n`, if
/// any ("ONE" for 1 in English, "FEW" for 2–4 in Russian...).
fn plural_form(code: &str, n: u64) -> Option<&'static str> {
    match code {
        "ja" | "zh" | "ko" => None,
        "fr" => (n <= 1).then_some("ONE"),
        "ru" => {
            let (last, last2) = (n % 10, n % 100);
            if last == 1 && last2 != 11 {
                Some("ONE")
            } else if (2..=4).contains(&last) && !(12..=14).contains(&last2) {
                Some("FEW")
            } else {
                None
            }
        }
        _ => (n == 1).then_some("ONE"),
    }
}

pub(crate) fn current_lang_code() -> String {
    LANG.read().unwrap().code.clone()
}

/// Makes `code` the active language; the UI shows it from the next frame.
pub(crate) fn set_language(code: &str) {
    *LANG.write().unwrap() = Lang::load(code);
}

/// (code, display name) of the built-in languages, then of any other
/// `<code>.lang` file in the user's language folder.
pub(crate) fn available_languages() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = BUILT_IN
        .iter()
        .map(|(code, text)| {
            (
                code.to_string(),
                display_name(text).unwrap_or_else(|| code.to_string()),
            )
        })
        .collect();
    if let Ok(rd) = std::fs::read_dir(lang_dir()) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "lang") {
                continue;
            }
            let Some(code) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            if built_in(&code).is_some() {
                continue;
            }
            // Files that can't be read (not regular, too big) aren't offered.
            let Some(text) = read_small_file(&path) else {
                continue;
            };
            let name = display_name(&text).unwrap_or_else(|| code.clone());
            out.push((code, name));
        }
    }
    out
}

/// Why the user's file for language `code` can't be used, for the Issues
/// log; None if it's fine or there is none.
pub(crate) fn lang_file_problem(code: &str) -> Option<String> {
    let path = lang_dir().join(format!("{code}.lang"));
    let meta = std::fs::metadata(&path).ok()?;
    let why = if !meta.is_file() {
        tr("ERR_SETTINGS_NOT_FILE")
    } else if meta.len() > 1 << 20 {
        tr("ERR_SETTINGS_TOO_BIG")
    } else {
        return None;
    };
    Some(trf("ERR_LANG_FILE", &[&show_path(&path), &why]))
}

/// The text of a user file, if it's a regular file of at most 1 MiB. A
/// FIFO, a device (a link to /dev/zero) or a huge file is never read: it
/// could block or fill memory.
pub(crate) fn read_small_file(path: &Path) -> Option<String> {
    use std::io::Read;
    const MAX: u64 = 1 << 20;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIFOs, devices and huge files are skipped at once instead of being
    /// read.
    #[test]
    fn only_small_regular_files_are_read() {
        let dir = std::env::temp_dir().join(format!("spacemap-langfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("fifo.lang");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `c` is a valid C string that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        std::os::unix::fs::symlink("/dev/zero", dir.join("zero.lang")).unwrap();
        std::fs::write(dir.join("big.lang"), vec![b'#'; (1 << 20) + 1]).unwrap();
        std::fs::write(dir.join("ok.lang"), "# name: Test\nA=b\n").unwrap();
        let start = Instant::now();
        assert!(read_small_file(&fifo).is_none());
        assert!(read_small_file(&dir.join("zero.lang")).is_none());
        assert!(read_small_file(&dir.join("big.lang")).is_none());
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(
            read_small_file(&dir.join("ok.lang")).as_deref(),
            Some("# name: Test\nA=b\n")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every built-in language has a name and exactly English's keys (plural
    /// forms aside: each language has its own), each with as many `%s`
    /// placeholders as in English.
    #[test]
    fn built_in_languages_match_english() {
        let en = parse_kv_file(built_in("en").unwrap());
        for (code, text) in BUILT_IN {
            assert!(
                display_name(text).is_some(),
                "{code}: no '# name:' first line"
            );
            let map = parse_kv_file(text);
            let form = |k: &str| k.ends_with("_ONE") || k.ends_with("_FEW");
            let missing: Vec<&String> = en
                .keys()
                .filter(|k| !form(k) && !map.contains_key(*k))
                .collect();
            let extra: Vec<&String> = map
                .keys()
                .filter(|k| !form(k) && !en.contains_key(*k))
                .collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{code}: missing {missing:?}, extra {extra:?}"
            );
            for (key, value) in &map {
                let base = key.trim_end_matches("_ONE").trim_end_matches("_FEW");
                assert!(en.contains_key(base), "{code}: {key} has no base phrase");
                assert_eq!(
                    value.matches("%s").count(),
                    en[base].matches("%s").count(),
                    "{code}: {key}"
                );
                assert!(!value.trim().is_empty(), "{code}: {key} is empty");
            }
        }
    }

    fn lang(code: &str, own: &[(&str, &str)]) -> Lang {
        let mut lang = Lang::load("en");
        lang.code = code.into();
        for (k, v) in own {
            lang.map.insert(k.to_string(), v.to_string());
            lang.own.insert(k.to_string());
        }
        lang
    }

    /// Each language picks the right form at its edges: Russian 1, 2–4, 11–14,
    /// 21, 111, 0; French 0 and 1; none in Japanese; English 1 vs 0 and 2.
    #[test]
    fn plural_forms_at_their_edges() {
        let ru = lang(
            "ru",
            &[("N", "%s many"), ("N_ONE", "%s one"), ("N_FEW", "%s few")],
        );
        for (n, form) in [
            (0, "many"),
            (1, "one"),
            (2, "few"),
            (4, "few"),
            (5, "many"),
            (11, "many"),
            (12, "many"),
            (14, "many"),
            (21, "one"),
            (22, "few"),
            (111, "many"),
            (112, "many"),
            (1001, "one"),
            (u64::MAX, "many"),
        ] {
            assert_eq!(ru.tn("N", n, &[&n.to_string()]), format!("{n} {form}"));
        }
        let fr = lang("fr", &[("N", "%s pl"), ("N_ONE", "%s sg")]);
        assert_eq!(fr.tn("N", 0, &["0"]), "0 sg");
        assert_eq!(fr.tn("N", 1, &["1"]), "1 sg");
        assert_eq!(fr.tn("N", 2, &["2"]), "2 pl");
        let ja = lang("ja", &[("N", "%s x"), ("N_ONE", "%s never")]);
        assert_eq!(ja.tn("N", 1, &["1"]), "1 x");
        let en = Lang::load("en");
        assert_eq!(en.tn("COUNT_FILES", 1, &["1"]), "1 file");
        assert_eq!(en.tn("COUNT_FILES", 0, &["0"]), "0 files");
        assert_eq!(en.tn("COUNT_FILES", 2, &["2"]), "2 files");
    }

    /// A language that translates a phrase but not its "one" form keeps its
    /// own phrase for 1 (never the English singular); a language missing
    /// both falls back to English for both.
    #[test]
    fn plural_forms_never_mix_languages() {
        let es = lang("es", &[("COUNT_FILES", "%s archivos")]);
        assert_eq!(es.tn("COUNT_FILES", 1, &["1"]), "1 archivos");
        let xx = lang("xx", &[]);
        assert_eq!(xx.tn("COUNT_FILES", 1, &["1"]), "1 file");
        assert_eq!(xx.tn("COUNT_FILES", 3, &["3"]), "3 files");
        assert_eq!(xx.tn("NO_SUCH_KEY", 1, &["1"]), "NO_SUCH_KEY");
    }
}
