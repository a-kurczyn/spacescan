# spacemap — manual test cases

Covers the features added or changed on 2026-09-27/28: the Summary table,
keyboard navigation, delete/trash, settings file, chart context menu,
permissions column, column reordering. Build: `cargo build --release`
in `~/spacemap`, binary at `target/release/spacemap` (installed copy:
`~/.local/bin/spacemap`).

## Safety rules — read first

1. **Only delete/trash dummy files.** Run every test against the scratch
   tree below, never against real folders.
2. **Isolate the config:** launch with `HOME=$FH` (a throwaway home) so the
   user's `~/.config/spacemap/settings.json` isn't touched. Theme colors will
   be the default ones under the fake home; that's expected.
3. **Do not run "Empty trash" (toolbar 🗑) on this machine** unless the user
   says so: it lists and permanently purges trash folders on *every*
   mounted drive (e.g. `/mnt/DATA/.Trash-1000`), not just the fake home's.
   Test T-16 describes it for a VM or with explicit consent.
4. Keyboard shortcuts act on the focused spacemap window. When done, close
   spacemap and delete `$T` and `$FH`.

## Setup

```bash
export T=/tmp/spacemap-test/tree FH=/tmp/spacemap-test/home
rm -rf /tmp/spacemap-test && mkdir -p $T/{photos,music/albums,docs,empty,.cache} $FH
head -c 40M /dev/zero > $T/photos/a.jpg
head -c 12M /dev/zero > $T/music/albums/x.flac
head -c 3M  /dev/zero > $T/docs/r.pdf
head -c 25M /dev/zero > $T/big.iso
head -c 500K /dev/zero > $T/notes.txt
head -c 2M  /dev/zero > $T/.cache/c.bin
head -c 1M  /dev/zero > $T/.hidden
chmod 600 $T/notes.txt; chmod 1777 $T/photos
HOME=$FH ~/spacemap/target/release/spacemap &
```

Then type `$T`'s path into the path bar and press Enter. Switch to the
table with the table icon in the toolbar (the "Summary view").

---

## A. Table display

| ID | Steps | Expected |
|----|-------|----------|
| A-1 | Open the Summary view on `$T`. | Columns left→right: (mark), bar, %, Size, Files, Modified, Changed, Permissions, Name. No vertical lines between columns (a line appears only while hovering a column edge). |
| A-2 | Look at the % column. | Percentages of the listed rows add up to ~100%. Bars match the % values (40 MB row ≈ 48% filled). |
| A-3 | Look at Name. | Folders in accent color, no folder icons; files in normal text color; all names left-aligned. |
| A-4 | Look at Permissions. | `ls -l` style plus owner, e.g. `drwxrwxrwt user:group` for `photos` (sticky `t`), `-rw------- …` for `notes.txt`. Monospace, rows align. |
| A-5 | Cursor row (first row by default). | Highlighted; its name, check mark and bar stay readable (bar keeps its orange fill on a dark track with a background-colored border). |
| A-6 | Heading line. | "Contents · press ? for keyboard shortcuts"; tags appear when relevant (see C, D). |
| A-7 | Resize a column by dragging its edge. | Width changes; resize line visible only while hovering/dragging. |

## B. Keyboard navigation (table)

| ID | Steps | Expected |
|----|-------|----------|
| B-1 | ⬇ / ⬆ | Cursor moves one row; stops at first/last row. |
| B-2 | PgDn / PgUp, Home / End | Moves a screen / to first / last row; table scrolls to keep cursor visible. |
| B-3 | Cursor on `photos`, press ➡ | Opens `photos`; cursor on its first row. |
| B-4 | Press ⬅ (or Backspace) inside `photos` | Back to `$T` with cursor on `photos`. |
| B-5 | ➡ on a file | Nothing happens. |
| B-6 | Enter on a folder | Opens it (like ➡). |
| B-7 | Enter on `notes.txt` | Opens it in the default text editor (xdg-open). |
| B-8 | Double-click a file row / folder row | Same as Enter. |
| B-9 | Press `/`, type `no` | "Jump to:" field appears; cursor jumps to `notes.txt`. Enter or Esc closes the field; cursor stays. |
| B-10 | Press `?` | Keyboard shortcuts overlay (three sections). `?`, Esc, Close or clicking outside closes it. |
| B-11 | Press `i` | Details panel top-right for the cursor row (path, size, perms, dates, owner, type). Moves with the cursor. `i` or Esc closes it. |

## C. Sorting and columns

| ID | Steps | Expected |
|----|-------|----------|
| C-1 | `s` | Sort by size (largest first); `s` again reverses. Header shows a chevron on Size. |
| C-2 | `n`, `f`, `m`, `c`, `p` | Sort by name (A→Z first), files, modified, changed, permissions; repeating flips. |
| C-3 | Click a header (e.g. Files) | Same as its key. |
| C-4 | `S` `F` `M` `C` `P` `%` `G` | Hide/show Size, Files, Modified, Changed, Permissions, Percent, Bar. |
| C-5 | Sort by Files, then `F` | Files hides; sort falls back to Size (largest first). With Size also hidden → Name A→Z. |
| C-6 | Hide Modified with `M`, then press `m` | Modified reappears and becomes the sort column. |
| C-7 | `t` | Folders listed before files; heading shows "folders first". `t` again undoes it. |
| C-8 | `e` | Dotfiles (`.cache`, `.hidden`) disappear; heading shows "dotfiles hidden (3.00 MB)"; % recomputed over the remaining rows (~100%). `e` again shows them. |
| C-9 | Sort by Size, press `>` a few times | Size column moves right one visible column per press, stops before Name. `<` moves it back left. |
| C-10 | Press `%` twice (hide, show), then `>` | The Percent column is the one that moves. |
| C-11 | Sort by Name, press `<` / `>` | Nothing moves (Name is fixed last). |

## D. Marking, delete, trash (table)

| ID | Steps | Expected |
|----|-------|----------|
| D-1 | Space on two rows | Check mark on each, cursor moves down after each; heading "2 marked (size)". |
| D-2 | Ctrl+click a row | Toggles its mark, cursor moves to it (not down). Quick Ctrl+double-click toggles twice, doesn't open. |
| D-3 | Esc with marks | Marks cleared. |
| D-4 | Cursor on `big.iso` (no marks), `D` | Dialog "Delete permanently?" with full path and size; Cancel focused. Esc / Cancel / click outside → nothing deleted. |
| D-5 | Repeat D-4, click Delete | `big.iso` gone from disk (`ls $T`) and table; total at top shrinks by 25 MB; cursor on next row. No rescan (instant). |
| D-6 | Mark `docs` + `notes.txt`, `D` | Dialog lists 2 items with combined size & file count; Delete removes both. |
| D-7 | Cursor on `empty`, `T` | Moved to trash without a dialog (check `$FH/.local/share/Trash/files` or `/tmp/.Trash-$UID`); table updates in place. |
| D-8 | Folder opened, then leave it | Marks don't carry over to other folders. |
| D-9 | `r` inside `photos` after `head -c 5M /dev/zero > $T/photos/new.bin` | Only `photos` rescans; afterwards you're back in `photos`, `new.bin` listed, parent totals include it. |

## E. Chart view

Switch back to the chart (Chart button in the toolbar).

| ID | Steps | Expected |
|----|-------|----------|
| E-1 | Look at the chart | No d-pad in the top-right corner. |
| E-2 | ⬇ / ⬆ | Highlight moves to next / previous slice in the same ring (wraps). |
| E-3 | ➡ on a folder slice | Highlight moves to its first child on the next ring out. |
| E-4 | ⬅ | Highlight to parent slice; from the inner ring, view goes up to the parent folder with the left folder highlighted. |
| E-5 | Backspace | Parent folder. |
| E-6 | Enter on highlighted folder / Esc | Opens it / clears highlight. |
| E-7 | Highlight a file slice, `D` | Same delete dialog as the table; confirming updates the chart in place. |
| E-8 | Highlight a slice, `T` | Trashed, chart updates in place. |
| E-9 | Highlight the "(N other items)" slice, `D` / `T` | Nothing happens. |
| E-10 | Chart's top-right corner | Two buttons, "9⌄" (largest first) and "A⌄" (A–Z); the active one highlighted; clicking switches the chart order. Not shown in table view. |
| E-11 | Toolbar right side | `[Chart][Table][🗑]` then `[Filter][⚙]` after a separator: Chart and Table switch views, the active one highlighted. |

## F. Chart right-click menu

| ID | Steps | Expected |
|----|-------|----------|
| F-1 | Right-click a slice | Menu: Zoom, Rescan, Open, Hide, Move to trash, Delete permanently. No Cancel entry. |
| F-2 | Click elsewhere / press Esc | Menu closes, nothing happens. |
| F-3 | "Delete permanently" | Opens the confirmation dialog (does not delete immediately). |
| F-4 | Right-click a slice inside a subfolder view, then (menu open) click the hub to go up, then "Move to trash" | Either the menu closed on the hub click, or it trashes the **originally right-clicked** item — never a different one. Check `ls`. |
| F-5 | Right-click the "other" slice | No menu. |

## G. Settings file

Config lives in `$FH/.config/spacemap/settings.json` in these tests.

| ID | Steps | Expected |
|----|-------|----------|
| G-1 | First launch with empty `$FH` | `settings.json` created (pretty JSON: language, chart, table). |
| G-2 | Change a ⚙ slider, sort, hide a column, `t`; quit; relaunch | All restored. Dotfile hiding (`e`) is **not** restored (always shown at launch). |
| G-4 | Edit `settings.json`: `"max_render_depth": 99`; launch | Value clamped to 12 in the editor and rewritten to the file. |
| G-5 | Write invalid JSON to `settings.json`; launch | Defaults used; file renamed `settings.json.bad`; message in the Issues bar. |
| G-6 | Change language in ⚙ to Español; relaunch | Spanish UI, including the `?` overlay and dialogs. |

## H. Other

| ID | Steps | Expected |
|----|-------|----------|
| H-1 | Table on a folder with 50k+ entries (e.g. `mkdir $T/many && cd $T/many && seq 50000 \| xargs touch`, then rescan) | Scrolling and moving the mouse stay smooth; sorting keys respond. |
| H-2 | Keys with a non-US layout (Spanish): `%` (Shift+5), `<` / `>` (key left of Z), `?`, `/` | All work. |
| H-3 | Toolbar 🗑 when every trash folder is empty | Status "The trash is already empty"; no dialog. |
| H-4 | Click the Filter button (toolbar, next to ⚙) | Filters panel opens and the button is highlighted; click again → panel closes, highlight off. |
| H-5 | Open Filters, type a name pattern (e.g. `*.iso`), Apply, then close the panel | Button stays highlighted while the filter is applied; its tooltip says filters are active. Clear the filter → highlight off. |
| H-6 | Window not maximized: open ⚙ Settings, then Filters, then close both | Window widens by ~320 px, then ~340 px more; closing each gives its width back (ends at the original width). Main area keeps its size throughout. |
| H-7 | Window maximized: open Filters / ⚙ | Window doesn't change size; the panel takes room from the main area. |
| H-8 | Table view with a side panel open (any window size) | Contents and extension tables are cut off at the panel's edge, never drawn over it. |
| H-9 | Table view: press `?` once, wait, press `?` again | First press opens the shortcuts overlay and it stays open; second press closes it. (Regression: it used to close in the same frame it opened.) |

## T-16. Empty trash — only in a VM or with the user's explicit consent

1. Trash a dummy file (D-7).
2. Toolbar 🗑 → dialog "Empty the trash?" with item count (all drives); Cancel focused.
3. Confirm → status "Emptying the trash…" then "Trash emptied"; window stays responsive; trash folders in the scanned tree lose their contents without a rescan.

## Cleanup

```bash
pkill -x spacemap; rm -rf /tmp/spacemap-test
```
