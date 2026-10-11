//! Facts about the filesystem: mount points, spinning disks, free
//! space, creation times and the on-disk case of a typed path.

use super::*;

/// Birth (creation) time in Unix seconds; 0 if unavailable.
pub(crate) fn birth_secs(m: &std::fs::Metadata) -> i64 {
    m.created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Every mount point, from /proc/self/mountinfo. A folder in this set has
/// another filesystem mounted on it; scans and deletes stop there.
pub(crate) fn mount_points() -> HashSet<PathBuf> {
    let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return HashSet::new();
    };
    info.lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(|field| PathBuf::from(unescape_mountinfo(field)))
        .collect()
}

/// True if `path` is on a spinning disk. Linux reports it per disk in
/// `/sys/dev/block/<major>:<minor>/queue/rotational` (for a partition, in its
/// disk's folder, one level up). Network shares and virtual devices have no
/// such entry and count as not rotational.
pub(crate) fn is_rotational(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let dev = meta.dev();
    let (major, minor) = (libc::major(dev), libc::minor(dev));
    let block = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
    [
        block.join("queue/rotational"),
        block.join("../queue/rotational"),
    ]
    .iter()
    .find_map(|f| std::fs::read_to_string(f).ok())
    .is_some_and(|v| v.trim() == "1")
}

/// mountinfo writes space, tab, newline and backslash as octal escapes
/// (`\040` for a space).
pub(crate) fn unescape_mountinfo(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Paths longer than this are opened relative to their parent folder
/// (see `DirHandle`), safely under the kernel's 4096-byte PATH_MAX.
pub(crate) const LONG_PATH: usize = 3800;

/// An open folder, kept while scanning a deep subtree so that folders
/// whose full path is too long for the kernel can still be opened, relative
/// to it, via the short `/proc/self/fd/<fd>/<name>`.
pub(crate) type DirHandle = Option<std::fs::File>;

/// A path the kernel will accept for `path`: itself, or — when it's too
/// long — `/proc/self/fd/<parent fd>/<name>` relative to its open parent.
pub(crate) fn openable(path: &Path, parent: &DirHandle) -> PathBuf {
    use std::os::fd::AsRawFd;
    match parent {
        Some(f) if path.as_os_str().len() > LONG_PATH => {
            let mut p = PathBuf::from(format!("/proc/self/fd/{}", f.as_raw_fd()));
            if let Some(name) = path.file_name() {
                p.push(name);
            }
            p
        }
        _ => path.to_path_buf(),
    }
}

/// Returns (total_bytes, free_bytes) for the filesystem containing `path`.
pub(crate) fn fs_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `cpath` is a valid C string that outlives the call, and
    // `stat` is a plain C struct that statvfs fills in.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(cpath.as_ptr(), &mut stat) != 0 {
            return None;
        }
        let frsize = stat.f_frsize as u64;
        let total = stat.f_blocks as u64 * frsize;
        let free = stat.f_bavail as u64 * frsize;
        Some((total, free))
    }
}

/// True if `path` is a mount point (on another device than its parent).
pub(crate) fn is_real_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = path.parent() else {
        return true; // "/"
    };
    let (Ok(here), Ok(up)) = (std::fs::metadata(path), std::fs::metadata(parent)) else {
        return false;
    };
    here.dev() != up.dev()
}

/// `p` spelled as on disk: on a case-insensitive filesystem, a component
/// typed in another case ("PHOTOS") becomes the real name ("Photos").
pub(crate) fn true_case(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        let Component::Normal(name) = comp else {
            out.push(comp.as_os_str());
            continue;
        };
        let folded = |s: &std::ffi::OsStr| s.to_string_lossy().to_lowercase();
        let actual = std::fs::read_dir(&out).ok().and_then(|entries| {
            let mut other_case = None;
            for e in entries.flatten() {
                let n = e.file_name();
                if n == name {
                    return None; // spelled right already
                }
                if other_case.is_none() && folded(&n) == folded(name) {
                    other_case = Some(n);
                }
            }
            other_case
        });
        out.push(actual.as_deref().unwrap_or(name));
    }
    out
}

#[cfg(test)]
mod case_tests {
    use super::*;
    #[test]
    fn typed_case_becomes_disk_case() {
        // Case-sensitive filesystems are left alone.
        assert_eq!(
            true_case(Path::new("/usr/share")),
            PathBuf::from("/usr/share")
        );
        // On the dev machine /mnt/DATA is exFAT (case-insensitive).
        let real = PathBuf::from("/mnt/DATA/System Volume Information");
        let typed = PathBuf::from("/mnt/DATA/SYSTEM VOLUME INFORMATION");
        if typed.is_dir() && real.is_dir() && typed != real {
            assert_eq!(true_case(&typed), real);
        }
    }
}
