//! Everything spacemap remembers between launches — language, chart
//! settings and the Summary table's layout — in one JSON file,
//! ~/.config/spacemap/settings.json. The app compares its state against the
//! last saved copy once per frame and rewrites the file when it changes
//! (see `DiskScanApp::save_config_if_changed`).

use super::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use table::TableCol;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub(crate) struct Config {
    pub language: String,
    pub chart: Settings,
    pub table: TablePrefs,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "en".to_string(), chart: Settings::default(), table: TablePrefs::default() }
    }
}

/// The Summary table's layout.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub(crate) struct TablePrefs {
    pub sort: SortColumn,
    pub ascending: bool,
    pub hidden_columns: Vec<TableCol>,
    /// Left-to-right order of the optional columns (Name is always last).
    pub column_order: Vec<TableCol>,
    pub dirs_first: bool,
    /// "By file extension" beside the contents table instead of below.
    pub ext_beside: bool,
}

impl Default for TablePrefs {
    fn default() -> Self {
        TablePrefs {
            sort: SortColumn::Size,
            ascending: false,
            hidden_columns: Vec::new(),
            column_order: TableCol::ALL.to_vec(),
            dirs_first: false,
            ext_beside: false,
        }
    }
}

fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

/// Settings files bigger than this aren't read (a mistake, or a symlink to
/// something like /dev/zero).
const MAX_SETTINGS_BYTES: u64 = 1 << 20;

/// Why settings.json wasn't read.
enum Unreadable {
    NotAFile,
    TooBig,
    Io(std::io::Error),
}

impl Unreadable {
    /// The Issues-log line. Not built inside `read_settings`: that also runs
    /// while the translations themselves are loading (`language_setting`),
    /// where looking one up would deadlock.
    fn message(&self) -> String {
        let why = match self {
            Unreadable::NotAFile => tr("ERR_SETTINGS_NOT_FILE"),
            Unreadable::TooBig => tr("ERR_SETTINGS_TOO_BIG"),
            Unreadable::Io(e) => e.to_string(),
        };
        trf("ERR_SETTINGS_UNREADABLE", &[&show_path(&settings_file()), &why])
    }
}

/// settings.json's text: None if there's no file; Err if it's something
/// that shouldn't be read (not a regular file, or far too big).
fn read_settings() -> Result<Option<String>, Unreadable> {
    let path = settings_file();
    match std::fs::metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Unreadable::Io(e)),
        Ok(m) if !m.is_file() => Err(Unreadable::NotAFile),
        Ok(m) if m.len() > MAX_SETTINGS_BYTES => Err(Unreadable::TooBig),
        Ok(_) => std::fs::read_to_string(&path).map(Some).map_err(Unreadable::Io),
    }
}

/// `file` (a JSON object) read into `T` one field at a time, starting from
/// `default`: a field whose value doesn't fit keeps its default and is
/// listed in `dropped` as "section.field". For a list, only the entries that
/// don't fit are dropped.
fn lenient<T: Serialize + DeserializeOwned>(default: T, file: Option<&Value>, section: &str, dropped: &mut Vec<String>) -> T {
    let Some(file) = file else { return default };
    let Value::Object(fields) = file else {
        dropped.push(section.to_string());
        return default;
    };
    let Ok(mut merged) = serde_json::to_value(&default) else { return default };
    let fits = |v: &Value| serde_json::from_value::<T>(v.clone()).is_ok();
    for (key, value) in fields {
        let mut trial = merged.clone();
        trial[key] = value.clone();
        if fits(&trial) {
            merged = trial;
            continue;
        }
        if let Value::Array(items) = value {
            let keep: Vec<Value> = items
                .iter()
                .filter(|item| {
                    let mut one = merged.clone();
                    one[key] = Value::Array(vec![(*item).clone()]);
                    fits(&one)
                })
                .cloned()
                .collect();
            trial[key] = Value::Array(keep);
            if fits(&trial) {
                merged = trial;
            }
        }
        dropped.push(format!("{section}.{key}"));
    }
    serde_json::from_value(merged).unwrap_or(default)
}

impl Config {
    /// The saved configuration, or defaults, plus whether the file needs
    /// (re)writing — it's missing (first launch), or some values had to be
    /// corrected or dropped — and a problem for the Issues log, if any.
    /// Values that can't be used fall back one by one (the rest is kept);
    /// the original file is copied to settings.json.bad first. A file that
    /// isn't JSON at all is moved there instead.
    pub(crate) fn load() -> (Config, bool, Option<String>) {
        let path = settings_file();
        let bad = path.with_extension("json.bad");
        let text = match read_settings() {
            Ok(Some(text)) => text,
            Ok(None) => return (Config::default(), true, None),
            // Not something to overwrite either: leave it and don't save.
            Err(why) => return (Config::default(), false, Some(why.message())),
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                let _ = std::fs::rename(&path, &bad);
                let problem = trf("ERR_SETTINGS_FILE", &[&show_path(&bad), &e.to_string()]);
                return (Config::default(), true, Some(problem));
            }
        };
        let mut dropped = Vec::new();
        let defaults = Config::default();
        let language = match value.get("language") {
            None => defaults.language,
            Some(Value::String(l)) => l.clone(),
            Some(_) => {
                dropped.push("language".to_string());
                defaults.language
            }
        };
        let chart = lenient(defaults.chart, value.get("chart"), "chart", &mut dropped);
        let table = lenient(defaults.table, value.get("table"), "table", &mut dropped);
        let sanitized = chart.clone().sanitized();
        let corrected = sanitized != chart;
        let cfg = Config { language, chart: sanitized, table };
        if dropped.is_empty() {
            return (cfg, corrected, None);
        }
        let _ = std::fs::copy(&path, &bad);
        let problem = trf("ERR_SETTINGS_FIELDS", &[&dropped.join(", "), &show_path(&bad)]);
        (cfg, true, Some(problem))
    }

    /// Writes the file via a temporary file, so a crash mid-write can't leave
    /// it half-written. If settings.json is a symlink (dotfiles managers),
    /// the file it points to is updated instead of being replaced, keeping
    /// its permissions.
    pub(crate) fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir())?;
        let link = settings_file();
        let target = match std::fs::symlink_metadata(&link) {
            Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(&link)?,
            _ => link,
        };
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = target.with_file_name(format!(".{name}.tmp"));
        let write = || -> std::io::Result<()> {
            std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)?;
            if let Ok(m) = std::fs::metadata(&target) {
                std::fs::set_permissions(&tmp, m.permissions())?;
            }
            std::fs::rename(&tmp, &target)
        };
        let result = write();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

impl DiskScanApp {
    /// The configuration as it stands right now.
    fn current_config(&self) -> Config {
        Config { language: current_lang_code(), chart: self.settings.clone(), table: self.table_prefs() }
    }

    /// Once per frame: saves the configuration if anything in it changed
    /// (or it has never been saved, see `Config::load`).
    pub(crate) fn save_config_if_changed(&mut self) {
        let cfg = self.current_config();
        if self.saved_config.as_ref() != Some(&cfg) {
            if let Err(e) = cfg.save() {
                self.log_issue(trf("ERR_SETTINGS_SAVE", &[&e.to_string()]));
            }
            self.saved_config = Some(cfg);
        }
    }
}

/// Just the language, for loading translations at startup (before the app
/// exists, so this never reports problems — `Config::load` does).
pub(crate) fn language_setting() -> String {
    read_settings()
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("language")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "en".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Runs `f` with HOME pointing at a fresh temporary folder (one test at
    /// a time: HOME is process-wide).
    fn with_home(f: impl FnOnce(&Path)) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("spacemap-cfg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/spacemap")).unwrap();
        let old = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &home) };
        f(&home);
        match old {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn one_bad_value_keeps_the_rest() {
        with_home(|_| {
            std::fs::write(
                settings_file(),
                r#"{"language":"es","chart":{"ring_sat":0.9,"stroke_alpha":999,"max_render_depth":-1},
                    "table":{"sort":"files","dirs_first":true,"hidden_columns":["perms","name","bogus"]}}"#,
            )
            .unwrap();
            let (cfg, needs_save, problem) = Config::load();
            assert_eq!(cfg.language, "es");
            assert_eq!(cfg.chart.ring_sat, 0.9);
            assert_eq!(cfg.chart.stroke_alpha, Settings::default().stroke_alpha);
            assert_eq!(cfg.chart.max_render_depth, Settings::default().max_render_depth);
            assert!(cfg.table.sort == SortColumn::Files && cfg.table.dirs_first);
            assert!(cfg.table.hidden_columns == vec![TableCol::Perms]);
            assert!(needs_save);
            let problem = problem.unwrap();
            assert!(problem.contains("chart.stroke_alpha") && problem.contains("table.hidden_columns"));
            assert!(settings_file().with_extension("json.bad").exists());
        });
    }

    #[test]
    fn device_or_directory_is_not_read_or_overwritten() {
        with_home(|_| {
            std::os::unix::fs::symlink("/dev/zero", settings_file()).unwrap();
            let (_, needs_save, problem) = Config::load();
            assert!(!needs_save && problem.is_some());
            assert_eq!(language_setting(), "en");
        });
        with_home(|_| {
            std::fs::create_dir(settings_file()).unwrap();
            let (_, needs_save, problem) = Config::load();
            assert!(!needs_save && problem.is_some());
            assert!(Config::default().save().is_err());
            let leftovers: Vec<_> = std::fs::read_dir(config_dir()).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
            assert_eq!(leftovers, vec![std::ffi::OsString::from("settings.json")]);
        });
    }

    #[test]
    fn symlinked_file_is_updated_in_place() {
        with_home(|home| {
            let real = home.join("dotfiles-spacemap.json");
            std::fs::write(&real, "{}").unwrap();
            std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::os::unix::fs::symlink(&real, settings_file()).unwrap();
            let mut cfg = Config::default();
            cfg.language = "es".into();
            cfg.save().unwrap();
            assert!(std::fs::symlink_metadata(settings_file()).unwrap().file_type().is_symlink());
            assert!(std::fs::read_to_string(&real).unwrap().contains("\"es\""));
            assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
        });
    }
}
