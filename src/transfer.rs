//! Copying and moving files: Ctrl+C or Ctrl+X picks the marked rows (or the
//! row or slice under the cursor), Ctrl+V puts them into the folder being
//! viewed. The work runs on its own thread with a progress bar and a Cancel
//! button; a name clash pauses it to ask. Afterwards moved items leave the
//! tree and the target folder is rescanned.
//!
//! Mount safety, as for deleting: a move never takes anything from inside
//! another mounted filesystem, and a copy skips mount points inside the
//! folders it copies. Links are copied as links, never followed.

use super::*;
use crate::delete::{find_node, mount_guard};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ClipMode {
    Copy,
    Move,
}

/// What Ctrl+C or Ctrl+X picked, waiting for Ctrl+V.
pub(crate) struct Clip {
    pub paths: Vec<PathBuf>,
    pub mode: ClipMode,
}

/// The answer to a name clash.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ClashChoice {
    /// Overwrite the file there; for two folders, put the contents in the
    /// folder that's there.
    Replace,
    Skip,
    /// Give the new item a free name ("name (2).ext").
    KeepBoth,
}

/// A name clash the worker needs answered.
#[derive(Clone, Debug)]
pub(crate) struct Clash {
    /// The item already there.
    target: PathBuf,
    /// Both are folders: Replace merges them.
    merge: bool,
    /// Replace is offered (not for an item onto itself, or a file onto a
    /// folder or the other way round).
    can_replace: bool,
}

/// How putting one item in place went.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Outcome {
    /// All of it got there.
    Done,
    /// Left out by a clash answer.
    Skipped,
    /// Some of it couldn't be copied or moved (see the issues).
    Incomplete,
}

impl Outcome {
    /// The outcome of a folder from its contents' outcomes.
    fn and(self, other: Outcome) -> Outcome {
        match (self, other) {
            (Outcome::Incomplete, _) | (_, Outcome::Incomplete) => Outcome::Incomplete,
            (Outcome::Done, _) | (_, Outcome::Done) => Outcome::Done,
            _ => Outcome::Skipped,
        }
    }
}

/// What the worker reports.
enum Report {
    /// Bytes done so far.
    Progress(u64),
    Clash(Clash),
    Issue(String),
    /// Nothing was done, for this reason.
    Refused(String),
    /// Finished (or cancelled).
    Done(Results),
}

/// What a copy or move did.
#[derive(Default, Debug)]
struct Results {
    /// The paths that no longer exist at the source.
    removed: Vec<PathBuf>,
    /// Items put in place completely, left out by a clash answer, or only
    /// in part.
    complete: usize,
    skipped: usize,
    incomplete: usize,
    cancelled: bool,
}

/// A copy or move in progress.
struct Job {
    mode: ClipMode,
    target: PathBuf,
    total: u64,
    done: u64,
    reports: Receiver<Report>,
    /// Clash answers: None cancels everything.
    answers: Sender<Option<(ClashChoice, bool)>>,
    cancel: Arc<AtomicBool>,
    /// The clash waiting for an answer, and the "for every clash" tick box.
    clash: Option<(Clash, bool)>,
    /// Problems so far, listed again after the target folder's rescan.
    issues: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Transfer {
    pub clip: Option<Clip>,
    job: Option<Job>,
    /// When the last paste with text arrived (see `clipboard_events`).
    text_pasted_at: Option<Instant>,
    /// A V key press reached the app and its release hasn't yet.
    v_down: bool,
}

/// The worker gave up: the user cancelled.
struct Stop;

/// The copying itself, on its own thread.
struct Worker {
    reports: Sender<Report>,
    answers: Receiver<Option<(ClashChoice, bool)>>,
    cancel: Arc<AtomicBool>,
    /// Mount points, which copies don't enter.
    mounts: HashSet<PathBuf>,
    /// The answer chosen "for every clash", if any.
    always: Option<ClashChoice>,
    bytes: u64,
    last_report: Instant,
    /// Files with several hard links copied so far, by (device, inode), and
    /// where their copy is: their other names become links to it too.
    links: HashMap<(u64, u64), PathBuf>,
    /// Sources removed by a move (a folder in place of all it held).
    removed: Vec<PathBuf>,
}

impl Worker {
    fn issue(&self, text: String) {
        let _ = self.reports.send(Report::Issue(text));
    }

    fn check_cancel(&self) -> Result<(), Stop> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Stop)
        } else {
            Ok(())
        }
    }

    fn add_bytes(&mut self, n: u64) {
        self.bytes = self.bytes.saturating_add(n);
        if self.last_report.elapsed() >= std::time::Duration::from_millis(100) {
            self.last_report = Instant::now();
            let _ = self.reports.send(Report::Progress(self.bytes));
        }
    }

    /// The answer to `clash`: the one chosen for every clash if it applies,
    /// else the user's.
    fn choose(&mut self, clash: Clash) -> Result<ClashChoice, Stop> {
        match self.always {
            Some(ClashChoice::Replace) if !clash.can_replace => {}
            Some(choice) => return Ok(choice),
            None => {}
        }
        let _ = self.reports.send(Report::Clash(clash));
        match self.answers.recv() {
            Ok(Some((choice, all))) => {
                if all {
                    self.always = Some(choice);
                }
                Ok(choice)
            }
            _ => {
                self.cancel.store(true, Ordering::Relaxed);
                Err(Stop)
            }
        }
    }

    /// Err with the reason if the free space where `sources` go is less than
    /// what copying them takes (a move within one filesystem takes none).
    fn check_space(
        &self,
        sources: &[PathBuf],
        known: &[Option<u64>],
        target: &Path,
        mode: ClipMode,
    ) -> Result<(), String> {
        let dev = |p: &Path| std::fs::symlink_metadata(p).map(|m| m.dev()).ok();
        let mut seen = HashSet::new();
        let mut needs = 0u64;
        for (p, size) in sources.iter().zip(known) {
            if mode == ClipMode::Move && dev(p) == dev(target) {
                continue;
            }
            let size = size.unwrap_or_else(|| disk_usage(p, &self.mounts, &mut seen));
            needs = needs.saturating_add(size);
        }
        match fs_space(target) {
            Some((_, free)) if needs > free => Err(trf(
                "ERR_NO_SPACE",
                &[&show_path(target), &human_size(needs), &human_size(free)],
            )),
            _ => Ok(()),
        }
    }

    /// Puts each of `sources` into the folder `target`.
    fn put_all(&mut self, sources: &[PathBuf], target: &Path, mode: ClipMode) -> Results {
        let mut r = Results::default();
        for src in sources {
            let dst = target.join(src.file_name().unwrap_or_default());
            match self.put(src, &dst, mode) {
                Ok(Outcome::Done) => r.complete += 1,
                Ok(Outcome::Skipped) => r.skipped += 1,
                Ok(Outcome::Incomplete) => r.incomplete += 1,
                Err(Stop) => {
                    r.cancelled = true;
                    break;
                }
            }
        }
        r.removed = std::mem::take(&mut self.removed);
        r
    }

    /// Puts `src` at `dst`, copying or moving it.
    fn put(&mut self, src: &Path, dst: &Path, mode: ClipMode) -> Result<Outcome, Stop> {
        self.check_cancel()?;
        let meta = match std::fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.issue(trf("ERR_COPY_FAILED", &[&show_path(src), &io_reason(&e)]));
                return Ok(Outcome::Incomplete);
            }
        };
        let mut dst = dst.to_path_buf();
        if let Ok(there) = std::fs::symlink_metadata(&dst) {
            let same = src == dst;
            let merge = !same && meta.is_dir() && there.is_dir();
            let can_replace = !same && (merge || (!meta.is_dir() && !there.is_dir()));
            match self.choose(Clash {
                target: dst.clone(),
                merge,
                can_replace,
            })? {
                ClashChoice::Skip => return Ok(Outcome::Skipped),
                ClashChoice::KeepBoth => dst = free_name(&dst),
                ClashChoice::Replace if merge => return self.merge(src, &dst, mode),
                ClashChoice::Replace => {}
            }
        }
        if mode == ClipMode::Move {
            match std::fs::rename(src, &dst) {
                Ok(()) => {
                    self.add_bytes(meta.len());
                    self.removed.push(src.to_path_buf());
                    return Ok(Outcome::Done);
                }
                // Another filesystem: copied, each part removed once there.
                Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {}
                Err(e) => {
                    self.issue(trf("ERR_MOVE_FAILED", &[&show_path(src), &io_reason(&e)]));
                    return Ok(Outcome::Incomplete);
                }
            }
        }
        self.copy(src, &dst, &meta, mode == ClipMode::Move)
    }

    /// Puts the contents of folder `src` into the folder `dst` that's
    /// already there; a move then removes `src` if nothing is left in it.
    fn merge(&mut self, src: &Path, dst: &Path, mode: ClipMode) -> Result<Outcome, Stop> {
        let entries = match std::fs::read_dir(src) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect::<Vec<_>>(),
            Err(e) => {
                self.issue(trf("ERR_COPY_FAILED", &[&show_path(src), &io_reason(&e)]));
                return Ok(Outcome::Incomplete);
            }
        };
        let start = self.removed.len();
        let mut outcome = Outcome::Done;
        for (i, entry) in entries.iter().enumerate() {
            let name = entry.file_name();
            let one = deep(|| self.put(&src.join(&name), &dst.join(&name), mode))?;
            outcome = if i == 0 { one } else { outcome.and(one) };
        }
        if mode == ClipMode::Move && std::fs::remove_dir(src).is_ok() {
            self.removed.truncate(start);
            self.removed.push(src.to_path_buf());
        }
        Ok(outcome)
    }

    /// Copies `src` (described by `meta`) to the new path `dst`, keeping
    /// permissions, times and hard links between the files copied. With
    /// `remove` (a move to another filesystem), each part of `src` is
    /// removed as soon as it's in place, so whatever can't be moved is all
    /// that stays.
    fn copy(
        &mut self,
        src: &Path,
        dst: &Path,
        meta: &std::fs::Metadata,
        remove: bool,
    ) -> Result<Outcome, Stop> {
        self.check_cancel()?;
        let ft = meta.file_type();
        let failed = |w: &Self, e: std::io::Error| {
            w.issue(trf("ERR_COPY_FAILED", &[&show_path(src), &io_reason(&e)]));
            Ok(Outcome::Incomplete)
        };
        if ft.is_dir() {
            if self.mounts.contains(src) {
                self.issue(trf("ERR_OTHER_FS_SKIPPED", &[&show_path(src)]));
                return Ok(Outcome::Incomplete);
            }
            if let Err(e) = std::fs::create_dir(dst) {
                return failed(self, e);
            }
            let entries = match std::fs::read_dir(src) {
                Ok(rd) => rd.filter_map(|e| e.ok()).collect::<Vec<_>>(),
                Err(e) => return failed(self, e),
            };
            let start = self.removed.len();
            let mut outcome = Outcome::Done;
            for entry in entries {
                let child = entry.path();
                let one = match entry.metadata() {
                    Ok(m) => deep(|| self.copy(&child, &dst.join(entry.file_name()), &m, remove))?,
                    Err(e) => failed(self, e)?,
                };
                outcome = outcome.and(one);
            }
            let _ = std::fs::set_permissions(dst, meta.permissions());
            keep_times(dst, meta);
            if remove && outcome == Outcome::Done {
                match std::fs::remove_dir(src) {
                    Ok(()) => {
                        self.removed.truncate(start);
                        self.removed.push(src.to_path_buf());
                    }
                    Err(e) => {
                        self.issue(trf("ERR_MOVE_FAILED", &[&show_path(src), &io_reason(&e)]));
                        return Ok(Outcome::Incomplete);
                    }
                }
            }
            return Ok(outcome);
        }
        // Another name of a file already copied: linked to that copy. (Its
        // link count may have dropped since: a move removes names it copied.)
        let key = (meta.dev(), meta.ino());
        let linked = if ft.is_symlink() {
            None
        } else {
            self.links.get(&key).cloned()
        };
        let made = if let Some(first) = linked {
            replace_with(dst, |dst| std::fs::hard_link(&first, dst))
        } else if ft.is_symlink() {
            std::fs::read_link(src).and_then(|target| {
                replace_with(dst, |dst| std::os::unix::fs::symlink(&target, dst))
            })
        } else if ft.is_fifo() {
            replace_with(dst, |dst| make_fifo(dst, meta.mode()))
        } else if ft.is_file() {
            match self.copy_file(src, dst) {
                Ok(()) => Ok(()),
                Err(e) => {
                    // No half-written file is left behind.
                    let _ = std::fs::remove_file(dst);
                    match e {
                        Some(e) => Err(e),
                        None => return Err(Stop),
                    }
                }
            }
        } else {
            self.issue(trf("ERR_SPECIAL_FILE", &[&show_path(src)]));
            return Ok(Outcome::Incomplete);
        };
        if let Err(e) = made {
            return failed(self, e);
        }
        if !ft.is_symlink() {
            let _ = std::fs::set_permissions(dst, meta.permissions());
            keep_times(dst, meta);
            if meta.nlink() > 1 {
                self.links.entry(key).or_insert_with(|| dst.to_path_buf());
            }
        }
        if remove {
            if let Err(e) = std::fs::remove_file(src) {
                self.issue(trf("ERR_MOVE_FAILED", &[&show_path(src), &io_reason(&e)]));
                return Ok(Outcome::Incomplete);
            }
            self.removed.push(src.to_path_buf());
        }
        Ok(Outcome::Done)
    }

    /// Copies one file's contents, in pieces so progress shows and Cancel
    /// works inside big files. Err(None): cancelled.
    fn copy_file(&mut self, src: &Path, dst: &Path) -> Result<(), Option<std::io::Error>> {
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;
        const PIECE: usize = 16 << 20;
        let mut from = std::fs::File::open(src)?;
        let mut to = std::fs::File::create(dst)?;
        // The kernel copies directly where it can.
        let mut in_kernel = true;
        let mut buf = Vec::new();
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(None);
            }
            let n = if in_kernel {
                // SAFETY: both descriptors are open files for the whole call,
                // and null offsets mean "use and advance the file positions".
                let n = unsafe {
                    libc::copy_file_range(
                        from.as_raw_fd(),
                        std::ptr::null_mut(),
                        to.as_raw_fd(),
                        std::ptr::null_mut(),
                        PIECE,
                        0,
                    )
                };
                if n < 0 {
                    let e = std::io::Error::last_os_error();
                    match e.raw_os_error() {
                        Some(libc::EXDEV | libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP) => {
                            in_kernel = false;
                            continue;
                        }
                        _ => return Err(Some(e)),
                    }
                }
                n as usize
            } else {
                buf.resize(PIECE.min(1 << 20), 0);
                let n = from.read(&mut buf)?;
                to.write_all(&buf[..n])?;
                n
            };
            if n == 0 {
                return Ok(());
            }
            self.add_bytes(n as u64);
        }
    }
}

/// Makes `dst` with `make`, first removing a file already there (a clash
/// answered with Replace).
fn replace_with(
    dst: &Path,
    make: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    if std::fs::symlink_metadata(dst).is_ok_and(|m| !m.is_dir()) {
        std::fs::remove_file(dst)?;
    }
    make(dst)
}

/// Makes a named pipe at `path` with permissions `mode`.
fn make_fifo(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `c` is a valid C string that outlives the call.
    if unsafe { libc::mkfifo(c.as_ptr(), (mode & 0o7777) as libc::mode_t) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Disk space the files under `path` use (each file with several hard
/// links once), without entering other filesystems.
fn disk_usage(path: &Path, mounts: &HashSet<PathBuf>, seen: &mut HashSet<(u64, u64)>) -> u64 {
    let Ok(m) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if m.nlink() > 1 && !m.is_dir() && !seen.insert((m.dev(), m.ino())) {
        return 0;
    }
    let own = m.blocks() * 512;
    if !m.is_dir() || mounts.contains(path) {
        return own;
    }
    let Ok(rd) = std::fs::read_dir(path) else {
        return own;
    };
    rd.filter_map(|e| e.ok())
        .map(|e| deep(|| disk_usage(&e.path(), mounts, seen)))
        .fold(own, u64::saturating_add)
}

/// Gives `dst` the modified and accessed times in `meta`.
/// (Set by path, without opening `dst`: opening a named pipe would wait
/// for a writer.)
fn keep_times(dst: &Path, meta: &std::fs::Metadata) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dst.as_os_str().as_bytes()) else {
        return;
    };
    let times = [
        libc::timespec {
            tv_sec: meta.atime(),
            tv_nsec: meta.atime_nsec(),
        },
        libc::timespec {
            tv_sec: meta.mtime(),
            tv_nsec: meta.mtime_nsec(),
        },
    ];
    // SAFETY: `c` is a valid C string and `times` two timespecs, both
    // outliving the call.
    unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        );
    }
}

/// A name next to `path` that isn't taken: "name (2).ext", "name (3).ext"…
/// (folders and dotfiles get the number at the end).
pub(crate) fn free_name(path: &Path) -> PathBuf {
    let name = file_name_of(path);
    let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    (2u64..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("some number is free")
}

/// `path` as a file:// link, with anything but plain characters escaped.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// True on a Wayland desktop (else the clipboard is X11's).
fn on_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty())
}

/// `paths` in the formats file managers and other programs read: a list of
/// file links, the same marked as copied or cut the way GNOME and KDE mark
/// it, and the paths as text.
fn clipboard_formats(paths: &[PathBuf], mode: ClipMode) -> Vec<(&'static str, Vec<u8>)> {
    let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
    let cut = mode == ClipMode::Move;
    let text = paths
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        ("text/uri-list", (uris.join("\r\n") + "\r\n").into_bytes()),
        (
            "x-special/gnome-copied-files",
            format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n")).into_bytes(),
        ),
        (
            "application/x-kde-cutselection",
            (if cut { "1" } else { "0" }).into(),
        ),
        ("text/plain;charset=utf-8", text.clone().into_bytes()),
        ("UTF8_STRING", text.into_bytes()),
    ]
}

/// Puts `paths` on the system clipboard as files (marked as cut after
/// Ctrl+X) and as text. False if that failed.
fn offer_on_clipboard(paths: &[PathBuf], mode: ClipMode) -> bool {
    let formats = clipboard_formats(paths, mode);
    if !on_wayland() {
        return crate::x11clip::offer(formats);
    }
    use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};
    let sources = formats
        .into_iter()
        // Wayland names the text formats itself.
        .filter(|(name, _)| *name != "UTF8_STRING")
        .map(|(name, data)| MimeSource {
            source: Source::Bytes(data.into_boxed_slice()),
            mime_type: if name.starts_with("text/plain") {
                MimeType::Text
            } else {
                MimeType::Specific(name.to_string())
            },
        })
        .collect();
    Options::new().copy_multi(sources).is_ok()
}

/// The system clipboard's contents in format `format`, if it has them.
fn clipboard_read(format: &str) -> Option<Vec<u8>> {
    if !on_wayland() {
        return crate::x11clip::read(format);
    }
    use wl_clipboard_rs::paste::{ClipboardType, MimeType, Seat, get_contents};
    let (mut pipe, _) = get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        MimeType::Specific(format),
    )
    .ok()?;
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut pipe, &mut data).ok()?;
    Some(data)
}

/// True if the files on the system clipboard were cut (to be moved), going
/// by KDE's or GNOME's mark.
fn clipboard_cut() -> bool {
    clipboard_read("application/x-kde-cutselection").is_some_and(|d| d.trim_ascii() == b"1")
        || clipboard_read("x-special/gnome-copied-files")
            .is_some_and(|d| d.split(|&b| b == b'\n').next() == Some(b"cut"))
}

/// The files on the system clipboard as a file list (what file managers
/// copy), if any.
fn clipboard_files() -> Option<Vec<PathBuf>> {
    let files = parse_uri_list(&clipboard_read("text/uri-list")?);
    (!files.is_empty()).then_some(files)
}

/// The local files in a list of links (one per line, "\r\n" or "\n";
/// "#" lines are comments), byte for byte: a name that isn't UTF-8 comes
/// through as it is on disk.
fn parse_uri_list(data: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    data.split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .filter_map(|line| {
            let rest = line.strip_prefix(b"file:")?;
            // "file:///p" and "file://localhost/p"; also "file:/p".
            let path = match rest.strip_prefix(b"//") {
                Some(host_and_path) => {
                    let slash = host_and_path.iter().position(|&b| b == b'/')?;
                    let host = &host_and_path[..slash];
                    if !host.is_empty() && host != b"localhost" {
                        return None;
                    }
                    &host_and_path[slash..]
                }
                None => rest,
            };
            path.starts_with(b"/")
                .then(|| PathBuf::from(std::ffi::OsString::from_vec(percent_decode(path))))
        })
        .collect()
}

/// How to paste files copied in another program: a move if they were cut.
fn external_mode() -> ClipMode {
    if clipboard_cut() {
        ClipMode::Move
    } else {
        ClipMode::Copy
    }
}

/// The files in pasted text, if every line names an existing file or
/// folder: absolute paths, or file:// links (as file managers copy them).
fn paths_in_text(text: &str) -> Option<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| match l.strip_prefix("file://") {
            Some(rest) => {
                use std::os::unix::ffi::OsStringExt;
                PathBuf::from(std::ffi::OsString::from_vec(percent_decode(
                    rest.as_bytes(),
                )))
            }
            None => PathBuf::from(l),
        })
        .collect();
    (!paths.is_empty()
        && paths
            .iter()
            .all(|p| p.is_absolute() && std::fs::symlink_metadata(p).is_ok()))
    .then_some(paths)
}

/// `b` with %XX escapes turned back into bytes.
fn percent_decode(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|&c| hex(c)),
                b.get(i + 2).and_then(|&c| hex(c)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

impl DiskScanApp {
    /// This frame's Ctrl+C, Ctrl+X, and Ctrl+V (with the pasted text, empty
    /// if the clipboard holds no text). A Ctrl+V only arrives as a paste
    /// when the clipboard has text, so a Ctrl+V whose key comes up without
    /// one also counts.
    pub(crate) fn clipboard_events(&mut self, ctx: &egui::Context) -> (bool, bool, Option<String>) {
        let (mut copy, mut cut, mut text, mut v_released) = (false, false, None, false);
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Copy => copy = true,
                    egui::Event::Cut => cut = true,
                    egui::Event::Paste(t) => text = Some(t.clone()),
                    egui::Event::Key {
                        key: egui::Key::V,
                        pressed: true,
                        ..
                    } => self.transfer.v_down = true,
                    egui::Event::Key {
                        key: egui::Key::V,
                        pressed: false,
                        modifiers,
                        ..
                    } => {
                        // The press of Ctrl+V never reaches the app, and on X11
                        // the release can come without Ctrl.
                        let press_hidden = !std::mem::take(&mut self.transfer.v_down);
                        v_released |= modifiers.command || press_hidden;
                    }
                    _ => {}
                }
            }
        });
        if text.is_some() {
            self.transfer.text_pasted_at = Some(Instant::now());
        } else if v_released {
            let just_pasted = self
                .transfer
                .text_pasted_at
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1));
            if !just_pasted {
                text = Some(String::new());
            }
        }
        (copy, cut, text)
    }

    /// True while a copy or move runs (other changes to files wait).
    pub(crate) fn transfer_busy(&self) -> bool {
        self.transfer.job.is_some()
    }

    /// Ctrl+C / Ctrl+X: remembers `paths` for Ctrl+V, and (if set) puts
    /// them on the system clipboard, for file managers and as text.
    pub(crate) fn clip(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, mode: ClipMode) {
        if paths.is_empty() {
            return;
        }
        if self.settings.paths_to_clipboard && !offer_on_clipboard(&paths, mode) {
            // At least as text.
            let text: Vec<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            ctx.copy_text(text.join("\n"));
        }
        let key = match mode {
            ClipMode::Copy => "STATUS_CLIP_COPY",
            ClipMode::Move => "STATUS_CLIP_MOVE",
        };
        self.status = trn(
            key,
            paths.len() as u64,
            &[&format_count(paths.len() as u64)],
        );
        self.transfer.clip = Some(Clip { paths, mode });
    }

    /// Ctrl+V into folder `dest`: what Ctrl+C or Ctrl+X picked, or else
    /// files named in the pasted `text` (copied in a file manager).
    pub(crate) fn paste_into(&mut self, dest: PathBuf, text: &str) {
        if self.transfer_busy() {
            self.status = tr("STATUS_TRANSFER_BUSY");
            return;
        }
        // Files copied in another program after spacemap's own Ctrl+C or
        // Ctrl+X replaced its paths on the clipboard, so they win. (With the
        // paths kept off the clipboard, spacemap's own pick always wins.)
        let copied = clipboard_files().or_else(|| paths_in_text(text));
        let (paths, mode) = match (&self.transfer.clip, copied) {
            (Some(clip), Some(copied))
                if self.settings.paths_to_clipboard && copied != clip.paths =>
            {
                (copied, external_mode())
            }
            (Some(clip), _) => (clip.paths.clone(), clip.mode),
            (None, Some(copied)) => (copied, external_mode()),
            (None, None) => {
                self.status = tr("STATUS_PASTE_NOTHING");
                return;
            }
        };
        let mut sources = Vec::new();
        for p in paths {
            if std::fs::symlink_metadata(&p).is_err() {
                self.log_issue(trf(
                    "ERR_COPY_FAILED",
                    &[&show_path(&p), &tr("ERR_IO_NOT_FOUND")],
                ));
            } else if p.is_dir() && dest.starts_with(&p) {
                self.log_issue(trf("ERR_PASTE_INTO_ITSELF", &[&show_path(&p)]));
            } else if mode == ClipMode::Move && p.parent() == Some(dest.as_path()) {
                // Already there.
            } else if mode == ClipMode::Move
                && let Err(e) = mount_guard(&p)
            {
                self.log_issue(e);
            } else {
                sources.push(p);
            }
        }
        if sources.is_empty() {
            return;
        }
        // Sizes from the scan where known; the rest are measured first.
        let full = self.full_root.clone();
        let known: Vec<Option<u64>> = sources
            .iter()
            .map(|p| full.as_ref().and_then(|r| find_node(r, p)).map(|n| n.size))
            .collect();
        let total: u64 = known.iter().flatten().fold(0, |a, b| a.saturating_add(*b));
        let (report_tx, reports) = channel();
        let (answers, answer_rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let target = dest.clone();
        std::thread::spawn(move || {
            let mut w = Worker {
                reports: report_tx,
                answers: answer_rx,
                cancel: worker_cancel,
                mounts: mount_points(),
                always: None,
                bytes: 0,
                last_report: Instant::now(),
                links: HashMap::new(),
                removed: Vec::new(),
            };
            if let Err(why) = w.check_space(&sources, &known, &target, mode) {
                let _ = w.reports.send(Report::Refused(why));
                return;
            }
            let results = w.put_all(&sources, &target, mode);
            let _ = w.reports.send(Report::Progress(w.bytes));
            let _ = w.reports.send(Report::Done(results));
        });
        self.transfer.job = Some(Job {
            mode,
            target: dest,
            total,
            done: 0,
            reports,
            answers,
            cancel,
            clash: None,
            issues: Vec::new(),
        });
    }

    /// Every frame: follows a copy or move, shows its progress and asks
    /// about name clashes.
    pub(crate) fn transfer_ui(&mut self, ctx: &egui::Context) {
        let Some(job) = &mut self.transfer.job else {
            return;
        };
        let mut finished = None;
        let mut issues = Vec::new();
        while let Ok(report) = job.reports.try_recv() {
            match report {
                Report::Progress(b) => job.done = b,
                Report::Clash(c) => job.clash = Some((c, false)),
                Report::Issue(text) => issues.push(text),
                Report::Refused(why) => {
                    issues.push(why.clone());
                    finished = Some(Err(why));
                }
                Report::Done(results) => finished = Some(Ok(results)),
            }
        }
        job.issues.extend(issues.iter().cloned());
        ctx.request_repaint_after(std::time::Duration::from_millis(100));

        // Progress, bottom right.
        let verb = match job.mode {
            ClipMode::Copy => "TRANSFER_COPYING",
            ClipMode::Move => "TRANSFER_MOVING",
        };
        let fraction = job.done as f32 / job.total.max(1) as f32;
        let mut cancel = false;
        egui::Area::new("transfer_progress".into())
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-12.0, -40.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(320.0);
                    ui.label(trf(
                        verb,
                        &[
                            &human_size(job.done.min(job.total)),
                            &human_size(job.total),
                            &short_path(&job.target),
                        ],
                    ));
                    ui.add(egui::ProgressBar::new(fraction.min(1.0)));
                    if ui.button(tr("MENU_CANCEL")).clicked() {
                        cancel = true;
                    }
                });
            });

        // A name clash waits for an answer.
        let mut answer: Option<Option<(ClashChoice, bool)>> = None;
        if let Some((clash, all)) = &mut job.clash {
            let modal = egui::Modal::new("transfer_clash".into()).show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.heading(tr("CLASH_TITLE"));
                ui.add_space(6.0);
                let folder = clash.target.parent().map(short_path).unwrap_or_default();
                ui.label(trf("CLASH_BODY", &[&file_name_of(&clash.target), &folder]));
                ui.add_space(6.0);
                ui.checkbox(all, tr("CLASH_ALL"));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("CLASH_SKIP")).clicked() {
                        answer = Some(Some((ClashChoice::Skip, *all)));
                    }
                    if ui.button(tr("CLASH_KEEP_BOTH")).clicked() {
                        answer = Some(Some((ClashChoice::KeepBoth, *all)));
                    }
                    if clash.can_replace {
                        let key = if clash.merge {
                            "CLASH_MERGE"
                        } else {
                            "CLASH_REPLACE"
                        };
                        let replace =
                            egui::RichText::new(tr(key)).color(ui.visuals().warn_fg_color);
                        if ui.button(replace).clicked() {
                            answer = Some(Some((ClashChoice::Replace, *all)));
                        }
                    }
                    if ui.button(tr("CLASH_CANCEL")).clicked() {
                        answer = Some(None);
                    }
                });
            });
            if modal.should_close() && answer.is_none() {
                answer = Some(None);
            }
        }
        if let Some(a) = answer {
            job.clash = None;
            let _ = job.answers.send(a);
        }
        if cancel {
            job.cancel.store(true, Ordering::Relaxed);
            // A worker waiting on a clash stops too.
            let _ = job.answers.send(None);
        }

        for text in issues {
            self.log_issue(text);
        }
        if let Some(finished) = finished {
            self.finish_transfer(finished);
        }
    }

    /// A copy or move ended (or was refused, with the reason): moved items
    /// leave the tree, the target folder is rescanned to show what arrived,
    /// and the status line says what happened.
    fn finish_transfer(&mut self, finished: Result<Results, String>) {
        let Some(job) = self.transfer.job.take() else {
            return;
        };
        let r = match finished {
            Ok(r) => r,
            // Nothing happened: what was picked stays picked.
            Err(why) => {
                self.status = why;
                return;
            }
        };
        // A move's files are now where they were pasted.
        if job.mode == ClipMode::Move {
            self.transfer.clip = None;
        }
        self.status = if r.cancelled {
            tr("STATUS_TRANSFER_CANCELLED")
        } else {
            let n = r.complete as u64;
            let (done, short) = match job.mode {
                ClipMode::Copy => ("STATUS_COPIED", "STATUS_NOT_ALL_COPIED"),
                ClipMode::Move => ("STATUS_MOVED", "STATUS_NOT_ALL_MOVED"),
            };
            let done = trn(done, n, &[&format_count(n)]);
            match r.incomplete as u64 {
                0 => done,
                bad => trf(
                    "STATUS_WITH_PROBLEMS",
                    &[&done, &trn(short, bad, &[&format_count(bad)])],
                ),
            }
        };
        let status = self.status.clone();
        self.drop_from_tree(&r.removed);
        let in_tree = self
            .full_root
            .as_ref()
            .is_some_and(|r| find_node(r, &job.target).is_some());
        if in_tree && !self.scanning {
            self.rescan_folder(job.target);
            // This message stays, not the rescan's, and so do the problems.
            for issue in job.issues {
                self.log_issue(issue);
            }
            self.status = status.clone();
            self.status_after_rescan = Some(status);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("spacemap-transfer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A worker whose clashes get `choice`; its reports are returned too.
    fn worker(choice: ClashChoice) -> (Worker, Receiver<Report>) {
        let (tx, rx) = channel();
        let (atx, arx) = channel();
        for _ in 0..16 {
            atx.send(Some((choice, false))).unwrap();
        }
        // Kept open by leaking: the worker may ask fewer times.
        std::mem::forget(atx);
        (
            Worker {
                reports: tx,
                answers: arx,
                cancel: Default::default(),
                mounts: HashSet::new(),
                always: None,
                bytes: 0,
                last_report: Instant::now(),
                links: HashMap::new(),
                removed: Vec::new(),
            },
            rx,
        )
    }

    #[test]
    fn copies_folders_with_links_and_keeps_times() {
        let d = scratch("copy");
        let src = d.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/a.txt"), b"hello").unwrap();
        std::os::unix::fs::symlink("sub/a.txt", src.join("link")).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let f = std::fs::File::options()
            .write(true)
            .open(src.join("sub/a.txt"))
            .unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        drop(f);

        let (mut w, _) = worker(ClashChoice::Skip);
        let dst = d.join("dst");
        assert_eq!(w.put(&src, &dst, ClipMode::Copy).ok(), Some(Outcome::Done));
        assert_eq!(std::fs::read(dst.join("sub/a.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read_link(dst.join("link")).unwrap(),
            Path::new("sub/a.txt")
        );
        let copied = std::fs::metadata(dst.join("sub/a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(copied, old);
        assert!(src.exists(), "a copy keeps the source");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn clashes_skip_keep_both_or_replace() {
        let d = scratch("clash");
        std::fs::write(d.join("a.txt"), b"new").unwrap();
        let there = d.join("there");
        std::fs::create_dir(&there).unwrap();
        std::fs::write(there.join("a.txt"), b"old").unwrap();

        let (mut w, _) = worker(ClashChoice::Skip);
        assert_eq!(
            w.put(&d.join("a.txt"), &there.join("a.txt"), ClipMode::Copy)
                .ok(),
            Some(Outcome::Skipped)
        );
        assert_eq!(std::fs::read(there.join("a.txt")).unwrap(), b"old");

        let (mut w, _) = worker(ClashChoice::KeepBoth);
        w.put(&d.join("a.txt"), &there.join("a.txt"), ClipMode::Copy)
            .ok()
            .unwrap();
        assert_eq!(std::fs::read(there.join("a (2).txt")).unwrap(), b"new");

        let (mut w, _) = worker(ClashChoice::Replace);
        w.put(&d.join("a.txt"), &there.join("a.txt"), ClipMode::Copy)
            .ok()
            .unwrap();
        assert_eq!(std::fs::read(there.join("a.txt")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn moves_merge_folders_and_remove_the_source() {
        let d = scratch("move");
        let src = d.join("music");
        std::fs::create_dir_all(src.join("x")).unwrap();
        std::fs::write(src.join("x/1.ogg"), b"1").unwrap();
        let dst = d.join("to/music");
        std::fs::create_dir_all(dst.join("x")).unwrap();
        std::fs::write(dst.join("x/2.ogg"), b"2").unwrap();

        let (mut w, _) = worker(ClashChoice::Replace);
        assert_eq!(w.put(&src, &dst, ClipMode::Move).ok(), Some(Outcome::Done));
        assert!(dst.join("x/1.ogg").exists() && dst.join("x/2.ogg").exists());
        assert!(!src.exists(), "the merged source is gone");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Every path under `p` (relative), with its kind, for comparing trees.
    fn listing(p: &Path) -> Vec<(PathBuf, &'static str)> {
        let mut out = Vec::new();
        let mut stack = vec![p.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().map(|e| e.unwrap()) {
                let ft = e.file_type().unwrap();
                let kind = if ft.is_dir() {
                    stack.push(e.path());
                    "dir"
                } else if ft.is_symlink() {
                    "link"
                } else if ft.is_fifo() {
                    "fifo"
                } else if ft.is_socket() {
                    "socket"
                } else {
                    "file"
                };
                out.push((e.path().strip_prefix(p).unwrap().to_path_buf(), kind));
            }
        }
        out.sort();
        out
    }

    /// A move to another filesystem of a folder holding files, a link, a
    /// named pipe, hard links and a socket (which can't be moved): all but
    /// the socket arrives (the pipe recreated, the hard links still linked)
    /// and leaves the source; only the socket and the folders above it
    /// stay; the removed paths are exactly what left; the outcome says it
    /// wasn't all moved.
    #[test]
    fn move_to_another_filesystem_leaves_only_what_could_not_move() {
        use std::os::unix::fs::FileTypeExt;
        let shm = Path::new("/dev/shm");
        let d = scratch("xfs");
        let Ok(src_root) = std::fs::metadata(shm)
            .map(|_| shm.join(format!("spacemap-xfs-{}", std::process::id())))
        else {
            return;
        };
        let _ = std::fs::remove_dir_all(&src_root);
        if std::fs::metadata(shm).unwrap().dev() == std::fs::metadata(&d).unwrap().dev() {
            eprintln!("skipped: /dev/shm is on the same filesystem as the temp folder");
            return;
        }
        let src = src_root.join("proj");
        std::fs::create_dir_all(src.join("all/deep/er")).unwrap();
        std::fs::create_dir_all(src.join("part/sub")).unwrap();
        std::fs::write(src.join("all/deep/er/f"), vec![1u8; 70_000]).unwrap();
        std::fs::write(src.join("part/a.txt"), b"a").unwrap();
        std::fs::write(src.join("part/sub/b.txt"), b"b").unwrap();
        std::fs::hard_link(src.join("part/a.txt"), src.join("all/a-link")).unwrap();
        std::os::unix::fs::symlink("../part/a.txt", src.join("all/sym")).unwrap();
        let fifo = src.join("all/pipe");
        make_fifo(&fifo, 0o640).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(src.join("part/sock")).unwrap();
        let before = listing(&src);

        let (mut w, reports) = worker(ClashChoice::Skip);
        let dst = d.join("proj");
        assert_eq!(
            w.put(&src, &dst, ClipMode::Move).ok(),
            Some(Outcome::Incomplete)
        );
        let mut arrived = listing(&dst);
        arrived.push((PathBuf::from("part/sock"), "socket"));
        arrived.sort();
        assert_eq!(arrived, before, "everything but the socket arrived");
        assert!(
            std::fs::symlink_metadata(dst.join("all/pipe"))
                .unwrap()
                .file_type()
                .is_fifo()
        );
        assert_eq!(
            std::fs::metadata(dst.join("all/pipe")).unwrap().mode() & 0o777,
            0o640
        );
        let (a, l) = (
            std::fs::metadata(dst.join("part/a.txt")).unwrap(),
            std::fs::metadata(dst.join("all/a-link")).unwrap(),
        );
        assert_eq!((a.ino(), a.nlink()), (l.ino(), 2), "hard links stay linked");
        assert_eq!(
            listing(&src),
            vec![
                (PathBuf::from("part"), "dir"),
                (PathBuf::from("part/sock"), "socket")
            ]
        );
        let mut removed = std::mem::take(&mut w.removed);
        removed.sort();
        assert_eq!(
            removed,
            vec![
                src.join("all"),
                src.join("part/a.txt"),
                src.join("part/sub")
            ],
            "a folder that left entirely is listed instead of what it held"
        );
        let issues: Vec<String> = reports
            .try_iter()
            .filter_map(|r| match r {
                Report::Issue(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("sock"));
        let _ = std::fs::remove_dir_all(&src_root);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A merge into a folder with a read-only part: what can't go there
    /// stays at the source, with a note, and the rest moves.
    #[test]
    fn move_into_a_read_only_part_keeps_that_part() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("readonly");
        let src = d.join("m");
        std::fs::create_dir_all(src.join("locked")).unwrap();
        std::fs::create_dir_all(src.join("open")).unwrap();
        std::fs::write(src.join("locked/x"), b"x").unwrap();
        std::fs::write(src.join("open/y"), b"y").unwrap();
        let dst = d.join("to/m");
        std::fs::create_dir_all(dst.join("locked")).unwrap();
        std::fs::create_dir_all(dst.join("open")).unwrap();
        std::fs::set_permissions(dst.join("locked"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let (mut w, reports) = worker(ClashChoice::Replace);
        let outcome = w.put(&src, &dst, ClipMode::Move).ok();
        std::fs::set_permissions(dst.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        if std::fs::write(dst.join("locked/probe"), b"").is_ok() && outcome == Some(Outcome::Done) {
            // Running as root: permissions don't stop anything.
            let _ = std::fs::remove_dir_all(&d);
            return;
        }
        assert_eq!(outcome, Some(Outcome::Incomplete));
        assert!(dst.join("open/y").exists() && !src.join("open").exists());
        assert!(src.join("locked/x").exists(), "what couldn't move stays");
        assert!(
            reports
                .try_iter()
                .any(|r| matches!(r, Report::Issue(t) if t.contains("locked/x")))
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Hard links inside a copied folder stay linked in the copy (one copy
    /// of the data), also with a third name outside what was copied.
    #[test]
    fn copies_keep_hard_links_between_copied_files() {
        let d = scratch("links");
        let src = d.join("src");
        std::fs::create_dir_all(src.join("a")).unwrap();
        std::fs::create_dir_all(src.join("b")).unwrap();
        std::fs::write(src.join("a/one"), vec![5u8; 100_000]).unwrap();
        std::fs::hard_link(src.join("a/one"), src.join("b/two")).unwrap();
        std::fs::hard_link(src.join("a/one"), d.join("outside")).unwrap();
        std::fs::write(src.join("b/plain"), b"p").unwrap();
        let (mut w, _) = worker(ClashChoice::Skip);
        let dst = d.join("dst");
        assert_eq!(w.put(&src, &dst, ClipMode::Copy).ok(), Some(Outcome::Done));
        let (one, two) = (
            std::fs::metadata(dst.join("a/one")).unwrap(),
            std::fs::metadata(dst.join("b/two")).unwrap(),
        );
        assert_eq!(one.ino(), two.ino());
        assert_eq!(one.nlink(), 2);
        assert_ne!(
            one.ino(),
            std::fs::metadata(src.join("a/one")).unwrap().ino()
        );
        assert_eq!(
            std::fs::read(dst.join("b/two")).unwrap(),
            vec![5u8; 100_000]
        );
        assert_eq!(std::fs::metadata(src.join("a/one")).unwrap().nlink(), 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Too little room refuses before anything is written; a move within
    /// one filesystem needs no room; sizes the scan doesn't know are
    /// measured, hard links once.
    #[test]
    fn space_is_checked_before_anything_is_written() {
        let d = scratch("space");
        let src = d.join("big");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("f"), vec![1u8; 3 << 20]).unwrap();
        std::fs::hard_link(src.join("f"), src.join("g")).unwrap();
        let mut seen = HashSet::new();
        let used = disk_usage(&src, &HashSet::new(), &mut seen);
        assert!((3 << 20..(3 << 20) + 65_536).contains(&used), "{used}");
        let (w, _) = worker(ClashChoice::Skip);
        let target = d.join("to");
        std::fs::create_dir_all(&target).unwrap();
        let huge = [Some(u64::MAX)];
        assert!(
            w.check_space(std::slice::from_ref(&src), &huge, &target, ClipMode::Copy)
                .is_err()
        );
        assert!(
            w.check_space(std::slice::from_ref(&src), &huge, &target, ClipMode::Move)
                .is_ok()
        );
        assert!(
            w.check_space(std::slice::from_ref(&src), &[None], &target, ClipMode::Copy)
                .is_ok()
        );
        assert!(listing(&target).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// File lists as programs write them: either line ending, comments,
    /// "localhost" or no host, another host (skipped), bytes that aren't
    /// UTF-8, an escaped newline, a literal "%", web links (skipped), blank
    /// and broken lines.
    #[test]
    fn uri_lists_keep_every_byte() {
        use std::os::unix::ffi::OsStrExt;
        let list = b"# a comment\r\n\
            file:///tmp/plain\r\n\
            file://localhost/tmp/with%20space\n\
            file:/tmp/one%2Fslash\n\
            file:///tmp/bad%FF%FEname\r\n\
            file:///tmp/line%0Abreak\n\
            file:///tmp/100%\n\
            file://otherhost/tmp/remote\n\
            https://example.com/x\n\
            \r\n\
            file://\n\
            file:///tmp/last";
        let got: Vec<Vec<u8>> = parse_uri_list(list)
            .iter()
            .map(|p| p.as_os_str().as_bytes().to_vec())
            .collect();
        let want: Vec<&[u8]> = vec![
            b"/tmp/plain",
            b"/tmp/with space",
            b"/tmp/one/slash",
            b"/tmp/bad\xff\xfename",
            b"/tmp/line\nbreak",
            b"/tmp/100%",
            b"/tmp/last",
        ];
        assert_eq!(got, want);
        // What spacemap offers reads back as the same paths.
        let odd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/a b\n\xff%c\\d"));
        let formats = clipboard_formats(std::slice::from_ref(&odd), ClipMode::Move);
        let uris = &formats.iter().find(|f| f.0 == "text/uri-list").unwrap().1;
        assert!(uris.ends_with(b"\r\n"));
        assert_eq!(parse_uri_list(uris), vec![odd]);
        let gnome = &formats
            .iter()
            .find(|f| f.0 == "x-special/gnome-copied-files")
            .unwrap()
            .1;
        assert!(gnome.starts_with(b"cut\n"));
    }

    /// Needs an X11 display (run under Xvfb): files offered on the X11
    /// clipboard read back exactly in every format.
    #[test]
    #[ignore]
    fn x11_clipboard_round_trip() {
        use std::os::unix::ffi::OsStrExt;
        let paths = vec![
            PathBuf::from("/tmp/x y"),
            PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff")),
        ];
        let formats = clipboard_formats(&paths, ClipMode::Move);
        assert!(crate::x11clip::offer(formats.clone()));
        for (name, data) in &formats {
            assert_eq!(crate::x11clip::read(name).as_ref(), Some(data), "{name}");
        }
        assert_eq!(
            parse_uri_list(&crate::x11clip::read("text/uri-list").unwrap()),
            paths
        );
        assert_eq!(crate::x11clip::read("image/png"), None);
        // A big list (a few MB) comes through whole.
        let many: Vec<PathBuf> = (0..60_000)
            .map(|i| PathBuf::from(format!("/tmp/some/longer/folder/name/file-{i:06}.dat")))
            .collect();
        assert!(crate::x11clip::offer(clipboard_formats(
            &many,
            ClipMode::Copy
        )));
        assert_eq!(
            parse_uri_list(&crate::x11clip::read("text/uri-list").unwrap()),
            many
        );
    }

    #[test]
    fn free_names_and_pasted_text() {
        let d = scratch("names");
        std::fs::write(d.join("a.txt"), b"").unwrap();
        std::fs::write(d.join("a (2).txt"), b"").unwrap();
        assert_eq!(free_name(&d.join("a.txt")), d.join("a (3).txt"));
        std::fs::create_dir(d.join("dir.v1")).unwrap();
        assert_eq!(free_name(&d.join("dir.v1")), d.join("dir.v1 (2)"));
        std::fs::write(d.join("with space"), b"").unwrap();
        let text = format!(
            "file://{}/with%20space\n{}/a.txt\n",
            d.display(),
            d.display()
        );
        assert_eq!(
            paths_in_text(&text),
            Some(vec![d.join("with space"), d.join("a.txt")])
        );
        assert_eq!(paths_in_text("hello"), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
