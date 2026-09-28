//! Everything spacemap remembers between launches — language, chart
//! settings and the Summary table's layout — in one JSON file,
//! ~/.config/spacemap/settings.json. The app compares its state against the
//! last saved copy once per frame and rewrites the file when it changes
//! (see `DiskScanApp::save_config_if_changed`).

use super::*;
use serde::{Deserialize, Serialize};
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

impl Config {
    /// The saved configuration, or defaults, plus whether the file needs
    /// (re)writing: it's missing (first launch), or held out-of-range values
    /// that were corrected.
    /// A file that doesn't parse is kept aside as settings.json.bad rather
    /// than overwritten, and the problem returned for the Issues log.
    pub(crate) fn load() -> (Config, bool, Option<String>) {
        let path = settings_file();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return (Config::default(), true, None);
        };
        match serde_json::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                let sanitized = cfg.chart.clone().sanitized();
                let corrected = sanitized != cfg.chart;
                cfg.chart = sanitized;
                (cfg, corrected, None)
            }
            Err(e) => {
                let bad = path.with_extension("json.bad");
                let _ = std::fs::rename(&path, &bad);
                let problem = trf("ERR_SETTINGS_FILE", &[&bad.display().to_string(), &e.to_string()]);
                (Config::default(), true, Some(problem))
            }
        }
    }

    /// Writes the file (via a temporary file, so a crash mid-write can't
    /// leave it half-written).
    pub(crate) fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir())?;
        let path = settings_file();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
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
    std::fs::read_to_string(settings_file())
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("language")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "en".to_string())
}
