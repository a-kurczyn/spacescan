//! Translations. Every user-facing string is a `KEY=value` line in a
//! language file: English (lang/en.lang) is built in, other languages are
//! `<code>.lang` files in ~/.config/spacemap/lang/. Values may contain `%s`
//! placeholders, filled in order by `trf`.

use super::*;

/// The built-in English strings, used for any key a translation lacks.
pub(crate) static DEFAULT_LANG: &str = include_str!("../lang/en.lang");

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
    /// Loads language `code` on top of English, so keys it lacks stay
    /// English.
    pub(crate) fn load(code: &str) -> Lang {
        let mut map = parse_kv_file(DEFAULT_LANG);
        if code != "en" {
            if let Ok(text) = std::fs::read_to_string(lang_dir().join(format!("{code}.lang"))) {
                map.extend(parse_kv_file(&text));
            }
        }
        Lang { code: code.to_string(), map }
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

/// The active language.
pub(crate) static LANG: LazyLock<RwLock<Lang>> = LazyLock::new(|| RwLock::new(Lang::load(&config::language_setting())));

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

/// (code, display name) of English and of every `<code>.lang` file in the
/// language folder. The display name is the file's `# name:` first line.
pub(crate) fn available_languages() -> Vec<(String, String)> {
    let mut out = vec![("en".to_string(), "English".to_string())];
    if let Ok(rd) = std::fs::read_dir(lang_dir()) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "lang") {
                continue;
            }
            let Some(code) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else { continue };
            if code == "en" {
                continue;
            }
            let name = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| text.lines().next().map(str::to_string))
                .and_then(|first| first.strip_prefix("# name:").map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| code.clone());
            out.push((code, name));
        }
    }
    out
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}
