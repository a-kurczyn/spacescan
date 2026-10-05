# spacemap — QA findings (shared with the coding instance)

Source: independent black-box testing of the installed binary (`~/.local/bin/spacemap`, build of 2026-09-28 12:58, HEAD 704f7a6).
The tester never reads the source. Findings describe symptoms, repro steps and expected behaviour only.

## Workflow
1. **OPEN**: reported by QA.
2. **FIXED**: the coder fills in *Fix (commit)* and optionally a one-line note, and sets the status to FIXED. Please don't edit the repro/expected text; add a note under the finding if you disagree with it (for example "won't fix — by design because …").
3. QA re-tests the new build:
   - pass → **VERIFIED**; QA keeps re-checking it in every later round (QA's regression suite is private);
   - fail → **REOPENED**, with a QA note that describes the symptom only.
4. Won't-fix items need the user's agreement; QA marks them **WONTFIX (agreed)**.

**Test visibility (user decision, 2026-09-28):** QA's test cases, fixtures, thresholds and extra probes stay private, so fixes target the behaviour rather than specific checks.
Findings from now on describe the *scenario and symptom* (e.g. "a folder that contains a mounted drive"), not QA's exact test data. Please test fixes more broadly than the example in the repro.
A REOPENED note says what is still wrong, not which check failed. Any verified behaviour that later breaks comes back as REOPENED.

## Status

| ID | Sev | Title | Status | Fix (commit) | QA note |
|----|-----|-------|--------|--------------|---------|
| SM-00 | S1 | Delete follows into mounted filesystems and wipes them | VERIFIED | dce363f | D / T / Empty trash refused (Issue names the mounts) if the target is or contains a mount point per /proc/self/mountinfo, bind mounts included; permanent delete also stops at any other-device folder. — **QA ✔ (build 15:0x): the 3 original repros are refused with a clear Issue, mounted data intact; extra adversarial variants pass too. Nit (S4): the Issue lists a mount twice when mounts are stacked at one path (e.g. autofs + cifs, as on /mnt/minipc/*), so please dedupe.** **QA nit ✔ (3f7af1a): stacked mounts listed once.** |
| SM-01 | S2 | Sparse files are reported at apparent size, not disk usage | VERIFIED | b532cc2 | Sizes = allocated blocks (du semantics); new ⚙ Scanning › "Count apparent size" for the old behaviour (next scan). — **QA ✔ 8 GiB sparse file with 1 MiB written = 1.00 MiB.** |
| SM-02 | S2 | Hard links counted once per name | VERIFIED | b532cc2 | Multi-link inodes counted once, at the first name found (parallel scan: which name is not deterministic). — **QA ✔ 5 names / one 10 MiB inode = 10.00 MiB.** |
| SM-03 | S3 | Size units are labelled decimal but computed binary | VERIFIED | b532cc2 | Labels now KiB/MiB/GiB/TiB (values were already binary). Filter sizes keep 1024 multiples; kib/mib accepted. — **QA ✔ 100,000,000 B = 95.37 MiB.** |
| SM-04 | S2 | Paths longer than PATH_MAX are not scanned | VERIFIED | b532cc2 | Deep folders opened relative to the open parent via /proc/self/fd; 300×24-char test tree (7.8 KB path) fully scanned. Delete of such deep paths not changed. — **QA ✔ 300-level tree fully scanned, no 'File name too long'. Deep delete gets a separate probe.** |
| SM-05 | S3 | Files column: rows don't add up to the parent total | VERIFIED | b532cc2 | Empty folders show 0 files; sort and delete dialog use the same count. — **QA ✔ rows (2+1+0+0) = header 3.** |
| SM-06 | S3 | Issues bar: long error messages overflow and hide the actual error | VERIFIED | a018336 | Each issue is one line; long ones lose their middle ("…"), keeping the reason; hover shows the full text. — **QA ✔ long paths are middle-elided on one line; the reason stays visible.** |
| SM-07 | S4 | Directory entries' own size ignored | VERIFIED | b532cc2 | Folders include their own entry's disk usage. — **QA ✔ 20k-entry folder = 808 KiB (du: 808 KiB).** |
| SM-08 | S4 | Non-Latin names render as tofu boxes | VERIFIED | 90cabab | Korean names render too: after a scan meets a Hangul name, a Korean system font is loaded (only then, as it is large). — **QA ✔ Korean, Japanese and Chinese names render.** |
| SM-09 | S4 | Accessibility: unlabeled controls | VERIFIED | 70ec8f3 | Icon and glyph buttons and sortable headers have names for screen readers (headers include their sort state); the chart area has a name. — **QA ✔ all controls and headers have screen-reader names.** |
| SM-10 | S3 | Two different files can look identical, with no way to tell them apart | VERIFIED | a018336 | Invalid bytes and control characters shown escaped (x\xFFy, \n) in rows, dialogs, details and Issues. — **QA ✔ invalid bytes shown escaped; look-alikes are distinguishable.** |
| SM-11 | S3 | Delete of an already-vanished file leaves a ghost row and mark | VERIFIED | a018336 | Deleting/trashing something already gone removes its row and mark without an error. — **QA ✔ vanished item disappears from the view.** |
| SM-12 | S4 | Filters: after an invalid value the table keeps the previous filter | VERIFIED | a018336 | **QA ✔ invalid field outlined, Apply disabled, stale-result note shown.** |
| SM-13 | S4 | Delete dialog doesn't warn about unreadable content | VERIFIED | 2b15567 | Delete dialog warns when the selection contains folders the scan couldn't read (size unknown, may stop partway); a failed folder delete says it may be partly gone. — **QA ✔ dialog warns about unreadable folders (single and multi-select).** |
| SM-14 | S4 | Name sort is case-sensitive ASCII | VERIFIED | 85cfdef | Names sort case-insensitively with numbers by value (file2 before file10), in the table and the chart. — **QA ✔ case-insensitive + natural number order. See SM-34 for accented names.** |
| SM-15 | S4 | Details panel (`i`) on a symlink doesn't say it's a symlink or show its target | VERIFIED | 2b15567 | Details panel and hover card say "Symbolic link → target (not counted in sizes)"; the delete dialog says only the link is removed. — **QA ✔ symlinks identified with target in details and dialog, including broken and folder links.** |
| SM-16 | S4 | Filter name field: a literal space can't be matched | VERIFIED | 2b15567 | Name patterns can contain spaces or commas inside "double quotes" or after a backslash; the field tooltip explains it. — **QA ✔ quoted and escaped spaces/commas work; unbalanced quote is harmless.** |
| SM-17 | S4 | accesskit thread panics when the AT-SPI registry isn't activatable | VERIFIED | 90cabab | When the session has no accessibility service, the start-up crash message no longer appears (nothing could use accessibility there anyway). The underlying library bug remains until an egui release updates it. — **QA ✔ no crash message when no accessibility service exists.** |
| SM-18 | S3 | Table view does O(n) work on every keypress in big folders | VERIFIED | 3f7af1a | Moving the cursor no longer slows down with folder size, wherever the cursor is in the list; re-sorting a very large folder is about 3× faster than before. — **QA ✔ cursor latency no longer depends on folder size. Re-sort cost filed separately as SM-33.** |
| SM-19 | S3 | Memory is retained after rescans (≈ one extra tree, and it keeps creeping) | VERIFIED | 00ebde7 | Memory returns to about one tree's worth after each rescan instead of growing (checked with repeated rescans of a ~3M-file tree). — **QA ✔ multi-million-entry drive: +11% after 6 rescans (was +116%). Residual creep ≈ 1–2% per rescan; fine for now.** |
| SM-20 | S3 | Status bar shows the previous "Scan completed in X s" while a new scan runs | VERIFIED | a018336 | Status reads "Scanning… (Esc to cancel)" while a scan runs. — **QA ✔ no stale completion message during a scan.** |
| SM-21 | S3 | Changing one out-of-range setting wipes the whole configuration | VERIFIED | a018336 | Bad values fall back one by one (bad list entries dropped individually); original copied to settings.json.bad; Issue names the ignored fields. — **QA ✔ other settings survive one bad value.** |
| SM-22 | S4 | Settings read has no size limit (a symlink to `/dev/zero` eats RAM) | VERIFIED | a018336 | A settings file that isn't a regular file or is over 1 MiB isn't read (Issue shown, defaults used, not overwritten). — **QA ✔ endless settings source no longer eats memory.** |
| SM-23 | S4 | Saving settings replaces a symlinked settings.json with a regular file | VERIFIED | a018336 | Saving through a symlinked settings.json updates the target and keeps its permissions. — **QA ✔ symlinked settings file stays a symlink.** |
| SM-24 | S4 | settings.json is a directory: silent failure and a leftover `.tmp` | VERIFIED | a018336 | settings.json as a directory: Issue shown, no leftover temp file. — **QA ✔ no leftover temp file.** |
| SM-25 | S4 | Scan cancel is undiscoverable; `r` doesn't rescan in chart view | WONTFIX (agreed) | a018336 | Esc hint in status while scanning; r rescans the current folder in chart view too. — **QA ✔ "Esc to cancel" hint while scanning; r rescans in chart view. Nit: the hint is small text in the status bar, far from the progress bar.** **QA ✘ (1f446be) the Esc hint was removed from the status bar and the progress bar; during a scan nothing on screen says how to stop it (progress bar shows only "N items", status "Scanning…"). Esc still works. If the removal is intended, please confirm with the user (then WONTFIX) or put a visible Stop/✕ next to the progress bar.** **User decision 2026-10-03: removing the Esc hint was the user's call; Esc still cancels.** |
| SM-26 | S4 | Wrong error wording for files | VERIFIED | a018336 | Not-found wording is now "not found (no such file or folder)". — **QA ✔ file error now says "not found (no such file or folder)".** |
| SM-27 | S4 | Scanning is ~2× slower when accessibility is active | VERIFIED | 90cabab | While scanning, the window redraws about 4 times a second (was about 10), reducing UI and accessibility work during scans further. — **QA ✔ cold scan with an accessibility client within limits of the plain scan.** |
| SM-28 | S3 | Trashing something that is already in the Trash corrupts its restore info | VERIFIED | a018336 | Trash refused (Issue suggests Delete permanently) for items inside a trash folder or containing one. — **QA ✔ trashing inside the Trash is refused; restore info intact.** |
| SM-29 | S3 | Path bar: `~` is not expanded; misleading "Not a directory" for missing paths | VERIFIED | a018336 | ~ and ~/… expand; errors distinguish not found / is a file / permission. — **QA ✔ ~ expands; missing path says "Path not found".** |
| SM-30 | S4 | Paths are not canonicalized | VERIFIED | a018336 | Scan targets are canonical absolute paths. — **QA ✔ breadcrumbs canonical.** |
| SM-31 | S3 | Chart context menu stays open (bound to a hidden item) while the keyboard navigates elsewhere | VERIFIED | a018336 | While the chart's right-click menu is open, navigation keys are ignored (Esc closes it) and the hover card is hidden. — **QA ✔ while the menu is open, navigation keys are ignored; Esc closes it.** |
| SM-32 | S4 | Permanent delete fails on trees deeper than PATH_MAX (scan and Trash work) | VERIFIED | 78ff8b2 | Permanent delete removes folders nested deeper than the system path limit completely; symbolic links inside are removed, not followed. — **QA ✔ deep trees delete fully; links inside are not followed; related edge cases behave.** |
| SM-33 | S4 | Re-sorting a very large folder lags (~0.3 s per sort key press) | VERIFIED | f919640 | The table jumps straight to the cursor row after a sort or long jump instead of scrolling there over up to 0.3 s. — **QA ✔ sort toggle ≈ 45–70 ms on a few hundred thousand entries.** |
| SM-34 | S4 | Names starting with accented letters sort after Z | VERIFIED | 90cabab | Accented initials sort with their base letter (émile with emile, Árbol with a), after the unaccented twin. — **QA ✔ Árbol, avión, Echo, émile … zeta.** |
| SM-35 | S3 | Table view is replaced by the chart while a scan runs | VERIFIED | df4d161 | Table view stays during a scan: progress bar on top, rows grow and re-sort a few times a second, cursor stays on its item without the view jumping; opening folders, delete, trash and details wait for the scan to finish (heading says so). — **QA ✔ visual + automated: table stays, rows grow and re-sort live, cursor keeps its item, destructive actions wait. Nit (S4): while a folder is still being scanned its row shows `d--------- root:root` and "-" dates, which read as real "no permissions" values; show a neutral placeholder (…) instead.** **QA nit ✔ (002e693): unfinished rows show "…" placeholders.** |
| SM-36 | S3 | App crashes (stack overflow) when scanning a very deeply nested folder tree | VERIFIED | 002e693 | Scanning, browsing, filtering, deleting and rescanning work on folder chains of any depth (tested to 30,000 levels); very deep paths are shortened with "…" in labels and the path bar. — **QA ✔ 1k/5k/20k levels: scan, views, rescan, delete, filter, sort, chart drilling, trash all survive.** |
| SM-37 | S4 | Files dated exactly 1970-01-01 00:00 UTC show "-" instead of a date | VERIFIED | 90cabab | Files dated exactly 1970-01-01 show that date; "-" now only means unknown, and unknown dates never match date filters. — **QA ✔ epoch-dated file shows its date.** |
| SM-38 | S4 | Apparent-size totals above 16 EiB wrap around; no EiB unit | VERIFIED | 90cabab | Size totals stop at the maximum instead of wrapping around; an EiB unit is shown for sizes that large. — **QA ✔ saturates at "16.00 EiB" (true 20 EiB); no wrap. Optional: mark saturated totals with "≥".** **QA ≥ 16 EiB ✔ (ff6a978).** |
| SM-39 | S4 | Trash errors show raw internal error text | VERIFIED | 90cabab | Trash errors show a plain reason (not found, no permission, root folder) instead of internal error text. — **QA ✔ plain-language trash errors.** |
| SM-40 | S4 | Case-insensitive drives: path typed in a different case is shown in that case | VERIFIED | 90cabab | A path typed in a different case on a case-insensitive drive is shown with the real names' case. — **QA ✔ on-disk case shown for wrong-case typed paths.** |
| SM-41 | S4 | Sizes show needless decimals (".00") — user request | VERIFIED | ff6a978 | Sizes show at most one decimal and none when it would be .0 (4 KiB, 1.5 GiB, 95.4 MiB) everywhere; size columns are right-aligned; a capped total reads "≥ 16 EiB" (SM-38 note). — **QA ✔ every size string on screen (table, chart, details, dialog, filter summary, extension table) has at most one decimal and no ".0"; unit rollover at 1023.95 is correct; columns right-aligned.** |
| SM-42 | S3 | Chart: one item stretched over a whole folder's arc; similar items not comparable — user report | VERIFIED | c6de79b (branch chart-other-share) | A ring holds as many slices as fit at the min slice angle (at most 360 around a full circle, and no more than "Max slices"). The largest items fill all but the last, sized in proportion to each other; no slice is drawn narrower than the min angle, and the rest go into one min-width "other" at the end. | **QA ✔ (build 0bc8794, real window): a folder of ~1,200 similar items fills its ring with comparable slices up to Max slices, one "other" at the end; a small sibling folder gets a min-width slice, no sliver bands.** |
| SM-43 | S1 | Delete/Trash of a folder while a filter or category is active removes hidden files; the dialog counts only the visible ones | VERIFIED | 507268d | Delete and trash always act on the whole selected item, as before. Their dialog now shows its real size and file count, and when the active filter or category hides files inside, a highlighted warning gives their count and size and says they go too (filters don't limit what is removed). Trash now asks first whenever the filter hides files inside; otherwise it still acts at once. | **QA ✔ (d6248c5, real window): D shows the real size/count plus "The active filter hides N of these files (…). They will be deleted too"; T now asks with the same warning when files are hidden; Esc keeps everything; checked with a category and with a Filters-panel pattern.** |
| SM-44 | S3 | Problems in the categories file are never visible: the Issue is cleared when any scan starts | VERIFIED | cb25800 | Problems in the categories file stay in Issues for the scan that uses the file (on every new scan and folder rescan). | **QA ✔ invalid file and a duplicate extension stay listed in Issues after a scan and after r.** |
| SM-45 | S3 | Chart view with a picked category: no sign of the category and no way to clear it | VERIFIED | d6248c5 | With a picked category, the chart shows "Only showing: <category>" with a × to clear it next to the order buttons; the Filters panel shows the same line with a ×, and its Clear also clears the category. | **QA ✔ chart shows "Only showing: Video ×"; Filters panel shows it too and its Clear clears the category.** |
| SM-46 | S3 | Picked category absent from the current folder: empty table with no explanation | VERIFIED | d6248c5 | The table heading shows "· only <category>" while one is picked. In a folder without files of the picked category, the panel says "No files of category <X> here (N other files hidden)"; × clears it. | **QA ✔ "· only <category>" in the heading; absent category explained with the hidden-file count.** |
| SM-47 | S4 | Category bar is not accessible (unnamed segments and labels, no keyboard) | VERIFIED | 056cea7 | In the Summary view, keys 1–9 pick the category at that position in the bar, counting from the top (user decision: by position, not by list order); the same key again, or 0, shows all files; a key past the last category does nothing. Each of the first nine labels shows its key. Each segment is a named button for screen readers, e.g. "Video, 48.0%, 17 MiB, 7 files, key 1", marked as selected when picked; "No files here" is exposed as text. The ? help lists the keys. Esc is unchanged. | **QA ✔ (056cea7, real window): 9 segments exposed as named toggle buttons ("Video, 31.1%, 11 MiB, 7 files, key 1"), activation through the screen-reader action works; keys 1–9 pick by position per folder, again/0 clear, a key past the last does nothing, nothing in chart view; digits in the jump field, Filters field and delete dialog never pick; labels show their key; help row present; "No files here" exposed. Nit: "1 files" in the spoken name and tooltip should be "1 file".** |
| SM-48 | S4 | Category ✕ (clear) button draws as an empty box | VERIFIED | 781e7c0 | The clear button shows ×, like the other close buttons. | **QA ✔ × visible.** |
| SM-49 | S4 | Category panel: labels pile up in short windows; panel doesn't shrink in narrow windows | VERIFIED | 9557834 | Short windows: labels drop to one line when two don't fit, and if even that doesn't fit only the largest categories are labelled (the others keep their segment, tooltip and key). Narrow windows: the panel takes at most about 30% of the width (150–210 px), and below about 560 px it shows the bar alone, without heading or labels. | **QA ✔ (9557834): 500 px wide → bar only, key pick still works and the table keeps its width; 700 px → full panel; 260 px tall → one-line labels for the largest categories, no overlap, all segments still named.** |
| SM-50 | S4 | Spanish UI: the category panel and tooltips stay English with the installed language file | VERIFIED | b8155cd | All languages are built into the app; a language file in the user's folder only overrides the lines it contains, so anything it lacks shows in the built-in translation (Spanish here), not English. Adds French, German, Italian, Portuguese, Russian, Japanese, Chinese and Korean. | **QA ✔ with the older installed es.lang the whole category panel, tooltips and heading are Spanish.** |
| SM-51 | S4 | Category names and extensions: display oddities | VERIFIED | 9557834 | A category named like the built-in Other (in any case, or the CAT_OTHER token) is skipped with a note in Issues. Long names end in "…" (full name in the tooltip). An extension with spaces or control characters is shown quoted in the tooltip, e.g. ".mkv ". .ts is now Code (user decision); .idx stays in Video. Also (round-11 nit): the tooltip and spoken name say "files: N" instead of "N files". | **QA ✔ "oThEr" and CAT_OTHER skipped with an Issue (one grey Other); 300-char name ends in "…"; ".mkv " quoted in the tooltip; "Files: N". Nit: the Issue reads "…is built in; skipped; "Other": …" — slightly repetitive.** |
| SM-52 | S3 | Light KDE colour scheme: highlighted text is white on white (category names, current folder, chart hub) | VERIFIED | 20808f2 | With a light KDE scheme the whole UI uses a light style: highlighted, selected and hovered text, table headers, the chart hub and path are dark on light; category colors use their light-background shades; the free-space slice is a little darker than the background instead of invisible. | **QA ✔ Breeze Light: category names, tooltip titles, picked label, current breadcrumb, chart path and hub text readable.** |
| SM-53 | S1 | btrfs: Delete on a subvolume/snapshot removes all of it while the dialog says "0 B, file count: 0" | VERIFIED | 3b98085 | Scans and deletes stop only at real mount points (from the kernel's mount table), not at every device change. A btrfs subvolume or snapshot that isn't mounted separately is scanned like a folder, so the delete dialog shows its real size and file count. Folders where another filesystem is mounted (network, USB, bind mounts) are still refused or skipped as before. | **QA ✔ (90c2d10, btrfs image): D on a snapshot says "60 MiB, file count: 2"; confirming removes just the snapshot, its source intact; a real bind mount inside a subvolume still blocks D/T with the mount Issue.** |
| SM-54 | S2 | btrfs: subvolumes and snapshots shown as "[other filesystem]", 0 B, not scanned | VERIFIED | 3b98085 | btrfs subvolumes and snapshots that aren't separate mounts are scanned and counted in all totals and in the progress count; only real mount points show as "[other filesystem]". Bind mounts are now also left out (they were counted twice before). | **QA ✔ subvolumes, snapshots and a nested subvolume are scanned and counted (totals match du); a bind mount is not counted twice.** |
| SM-55 | S3 | A user language file that is a FIFO or a link to /dev/zero hangs startup / eats memory (13 GB in 2 s) | VERIFIED | e9a5a46 | A user language file is only read if it is a regular file of at most 1 MiB. A FIFO, a device link or a huge file is skipped at once, reported in Issues ("Couldn't use the language file …; using the built-in language") at startup and when picking that language, and not offered in the language list unless it shadows a built-in language. | **QA ✔ FIFO, /dev/zero link and a 2 MiB file as the language file: start in seconds, memory ~180 MB, Issue shown, built-in language used.** |
| SM-56 | S4 | Action buttons (Rescan, Empty trash, Choose folder) are announced as toggle buttons | VERIFIED | 4ea3a19 | Only on/off controls (views, Filters, Chart settings, chart order, Categories/Extensions) are toggle buttons for screen readers. Choose folder, Root, Home, Rescan and Empty trash are plain buttons; Root and Home still look lit while they're the current scan target. The category panel's × is named "Show all categories". | **QA ✔ Choose folder, Rescan, Empty trash are plain buttons; views, Filters, Chart settings, Categories/Extensions are toggles; clear is named "Show all categories".** |
| SM-57 | S4 | Extensions table: "(no extension)" is cut to "(no extens…" at the default panel width | VERIFIED | fdd89e9 | The Size and Files columns are now exactly as wide as their widest value or header, so the Extension column gets the rest; "(no extension)" reads in full at the default window size and at 1400 px with a fresh config. A label that still doesn't fit ends in "…" with the full text on hover. | QA ✘ (90c2d10, fresh config, 1400 px window): the panel is wider but the Extension column is not; the row still reads "(no extens…". The extra width went to the gap before Files. **QA ✔ (1f446be) "(no extension)" shown in full at 1400 and 1024 px windows. Nit: still "(no extens…" at 800 px.** |
| SM-58 | S4 | Extensions table rows can't be picked with a screen reader (exposed as plain text) | VERIFIED | 90c2d10 | User decision reversed (asked for screen-reader compliance). Each extension row is a named button, e.g. ".mkv, 48.0%, 17 MiB, files: 7", marked selected when picked. | **QA ✔ rows are named toggle buttons (".gz, 14.6%, 5.2 MiB, files: 2"), "pressed" when picked; the screen-reader action picks.** |
| SM-59 | S4 | Picking an extension during a scan gives no feedback (categories say "Applies when a scan finishes.") | VERIFIED | 81a60c6 | A pick made during a scan (category or extension) shows "Applies when a scan finishes." under the panel buttons, the picked row or segment highlighted, and a Contents heading tag "· only .png, when the scan finishes"; after the scan the tag reads "· only .png" as before. | **QA ✔ (1f446be) extension picked mid-scan: "Applies when a scan finishes." + "· only .dat, when the scan finishes"; "· only .dat" after the scan.** |
| SM-60 | S3 | Settings and Filters windows are invisible to screen readers | VERIFIED | a43b72a, 1473eed | While a screen reader is active, Settings and Filters open as windows inside the main window (with a Close button), so they and all their controls are in the accessibility tree; without a screen reader they stay separate windows. Each filter field is named by its row and column ("Size from / min" …), the Aa toggle is "Match case", the language list "Language", the path bar "Folder path". | **QA ✔ (8efc68b, screen reader active) Settings and Filters open inside the main window; every slider/spin button/check box, the filter entries ("Size from / min" …), "Match case", "Language", "Folder path", Apply/Clear and "Close window" are named; filters typed through them apply. Not checked on a real Plasma session.** |
| SM-61 | S3 | Copy/cut then paste inside spacemap does nothing unless another program holds text on the clipboard | VERIFIED | ffcca40, 1473eed | Ctrl+V pastes spacemap's own Ctrl+C/Ctrl+X pick whatever the clipboard holds, including nothing at all, with paths_to_clipboard on or off. A plain V doesn't paste, nor does a V released while a text field has the keyboard or right after the window got it. | **QA ✔ own pick pasted with an empty clipboard, with paths_to_clipboard on and off; a plain v, v typed in the jump field don't paste.** |
| SM-62 | S3 | Move says "Moved N item(s)" and "Issues: none" when nothing (or only part) was moved | VERIFIED | 9d1c1ce, 1473eed | Free space is checked before anything is written, also for items outside the scanned folder: too little room refuses ("Not enough free space…" in the status line and Issues), nothing written, the pick kept. A move to another filesystem removes each file/folder from the source once it is safely at the target, so only what couldn't be moved stays. Named pipes are recreated; sockets and devices are skipped with a note. The status says what happened, e.g. "Moved 0 items · 1 item not fully moved: what couldn't be moved is still where it was (see Issues)", and the notes stay in Issues after the target's rescan. Copies keep hard links between the copied files. Replace is never offered onto the same file under another name (link or hard link to the source), never writes through a link, pipe or shared hard link at the target, and a failed copy leaves the old target file as it was. | **QA ✔ 20 MiB into 8 MiB: "Not enough free space … 20 MiB needed, 8 MiB free", nothing written; cross-fs move with FIFO/socket/read-only target sub-folder: FIFO recreated, socket skipped with a note, unmovable part stays in the source, nothing lost, status "… 1 item not fully moved (see Issues)"; copies keep hard links; Replace never offered onto a link/hard link of the source, never writes through a link or FIFO, a failing Replace keeps the old target.** |
| SM-63 | S3 | Clipboard exchange with file managers on X11: no cut marker, no GNOME format, cut-in-file-manager pastes as copy, one odd name blocks the paste | VERIFIED | f1066f2, 1473eed | On X11 as on Wayland, Ctrl+C/Ctrl+X offer text/uri-list (CRLF), x-special/gnome-copied-files (copy/cut), application/x-kde-cutselection (0/1) and plain text (UTF8_STRING, text/plain;charset=utf-8). Files cut in a file manager (GNOME or KDE mark) are moved on Ctrl+V. File lists are read byte for byte: names that aren't UTF-8 paste; missing ones are reported and the rest still paste. A clipboard owner that doesn't answer within about a second stops the paste with "The clipboard didn't answer: nothing was pasted". | **QA ✔ (X11, with a GTK clipboard client offering what Dolphin/Nautilus offer) Ctrl+C/X offer CRLF uri-list, GNOME copied-files, KDE cut mark and text; odd names byte-exact; a file-manager cut is moved; non-UTF-8 names paste. Real Dolphin not tested yet. See SM-67, SM-68.** |
| SM-64 | S3 | Contents table goes blank for good after fast key repeat (holding ↓) | VERIFIED | 52951ff | Holding ↓ (or any key) at any window height keeps the table and the cursor row in view; it can no longer go blank. | **QA ✔ fast key repeat through 300 rows, folder view and flat list, 900/1000/1300/1400 px: never blank.** |
| SM-65 | S4 | A file hard-linked into two folders: its size jumps between the folders on every rescan | VERIFIED | a8c5e6f, 1d6d9d5, 1473eed | A file with several hard links counts under its first name in byte order (e.g. a-c/x before a/b/x), the same in every scan, and also when only a folder is scanned again (r): a file counted outside that folder counts nothing in it, so its sizes match the full scan's. Folders holding such files list equal sizes by name, so the order doesn't change between scans either. | **QA ✔ "a-c/x" gets the size before "a/b/x", "A/" before "z/" and a non-UTF-8 name; identical over 3 scans; rescanning a/ shows 0 B for the file counted outside it, root unchanged afterwards.** |
| SM-66 | S4 | Path bar drops trailing spaces: a folder named "name " cannot be opened by typing its path | VERIFIED | e427b04 | The path bar opens a path exactly as typed when it exists (names starting or ending with spaces open); otherwise it's tried without the spaces around it. | **QA ✔ with both "x" and "x " present each opens exactly; " lead" opens; an absent "x  " falls back to "x".** |
| SM-67 | S3 | Ctrl+V freezes the whole window for ~25 s when the clipboard owner doesn't answer | WONTFIX (agreed) | — | User decision 2026-10-03: known limitation. The wait happens inside the GUI toolkit's own clipboard read on Ctrl+V (it tries several text formats, ~4 s each, before the app sees the key), which the toolkit gives no way to shorten or switch off; it only happens while another program's clipboard is hung. Nothing is pasted afterwards (as seen). | **QA: user decision relayed by the coder (2026-10-03); limitation of the toolkit's clipboard read; nothing is pasted afterwards (checked).** **User confirmed WONTFIX directly to QA (2026-10-04); a report about the toolkit behaviour has been made.** |
| SM-68 | S4 | A file-manager list with a path that no longer exists: the missing entry isn't reported | VERIFIED | c117faa | Entries of a pasted file list that no longer exist are listed in Issues ("Couldn't copy …: not found") and stay listed after the target folder's rescan; the rest are pasted. | **QA ✔ (2d2c2c8) missing entries listed as "Couldn't copy …: not found" (all missing, non-UTF-8 name escaped, missing parent folder, next to a clash), the rest pasted; 1,000 missing of 2,000 → 1,000 copied, Issues capped by the log limit.** |
| SM-69 | S4 | Whole-drive scan progress ("N% · X files · Y") isn't exposed to screen readers | VERIFIED | c117faa | The whole-drive progress bar carries its text ("N% · X files · Y") as its name for screen readers, together with its value. | **QA ✔ whole-filesystem scan: progress bar named "6% · 431,585 files · 20.3 MiB", % only grows.** |
| SM-70 | S4 | Scanning a folder that directly holds ~300,000 files: the heading says "0 folders, 0 files" and the table stays empty until the end | VERIFIED | c117faa | During a scan the contents heading reads "· X files so far" (all files found under the folder shown so far) instead of "0 folders, 0 files"; files directly in a folder still appear as rows once that folder has been read. | **QA ✔ 300k-file folder: heading "2,761 files so far" … "283,692 files so far" (only grows, ≤ final), then "0 folders, 300,000 files".** |
| SM-71 | S3 | After an in-place delete (or move) the chart differs from a fresh scan of the same data: a folder's slice colour and its child slices aren't redone | VERIFIED | | | **QA ✔ (319b5ba) after D, T, cut+paste, copy+paste, D under a category, D under a filter, a delete that makes two folders equal, one that drops a folder below its sibling (category picked) and grow/shrink + r: every folder, the bar, the extensions and the chart pixels equal a fresh scan. Exception: SM-73.** |
| SM-72 | S4 | Rows of exactly equal size: order after a delete differs from a rescan / fresh scan (and fresh scans order ties by disk order, not by name) | VERIFIED | | | **QA ✔ equal sizes listed by name (byte order) in fresh scans, after a delete and after r ("eqA, eqB"; "alpha, mid, zeta").** |
| SM-73 | S4 | After deleting/trashing many files from a folder, the folder's own size isn't updated until a rescan (filesystem freed its directory blocks) | VERIFIED | 18fe611 | After a delete, trash or move, each folder that lost entries takes its own size from the disk again (counted the way the scan counted it: disk space, or lengths with apparent sizes on), and the folders above change by the same amount, so the row and chart equal a fresh scan or r. |  **QA ✔ (18fe611) mass trash under a filter on XFS, several folders losing entries in one action (flat-list marks), a folder emptied completely, a folder deleted with its parent in one selection — disk and apparent size: live = fresh scan, chart pixels too.** |
| SM-74 | S4 | Command line: rows of equal size or equal date are in a different order than in the app | VERIFIED | | |  **QA ✔ (d6c7a2e) list --sort size/name/modified/changed, default and --reverse, give exactly the app table's order (ties: byte order; date ties by size, then name); --limit with --reverse = first rows of --reverse; header says "1 file".** |
| SM-75 | S4 | Command line: with stdout closed, list and flat exit 0 with no message (output silently lost) | OPEN | | |  **QA: coder's explanation checked against behaviour (closed fd 1 already points at /dev/null before main, so it can't be told apart from >/dev/null; a real write error — full disk — gives exit 1). Waiting for the user's WONTFIX decision.** |

Severity: **S1** data loss/safety · **S2** wrong numbers / missed data · **S3** functional/UX bug · **S4** polish / a11y

---

## Details

### SM-00 · **S1 — DATA LOSS** · Delete follows into mounted filesystems and wipes them
The UI marks a mount point inside the scanned tree as `name [other filesystem]`, 0 B, and leaves it out of totals.
Permanent delete then recurses **into** that mount and deletes everything on the other filesystem.
Reproduced 3 times with a dummy tmpfs:
```
/var/tmp/spacemap-qa/mnt/hasmount/            (XFS)
├── local.bin              1 MiB
└── m/  [other filesystem] (tmpfs, holds victim_on_tmpfs.bin 4 MiB + sub/nested.txt)
```
1. Cursor on `hasmount` → `D`. Dialog: "The folder and everything in it (**1.00 MB, file count: 1**)". Delete.
   Result: **every file on the tmpfs deleted** (including nested dirs), then "Couldn't delete …/hasmount: Device or resource busy".
   `local.bin`, the only file the dialog promised to delete, **survives**. What was promised and what happened are inverted.
2. Open `hasmount`, cursor on the `m [other filesystem]` row → `D`. Dialog: "(**0 B**, file count: 1)". Delete: the entire mounted filesystem is wiped.
3. `T` (trash) on `hasmount`: the directory is renamed into `~/.local/share/Trash/files/hasmount`, **with the live mount inside it**
   (`findmnt` shows the tmpfs now mounted under the Trash). A later "Empty trash" would recurse into it.

Real-world impact on this machine: `/mnt/minipc/{MEDIA,18TB,Stage,…}` are CIFS mounts (MEDIA: 64 TB used). Scan `/mnt`, open `minipc`, and a NAS share shows as
`MEDIA [other filesystem] 0 B`, which looks like an empty folder. One `D` and a confirm on "0 B" starts deleting the NAS over SMB.
The same goes for USB drives under `/run/media/$USER`, bind mounts, `/home` on its own partition when deleting from `/`, and `/boot/efi`.

Expected:
- Never cross a filesystem boundary when deleting (`rm -r --one-file-system` semantics: compare `st_dev` of each dir with the root's, skip or refuse).
- Refuse `D`/`T` on a mount point itself, and on any folder that **contains** a mount point, or at least show a blocking warning that names the mounts.
- The dialog's size/count must describe exactly what will be removed.

### SM-01 · S2 · Sparse files are reported at apparent size, not disk usage
- Repro: `truncate -s 8G sparse.img` (+1 MiB written). Scan the parent.
- Actual: `sparse` = **8.00 GB**, 99.4% of the tree; chart is almost entirely this file.
- Expected (a *disk space* tool): ~1 MiB, the space actually allocated (`du`: 1,048,576 B). Whole tree uses 13.9 MiB on disk.
- Also: scanning `/proc` shows **128.00 TB** (from `/proc/kcore`).
- Impact: VM images, databases, torrent pre-allocations, `.cache` files look huge, so users may delete the wrong thing. No setting to switch between disk usage and apparent size.

### SM-02 · S2 · Hard links counted once per name
- Repro: one 10 MiB file + 4 hard links (`ln`) in `hard/`.
- Actual: `hard` = **50.00 MB**, Files 5. Expected 10 MiB on disk (du counts the inode once).
- Impact: inflated totals on hardlink-heavy trees (rsnapshot/backup trees, `ostree`, `/usr` on some distros, Steam/Proton, deduped media libraries).

### SM-03 · S3 · Size units are labelled decimal but computed binary
- 8 GiB (8,589,934,592 B) is shown as "8.00 GB"; 50 MiB as "50.00 MB"; 12,000 B as "11.72 KB".
- Either use KiB/MiB/GiB or divide by 1000. Right now totals don't match `df -h`/Dolphin (decimal) or `du -h` (binary, but labelled G). The difference is 7.4% at GB and 10% at TB, so on the 16 TB drive it's ~1.4 TB.

### SM-04 · S2 · Paths longer than PATH_MAX are not scanned
- Repro: 300 nested dirs of 24 chars each (full path ≈ 7.3 KB), one file at the bottom.
- Actual: Issue "couldn't be read (File name too long (os error 36))" at depth ~163; everything below is missing (`deep` = 0 B). `du`/`find` handle it (they traverse relative to directory fds).
- Expected: relative (`openat`-style) traversal, or at least a clear "N folders skipped" note.

### SM-05 · S3 · Files column: rows don't add up to the parent total
- In `adv`: rows show Files = 1 for the **empty** folders `empty`, `noperm`, `nox`, `deep` (they hold 0 readable files). The rows sum to 20,031 but the header says "Files: 20,027".
- Expected: 0 for folders with no files (or a documented rule applied consistently to header and rows).

### SM-06 · S3 · Issues bar: long error messages overflow and hide the actual error
- A long path error wraps across the whole bottom bar and is cut off, so the reason ("File name too long") is never visible. The bar takes space from the main view.
- Expected: elide the middle of the path (`/var/tmp/…/d62_x`), show the reason first, and make the bar scrollable or expandable.

### SM-07 · S4 · Directory entries' own size ignored
- `wide/` (20,000 empty files) shows 0 B; the directory itself takes 808 KiB on XFS (`du`). Minor, but visible on maildirs, caches and node_modules.

### SM-08 · S4 · Non-Latin names render as tofu boxes
- `ünïcödé 日本語 🎉` renders as `ünïcödé □□□ 🎉`; no CJK fallback font is loaded. Non-UTF-8 bytes render as `□` (acceptable).

### SM-09 · S4 · Accessibility: unlabeled controls
- The AT-SPI tree shows toolbar toggle buttons (Chart, Table, Filter, and one more) and **all** table sort headers (Size, Files, Modified…) with empty names. The chart exposes nothing. Screen-reader users can't use it, and automation can't address the controls.

### SM-10 · S3 · Two different files can look identical, with no way to tell them apart
- Repro: files `x\xffy` (non-UTF-8, 1000 B) and `x\u{FFFD}y` (valid UTF-8 look-alike, 2000 B) in one folder.
- Both rows, the delete dialog and the details panel show `x□y` / `x�y`. The user can't tell which one a dialog refers to.
- Expected: escape invalid bytes visibly, e.g. `x\xFFy` (as `ls -b` does) or a ⚠ marker, in rows, dialog and details.
- Deletion itself acted on the **correct** raw name (see Passed tests).

### SM-11 · S3 · Delete of an already-vanished file leaves a ghost row and mark
- Repro: mark `alpha`, `rm alpha` outside the app, press D → Delete.
- Actual: Issue "Couldn't delete …/alpha: No such file or directory"; the row stays and stays marked ("1 marked (5.72 MB)"). Header totals still include it.
- Expected: ENOENT means the goal is already reached, so drop the row and the mark and update totals (maybe note "was already gone").

### SM-12 · S4 (was S3) · Filters: after an invalid value the table keeps the previous filter
> **QA correction (re-test):** the panel *does* show an error, e.g. "Min size: 'nan' is not a size (e.g. 500M, 1.5G)". Round 1 missed it, so the "no error or red outline" line below is wrong. Remaining issue: the rows still reflect the previous valid filter. Either clear that bound or visibly mark the result as stale.
- Repro: Size min `0.5K` → Apply (9 rows). Change it to `1.5.5G` → Apply: still 9 rows. Then `  7M ` → 1 row; change to `NaN` → Apply: still 1 row.
- The field shows `NaN`, but the table still uses 7M. The same happens with `1TD` and `1e3M`. There's no error or red outline.
- Expected: reject the invalid field visibly and don't apply, or clear that bound; never keep a hidden stale value.
- Also: `-5M` is accepted silently.

### SM-13 · S4 · Delete dialog doesn't warn about unreadable content
- `locked/` shows "1.00 MB, file count: 1", but it contains an unreadable subfolder of unknown size. Delete then fails halfway ("Permission denied") and leaves a partial folder.
- Expected: the dialog says "contains 1 unreadable folder — size unknown, may fail". After a partial failure, report how much was deleted.

### SM-14 · S4 · Name sort is case-sensitive ASCII
- A→Z gives `.dotfile, Dx, Tx, aaa_new, alpha, …`. File managers (Dolphin, Nautilus) sort case-insensitively and naturally (`file2` < `file10`).

### SM-15 · S4 · Details panel (`i`) on a symlink doesn't say it's a symlink or show its target
- For `link_to_vfile` it shows Size 33 B, perms 777, and nothing about the target `/var/tmp/.../victim/vfile`. The delete dialog calls it "File".
- Expected: "Symbolic link → /target (target not included in totals)".

### SM-16 · S4 · Filter name field: a literal space can't be matched
- Spaces (and commas) separate patterns, so `sp ace*` becomes two patterns. Leading spaces are trimmed (` leading space` can't be targeted). There's no quoting or escape.

### SM-17 · S4 (upstream) · accesskit thread panics when the AT-SPI registry isn't activatable
- With a11y enabled but `org.a11y.atspi.Registry` not activatable (minimal sessions, some containers, a broken at-spi install), stderr shows:
  `thread '<unnamed>' panicked at accesskit_unix-0.21.1/src/context.rs:61:78: called Result::unwrap() on an Err value … NameHasNoOwner`.
- The app keeps running, but accessibility is dead for the session. Worth checking for a newer accesskit, or reporting upstream.

### SM-18 · S3 · Table view does O(n) work on every keypress in big folders
> **QA re-test (b532cc2):** 300k folder: ↓ ≈ 156 ms vs 113 ms baseline, but the **size-sort toggle now costs ≈ 1,150 ms per press** (round 1: ≈ 446 ms, different fixture). Possible regression, please check.
- Folder with 300,000 files, table view. CPU per key press, averaged over 10 presses:
  - tiny folder (11 rows), any key: ~112 ms (rendering floor under llvmpipe)
  - 300k folder, `↓`/`↑`: **~194 ms / ~105 ms**, so ≈ +80 ms of work just to move the cursor
  - 300k folder, `s` (re-sort): **~446 ms**, so ≈ +330 ms per sort toggle
- Moving the cursor shouldn't depend on N. Likely candidates: rebuilding, filtering or sorting the row vector every frame, or recomputing the "By file extension" table every frame.
- Expected: cache the sorted/filtered row index and invalidate it only on sort, filter or tree change. Sorting 300k entries by u64 should take <30 ms (with cached lowercase keys for name sort).
- Memory: 276 B/entry means a 10M-file NAS scan needs ~2.8 GB. Consider interning names (one arena `Vec<u8>` + offsets), u32 child indices, and packing mtime/ctime/mode.

### SM-19 · S3 · Memory is retained after rescans (≈ one extra tree, and it keeps creeping)
- 16TB drive (4.15M files): RSS after first scan 2.05 GB → after rescans **3.41 → 4.27 → 4.69 → 4.86 GB**, for the same tree.
- 1M-file synthetic tree, 8 rescans: default glibc `519 | 782 815 849 861 863 868 869 870 MB`; with `MALLOC_ARENA_MAX=1`: `517 | 779 784 787 787 789 789 789 789 MB`.
- So it isn't an unbounded leak: the old tree's memory is freed but kept by glibc (and fragmented across per-thread arenas from the parallel scan).
- Expected: RSS returns close to one tree after a rescan. Options: `malloc_trim(0)` after dropping the old tree, a compact arena-backed tree (few big allocations), or a different global allocator (mimalloc/jemalloc). Also consider dropping the old tree *before* the new scan when memory is tight. Peak during a rescan is 2 trees.

### SM-20 · S3 · Status bar shows the previous "Scan completed in X s" while a new scan runs
- Repro: scan `/usr` (0.2 s), then start scanning the 16TB drive. For 5 minutes the bottom-right still reads "Scan completed in 0.2s" while the progress bar is moving.
- Expected: "Scanning… N items, elapsed M s" while running. Clear the old message when a new scan starts.

### SM-21 · S3 · Changing one out-of-range setting wipes the whole configuration
- Repro: settings.json with `"language":"es"`, `"sort":"files"`, `"dirs_first":true`, `"ring_sat":0.9` and one bad value `"stroke_alpha":999`. Launch.
- Actual: the whole file goes to `settings.json.bad`, and everything resets to defaults (language back to English, sort, folders-first, colours).
- The same happens for `max_render_depth:-1`, an unknown `sort` value, an unknown column name, or `"name"` in `hidden_columns`. It also happens if a *future* version adds a sort key and the user downgrades.
- Expected: per-field fallback (`#[serde(default)]` + lenient per-field parsing). Clamp numeric values; only replace invalid fields. Numeric out-of-range values like depth 99 are already clamped correctly (see Passed).

### SM-22 · S4 · Settings read has no size limit (a symlink to `/dev/zero` eats RAM)
- `settings.json → /dev/zero`: RSS **15.6 GB within ~10 s**, 2 cores busy, the window never appears (killed by the harness). `/dev/urandom`: 3.4 GB and climbing.
- Contrived, but a 1-line guard (refuse a non-regular file or >1 MB) prevents a machine-freezing OOM from a corrupted or odd config.

### SM-23 · S4 · Saving settings replaces a symlinked settings.json with a regular file
- `settings.json → ~/dotfiles/spacemap.json` (stow/chezmoi style). After launch, `settings.json` is a regular file and the dotfiles copy is stale.
- It also resets a deliberate `chmod 444` to 644. Expected: resolve the symlink and write-rename in the *target's* directory, preserving the mode.

### SM-24 · S4 · settings.json is a directory: silent failure and a leftover `.tmp`
- Launch works (defaults), but every save leaves `settings.json.tmp`, and settings silently never persist. No Issue is shown.

### SM-25 · S4 · Scan cancel is undiscoverable; `r` doesn't rescan in chart view
- A running scan can only be cancelled with **Esc** ("Scan aborted", previous tree restored). There's no visible Stop button and no hint next to the progress bar. The ⟳ button isn't named in the a11y tree either (it is "⟳").
- `r` did nothing in chart view (16TB root). The ⟳ toolbar button works.

### SM-26 · S4 · Wrong error wording for files
- NTFS file whose name the kernel can't resolve: Issue says "…/Bench͜ystemConfig(2).Gbx — **can't find that directory**". It's a file. Use "can't read this entry (No such file or directory)".

### SM-27 · S4 · Scanning is ~2× slower when accessibility is active
- 16TB cold scan: 133 s with no AT-SPI consumer vs 288 s with an AT-SPI registry active (same drive, same cache state: fresh mount vs `drop_caches`).
- Likely cause: the progress UI regenerates a large AccessKit tree every frame during the scan (the chart is redrawn live). Screen-reader users, and any desktop where at-spi is always on (GNOME with some extensions, KDE with accessibility), pay for it.
- Suggestion: throttle live chart updates during scans to ~4 Hz, and don't emit accesskit nodes for chart slices.

### SM-28 · S3 · Trashing something that is already in the Trash corrupts its restore info
- Repro (dummy): `T` on `trtest/victim.bin` → it goes to `Trash/files/victim.bin` with `Path=/var/tmp/…/trtest/victim.bin`. Scan `~/.local/share/Trash`, open `files`, and press `T` on `victim.bin`.
- Actual: it's renamed to `Trash/files/victim.bin.2` with a **new** `victim.bin.2.trashinfo` saying `Path=…/Trash/files/victim.bin`. The original `victim.bin.trashinfo` stays behind **orphaned**.
- Result: the file manager's "Restore" puts it back *into the Trash*. The real original location is effectively lost, and the orphan shows as a broken entry.
- Expected: inside a trash directory (`$XDG_DATA_HOME/Trash`, `$topdir/.Trash-$uid`, `$topdir/.Trash/$uid`), refuse `T`, or offer "Delete permanently" instead.
- Also observed: random-input testing trashed the Trash's own `files/` and `info/` directories and the app's own `~/.config` (all dummy data inside the read-only sandbox). The app has no guard against trashing its own config or the Trash's own folders.

### SM-29 · S3 · Path bar: `~` is not expanded; misleading "Not a directory" for missing paths
- `~`, `~/` → "Not a directory: ~". The placeholder invites typing a path, and `~/Downloads` is what most people type.
- `/nonexistent/dir` → "Not a directory". It doesn't exist (ENOENT); say so.
- Expected: expand `~` and `~/…`. Distinguish "doesn't exist", "is a file" and "permission denied".

### SM-30 · S4 · Paths are not canonicalized
- `./` is accepted and scanned relative to the process CWD. Breadcrumbs show `'' './'`.
- `/usr/../etc` → breadcrumbs `/ › usr › /usr/.. › etc`, and Issues show `/usr/../etc/pki/…`.
- `////usr////` → Issues show `////usr////share/empty.sshd`.
- Expected: `canonicalize()` (or at least lexical normalization plus absolutizing) before scanning. Non-canonical roots also make every path shown in delete dialogs harder to verify.

### SM-31 · S3 · Chart context menu stays open (bound to a hidden item) while the keyboard navigates elsewhere
- Repro (dummy): zoom into `chart/sub`, right-click `inner_target.bin`, then with the menu open press `Backspace`, `↓`, `↓`.
- Actual: the view goes up to `chart`, the header shows `…/chart/other` highlighted, the hover card shows `…/chart/sub`, and **the menu is still open**. "Move to Trash" then trashes `sub/inner_target.bin`, which is no longer on screen, with no confirmation.
- The F-4 contract ("never a different item than the one right-clicked") technically holds, but everything on screen tells the user they're acting on `other` or `sub`.
- Expected: close the context menu on any navigation, zoom or highlight change (or swallow all keys except Esc/menu navigation while it's open). Also hide the hover card while a menu is open (it overlaps the menu).

### SM-32 · S4 · Permanent delete fails on trees deeper than PATH_MAX (scan and Trash work)
- Found in the re-test of b532cc2. Repro: the 300-level fixture from SM-04 (path ≈ 7.3 KB). The scan now works. Cursor on its top folder → `D` → Delete.
- Actual: "Couldn't delete …/deep: File name too long (os error 36)". Nothing is deleted (all 302 entries remain) and the row stays. It fails cleanly, which is good.
- `T` on the same folder works (moved to Trash). So once it's in the Trash, the user can't empty it with spacemap either.
- Expected: permanent delete handles any tree the scanner can read.

### SM-33 · S4 · Re-sorting a very large folder lags (~0.3 s per sort key press)
- Scenario: a folder with a few hundred thousand entries, table view. Press any sort key repeatedly (`s`, `n`, …).
- Actual: each press takes ≈ 0.3 s until the table settles, and it's about the same for size and name sort. Each press also burns a lot of CPU across threads (on the order of a second or more of CPU time).
- Cursor movement in the same folder is now instant (SM-18 fixed), so the sort is the remaining lag.
- Expected: sort changes feel instant (≲ 0.1 s) at this size.

### SM-34 · S4 · Names starting with accented letters sort after Z
- Scenario: a folder with names like `Echo`, `émile`, `zeta` (common with Spanish/French names: `Árbol`, `Óscar`, `Émile`). Sort by name A→Z.
- Actual: `Echo … zeta, émile`. Accented initials land after `z`.
- Expected: accented letters sort with their base letter (`Echo, émile, zeta`), as Dolphin does.

### SM-35 · S3 · Table view is replaced by the chart while a scan runs (reported by the user)
- Scenario: table (Summary) view selected; start a scan of a new source (path bar, 🔍, /, 🏠 or a rescan).
- Actual: while the scan runs, the chart is drawn and growing, even though the Table toggle stays highlighted. The table only comes back when the scan finishes.
- Expected (the user's request): the selected view stays. In table view, the rows appear and refresh live as data arrives, as lively as the chart's growing rings:
  - rows are added as they're discovered; size, %, bars and file counts grow; order follows the active sort;
  - the progress bar and "Esc to cancel" stay visible;
  - the cursor stays on the same *item* while rows re-order, and it doesn't jump or flicker; keyboard and scrolling keep working;
  - refreshes are throttled so a very large scan stays smooth.
- **Switching views during a scan** (user question): clicking Table or Chart while scanning highlights the clicked toggle, but the view doesn't change. The toggle then shows Table while the chart is on screen. The choice only applies after the scan ends. No crash or wrong totals, even with rapid toggling. Expected: the switch applies immediately, in both directions, and the scan continues unaffected.
- Please also decide and make clear what Delete/Trash do on an item **during** a scan: its size and count are partial then. Either disable them until the scan completes, or say "so far" in the dialog.

### SM-36 · S3 · App crashes (stack overflow) when scanning a very deeply nested folder tree
- Scenario: a folder containing a chain of roughly **1,000 or more** nested subfolders (short names are enough, e.g. `a/a/a/…`). These trees come from runaway scripts, recursive copies/backups, broken build tools, or a hostile archive.
- Actual: the whole app exits during the scan with "thread … has overflowed its stack". It happens as soon as the scan reaches the deep branch, so scanning a parent (e.g. the home folder) that contains such a tree also crashes. Around 800 levels still works.
- Expected: no crash at any depth the filesystem allows (the scan, the chart/table, sorting, delete, trash, rescan and closing the tree). Please test far deeper than the threshold, since several code paths may each have their own limit.

### SM-37 · S4 · Files dated exactly 1970-01-01 00:00 UTC show "-" instead of a date
- Scenario: a file whose modification time is exactly the Unix epoch. This is common: Flatpak/ostree deployments, Nix, reproducible builds and some archives set it on purpose.
- Actual: the Modified column shows "-", the same as "unknown". Dates just before or after it display fine.
- Expected: show `1970-01-01 …` (local time); reserve "-" for genuinely unknown values.

### SM-38 · S4 · Apparent-size totals above 16 EiB wrap around; no EiB unit
- Scenario: "Count apparent size" enabled on a folder of huge sparse files whose apparent sizes add up to more than 16 EiB (e.g. five 4 EiB sparse files; the filesystem allows them).
- Actual: the total shows **4096.00 PiB** (it wraps around instead of reaching 20 EiB), and the chart's slices don't add up to the parent. There's no crash. Sizes also never switch to EiB ("4096.00 PiB").
- Expected: saturate or use a wider total so it never wraps; show EiB for sizes ≥ 1 EiB.

### SM-39 · S4 · Trash errors show raw internal error text
- Scenario: any Trash that fails, e.g. the drive's trash folder isn't writable, or the file is immutable.
- Actual: the Issue reads like a program dump: "Couldn't move …/one_byte.bin to the trash: Error during a `trash` operation: FileSystem { path: "/mnt/DATA/.Trash-1000", source: Os { code: 30, kind: ReadOnlyFilesystem, message: "Read-only file system" } }".
- Expected: a plain sentence, e.g. "Couldn't move one_byte.bin to the trash: the trash folder /mnt/DATA/.Trash-1000 is read-only." The file is correctly kept, so only the wording is wrong.

### SM-40 · S4 · Case-insensitive drives: path typed in a different case is shown in that case
- Scenario: on an exFAT/FAT/NTFS-style case-insensitive drive, type a folder path in the wrong case (e.g. `/mnt/DATA/MYFOLDER` for `myfolder`).
- Actual: the scan works and the numbers are right, but breadcrumbs, the delete dialog and Issues show the typed case (`…/MYFOLDER/notes.txt`), which doesn't exist by that name in a file manager.
- Expected: show the on-disk name. Delete acted on the correct file, so only the display is wrong.

### SM-41 · S4 · Sizes show needless decimals (".00"), a user request
- Scenario: any size display (table, chart hub, headers, dialogs, filters summary).
- Actual: `4.00 KiB`, `10.00 MiB`, `1.50 GiB`, `4096.00 PiB`. The trailing zeros carry no information.
- **User decision (2026-09-28):** at most **one decimal**, and drop a trailing `.0`: `4 KiB`, `10 MiB`, `1.5 GiB`, `95.4 MiB`, `723.9 MiB`, `4 EiB`. Counts stay integers. Keep the column right-aligned so the numbers line up.
- Also approved by the user: when a total is capped at the maximum, show it as `≥ 16 EiB` (see SM-38).

### SM-42 · S3 · Chart: one item stretched over a whole folder's arc — user report (2026-09-29)
- Scenario: a folder with many similar-sized items (e.g. a movie library of ~1200 folders of 30–170 GB) that takes a large share of the chart, on the default chart settings.
- Actual: only the single largest item got its own slice and was drawn over nearly the whole folder's arc; items almost as large (a few GB smaller) were lumped into a thin "other". A folder of very unevenly sized items instead drew its tail as a band of 1–2 px slivers.
- Expected (user-approved behaviour): similar-sized items get comparable slices, in proportion to each other; a ring shows as many slices as fit at the min slice angle (never more than 360 around a full circle, nor more than the "Max slices" setting); no slice is narrower than the min angle; whatever doesn't fit goes into one min-width "other" slice at the end of the ring.

### SM-43 · S1 · Delete/Trash of a folder while a filter or category is active removes hidden files
- Scenario: pick a category in the bar (e.g. Video), or set a name pattern in the Filters panel. A folder that also holds other files now shows only its matching files (e.g. "1 file, 6 MiB"). Select that folder and press D (or T).
- Actual: the dialog says "The folder and everything in it (6 MiB, file count: 1)". Confirming permanently deletes the whole folder, including the files the filter hid, which the dialog never mentions. T moves the whole folder to the trash without a dialog. This also happens with the Filters panel alone, but a category makes it a single click away.
- Expected: the dialog must describe what will actually be deleted (the folder's real size and file count, and that N files are hidden by the active filter). Alternatively, delete/trash act only on the files shown, but whichever is chosen must match what the dialog says.

### SM-44 · S3 · Problems in the categories file are never visible
- Scenario: the categories file has invalid JSON, a duplicate extension, an entry without a name, more than 8 categories, or is a folder/special file. Start the app and scan anything, or edit the file and start a new scan (the moment a user who just edited the file would look).
- Actual: the Issue appears on the start screen, but it's cleared as soon as a scan starts, so it shows "Issues: none" while the fallback or skipped entries are in effect. Duplicates, skipped entries and the 9+ categories note were never visible in any case tried.
- Expected: file problems stay in Issues for the scan that uses that file (they're re-reported on each reload).

### SM-45 · S3 · Chart view with a picked category: no sign of the category and no way to clear it
- Scenario: pick a category in the Summary view, then switch to the chart.
- Actual: the chart, its hub and "Size/Files" show only that category's files, but nothing names the category. The Filters button shows "active", but the Filters panel is empty and its Clear doesn't remove the category. The only way out is to go back to the Summary view.
- Expected: the chart view shows which category is active (e.g. next to the size) and offers a way to clear it; Filters → Clear should clear it too, or the panel should say that a category is active.

### SM-46 · S3 · Picked category absent from the current folder: empty table with no explanation
- Scenario: pick a category, then open or scan a folder that has no files of that category.
- Actual: the table is empty with no message, and the bar has no segment for the picked category (everything is dimmed). The folder looks empty, although it isn't.
- Expected: say so, e.g. "No Video files here (N other files hidden) ✕", or keep the picked category visible at 0.

### SM-47 · S4 · Category bar is not accessible
- Scenario: screen reader or keyboard use of the Categories panel.
- Actual: the segments and labels are exposed as unnamed elements, so their names and sizes aren't read and they can't be reached with the keyboard. Only the ✕ button is named.
- Expected: each segment/label is a named button (e.g. "Video, 48%, 17 MiB, 7 files") that can be focused and activated with the keyboard.

### SM-48 · S4 · Category ✕ (clear) button draws as an empty box
- Scenario: pick any category and look at the button next to "Categories".
- Actual: it shows an empty rectangle (a missing glyph), although its accessible name is "✕". The Filters panel's close button draws fine.
- Expected: a visible ✕ (use the same glyph/icon as the other close buttons).

### SM-49 · S4 · Category panel in short or narrow windows
- Scenario: make the window about 300 px tall or less, or about 400 px wide or less.
- Actual: when short, the labels are drawn on top of each other and are unreadable. When narrow, the panel keeps its full width and squeezes the contents table to almost nothing.
- Expected: drop or abbreviate labels that don't fit (tooltips still work), and narrow or hide the panel when the window is narrow.

### SM-50 · S4 · Spanish UI: the category panel stays English with the installed language file
- Scenario: language Spanish using the language file currently installed in the user's config folder.
- Actual: the rest of the UI is Spanish, but "Categories", the category names, "files" and "Click to show only these files" are English.
- Expected: the installed language file gets the new strings when the app is updated, or missing strings fall back to built-in Spanish, not English.

### SM-51 · S4 · Category names and extensions: display oddities
- A user category named "Other" appears next to the built-in grey "Other", giving two "Other" entries. Expected: reject or rename it with an Issue.
- A very long category name is cut off at the panel edge with no ellipsis. Expected: ellipsis, with the full name in the tooltip.
- A file whose extension has a trailing space ("x.mkv ") is listed in Other's tooltip as ".mkv", which reads like a contradiction. Expected: show the space, e.g. quoted.
- For the user to decide: the default list puts `.ts` in Video (so TypeScript source shows as Video) and `.idx` in Video (so git pack indexes show as Video).
- Note (not a bug): `r` (rescan folder) does reread the categories file and clears a picked category if the list changed. The coder's description said it doesn't.

### SM-52 · S3 · Light KDE colour scheme: highlighted text is white on white
- Scenario: a light KDE colour scheme (e.g. Breeze Light) in the user's kdeglobals, which spacemap follows.
- Actual: every text drawn in the "strong/highlight" colour is invisible or nearly so: the category names in the bar (only "48.0% · 17 MiB" can be read), the tooltip's title line, the picked category's label (white on light blue), the hovered label (dark box, dark text), the current folder in the breadcrumbs (the path ends at "cat /"), and in the chart the path line and the hub's name and size. Table header labels are very faint too.
- Expected: text readable against the scheme's background (use the scheme's text colour for strong text), for all of the above.

### SM-53 · S1 · btrfs: Delete on a subvolume or snapshot removes all of it; the dialog says it is empty
- Scenario: on btrfs (the default on Fedora and openSUSE), a folder contains a subvolume or a snapshot (snapper snapshots, container or VM storage, a user's own subvolume). Select it and press D.
- Actual: the row reads "[other filesystem]", 0 B. The dialog says "The folder and everything in it (0 B, file count: 0)". Confirming permanently deletes the whole snapshot and all its files (tens of MB in our case), and reports nothing in Issues.
- Expected: the dialog states what will really be deleted (it isn't empty), or delete refuses as it does for other-device folders ("permanent delete also stops at any other-device folder" — SM-00), with a clear message. Trash should behave consistently.

### SM-54 · S2 · btrfs: subvolumes and snapshots are not counted
- Scenario: scan a btrfs folder that contains subvolumes or snapshots (not separate mounts).
- Actual: they're listed as "[other filesystem]" with 0 B and 0 files; opening one shows it empty; totals leave them out (e.g. 250 MiB where du reports 371 MiB). On a Fedora/openSUSE disk this can hide snapshots and container storage, often the biggest space users.
- Expected: a subvolume that isn't a mount point is part of the scanned filesystem: scan and count it (du does). If snapshots share data with their source, say so (e.g. a note), rather than showing 0.
- Related, informational: on compressed btrfs, sizes are the uncompressed sizes (like du); real usage can be far smaller.

### SM-55 · S3 · User language file that never ends: startup hang or runaway memory
- Scenario: ~/.config/spacemap/lang/<code>.lang is a FIFO, or a symlink to a device such as /dev/zero (a mistake, a sync tool, or a hostile config).
- Actual: FIFO → startup blocks ~10+ s and the window stays unusable. /dev/zero → memory grows to 13 GB within 2 s (our guard killed it); on a normal desktop this can freeze the machine. categories.json and settings.json are already safe against the same files.
- Expected: same rules as categories.json: regular files only, a size cap, non-blocking open; report in Issues and fall back to the built-in language.

### SM-56 · S4 · One-shot action buttons are announced as toggle buttons
- Scenario: screen reader on the toolbar, since the buttons were made the same square size.
- Actual: "Rescan this folder", "Empty trash" and "Choose any drive, mount point or folder to scan" are exposed as toggle buttons (read as "toggle button, not pressed").
- Expected: plain buttons for actions; toggle buttons only for on/off controls (views, Filters, Chart settings).

### SM-57 · S4 · "(no extension)" is cut off in the Extensions table
- Scenario: Extensions view at the default panel width.
- Actual: the row reads "(no extens…".
- Expected: the label fits (wider column or a shorter label such as "(none)"), with the full text in the tooltip.

### SM-58 · S4 · Extension rows can't be picked with a screen reader
- Scenario: screen reader in the Extensions view (picking an extension became possible in 53b33ee).
- Actual: each row is exposed as three plain text labels (extension, size, files); nothing is a button, so the pick can't be reached or announced. The category bar got this right in SM-47.
- Expected: each row is a named, selectable button (e.g. ".mkv, 11 MiB, files: 6"), marked selected when picked.

### SM-59 · S4 · Picking an extension during a scan gives no feedback
- Scenario: Extensions view, start a scan, click an extension row while it runs.
- Actual: nothing shows the pick: no "· only .x" tag, no "Applies when a scan finishes." note (a category picked mid-scan shows that note). The pick does apply when the scan ends.
- Expected: the same feedback as a category pick mid-scan.

### SM-60 · S3 · Settings and Filters windows are invisible to screen readers
- Scenario: with a screen reader (AT-SPI) running, open ⚙ Chart settings or Filters (they now open as separate windows).
- Actual: the new windows expose nothing: no frame, no sliders, check boxes, entries or Apply/Clear buttons in the accessibility tree; the main window's tree is unchanged. A screen-reader user can no longer change any setting or use the filters (both were reachable before they became separate windows). Keyboard focus into those windows was not testable for the same reason.
- Expected: each window exposed with its controls named, as in the main window (SM-09 / SM-58 level).

### SM-61 · S3 · Copy/cut then paste inside spacemap does nothing unless another program holds text on the clipboard
- Scenario (X11 session without a clipboard manager, e.g. a plain window manager; also the state right after login): mark or put the cursor on a file, Ctrl+C (or Ctrl+X), open another folder of the same scan, Ctrl+V.
- Actual: nothing happens — no copy, no progress, no message; the heading keeps "· 1 to copy". Same with `paths_to_clipboard` on and off. As soon as any other program owns plain text on the clipboard, the same Ctrl+V pastes correctly; a file list offered by a file manager also pastes.
- Expected: Ctrl+V pastes spacemap's own pick whatever the clipboard holds (or doesn't hold); if it can't, say why.

### SM-62 · S3 · Move says "Moved N item(s)" and "Issues: none" when nothing (or only part) was moved
- Scenario A: Ctrl+X a 20 MiB folder (two files, 12 + 8 MiB), paste into a filesystem with 8 MiB free.
- Actual A: no free-space refusal; the 8 MiB file is copied into the target, the 12 MiB one is not, no error; status "Moved 1 item(s)", "Issues: none"; the source stays complete. The user is told the move succeeded while a half folder sits in the target.
- Scenario B: Ctrl+X a folder that contains one FIFO (named pipe) among ~1,600 normal entries, paste into another filesystem.
- Actual B: every normal entry is copied, then NOTHING is removed from the source (all ~120 MB now exist twice); status "Moved 1 item(s)", no note about the skipped FIFO or the kept source. Without the FIFO the same move works perfectly.
- Expected: free space checked first ("not enough free space", nothing written); any skipped/failed item reported; the status says what really happened (e.g. "Copied 1,580 items; 1 special file skipped — source kept"); copied items that were safely duplicated are removed from the source, or the user is told why not.

### SM-63 · S3 · Clipboard exchange with file managers on X11
- Scenario: X11 session; spacemap Ctrl+C / Ctrl+X on several files (incl. names with spaces, newlines, non-UTF-8 bytes), then paste in a file manager; and the reverse direction.
- Actual (spacemap → file manager): only `text/uri-list` is offered (URIs correctly percent-encoded, but lines end with LF, not CRLF). No `x-special/gnome-copied-files` (GNOME Files / Nautilus can't paste), no cut marker after Ctrl+X (`application/x-kde-cutselection` or "cut" in the GNOME format), so a file manager COPIES what spacemap marked to move; no plain text, although the setting says Ctrl+C/X put paths on the clipboard.
- Actual (file manager → spacemap): files offered as "cut" (GNOME "cut" + KDE cut marker) are pasted as a copy and the originals stay. If the offered list contains one non-UTF-8 file name (percent-encoded `%FF`), the whole paste silently does nothing — the valid files in the same list aren't pasted either, no message.
- Expected: the standard formats both ways (uri-list with CRLF, GNOME copied-files, KDE cut marker, plain text when the setting is on); cut honoured in both directions; every valid path pasted and the bad one reported.
- Note: tested on X11 only (no Wayland compositor available to QA). Missing paths in the list are handled well ("Couldn't copy …: not found").

### SM-64 · S3 · Contents table goes blank for good after fast key repeat (holding ↓)
- Scenario: a folder with ~300 entries (folder view) or a flat list of ~300 files; hold ↓ (fast key repeat, ~16 keys/s) to move the cursor quickly down the list.
- Actual: after 60–330 rows the whole table area turns empty — no column header, no rows, scroll bars without handles; the heading still says "0 folders, 300 files" / "300 files from all subfolders". Home, End, ↑, the mouse wheel, `l` (twice), `r` and even a new scan of the same folder don't bring it back; only restarting the app does. Intermittent (timing): most often in 1000–1400 px tall windows, seen once at 900 px; not with slow single key presses.
- Expected: the table always shows the rows around the cursor.

### SM-65 · S4 · A file hard-linked into two folders: its size jumps between the folders on every rescan
- Scenario: `a/shared.bin` (3 MiB) and `b/shared.bin` are hard links to the same file; scan the parent, rescan (`r`), scan again.
- Actual: the 3 MiB are counted once (total right), but which folder gets them changes from scan to scan (a = 3 MiB, b = 0 B, then a = 0 B, b = 3 MiB); same for two hard-linked names in one folder. Users comparing two scans see sizes move with nothing changed on disk.
- Expected: a stable choice (e.g. the first in name/path order), so identical data gives identical numbers.

### SM-66 · S4 · Path bar drops trailing spaces
- Scenario: a folder named `sp ` (trailing space; legal on Linux). Type its full path in the path bar, Enter.
- Actual: "Path not found: …/sp" — the trailing space is removed. Opening it by drilling into the parent works.
- Expected: try the path as typed (at least when the trimmed path doesn't exist).

### SM-67 · S3 · Ctrl+V freezes the whole window for ~25 s when the clipboard owner doesn't answer
- Scenario: a file manager has files on the clipboard and then hangs (simulated: the clipboard owner is stopped); in spacemap press Ctrl+V.
- Actual: the window stops reacting — keys, redraws — for about 25 s; then "The clipboard didn't answer: nothing was pasted" appears and it reacts again (~100 ms). Nothing is pasted (correct).
- Expected: as announced, give up after about 1 s; the window must stay responsive while waiting.

### SM-68 · S4 · A file-manager list with a path that no longer exists: the missing entry isn't reported
- Scenario: a file manager offers two files; one of them was removed meanwhile; Ctrl+V in spacemap.
- Actual: "Copied 1 item", "Issues: none" — nothing says the second file was missing.
- Expected: a note/Issue naming the missing entry (as announced: "missing entries are reported").

### SM-69 · S4 · Whole-drive scan progress isn't exposed to screen readers
- Scenario: scan a mount point (whole filesystem) with a screen reader active.
- Actual: the bar shows "N% · X files · Y" on screen, but the accessibility tree only has "Scanning…" and an unnamed progress bar (the folder spinner's "X files · Y" label IS exposed).
- Expected: the same text (and the progress value) available to screen readers.
- Note: the percentage follows bytes of the filesystem's used space, so a drive of many tiny files stays near 1 % for most of the scan (cosmetic).

### SM-70 · S4 · Folder with ~300,000 files directly inside: "0 folders, 0 files" during the scan
- Scenario: table view; scan a folder that directly contains 300,000 files (cold cache, ~3.5 s scan).
- Actual: for the whole scan the table is empty and the heading reads "0 folders, 0 files" while the progress already shows "288,502 files · 13.3 MiB"; all rows appear at the end. (The window stays responsive.)
- Expected: rows (or at least a heading like "counting… 288,502 files so far") during the scan, as for folders whose files sit in subfolders.

### SM-71 · S3 · After an in-place delete (or move) the chart differs from a fresh scan of the same data
- Scenario: mixed tree (several categories, folders 2–3 levels deep); scan; open a sub-folder, delete its largest file (D → Delete); go back to the top and look at the chart. Then scan the same folder again (nothing else changed) and compare.
- Actual: the table numbers are identical to the fresh scan, but the chart is not: next to the edited folder the ring slice keeps the old colour ("other" grey where the fresh scan shows Code purple) and a child slice the fresh scan shows is missing (≈900 pixels differ). Reproducible for the same delete; deleting a different file in the same folder can give no difference. Also seen after cut+paste of a folder (≈900 px), and after a delete made with a category picked (large parts of the chart drawn in a different order/colour, ~29,000 px). Two fresh scans of unchanged data are pixel-identical, so the difference comes from the in-place update.
- Expected: after any edit the chart (slices, colours, grouping) equals a fresh scan, as the speed batch promises.

### SM-72 · S4 · Rows of exactly equal size: order after a delete differs from a rescan
- Scenario: folders zeta, alpha, mid created in that order, each 300 KiB; mid has one more 100 KiB file. Scan (mid first). Delete mid's extra file.
- Actual: after the delete the rows read mid, zeta, alpha; `r` or a fresh scan gives zeta, alpha, mid. Fresh scans order equal sizes by on-disk order, not by name (zeta before alpha).
- Expected: one deterministic order for ties in every path (name order would make both agree and be predictable).

### SM-73 · S4 · Folder's own size stale after a mass delete
- Scenario (XFS, like this machine's /var/tmp): a folder with 60 small .log files and 3 × 50 KB .dat files; filter "*.log", mark all 60, T (trash).
- Actual: the folder row shows 160 KiB; a fresh scan (or r) shows 156 KiB. When that many entries go, the filesystem shrinks the folder's own allocation (4 KiB → 0), and the in-place update keeps the old value. The chart differs from a fresh scan by a sliver (~400 px). All file numbers are right.
- Expected: after a delete/trash/move, the changed folders' own sizes are re-read so the tree equals a fresh scan (as SM-71's fix intends).

### SM-74 · S4 · Command line: tie order differs from the app
- Scenario: a folder with files beta, Alpha, _u (8 KiB each), gamma, 10, 9 (4 KiB each), delta (64 KiB); some with equal modification times. Compare `spacemap list DIR --sort size|modified|changed [--reverse]` with the app's Contents table sorted the same way.
- Actual: size sort — app: delta, Alpha, _u, beta, 10, 9, gamma (ties in byte order of the name); CLI: delta, _u, Alpha, beta, 9, 10, gamma (ties in natural, case-insensitive order). Modified/changed sorts — app breaks ties by size, then name; CLI by name only (e.g. modified: app …Alpha, _u, delta, beta, 9 vs CLI …_u, Alpha, 9, beta, delta). Name sort and --reverse otherwise agree.
- Expected: the same order as the app for every key and direction (as the brief promises).

### SM-75 · S4 · Command line: closed stdout gives exit 0 and no message
- Scenario: `spacemap flat DIR >&-` (or `list`), outside any wrapper.
- Actual: exit 0, nothing on stderr — the report is lost silently. (`| head -1` correctly exits 0 quietly; a full disk correctly gives exit 1 "No space left on device".)
- Expected: exit 1 with "spacemap: …" as for other write failures (the brief lists a closed stdout among those).

### Performance baseline (for SM-18, SM-19, SM-27)
Measured on this machine under a software-rendered virtual display. Use relative numbers.
- 1,000,000 files / 11,111 dirs, warm: scan 0.3 s; RSS +276 MB.
- 16TB NTFS HDD, 4.15M files: cold 133 s (du: 214 s), warm 2.4 s, peak RSS ~2.0 GB.
- 300k-file folder, table view: ~+80 ms CPU per cursor key, ~+330 ms per sort toggle over the small-folder baseline.
