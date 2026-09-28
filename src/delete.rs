//! Deleting, trashing and emptying the trash — shared by the chart and the
//! Summary table. Everything removed is dropped from the scanned tree in
//! place (the folders above shrink accordingly) instead of rescanning.
//! Permanent deletes and emptying the trash go through a confirmation
//! dialog first; moving to the trash doesn't, since it can be undone.

use super::*;
use std::sync::mpsc::channel;

/// What the confirmation dialog is asking about.
#[derive(Clone)]
enum Confirm {
    Delete {
        paths: Vec<PathBuf>,
        size: u64,
        file_count: u64,
        /// Whether the single item is a folder; None for several.
        single_is_dir: Option<bool>,
    },
    EmptyTrash(Vec<trash::TrashItem>),
}

#[derive(Default)]
pub(crate) struct Removal {
    confirm: Option<Confirm>,
    /// Deletes (true = permanent, false = to trash) confirmed or requested
    /// this frame, carried out at the start of the next one — before that
    /// frame's clones of the tree exist, so it can be edited in place
    /// instead of copied.
    pending: Option<(Vec<PathBuf>, bool)>,
    /// Emptying the trash runs on its own thread (it can be a lot of
    /// files); this delivers the outcome.
    purge_rx: Option<Receiver<Result<(), String>>>,
}

/// Removes the node at `target` from the tree, subtracting its size and
/// file count from every folder above it. Returns what was removed, or
/// None if `target` isn't in the tree.
fn remove_from_tree(node: &mut Node, target: &Path) -> Option<(u64, u64)> {
    let i = node.children.iter().position(|c| target.starts_with(&c.path))?;
    let removed = if node.children[i].path == target {
        let c = node.children.remove(i);
        (c.size, c.file_count)
    } else {
        remove_from_tree(&mut node.children[i], target)?
    };
    node.size = node.size.saturating_sub(removed.0);
    node.file_count = node.file_count.saturating_sub(removed.1);
    Some(removed)
}

/// The node at `path`, if it's in the tree.
pub(crate) fn find_node<'a>(root: &'a Node, path: &Path) -> Option<&'a Node> {
    if root.path == path {
        return Some(root);
    }
    let child = root.children.iter().find(|c| path.starts_with(&c.path))?;
    find_node(child, path)
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
                Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
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
        let Some(root) = self.root.clone() else { return };
        let nodes: Vec<&Node> = paths.iter().filter_map(|p| find_node(&root, p)).collect();
        if nodes.is_empty() {
            return;
        }
        self.removal.confirm = Some(Confirm::Delete {
            paths: nodes.iter().map(|n| n.path.clone()).collect(),
            size: nodes.iter().map(|n| n.size).sum(),
            file_count: nodes.iter().map(|n| n.file_count.max(1)).sum(),
            single_is_dir: (nodes.len() == 1).then(|| nodes[0].is_dir),
        });
    }

    /// Moves `paths` to the trash at the start of the next frame.
    pub(crate) fn queue_trash(&mut self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.removal.pending = Some((paths, false));
        }
    }

    /// The toolbar's 🗑: asks to empty every trash folder (home and other
    /// drives alike).
    pub(crate) fn ask_empty_trash(&mut self) {
        if self.removal.purge_rx.is_some() {
            return; // already emptying
        }
        match trash::os_limited::list() {
            Ok(items) if items.is_empty() => self.status = tr("STATUS_TRASH_EMPTY"),
            Ok(items) => self.removal.confirm = Some(Confirm::EmptyTrash(items)),
            Err(e) => self.log_issue(trf("ERR_EMPTY_TRASH", &[&e.to_string()])),
        }
    }

    /// The confirmation dialog, when one is pending.
    pub(crate) fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.removal.confirm.clone() else { return };
        let modal = egui::Modal::new("confirm_removal".into()).show(ctx, |ui| {
            ui.set_max_width(480.0);
            let yes_label = match &confirm {
                Confirm::Delete { paths, size, file_count, single_is_dir } => {
                    ui.heading(tr("DELETE_CONFIRM_TITLE"));
                    ui.add_space(6.0);
                    match single_is_dir {
                        Some(is_dir) => {
                            ui.label(egui::RichText::new(paths[0].display().to_string()).monospace());
                            ui.label(if *is_dir {
                                trf("DELETE_CONFIRM_DIR", &[&human_size(*size), &format_count(*file_count)])
                            } else {
                                trf("DELETE_CONFIRM_FILE", &[&human_size(*size)])
                            });
                        }
                        None => {
                            ui.label(trf(
                                "DELETE_CONFIRM_MANY",
                                &[&format_count(paths.len() as u64), &human_size(*size), &format_count(*file_count)],
                            ));
                            const SHOWN: usize = 8;
                            for p in paths.iter().take(SHOWN) {
                                ui.label(egui::RichText::new(file_name_of(p)).monospace());
                            }
                            if paths.len() > SHOWN {
                                ui.weak(trf("DELETE_CONFIRM_MORE", &[&format_count((paths.len() - SHOWN) as u64)]));
                            }
                        }
                    }
                    tr("DELETE_CONFIRM_YES")
                }
                Confirm::EmptyTrash(items) => {
                    ui.heading(tr("TRASH_CONFIRM_TITLE"));
                    ui.add_space(6.0);
                    ui.label(trf("TRASH_CONFIRM_BODY", &[&format_count(items.len() as u64)]));
                    tr("TRASH_CONFIRM_YES")
                }
            };
            ui.add_space(10.0);
            let mut choice = None;
            ui.horizontal(|ui| {
                // Cancel first and focused, so a stray Enter/Space never
                // deletes anything.
                let cancel = ui.button(tr("MENU_CANCEL"));
                if !ui.memory(|m| m.focused().is_some()) {
                    cancel.request_focus();
                }
                if cancel.clicked() {
                    choice = Some(false);
                }
                if ui.button(egui::RichText::new(yes_label).color(ui.visuals().error_fg_color)).clicked() {
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
                    Confirm::Delete { paths, .. } => self.removal.pending = Some((paths, true)),
                    Confirm::EmptyTrash(items) => {
                        let (tx, rx) = channel();
                        std::thread::spawn(move || {
                            let _ = tx.send(trash::os_limited::purge_all(&items).map_err(|e| e.to_string()));
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
            let result = if permanent {
                let is_dir = std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir());
                if is_dir { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) }.map_err(|e| e.to_string())
            } else {
                trash::delete(&p).map_err(|e| e.to_string())
            };
            match result {
                Ok(()) => done.push(p),
                Err(e) => {
                    let key = if permanent { "ERR_DELETE_FAILED" } else { "ERR_TRASH_FAILED" };
                    self.log_issue(trf(key, &[&p.display().to_string(), &e]));
                }
            }
        }
        self.drop_from_tree(&done);
    }

    /// After emptying the trash: drops whatever the scan holds of the
    /// trash folders' contents.
    fn drop_trash_from_tree(&mut self) {
        self.status = tr("STATUS_TRASH_EMPTIED");
        let (Some(full), Ok(folders)) = (self.full_root.clone(), trash::os_limited::trash_folders()) else { return };
        let gone: Vec<PathBuf> = folders
            .iter()
            .flat_map(|f| ["files", "info"].map(|sub| f.join(sub)))
            .filter_map(|dir| find_node(&full, &dir))
            .flat_map(|n| n.children.iter().map(|c| c.path.clone()))
            .collect();
        drop(full);
        self.drop_from_tree(&gone);
    }

    /// Drops `gone` from the scanned tree in place, keeping the view (and
    /// the table's cursor) where they were as far as possible.
    fn drop_from_tree(&mut self, gone: &[PathBuf]) {
        if gone.is_empty() {
            return;
        }
        self.table_forget(gone);
        let Some(root) = self.root.take() else { return };
        let view_paths = self.view_paths(&root);
        drop(root);
        if let Some(full) = &mut self.full_root {
            let full = Arc::make_mut(full);
            for p in gone {
                remove_from_tree(full, p);
            }
        }
        self.rebuild_view_tree();
        self.restore_view(&view_paths);
        if self.free_space.is_some() {
            self.free_space = self.root.as_ref().and_then(|r| fs_space(&r.path));
        }
    }

    pub(crate) fn view_paths(&self, root: &Node) -> Vec<PathBuf> {
        self.view_stack.iter().map(|vp| get_node(root, vp).path.clone()).collect()
    }

    /// Points the zoom history back at `paths` (skipping any that no
    /// longer exist) in the current tree.
    pub(crate) fn restore_view(&mut self, paths: &[PathBuf]) {
        let Some(root) = self.root.clone() else { return };
        self.view_stack = paths.iter().filter_map(|p| index_path_to(&root, p)).collect();
        self.view_stack.dedup();
        if self.view_stack.is_empty() {
            self.view_stack.push(vec![]);
        }
    }
}
