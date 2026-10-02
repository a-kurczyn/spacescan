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
use crate::delete::{find_node, mount_guard, remove_dir_one_fs};
use std::os::unix::fs::MetadataExt;
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

/// What the worker reports.
enum Report {
    /// Bytes done so far.
    Progress(u64),
    Clash(Clash),
    Issue(String),
    /// Finished (or cancelled): the sources that were moved away, and how
    /// many items were put in place.
    Done {
        moved: Vec<PathBuf>,
        placed: usize,
        cancelled: bool,
    },
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
}

#[derive(Default)]
pub(crate) struct Transfer {
    pub clip: Option<Clip>,
    job: Option<Job>,
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

    /// Puts `src` at `dst`. True if all of it got there (so a move can
    /// remove the source).
    fn put(&mut self, src: &Path, dst: &Path, mode: ClipMode) -> Result<bool, Stop> {
        self.check_cancel()?;
        let meta = match std::fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.issue(trf("ERR_COPY_FAILED", &[&show_path(src), &io_reason(&e)]));
                return Ok(false);
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
                ClashChoice::Skip => return Ok(false),
                ClashChoice::KeepBoth => dst = free_name(&dst),
                ClashChoice::Replace if merge => return self.merge(src, &dst, mode),
                ClashChoice::Replace => {}
            }
        }
        if mode == ClipMode::Move {
            match std::fs::rename(src, &dst) {
                Ok(()) => {
                    self.add_bytes(meta.len());
                    return Ok(true);
                }
                // Another filesystem: copy, then remove the source.
                Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {}
                Err(e) => {
                    self.issue(trf("ERR_MOVE_FAILED", &[&show_path(src), &io_reason(&e)]));
                    return Ok(false);
                }
            }
        }
        let complete = self.copy(src, &dst, &meta)?;
        if mode == ClipMode::Move && complete {
            let removed = if meta.is_dir() {
                remove_dir_one_fs(src, &self.mounts)
            } else {
                std::fs::remove_file(src)
            };
            if let Err(e) = removed {
                self.issue(trf("ERR_MOVE_FAILED", &[&show_path(src), &io_reason(&e)]));
                return Ok(false);
            }
        }
        Ok(complete)
    }

    /// Puts the contents of folder `src` into the folder `dst` that's
    /// already there; a move then removes `src` if it's empty.
    fn merge(&mut self, src: &Path, dst: &Path, mode: ClipMode) -> Result<bool, Stop> {
        let entries = match std::fs::read_dir(src) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect::<Vec<_>>(),
            Err(e) => {
                self.issue(trf("ERR_COPY_FAILED", &[&show_path(src), &io_reason(&e)]));
                return Ok(false);
            }
        };
        let mut complete = true;
        for entry in entries {
            let name = entry.file_name();
            complete &= deep(|| self.put(&src.join(&name), &dst.join(&name), mode))?;
        }
        if mode == ClipMode::Move && complete {
            let _ = std::fs::remove_dir(src);
        }
        Ok(complete)
    }

    /// Copies `src` (described by `meta`) to the new path `dst`, keeping
    /// permissions and times. True if everything was copied.
    fn copy(&mut self, src: &Path, dst: &Path, meta: &std::fs::Metadata) -> Result<bool, Stop> {
        self.check_cancel()?;
        let ft = meta.file_type();
        let failed = |w: &Self, e: std::io::Error| {
            w.issue(trf("ERR_COPY_FAILED", &[&show_path(src), &io_reason(&e)]));
            Ok(false)
        };
        if ft.is_symlink() {
            let result = std::fs::read_link(src).and_then(|target| {
                if std::fs::symlink_metadata(dst).is_ok() {
                    std::fs::remove_file(dst)?;
                }
                std::os::unix::fs::symlink(target, dst)
            });
            return match result {
                Ok(()) => Ok(true),
                Err(e) => failed(self, e),
            };
        }
        if ft.is_dir() {
            if self.mounts.contains(src) {
                self.issue(trf("ERR_OTHER_FS_SKIPPED", &[&show_path(src)]));
                return Ok(false);
            }
            if let Err(e) = std::fs::create_dir(dst) {
                return failed(self, e);
            }
            let entries = match std::fs::read_dir(src) {
                Ok(rd) => rd.filter_map(|e| e.ok()).collect::<Vec<_>>(),
                Err(e) => return failed(self, e),
            };
            let mut complete = true;
            for entry in entries {
                let child = entry.path();
                match entry.metadata() {
                    Ok(m) => {
                        complete &= deep(|| self.copy(&child, &dst.join(entry.file_name()), &m))?
                    }
                    Err(e) => {
                        complete = false;
                        let _ = failed(self, e);
                    }
                }
            }
            let _ = std::fs::set_permissions(dst, meta.permissions());
            keep_times(dst, meta);
            return Ok(complete);
        }
        if !ft.is_file() {
            self.issue(trf("ERR_SPECIAL_FILE", &[&show_path(src)]));
            return Ok(false);
        }
        match self.copy_file(src, dst) {
            Ok(()) => {
                let _ = std::fs::set_permissions(dst, meta.permissions());
                keep_times(dst, meta);
                Ok(true)
            }
            Err(Some(e)) => {
                let _ = std::fs::remove_file(dst);
                failed(self, e)
            }
            Err(None) => {
                // Cancelled partway: no half-written file is left behind.
                let _ = std::fs::remove_file(dst);
                Err(Stop)
            }
        }
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

/// Gives `dst` the modified and accessed times in `meta`.
fn keep_times(dst: &Path, meta: &std::fs::Metadata) {
    let (Ok(modified), Ok(accessed)) = (meta.modified(), meta.accessed()) else {
        return;
    };
    if let Ok(f) = std::fs::File::open(dst) {
        let times = std::fs::FileTimes::new()
            .set_modified(modified)
            .set_accessed(accessed);
        let _ = f.set_times(times);
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
                PathBuf::from(std::ffi::OsString::from_vec(percent_decode(rest)))
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

/// `s` with %XX escapes turned back into bytes.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
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
    /// True while a copy or move runs (other changes to files wait).
    pub(crate) fn transfer_busy(&self) -> bool {
        self.transfer.job.is_some()
    }

    /// Ctrl+C / Ctrl+X: remembers `paths` for Ctrl+V, and (if set) puts
    /// them on the clipboard as text.
    pub(crate) fn clip(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, mode: ClipMode) {
        if paths.is_empty() {
            return;
        }
        if self.settings.paths_to_clipboard {
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
        self.status = trf(key, &[&format_count(paths.len() as u64)]);
        self.transfer.clip = Some(Clip { paths, mode });
    }

    /// Ctrl+V into folder `dest`: what Ctrl+C or Ctrl+X picked, or else
    /// files named in the pasted `text` (copied in a file manager).
    pub(crate) fn paste_into(&mut self, dest: PathBuf, text: &str) {
        if self.transfer_busy() {
            self.status = tr("STATUS_TRANSFER_BUSY");
            return;
        }
        let (paths, mode) = match &self.transfer.clip {
            Some(clip) => (clip.paths.clone(), clip.mode),
            None => match paths_in_text(text) {
                Some(paths) => (paths, ClipMode::Copy),
                None => {
                    self.status = tr("STATUS_PASTE_NOTHING");
                    return;
                }
            },
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
        // Sizes from the scan where known; a guide for progress and space.
        let full = self.full_root.clone();
        let size_of = |p: &Path| {
            full.as_ref()
                .and_then(|r| find_node(r, p))
                .map(|n| n.size)
                .unwrap_or_else(|| std::fs::symlink_metadata(p).map_or(0, |m| m.len()))
        };
        let total: u64 = sources
            .iter()
            .map(|p| size_of(p))
            .fold(0, u64::saturating_add);
        // A move within one filesystem needs no space.
        let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
        let needs: u64 = sources
            .iter()
            .filter(|p| mode == ClipMode::Copy || dev(p) != dev(&dest))
            .map(|p| size_of(p))
            .fold(0, u64::saturating_add);
        if let Some((_, free)) = fs_space(&dest)
            && needs > free
        {
            self.log_issue(trf(
                "ERR_NO_SPACE",
                &[&show_path(&dest), &human_size(needs), &human_size(free)],
            ));
            return;
        }

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
            };
            let mut moved = Vec::new();
            let mut placed = 0;
            let mut cancelled = false;
            for src in &sources {
                let dst = target.join(src.file_name().unwrap_or_default());
                match w.put(src, &dst, mode) {
                    Ok(complete) => {
                        placed += 1;
                        if mode == ClipMode::Move && complete {
                            moved.push(src.clone());
                        }
                    }
                    Err(Stop) => {
                        cancelled = true;
                        break;
                    }
                }
            }
            let _ = w.reports.send(Report::Progress(w.bytes));
            let _ = w.reports.send(Report::Done {
                moved,
                placed,
                cancelled,
            });
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
        });
        // A move's files are now where they were pasted.
        if mode == ClipMode::Move {
            self.transfer.clip = None;
        }
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
                Report::Done {
                    moved,
                    placed,
                    cancelled,
                } => finished = Some((moved, placed, cancelled)),
            }
        }
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
        if let Some((moved, placed, cancelled)) = finished {
            self.finish_transfer(moved, placed, cancelled);
        }
    }

    /// A copy or move ended: moved items leave the tree, and the target
    /// folder is rescanned to show what arrived.
    fn finish_transfer(&mut self, moved: Vec<PathBuf>, placed: usize, cancelled: bool) {
        let Some(job) = self.transfer.job.take() else {
            return;
        };
        let count = format_count(placed as u64);
        self.status = if cancelled {
            tr("STATUS_TRANSFER_CANCELLED")
        } else {
            match job.mode {
                ClipMode::Copy => trf("STATUS_COPIED", &[&count]),
                ClipMode::Move => trf("STATUS_MOVED", &[&count]),
            }
        };
        let status = self.status.clone();
        self.drop_from_tree(&moved);
        let in_tree = self
            .full_root
            .as_ref()
            .is_some_and(|r| find_node(r, &job.target).is_some());
        if in_tree && !self.scanning {
            self.rescan_folder(job.target);
            // This message stays, not the rescan's.
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
        assert!(w.put(&src, &dst, ClipMode::Copy).ok().unwrap());
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
        assert!(
            !w.put(&d.join("a.txt"), &there.join("a.txt"), ClipMode::Copy)
                .ok()
                .unwrap()
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
        assert!(w.put(&src, &dst, ClipMode::Move).ok().unwrap());
        assert!(dst.join("x/1.ogg").exists() && dst.join("x/2.ogg").exists());
        assert!(!src.exists(), "the merged source is gone");
        let _ = std::fs::remove_dir_all(&d);
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
