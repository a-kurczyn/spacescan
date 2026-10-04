//! Deleting, moving to the trash and emptying the trash, for the chart and
//! the table. What's removed is dropped from the scanned tree, and the
//! folders above shrink, without a rescan. Permanent deletes and emptying
//! the trash ask for confirmation first; moving to the trash doesn't.

use super::*;
use std::sync::mpsc::channel;

// Mount safety: deleting or trashing must never reach into another mounted
// filesystem (a network share, a USB drive, a bind mount). A target that
// is or contains a mount point is refused, and the recursive delete also
// stops at any folder on another device.

/// Mount points at or below `path`.
fn mounts_at_or_under(path: &Path) -> Vec<PathBuf> {
    let mut mounts: Vec<PathBuf> = mount_points()
        .into_iter()
        .filter(|m| m.starts_with(path))
        .collect();
    mounts.sort();
    mounts
}

/// Err (for the Issues log) if deleting or trashing `path` would touch a
/// mounted filesystem: it is a mount point, or has one somewhere inside.
/// A symlink is only ever removed itself, so it's always fine.
pub(crate) fn mount_guard(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => return Ok(()),
        Ok(m) if !m.is_dir() => return Ok(()),
        Ok(_) => {}
        Err(_) => return Ok(()), // the delete itself reports this
    }
    // Compare against the mount table in its own (canonical) form.
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mounts = mounts_at_or_under(&canonical);
    if mounts.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = mounts.iter().map(|m| show_path(m)).collect();
    Err(trf(
        "ERR_CONTAINS_MOUNT",
        &[&show_path(path), &list.join(", ")],
    ))
}

/// Err (for the Issues log) if `path` is inside a trash folder or contains
/// one: trashing it would lose the trash's restore information.
fn trash_guard(path: &Path) -> Result<(), String> {
    let Ok(folders) = trash::os_limited::trash_folders() else {
        return Ok(());
    };
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let canonical_parent = path.parent().and_then(|p| std::fs::canonicalize(p).ok());
    for f in folders {
        let f = std::fs::canonicalize(&f).unwrap_or(f);
        // A symlink inside the trash canonicalizes to its target, so its
        // parent is checked too.
        let inside = canonical.starts_with(&f)
            || canonical_parent.as_ref().is_some_and(|p| p.starts_with(&f));
        if inside || f.starts_with(&canonical) {
            return Err(trf("ERR_TRASH_IN_TRASH", &[&show_path(path)]));
        }
    }
    Ok(())
}

/// Plain-language reason a move to the trash failed.
fn trash_reason(e: &trash::Error) -> String {
    match e {
        trash::Error::FileSystem { source, .. } => io_reason(source),
        trash::Error::TargetedRoot => tr("ERR_TRASH_ROOT"),
        trash::Error::Unknown { description } => trf("ERR_IO_OTHER", &[description]),
        other => trf("ERR_IO_OTHER", &[&other.to_string()]),
    }
}

/// Like `remove_dir_all`, but stops with an error at a folder where another
/// filesystem is mounted (one of `mounts`), and works in trees deeper than
/// the path length limit. btrfs subvolumes inside are removed like folders.
pub(crate) fn remove_dir_one_fs(dir: &Path, mounts: &HashSet<PathBuf>) -> std::io::Result<()> {
    remove_dir_in(dir, &None, mounts)
}

/// `dir`'s parent folder is open as `parent` when the path is long.
fn remove_dir_in(dir: &Path, parent: &DirHandle, mounts: &HashSet<PathBuf>) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let open_at = openable(dir, parent);
    // Keep this folder open while its entries' paths may be too long to
    // use directly.
    let handle: DirHandle = if dir.as_os_str().len() + 256 > LONG_PATH {
        Some(std::fs::File::open(&open_at)?)
    } else {
        None
    };
    let listing = match &handle {
        Some(f) => PathBuf::from(format!("/proc/self/fd/{}", f.as_raw_fd())),
        None => open_at.clone(),
    };
    for entry in std::fs::read_dir(&listing)? {
        let entry = entry?;
        let child = dir.join(entry.file_name());
        // Not following symlinks: a link is removed, never its target.
        let m = entry.metadata()?;
        if m.is_dir() {
            if mounts.contains(&child) {
                return Err(std::io::Error::other(trf(
                    "ERR_OTHER_FS_INSIDE",
                    &[&show_path(&child)],
                )));
            }
            deep(|| remove_dir_in(&child, &handle, mounts))?;
        } else {
            std::fs::remove_file(openable(&child, &handle))?;
        }
    }
    drop(handle);
    std::fs::remove_dir(open_at)
}

/// What the confirmation dialog is asking about.
#[derive(Clone)]
enum Confirm {
    /// Deleting (`permanent`) or moving to the trash.
    Delete {
        paths: Vec<PathBuf>,
        permanent: bool,
        /// Real totals of what goes, including files the filter hides.
        size: u64,
        file_count: u64,
        /// (files, size) inside the targets that the active filter or
        /// category hides: they go too.
        hidden: (u64, u64),
        /// Folders inside that the scan couldn't read: their size is
        /// unknown and deleting them may fail partway.
        unreadable: usize,
        /// Whether the single item is a folder; None for several.
        single_is_dir: Option<bool>,
    },
    EmptyTrash(Vec<trash::TrashItem>),
}

#[derive(Default)]
pub(crate) struct Removal {
    confirm: Option<Confirm>,
    /// Deletes (true = permanent, false = to the trash) to carry out at the
    /// start of the next frame, when the tree can be edited in place.
    pending: Option<(Vec<PathBuf>, bool)>,
    /// The result of emptying the trash, which runs on its own thread.
    purge_rx: Option<Receiver<Result<(), String>>>,
}

/// The dialog's warning that files hidden by the active filter go too
/// (`key` names the delete or trash wording); nothing if none are hidden.
fn hidden_warning(ui: &mut egui::Ui, key: &str, (files, size): (u64, u64)) {
    if files == 0 {
        return;
    }
    ui.add_space(6.0);
    let text = trf(key, &[&format_count(files), &human_size(size)]);
    ui.label(
        egui::RichText::new(text)
            .strong()
            .color(ui.visuals().warn_fg_color),
    );
}

/// Each of `folders` (in the tree under `root`) takes its own size from the
/// disk again, counted as the scan counted it (`apparent`: lengths, else
/// disk space), and the folders above change by as much.
pub(crate) fn refresh_own_sizes(root: &mut Node, folders: &[&Path], apparent: bool) {
    use std::os::unix::fs::MetadataExt;
    fn adjust(n: &mut Node, parts: &[&std::ffi::OsStr], now: u64) -> i128 {
        let delta = match parts.split_first() {
            None => {
                let held = n
                    .children
                    .iter()
                    .fold(0u64, |t, c| t.saturating_add(c.size));
                i128::from(now) - i128::from(n.size.saturating_sub(held))
            }
            Some((first, rest)) => match child_named(n, first) {
                Some(i) => deep(|| adjust(&mut n.children[i], rest, now)),
                None => return 0,
            },
        };
        n.size = u64::try_from((i128::from(n.size) + delta).max(0)).unwrap_or(u64::MAX);
        delta
    }
    let root_path = root.path();
    for dir in folders {
        let Ok(m) = std::fs::symlink_metadata(dir) else {
            continue;
        };
        let now = if apparent { m.len() } else { m.blocks() * 512 };
        if let Some(parts) = rel_parts(&root_path, dir) {
            adjust(root, &parts, now);
        }
    }
}

/// Removes every one of `gone` found in the tree under `root`, a folder's
/// entries in one pass (removing thousands from one folder stays quick),
/// and subtracts their sizes and file counts from the folders above.
pub(crate) fn remove_all_from_tree(root: &mut Node, gone: &[PathBuf]) {
    fn remove_in(
        node: &mut Node,
        parts: &[&std::ffi::OsStr],
        names: &HashSet<&std::ffi::OsStr>,
    ) -> (u64, u64) {
        let removed = match parts.split_first() {
            None => {
                let mut kids = std::mem::take(&mut node.children).into_vec();
                let mut removed = (0u64, 0u64);
                kids.retain(|c| {
                    let go = names.contains(c.disk_name());
                    if go {
                        removed = (removed.0.saturating_add(c.size), removed.1 + c.file_count);
                    }
                    !go
                });
                node.children = kids.into_boxed_slice();
                removed
            }
            Some((first, rest)) => match child_named(node, first) {
                Some(i) => deep(|| remove_in(&mut node.children[i], rest, names)),
                None => return (0, 0),
            },
        };
        node.size = node.size.saturating_sub(removed.0);
        node.file_count = node.file_count.saturating_sub(removed.1);
        removed
    }
    let mut by_folder: HashMap<&Path, HashSet<&std::ffi::OsStr>> = HashMap::new();
    for p in gone {
        if let (Some(dir), Some(name)) = (p.parent(), p.file_name()) {
            by_folder.entry(dir).or_default().insert(name);
        }
    }
    let root_path = root.path();
    for (dir, names) in by_folder {
        if let Some(parts) = rel_parts(&root_path, dir) {
            remove_in(root, &parts, &names);
        }
    }
}

/// The node at `path`, if it's in the tree.
pub(crate) fn find_node<'a>(root: &'a Node, path: &Path) -> Option<&'a Node> {
    let mut n = root;
    let root_path = root.path();
    for name in rel_parts(&root_path, path)? {
        n = &n.children[child_named(n, name)?];
    }
    Some(n)
}

impl DiskScanApp {
    /// Start of every frame: carries out a delete queued last frame and
    /// picks up a finished trash purge.
    pub(crate) fn removal_frame_start(&mut self, ctx: &egui::Context) {
        if let Some((paths, permanent)) = self.removal.pending.take() {
            self.remove_paths(paths, permanent);
        }
        if let Some(rx) = &self.removal.purge_rx {
            match rx.try_recv() {
                Ok(result) => {
                    self.removal.purge_rx = None;
                    match result {
                        Ok(()) => self.drop_trash_from_tree(),
                        Err(e) => self.log_issue(trf("ERR_EMPTY_TRASH", &[&e])),
                    }
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100))
                }
                Err(TryRecvError::Disconnected) => self.removal.purge_rx = None,
            }
        }
    }

    pub(crate) fn delete_dialog_open(&self) -> bool {
        self.removal.confirm.is_some()
    }

    /// Opens the delete confirmation for `paths`; the delete itself
    /// happens once confirmed.
    pub(crate) fn ask_delete(&mut self, paths: Vec<PathBuf>) {
        if !self.mount_check(&paths) {
            return;
        }
        if let Some(confirm) = self.removal_confirm(&paths, true) {
            self.removal.confirm = Some(confirm);
        }
    }

    /// The confirmation for removing `paths`, with their real totals (from
    /// the unfiltered scan) and what the filter hides inside them. None if
    /// none of them is in the tree.
    fn removal_confirm(&self, paths: &[PathBuf], permanent: bool) -> Option<Confirm> {
        let root = self.root.as_ref()?;
        let full = self.full_root.as_ref().unwrap_or(root);
        let shown: Vec<&Node> = paths.iter().filter_map(|p| find_node(root, p)).collect();
        if shown.is_empty() {
            return None;
        }
        let real: Vec<&Node> = shown
            .iter()
            .map(|n| find_node(full, &n.path()).unwrap_or(n))
            .collect();
        let total = |nodes: &[&Node]| {
            let size = nodes.iter().map(|n| n.size).fold(0u64, u64::saturating_add);
            (nodes.iter().map(|n| n.file_count).sum::<u64>(), size)
        };
        let ((shown_files, shown_size), (files, size)) = (total(&shown), total(&real));
        Some(Confirm::Delete {
            paths: shown.iter().map(|n| n.path()).collect(),
            permanent,
            size,
            file_count: files,
            hidden: (
                files.saturating_sub(shown_files),
                size.saturating_sub(shown_size),
            ),
            unreadable: self
                .unreadable
                .iter()
                .filter(|u| shown.iter().any(|n| u.starts_with(n.path())))
                .count(),
            single_is_dir: (shown.len() == 1).then(|| shown[0].is_dir),
        })
    }

    pub(crate) fn queue_trash(&mut self, paths: Vec<PathBuf>) {
        let mut ok = self.mount_check(&paths);
        for p in &paths {
            if let Err(e) = trash_guard(p) {
                self.log_issue(e);
                ok = false;
            }
        }
        if paths.is_empty() || !ok {
            return;
        }
        // With files hidden by a filter inside, ask first: they go too.
        match self.removal_confirm(&paths, false) {
            Some(
                confirm @ Confirm::Delete {
                    hidden: (hidden_files, _),
                    ..
                },
            ) if hidden_files > 0 => {
                self.removal.confirm = Some(confirm);
            }
            _ => self.removal.pending = Some((paths, false)),
        }
    }

    /// Logs why any of `paths` can't be deleted or trashed; true if none is
    /// blocked.
    fn mount_check(&mut self, paths: &[PathBuf]) -> bool {
        let mut ok = true;
        for p in paths {
            if let Err(e) = mount_guard(p) {
                self.log_issue(e);
                ok = false;
            }
        }
        ok
    }

    /// The toolbar's 🗑: asks to empty every trash folder, on every drive,
    /// unless something is mounted inside one.
    pub(crate) fn ask_empty_trash(&mut self) {
        if self.removal.purge_rx.is_some() {
            return; // already emptying
        }
        if let Ok(folders) = trash::os_limited::trash_folders() {
            let blocked: Vec<PathBuf> = folders.iter().map(|f| f.join("files")).collect();
            if !self.mount_check(&blocked) {
                return;
            }
        }
        match trash::os_limited::list() {
            Ok(items) if items.is_empty() => self.status = tr("STATUS_TRASH_EMPTY"),
            Ok(items) => self.removal.confirm = Some(Confirm::EmptyTrash(items)),
            Err(e) => self.log_issue(trf("ERR_EMPTY_TRASH", &[&trash_reason(&e)])),
        }
    }

    /// The confirmation dialog, when one is pending.
    pub(crate) fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.removal.confirm.clone() else {
            return;
        };
        let modal = egui::Modal::new("confirm_removal".into()).show(ctx, |ui| {
            ui.set_max_width(480.0);
            let yes_label = match &confirm {
                Confirm::Delete {
                    paths,
                    permanent: false,
                    size,
                    file_count,
                    hidden,
                    ..
                } => {
                    ui.heading(tr("TRASH_FILTER_TITLE"));
                    ui.add_space(6.0);
                    for p in paths.iter().take(8) {
                        ui.label(egui::RichText::new(short_path(p)).monospace());
                    }
                    ui.label(trf(
                        "REMOVE_TOTALS",
                        &[&human_size(*size), &format_count(*file_count)],
                    ));
                    hidden_warning(ui, "TRASH_HIDDEN_WARNING", *hidden);
                    tr("MENU_TRASH")
                }
                Confirm::Delete {
                    paths,
                    size,
                    file_count,
                    hidden,
                    unreadable,
                    single_is_dir,
                    ..
                } => {
                    ui.heading(tr("DELETE_CONFIRM_TITLE"));
                    ui.add_space(6.0);
                    match single_is_dir {
                        Some(is_dir) => {
                            ui.label(egui::RichText::new(short_path(&paths[0])).monospace());
                            let link = std::fs::read_link(&paths[0]).ok();
                            ui.label(match (&link, *is_dir) {
                                // Removing a link never touches its target.
                                (Some(target), _) => {
                                    trf("DELETE_CONFIRM_LINK", &[&show_path(target)])
                                }
                                (None, true) => trf(
                                    "DELETE_CONFIRM_DIR",
                                    &[&human_size(*size), &format_count(*file_count)],
                                ),
                                (None, false) => trf("DELETE_CONFIRM_FILE", &[&human_size(*size)]),
                            });
                        }
                        None => {
                            ui.label(trf(
                                "DELETE_CONFIRM_MANY",
                                &[
                                    &format_count(paths.len() as u64),
                                    &human_size(*size),
                                    &format_count(*file_count),
                                ],
                            ));
                            const SHOWN: usize = 8;
                            for p in paths.iter().take(SHOWN) {
                                ui.label(egui::RichText::new(file_name_of(p)).monospace());
                            }
                            if paths.len() > SHOWN {
                                ui.weak(trf(
                                    "DELETE_CONFIRM_MORE",
                                    &[&format_count((paths.len() - SHOWN) as u64)],
                                ));
                            }
                        }
                    }
                    hidden_warning(ui, "DELETE_HIDDEN_WARNING", *hidden);
                    if *unreadable > 0 {
                        ui.add_space(4.0);
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            trf(
                                "DELETE_CONFIRM_UNREADABLE",
                                &[&format_count(*unreadable as u64)],
                            ),
                        );
                    }
                    tr("DELETE_CONFIRM_YES")
                }
                Confirm::EmptyTrash(items) => {
                    ui.heading(tr("TRASH_CONFIRM_TITLE"));
                    ui.add_space(6.0);
                    ui.label(trn(
                        "TRASH_CONFIRM_BODY",
                        items.len() as u64,
                        &[&format_count(items.len() as u64)],
                    ));
                    tr("TRASH_CONFIRM_YES")
                }
            };
            ui.add_space(10.0);
            let mut choice = None;
            ui.horizontal(|ui| {
                // Cancel comes first and has focus, so Enter never deletes.
                let cancel = ui.button(tr("MENU_CANCEL"));
                if !ui.memory(|m| m.focused().is_some()) {
                    cancel.request_focus();
                }
                if cancel.clicked() {
                    choice = Some(false);
                }
                if ui
                    .button(egui::RichText::new(yes_label).color(ui.visuals().error_fg_color))
                    .clicked()
                {
                    choice = Some(true);
                }
            });
            choice
        });
        let mut choice = modal.inner;
        if choice.is_none() && modal.should_close() {
            choice = Some(false);
        }
        match choice {
            Some(true) => {
                self.removal.confirm = None;
                match confirm {
                    Confirm::Delete {
                        paths, permanent, ..
                    } => self.removal.pending = Some((paths, permanent)),
                    Confirm::EmptyTrash(items) => {
                        // Checked again: something may have been mounted meanwhile.
                        if let Ok(folders) = trash::os_limited::trash_folders() {
                            let files: Vec<PathBuf> =
                                folders.iter().map(|f| f.join("files")).collect();
                            if !self.mount_check(&files) {
                                return;
                            }
                        }
                        let (tx, rx) = channel();
                        std::thread::spawn(move || {
                            let _ = tx.send(
                                trash::os_limited::purge_all(&items).map_err(|e| trash_reason(&e)),
                            );
                        });
                        self.removal.purge_rx = Some(rx);
                        self.status = tr("STATUS_EMPTYING_TRASH");
                    }
                }
                ctx.request_repaint();
            }
            Some(false) => self.removal.confirm = None,
            None => {}
        }
    }

    /// Deletes `paths` from disk (permanently, or to the trash), then drops
    /// the ones that went from the tree.
    fn remove_paths(&mut self, paths: Vec<PathBuf>, permanent: bool) {
        let mut done: Vec<PathBuf> = Vec::new();
        for p in paths {
            // Checked again: something may have been mounted meanwhile.
            if let Err(e) =
                mount_guard(&p).and_then(|()| if permanent { Ok(()) } else { trash_guard(&p) })
            {
                self.log_issue(e);
                continue;
            }
            // Already gone from disk: just drop it from the tree.
            if std::fs::symlink_metadata(&p)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                done.push(p);
                continue;
            }
            let result = if permanent {
                match std::fs::symlink_metadata(&p) {
                    Ok(m) if m.is_dir() => remove_dir_one_fs(&p, &mount_points()),
                    _ => std::fs::remove_file(&p),
                }
                .map_err(|e| e.to_string())
            } else {
                trash::delete(&p).map_err(|e| trash_reason(&e))
            };
            match result {
                Ok(()) => done.push(p),
                Err(e) => {
                    let key = if permanent {
                        "ERR_DELETE_FAILED"
                    } else {
                        "ERR_TRASH_FAILED"
                    };
                    self.log_issue(trf(key, &[&show_path(&p), &e]));
                    // A folder may now be partly deleted: say so.
                    if permanent && std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
                        self.log_issue(trf("ERR_DELETE_PARTIAL", &[&show_path(&p)]));
                    }
                }
            }
        }
        self.drop_from_tree(&done);
    }

    /// After emptying the trash: drops the trash folders' contents from the
    /// tree.
    fn drop_trash_from_tree(&mut self) {
        self.status = tr("STATUS_TRASH_EMPTIED");
        let (Some(full), Ok(folders)) =
            (self.full_root.clone(), trash::os_limited::trash_folders())
        else {
            return;
        };
        let gone: Vec<PathBuf> = folders
            .iter()
            .flat_map(|f| ["files", "info"].map(|sub| f.join(sub)))
            .filter_map(|dir| find_node(&full, &dir))
            .flat_map(|n| n.children.iter().map(|c| c.path()))
            .collect();
        drop(full);
        self.drop_from_tree(&gone);
    }

    /// Drops `gone` from the scanned tree in place, keeping the view (and
    /// the table's cursor) where they were as far as possible.
    pub(crate) fn drop_from_tree(&mut self, gone: &[PathBuf]) {
        if gone.is_empty() {
            return;
        }
        // A file counted under a name that's gone counts at another of its
        // names on the next scan.
        self.link_owners
            .retain(|_, at| !gone.iter().any(|g| at.starts_with(g)));
        self.table_forget(gone);
        let Some(root) = self.root.take() else { return };
        let view_paths = self.view_paths(&root);
        drop(root);
        // The last other hold on the tree, so it's changed in place, not
        // copied first (rebuild_view_tree sets it again).
        self.cat_base = None;
        if let Some(full) = &mut self.full_root {
            let full = Arc::make_mut(full);
            remove_all_from_tree(full, gone);
            // A folder's own blocks can shrink when many entries leave it.
            let mut parents: Vec<&Path> = gone.iter().filter_map(|p| p.parent()).collect();
            parents.sort_unstable();
            parents.dedup();
            refresh_own_sizes(full, &parents, self.tree_apparent);
            // The folders above keep their order rule (largest first).
            resort_above(full, gone);
        }
        self.rebuild_view_tree();
        self.restore_view(&view_paths);
        if self.free_space.is_some() {
            self.free_space = self.root.as_ref().and_then(|r| fs_space(&r.path()));
        }
    }

    pub(crate) fn view_paths(&self, root: &Node) -> Vec<PathBuf> {
        self.view_stack
            .iter()
            .map(|vp| get_node(root, vp).path())
            .collect()
    }

    /// Points the zoom history back at `paths` (skipping any that no
    /// longer exist) in the current tree.
    pub(crate) fn restore_view(&mut self, paths: &[PathBuf]) {
        let Some(root) = self.root.clone() else {
            return;
        };
        self.view_stack = paths
            .iter()
            .filter_map(|p| index_path_to(&root, p))
            .collect();
        self.view_stack.dedup();
        if self.view_stack.is_empty() {
            self.view_stack.push(vec![]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_escapes() {
        assert_eq!(unescape_mountinfo("/mnt/Windows\\04010"), "/mnt/Windows 10");
        assert_eq!(unescape_mountinfo("/a\\011b\\134c"), "/a\tb\\c");
        assert_eq!(unescape_mountinfo("/plain"), "/plain");
        assert_eq!(unescape_mountinfo("/trailing\\04"), "/trailing\\04");
    }

    #[test]
    fn deletes_trees_deeper_than_path_max() {
        let base =
            std::env::temp_dir().join(format!("spacescan-deep-delete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let outside = base.join("outside.txt");
        std::fs::write(&outside, "keep me").unwrap();
        // Build 300 levels (~7.5 KB path) relative to open folders, like
        // `mkdir` loops in a shell would have to.
        let top = base.join("deep");
        std::fs::create_dir(&top).unwrap();
        let mut dir = top.clone();
        let mut handle: DirHandle = None;
        for i in 0..300 {
            let next = dir.join(format!("d{i:03}_xxxxxxxxxxxxxxxxxxxx"));
            std::fs::create_dir(openable(&next, &handle)).unwrap();
            if i % 50 == 0 {
                std::fs::write(openable(&next.join("f.bin"), &handle), [0u8; 100]).unwrap();
            }
            handle = if next.as_os_str().len() + 256 > LONG_PATH {
                Some(std::fs::File::open(openable(&next, &handle)).unwrap())
            } else {
                None
            };
            dir = next;
        }
        std::os::unix::fs::symlink(&outside, openable(&dir.join("link"), &handle)).unwrap();
        drop(handle);
        assert!(dir.as_os_str().len() > 7000);
        remove_dir_one_fs(&top, &HashSet::new()).unwrap();
        assert!(!top.exists());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep me");
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// With a category picked, removing a folder covers the files the
    /// filter hides: the dialog gives the real totals and warns about the
    /// hidden ones, and trash asks first instead of acting at once.
    #[test]
    fn removing_under_a_filter_warns_about_hidden_files() {
        let mut app = DiskScanApp::default();
        let folder = test_node(
            "/nonexistent-qa/mixed",
            0,
            true,
            vec![
                test_node("/nonexistent-qa/mixed/a.mkv", 6000, false, vec![]),
                test_node("/nonexistent-qa/mixed/b.pdf", 900, false, vec![]),
            ],
        );
        let mut full = test_node("/nonexistent-qa", 0, true, vec![folder]);
        full.children[0].size = 6900;
        full.children[0].file_count = 2;
        app.full_root = Some(Arc::new(full));
        app.pick = Some(Pick::Category(app.cats.of_name("x.mkv")));
        app.rebuild_view_tree();
        let target = vec![PathBuf::from("/nonexistent-qa/mixed")];

        app.ask_delete(target.clone());
        assert!(matches!(
            app.removal.confirm,
            Some(Confirm::Delete {
                permanent: true,
                size: 6900,
                file_count: 2,
                hidden: (1, 900),
                ..
            })
        ));
        app.removal.confirm = None;

        app.queue_trash(target.clone());
        assert!(app.removal.pending.is_none());
        assert!(matches!(
            app.removal.confirm,
            Some(Confirm::Delete {
                permanent: false,
                hidden: (1, 900),
                ..
            })
        ));
        app.removal.confirm = None;

        // Without a filter, trash acts at once as before.
        app.pick = None;
        app.rebuild_view_tree();
        app.queue_trash(target);
        assert!(app.removal.confirm.is_none() && app.removal.pending.is_some());
    }

    #[test]
    fn delete_dialog_counts_unreadable_folders_inside() {
        let mut app = DiskScanApp::default();
        let locked = test_node("/nonexistent-qa/locked", 0, true, vec![]);
        let other = test_node("/nonexistent-qa/other", 0, true, vec![]);
        app.root = Some(Arc::new(test_node(
            "/nonexistent-qa",
            0,
            true,
            vec![locked, other],
        )));
        app.unreadable = vec![
            PathBuf::from("/nonexistent-qa/locked/secret"),
            PathBuf::from("/nonexistent-qa/elsewhere"),
        ];
        app.ask_delete(vec![PathBuf::from("/nonexistent-qa/locked")]);
        assert!(matches!(
            app.removal.confirm,
            Some(Confirm::Delete { unreadable: 1, .. })
        ));
        app.removal.confirm = None;
        app.ask_delete(vec![PathBuf::from("/nonexistent-qa/other")]);
        assert!(matches!(
            app.removal.confirm,
            Some(Confirm::Delete { unreadable: 0, .. })
        ));
    }

    /// Builds a chain of `levels` nested folders named "d" (files every
    /// 500 levels and at the bottom), then runs every tree operation over
    /// it: scan, live-preview graft, category breakdown, filter, clone,
    /// find, replace, remove, drop, and the delete from disk.
    fn deep_chain(levels: usize) {
        // As the app does at start (the test binary skips `main`).
        crate::raise_open_file_limit();
        let base =
            std::env::temp_dir().join(format!("spacescan-deep-{levels}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let top = base.join("chain");
        std::fs::create_dir(&top).unwrap();
        let mut dir = top.clone();
        let mut handle: DirHandle = None;
        let mut files = 0;
        for i in 0..levels {
            let next = dir.join("d");
            std::fs::create_dir(openable(&next, &handle)).unwrap();
            handle = if next.as_os_str().len() + 256 > LONG_PATH {
                Some(std::fs::File::open(openable(&next, &handle)).unwrap())
            } else {
                None
            };
            dir = next;
            if i % 500 == 0 || i == levels - 1 {
                std::fs::write(openable(&dir.join("f.bin"), &handle), [1u8; 10]).unwrap();
                files += 1;
            }
        }
        drop(handle);
        let bottom = dir.clone();

        // Scan.
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let live = LiveTree::new(Arc::new(CategoryModel::defaults()));
        let ctx = ScanCtx {
            mounts: &HashSet::new(),
            progress: &tx,
            cancel: &Default::default(),
            apparent_size: true,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: Some(&live),
        };
        let tree = scan_dir(&top, &ctx);
        assert_eq!(tree.file_count, files);
        assert_eq!(
            find_node(&tree, &bottom.join("f.bin")).map(|n| n.size),
            Some(10)
        );

        // The live tree, read in full and as the chart shows it.
        let mut looks = LiveLooks::default();
        let partial = live.snapshot(usize::MAX, 0.0, &mut looks).unwrap();
        assert_eq!(partial.file_count, files);
        assert!(find_node(&partial, &bottom).is_some());
        assert!(live.snapshot(12, 1.3 / 360.0, &mut looks).is_some());

        // Categories, filter, clone.
        let cats = CategoryModel::defaults();
        assert_eq!(
            category_breakdown(&tree, &cats)
                .iter()
                .find(|r| r.cat == cats.of_name("f.bin"))
                .map(|r| r.files),
            Some(files)
        );
        let filter = CompiledFilter::compile(&FilterForm {
            name: "*.bin".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            filter_tree(&tree, &filter).map(|t| t.file_count),
            Some(files)
        );
        let mut copy = tree.clone();

        // Replace (a folder rescan) and remove (a delete) at the bottom.
        let fresh = find_node(&tree, &bottom).unwrap().clone();
        assert!(table::replace_in_tree(&mut copy, &bottom, fresh).is_some());
        let size = copy.size;
        remove_all_from_tree(&mut copy, &[bottom.join("f.bin")]);
        assert_eq!((copy.size, copy.file_count), (size - 10, files - 1));
        drop(copy);
        drop(partial);
        drop(live);
        drop(tree);

        // Delete from disk.
        remove_dir_one_fs(&top, &HashSet::new()).unwrap();
        assert!(!top.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn very_deep_folder_chains_are_handled() {
        deep_chain(5_000);
    }

    #[test]
    #[ignore]
    fn extremely_deep_folder_chains_are_handled() {
        deep_chain(30_000);
    }

    #[test]
    fn mount_table_is_prefix_matched_by_component() {
        // "/" is always a mount point and contains everything.
        assert!(!mounts_at_or_under(Path::new("/")).is_empty());
        // A component-wise prefix: /pro doesn't contain /proc.
        assert!(mounts_at_or_under(Path::new("/pro")).is_empty());
    }
}
