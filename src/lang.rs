//! Translations: every user-facing string comes from a lang/*.lang file.

use super::*;

// ---------------- Localization ----------------
//
// Every user-facing string lives in a `KEY=value` text file (lang/en.lang,
// bundled into the binary as the guaranteed fallback) instead of as Rust
// string literals, so a translation is just a text file: copy en.lang,
// translate the right-hand side, drop it in ~/.config/spacemap/lang/ as
// <code>.lang — no rebuild. A key missing from a translation falls back
// to English automatically. Where a template needs a runtime value, it
// contains a literal `%s` (printf-style, filled in order — see `Lang::t`);
// a translation can move `%s` elsewhere in the sentence but shouldn't
// remove, duplicate or relabel it.

pub(crate) static DEFAULT_LANG: &str = include_str!("../lang/en.lang");

pub(crate) fn config_dir() -> PathBuf {
    home_dir().join(".config/spacemap")
}

pub(crate) fn lang_dir() -> PathBuf {
    config_dir().join("lang")
}

/// Parses `KEY=value` text: blank lines and lines starting with `#` are
/// skipped. `\n` and `\\` in a value are unescaped, so a translation can
/// still contain a literal newline (e.g. a multi-line tooltip).
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

pub(crate) struct Lang {
    pub(crate) code: String,
    pub(crate) map: HashMap<String, String>,
}

impl Lang {
    /// Loads `code` with English merged in underneath it, so an
    /// incomplete translation still shows English for whatever key it's
    /// missing rather than a blank. "en" itself is just the bundled
    /// default, with nothing to merge.
    pub(crate) fn load(code: &str) -> Lang {
        let mut map = parse_kv_file(DEFAULT_LANG);
        if code != "en" {
            if let Ok(text) = std::fs::read_to_string(lang_dir().join(format!("{code}.lang"))) {
                map.extend(parse_kv_file(&text));
            }
        }
        Lang { code: code.to_string(), map }
    }

    /// Raw lookup; the key itself if even the English default doesn't
    /// have it, so a missing translation reads as an obviously-wrong key
    /// rather than silently vanishing.
    pub(crate) fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.map.get(key).map(|s| s.as_str()).unwrap_or(key)
    }

    /// Like `get`, but every `%s` in the template is replaced in order by
    /// one of `args`.
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

pub(crate) static LANG: LazyLock<RwLock<Lang>> = LazyLock::new(|| RwLock::new(Lang::load(&config::language_setting())));

/// A plain translated string with no placeholders.
pub(crate) fn tr(key: &str) -> String {
    LANG.read().unwrap().get(key).to_string()
}

/// A translated string with `%s` placeholders filled in order.
pub(crate) fn trf(key: &str, args: &[&str]) -> String {
    LANG.read().unwrap().t(key, args)
}

pub(crate) fn current_lang_code() -> String {
    LANG.read().unwrap().code.clone()
}

/// Switches the active language for every `tr`/`trf` call from the next
/// frame on (egui redraws continuously, so nothing else needs to react to
/// this explicitly) and remembers the choice for next launch.
pub(crate) fn set_language(code: &str) {
    *LANG.write().unwrap() = Lang::load(code);
}

/// Every `<code>.lang` file in the user's lang directory, plus the bundled
/// "en" — (code, display name for the dropdown). Scanned fresh each time
/// the settings panel is open, so a file dropped in while spacemap is
/// running shows up without a restart.
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
