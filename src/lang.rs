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
}

impl Lang {
    /// Loads language `code`: English, then the built-in file for `code`,
    /// then the user's file for it, each overriding the one before.
    pub(crate) fn load(code: &str) -> Lang {
        let mut map = parse_kv_file(built_in("en").unwrap_or_default());
        if code != "en"
            && let Some(text) = built_in(code)
        {
            map.extend(parse_kv_file(text));
        }
        if let Ok(text) = std::fs::read_to_string(lang_dir().join(format!("{code}.lang"))) {
            map.extend(parse_kv_file(&text));
        }
        Lang {
            code: code.to_string(),
            map,
        }
    }

    /// The string for `key`, or the key itself if there is none.
    pub(crate) fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.map.get(key).map(|s| s.as_str()).unwrap_or(key)
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
            let name = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| display_name(&text))
                .unwrap_or_else(|| code.clone());
            out.push((code, name));
        }
    }
    out
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every built-in language has a name and exactly English's keys, each
    /// with as many `%s` placeholders as in English.
    #[test]
    fn built_in_languages_match_english() {
        let en = parse_kv_file(built_in("en").unwrap());
        for (code, text) in BUILT_IN {
            assert!(
                display_name(text).is_some(),
                "{code}: no '# name:' first line"
            );
            let map = parse_kv_file(text);
            let missing: Vec<&String> = en.keys().filter(|k| !map.contains_key(*k)).collect();
            let extra: Vec<&String> = map.keys().filter(|k| !en.contains_key(*k)).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{code}: missing {missing:?}, extra {extra:?}"
            );
            for (key, value) in &map {
                assert_eq!(
                    value.matches("%s").count(),
                    en[key].matches("%s").count(),
                    "{code}: {key}"
                );
                assert!(!value.trim().is_empty(), "{code}: {key} is empty");
            }
        }
    }
}
