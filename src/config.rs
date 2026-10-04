//! Settings remembered between launches (language, chart settings, the
//! Summary table's layout), saved in ~/.config/spacemap/settings.json
//! whenever they change.

use super::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use table::{SidePanel, TableCol};

/// Everything in settings.json.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub(crate) struct Config {
    pub language: String,
    pub chart: Settings,
    pub table: TablePrefs,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "en".to_string(),
            chart: Settings::default(),
            table: TablePrefs::default(),
        }
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
    /// What the Summary view's left panel shows.
    pub side: SidePanel,
    /// The contents table lists files from all subfolders.
    pub flat: bool,
}

impl Default for TablePrefs {
    fn default() -> Self {
        TablePrefs {
            sort: SortColumn::Size,
            ascending: false,
            hidden_columns: Vec::new(),
            column_order: TableCol::ALL.to_vec(),
            dirs_first: false,
            side: SidePanel::Categories,
            flat: false,
        }
    }
}

fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

/// Settings files bigger than this aren't read (e.g. a link to /dev/zero).
const MAX_SETTINGS_BYTES: u64 = 1 << 20;

/// Why settings.json wasn't read.
enum Unreadable {
    NotAFile,
    TooBig,
    Io(std::io::Error),
}

impl Unreadable {
    /// The line for the Issues log. (Kept out of `read_settings`, which also
    /// runs while translations load, when looking one up would deadlock.)
    fn message(&self) -> String {
        let why = match self {
            Unreadable::NotAFile => tr("ERR_SETTINGS_NOT_FILE"),
            Unreadable::TooBig => tr("ERR_SETTINGS_TOO_BIG"),
            Unreadable::Io(e) => e.to_string(),
        };
        trf(
            "ERR_SETTINGS_UNREADABLE",
            &[&show_path(&settings_file()), &why],
        )
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
        Ok(_) => std::fs::read_to_string(&path)
            .map(Some)
            .map_err(Unreadable::Io),
    }
}

/// `file` (a JSON object) read into `T` one field at a time, starting from
/// `default`: a field whose value doesn't fit keeps its default and is
/// listed in `dropped` as "section.field". A number its field can't hold
/// (negative, fractional or too big) becomes the nearest one it can, for
/// the allowed ranges to clamp later. For a list, only the entries that
/// don't fit are dropped.
fn lenient<T: Serialize + DeserializeOwned>(
    default: T,
    file: Option<&Value>,
    section: &str,
    dropped: &mut Vec<String>,
) -> T {
    let Some(file) = file else { return default };
    let Value::Object(fields) = file else {
        dropped.push(section.to_string());
        return default;
    };
    let Ok(mut merged) = serde_json::to_value(&default) else {
        return default;
    };
    let fits = |v: &Value| serde_json::from_value::<T>(v.clone()).is_ok();
    for (key, value) in fields {
        let mut trial = merged.clone();
        trial[key] = value.clone();
        if fits(&trial) {
            merged = trial;
            continue;
        }
        if let Some(x) = value.as_f64() {
            let near = nearest_numbers(x).into_iter().find(|n| {
                trial[key] = n.clone();
                fits(&trial)
            });
            if let Some(n) = near {
                merged[key] = n;
                continue;
            }
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

/// Whole numbers closest to `x`, nearest first, for a field that can't hold
/// `x` itself: `x` rounded, then 0 if it's negative, else the largest value
/// of each unsigned size.
fn nearest_numbers(x: f64) -> Vec<Value> {
    let r = x.round();
    if r < 0.0 {
        vec![Value::from(r as i64), Value::from(0)]
    } else {
        let mut out = Vec::new();
        if r < u64::MAX as f64 {
            out.push(Value::from(r as u64));
        }
        out.extend([u64::MAX, u32::MAX.into(), u16::MAX.into(), u8::MAX.into()].map(Value::from));
        out
    }
}

impl Config {
    /// The saved settings (defaults where missing), whether the file needs
    /// writing, and a problem for the Issues log, if any. An unusable value
    /// falls back to its default and the rest is kept; the original file is
    /// first copied to settings.json.bad (moved there if it isn't JSON).
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
        let cfg = Config {
            language,
            chart: sanitized,
            table,
        };
        if dropped.is_empty() {
            return (cfg, corrected, None);
        }
        let _ = std::fs::copy(&path, &bad);
        let problem = trf(
            "ERR_SETTINGS_FIELDS",
            &[&dropped.join(", "), &show_path(&bad)],
        );
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
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = target.with_file_name(format!(".{name}.tmp"));
        let write = || -> std::io::Result<()> {
            std::fs::write(
                &tmp,
                serde_json::to_string_pretty(self).map_err(std::io::Error::other)?,
            )?;
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
        Config {
            language: current_lang_code(),
            chart: self.settings.clone(),
            table: self.table_prefs(),
        }
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
        // SAFETY (for the set_var/remove_var calls): tests that change HOME
        // hold LOCK, and no other test reads HOME: the app and translations
        // skip the user's files under cfg(test).
        unsafe { std::env::set_var("HOME", &home) };
        f(&home);
        match old {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// categories.json: written when missing, used when edited, left alone
    /// (built-in categories used) when broken.
    /// The command line's read-only loading never creates categories.json;
    /// the app's loading does.
    #[test]
    fn read_only_categories_write_nothing() {
        with_home(|_| {
            let path = config_dir().join("categories.json");
            let _ = std::fs::remove_file(&path);
            let (m, problem) = CategoryModel::load_read_only();
            assert!(problem.is_none() && !path.exists());
            assert_eq!(m, CategoryModel::defaults());
            CategoryModel::load();
            assert!(path.exists());
        });
    }

    #[test]
    fn categories_file_is_written_used_and_kept() {
        with_home(|_| {
            let path = config_dir().join("categories.json");
            let (m, problem) = CategoryModel::load();
            assert!(problem.is_none() && path.is_file());
            assert_eq!(m, CategoryModel::defaults());

            std::fs::write(
                &path,
                r#"{"categories": [{"name": "Mail", "extensions": ["eml"]}]}"#,
            )
            .unwrap();
            let (m, problem) = CategoryModel::load();
            assert!(problem.is_none());
            assert_eq!(m.label(m.of_name("a.EML")), "Mail");
            assert_eq!(m.of_name("a.mkv"), m.other());

            std::fs::write(&path, "{ oops").unwrap();
            let (m, problem) = CategoryModel::load();
            assert!(problem.is_some());
            assert_eq!(m, CategoryModel::defaults());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ oops");
        });
    }

    #[test]
    fn one_bad_value_keeps_the_rest() {
        with_home(|_| {
            std::fs::write(
                settings_file(),
                r#"{"language":"es","chart":{"hub_radius_frac":0.3,"stroke_alpha":999,"max_render_depth":-1},
                    "table":{"sort":"files","dirs_first":true,"hidden_columns":["perms","name","bogus"]}}"#,
            )
            .unwrap();
            let (cfg, needs_save, problem) = Config::load();
            assert_eq!(cfg.language, "es");
            assert_eq!(cfg.chart.hub_radius_frac, 0.3);
            assert_eq!(cfg.chart.stroke_alpha, u8::MAX);
            assert_eq!(cfg.chart.max_render_depth, *Settings::DEPTH.start());
            assert!(cfg.table.sort == SortColumn::Files && cfg.table.dirs_first);
            assert!(cfg.table.hidden_columns == vec![TableCol::Perms]);
            assert!(needs_save);
            let problem = problem.unwrap();
            assert!(!problem.contains("chart.") && problem.contains("table.hidden_columns"));
            assert!(settings_file().with_extension("json.bad").exists());
        });
    }

    /// Numbers a setting can't hold land on the nearest allowed value:
    /// negative to the lowest, huge (beyond any integer) to the highest,
    /// fractions rounded; a number given as text keeps the default.
    #[test]
    fn out_of_range_numbers_are_clamped() {
        with_home(|_| {
            std::fs::write(
                settings_file(),
                r#"{"chart":{"age_steps":-3,"age_darkest_pct":1e300,"age_days":40.6,
                    "flat_rows":-1e30,"max_children_shown":18446744073709551616,
                    "max_render_depth":"3","hub_radius_frac":1e300,"min_segment_angle_deg":-0.0}}"#,
            )
            .unwrap();
            let (cfg, needs_save, problem) = Config::load();
            let c = &cfg.chart;
            assert_eq!(c.age_steps, *Settings::AGE_STEPS.start());
            assert_eq!(c.age_darkest_pct, *Settings::AGE_DARKEST.end());
            assert_eq!(c.age_days, 41);
            assert_eq!(c.flat_rows, *Settings::FLAT_ROWS.start());
            assert_eq!(c.max_children_shown, *Settings::MAX_CHILDREN.end());
            assert_eq!(c.max_render_depth, Settings::default().max_render_depth);
            assert_eq!(c.hub_radius_frac, *Settings::HUB.end());
            assert_eq!(c.min_segment_angle_deg, *Settings::MIN_ANGLE.start());
            assert!(needs_save);
            assert_eq!(
                problem
                    .as_deref()
                    .map(|p| p.contains("chart.max_render_depth")),
                Some(true)
            );
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
            let leftovers: Vec<_> = std::fs::read_dir(config_dir())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name())
                .collect();
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
            let cfg = Config {
                language: "es".into(),
                ..Config::default()
            };
            cfg.save().unwrap();
            assert!(
                std::fs::symlink_metadata(settings_file())
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert!(std::fs::read_to_string(&real).unwrap().contains("\"es\""));
            assert_eq!(
                std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
                0o600
            );
        });
    }
}
