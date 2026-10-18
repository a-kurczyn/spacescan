//! `spacescan -U`: replaces the running AppImage with the latest release
//! from GitHub, after checking it against the release's SHA256SUMS. Only
//! when asked: SpaceScan never looks for updates by itself. Downloads go
//! through `curl`, so no network code is built in.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const RELEASES: &str = "https://github.com/a-kurczyn/spacescan/releases";
/// The AppImage's name in every release.
const ASSET: &str = "SpaceScan-x86_64.AppImage";

/// Runs the update and returns the exit code (0: updated or already up to
/// date). Messages go to the terminal.
pub(crate) fn run() -> i32 {
    match update(RELEASES, env!("CARGO_PKG_VERSION")) {
        Ok(message) => {
            println!("{message}");
            0
        }
        Err(why) => {
            eprintln!("spacescan: {why}");
            1
        }
    }
}

fn update(releases: &str, current: &str) -> Result<String, String> {
    // Set by the AppImage's runtime to the file it started from.
    let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from) else {
        return Err("only the AppImage updates itself; this copy was installed \
                    another way, so update it with your package manager"
            .to_string());
    };
    let appimage = std::fs::canonicalize(&appimage)
        .map_err(|e| format!("can't find the AppImage {}: {e}", appimage.display()))?;

    let latest_url = curl_text(
        &["-o", "/dev/null", "-w", "%{url_effective}"],
        &format!("{releases}/latest"),
    )?;
    let latest = tag_version(&latest_url)
        .ok_or_else(|| format!("can't tell the latest version from {latest_url}"))?;
    if !is_newer(&latest, current) {
        return Ok(format!("SpaceScan {current} is up to date."));
    }

    let download = format!("{releases}/download/v{latest}");
    let sums = curl_text(&[], &format!("{download}/SHA256SUMS"))?;
    let expected = checksum_for(&sums, ASSET)
        .ok_or_else(|| format!("the {latest} release lists no checksum for {ASSET}"))?;

    let folder = appimage.parent().unwrap_or(Path::new("/"));
    let partial = Partial::create(folder).map_err(|e| {
        format!(
            "can't write in {} ({e}); move the AppImage to a folder you own, \
             or download {download}/{ASSET} yourself",
            folder.display()
        )
    })?;
    eprintln!("Downloading SpaceScan {latest}…");
    let status = Command::new("curl")
        .args(["-fL", "--progress-bar", "-o"])
        .arg(&partial.path)
        .arg(format!("{download}/{ASSET}"))
        .status()
        .map_err(curl_missing)?;
    if !status.success() {
        return Err(format!("downloading {download}/{ASSET} failed"));
    }
    let actual = sha256(&partial.path)?;
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(
            "the download doesn't match the release's checksum; nothing was changed".to_string(),
        );
    }
    let permissions = std::fs::metadata(&appimage)
        .map_err(|e| format!("can't read {}: {e}", appimage.display()))?
        .permissions();
    std::fs::set_permissions(&partial.path, permissions)
        .map_err(|e| format!("can't set the new file's permissions: {e}"))?;
    partial.replace(&appimage)?;
    Ok(format!(
        "SpaceScan updated from {current} to {latest}. Start it again to use the new version."
    ))
}

/// The download in progress, next to the AppImage so the final rename stays
/// on one filesystem. Removed unless it replaced the AppImage.
struct Partial {
    path: PathBuf,
    done: bool,
}

impl Partial {
    fn create(folder: &Path) -> std::io::Result<Partial> {
        let path = folder.join(format!(".{ASSET}.download-{}", std::process::id()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Partial { path, done: false })
    }

    fn replace(mut self, target: &Path) -> Result<(), String> {
        std::fs::rename(&self.path, target)
            .map_err(|e| format!("can't replace {}: {e}", target.display()))?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// What curl prints for `url` (following redirects), as text.
fn curl_text(args: &[&str], url: &str) -> Result<String, String> {
    let out = Command::new("curl")
        .args(["-fsSL"])
        .args(args)
        .arg(url)
        .stderr(Stdio::piped())
        .output()
        .map_err(curl_missing)?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.trim().trim_start_matches("curl: ");
        return Err(format!("can't reach {url}: {why}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn curl_missing(e: std::io::Error) -> String {
    format!("updating needs curl, which couldn't be run ({e})")
}

/// The file's SHA-256, as hex.
fn sha256(path: &Path) -> Result<String, String> {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|e| format!("checking the download needs sha256sum ({e})"))?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .filter(|_| out.status.success())
        .map(str::to_string)
        .ok_or_else(|| "checking the download failed".to_string())
}

/// The version in a release URL ending in `/tag/v1.2.3`.
fn tag_version(url: &str) -> Option<String> {
    let tag = url.trim_end_matches('/').rsplit_once("/tag/")?.1;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    version_numbers(version).map(|_| version.to_string())
}

/// "1.2.3" as numbers, or None if it isn't one.
fn version_numbers(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|n| n.parse().ok()).collect()
}

/// True if `latest` is a later version than `current`.
fn is_newer(latest: &str, current: &str) -> bool {
    match (version_numbers(latest), version_numbers(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// The checksum SHA256SUMS lists for `file` (lines "HASH  NAME", the name
/// possibly marked binary with `*`).
fn checksum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == file && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_and_compared() {
        assert_eq!(
            tag_version("https://github.com/a-kurczyn/spacescan/releases/tag/v1.2.0").as_deref(),
            Some("1.2.0")
        );
        assert_eq!(
            tag_version("https://github.com/x/y/releases/tag/1.10.3/").as_deref(),
            Some("1.10.3")
        );
        assert_eq!(tag_version("https://github.com/x/y/releases"), None);
        assert_eq!(
            tag_version("https://github.com/x/y/releases/tag/nightly"),
            None
        );
        assert!(is_newer("1.2.0", "1.1.3"));
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(is_newer("2.0", "1.9.9"));
        assert!(!is_newer("1.1.3", "1.1.3"));
        assert!(!is_newer("1.1.2", "1.1.3"));
        assert!(!is_newer("1.x", "1.1.3"));
    }

    #[test]
    fn checksum_lines_are_found() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!(
            "{a}  spacescan_1.2.0-1_amd64.deb\n{b} *SpaceScan-x86_64.AppImage\nnot a line\n"
        );
        assert_eq!(checksum_for(&sums, ASSET), Some(b));
        assert_eq!(checksum_for(&sums, "other"), None);
        assert_eq!(checksum_for("abc  SpaceScan-x86_64.AppImage", ASSET), None);
    }

    /// A download that doesn't replace the AppImage leaves nothing behind.
    #[test]
    fn unfinished_downloads_are_removed() {
        let dir = std::env::temp_dir().join(format!("spacescan-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = {
            let partial = Partial::create(&dir).unwrap();
            assert!(partial.path.exists());
            partial.path.clone()
        };
        assert!(!path.exists());
        let partial = Partial::create(&dir).unwrap();
        let target = dir.join(ASSET);
        partial.replace(&target).unwrap();
        assert!(target.exists() && std::fs::read_dir(&dir).unwrap().count() == 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
