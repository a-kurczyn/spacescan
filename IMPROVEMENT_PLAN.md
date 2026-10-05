# Improvement plan (from the code review of 2026-10-05)

Working document for the coder session. If a session is lost, start here:
check the status marks, `git log`, and the QA tracker (QA_FINDINGS.md).

## Workflow

- Work batch by batch, in the order below. Unit-test every change myself
  (`cargo test`, `cargo clippy --all-targets`, `cargo fmt --check`), and
  check user-visible changes in the running app.
- After each batch: commit, install the release build to
  `~/.local/bin/spacescan`, then tell the QA agent (spacescan-qa-52) what
  changed, in behavior terms only (no code), and **wait for its GO / NO GO
  before starting the next batch**.
- S4 issues are fixed only when the user asks.
- CI is deferred until the code is in a clean state (the user's decision; the
  QA test process takes about 100 minutes).

## Done before this plan (uncommitted until batch 1's commit)

- [x] SM-76: C1 control characters shown as `\u{80}`, raw bytes as `\x80`.
- [x] Contents table: scroll bars only when needed.
- [x] GUI tests: `headless_frame` clears unused texture updates (epaint 0.36.2).
- [x] SM-78: menu shortcuts also given as the accessible description.
- [x] Live scan: a picked category filters the live chart and table; the
      live tree is read again at once when depth, smallest slice, pick or
      measure change; idle redraw 10 Hz (4 Hz with a screen reader); the
      progress bar counts the whole scan.

## Batch 1: safety, quick efficiency wins, small cleanups (done)

QA round 26 on 1fadef5: everything verified, but NO-GO for a new S3,
SM-79 (invisible characters made names look the same).

- [x] SM-79: characters that show as nothing or rearrange text (Unicode
      default-ignorables, line/paragraph separators) shown as `\u{200B}`;
      emoji joiners and presentation selectors inside emoji kept. The path
      bar reads escapes back when the text as typed doesn't exist.
- [x] SM-80 (QA on 2f72acf): a shown path could open a sibling really named
      like the escape text; the path bar now reads escapes first, then the
      text as typed.
- [x] Decided by the user: the S4 wording for a move that can't remove a
      source in a read-only folder is SM-81, deferred (don't fix now).
      Copied paths stay the real path (not escaped).

Safety
- [x] 1.1 Cross-drive move: make copies durable before deleting sources
      (sync the destination filesystem in batches, then remove the sources).
      Found while testing: the fallback copy loop filled a 1 MiB buffer per
      file; now sized to the file (tests 8.8 s -> 0.4 s).
- [x] 1.2 One helper for opening with the default app (`xdg-open`), always
      reaped; the chart's right-click "Open" left a zombie process.
- [x] 1.3 Delete module comment claimed a device check; it stops at mount
      points (btrfs subvolumes are deleted like folders, so no device check).

Efficiency (real-time)
- [x] 2.2 Settings saved after 0.5 s without changes (not every frame while
      a slider moves), and on exit.
- [x] 2.3 Marked rows' total size cached until marks or rows change.
- [x] 2.5 Extension table rows cached until the breakdown or sort changes.
- [x] N3 Scan message draining: 10 ms per frame instead of 30 ms.
- [x] 2.1 Command line: name sorts build each sort key once (`flat /usr
      --sort name`: 2.7 s -> 1.3 s CPU).
- [x] 2.7 Dropped: one thread per hovered file keeps a hung file (network
      share) from blocking later lookups; spawning is cheap.

Small cleanups
- [x] Misplaced doc comments (main.rs abort/work_running, scan.rs NO_TIME,
      cli.rs files_in_app_order).
- [x] Stale `#[allow(dead_code)]` on HoverInfo.
- [x] Redundant `exists()` before `metadata()` when a scan starts.
- [x] `poll_scan`: no duplicated log and scan-end code.
- [x] `Settings::sanitized`: one clamp helper.
- [x] `cargo doc` warnings.
- [x] `[lints]` table in Cargo.toml locking in current good practice.
- [x] Test: every translation key used in the code exists in en.lang.

## Batch 2: measure, then the bigger real-time work (done)

QA GO on 1c9c357. Measured with the new frame benchmark (`SPACESCAN_FILES=4000000
SPACESCAN_BENCH=/ cargo test --release frame_bench -- --ignored --nocapture`),
synthetic 4M-file tree, whole frames (UI + tessellation):

| Frame                              | before batch 2 | after |
|------------------------------------|----------------|-------|
| chart: still, hover, sliders, zoom | 4–8 ms         | same  |
| filter applied                     | 18–27 ms       | 22–30 ms (noise) |
| category picked                    | 44–51 ms       | 17–24 ms |
| flat list (first 1,000) re-sorted  | 190–450 ms     | 0.4 ms (sorted apart: shown after 0.3–1.3 s) |
| flat list (all files) re-sorted    | 0.7–1.3 s      | 0.3 ms (same)  |
| live scan of /, table view frames  | up to 0.5–0.9 s | under 26 ms (one ~110 ms frame at scan end) |

- [x] N5 Frame-time benchmark (`frame_bench`, ignored test in main.rs).
- [x] Flat list: numeric sorts by compact keys; files gathered folder by
      folder (ties keep their found order); cursor found by tree position;
      longest names picked in parallel.
- [x] Flat lists over 200,000 files sorted on another thread (one at a
      time); the rows shown stay, marked "sorting…"; tree edits in place
      (delete, measure, folder rescan) wait for it, so they copy nothing.
- [x] Category pick: stored categories from the scan; a pick reuses the
      filtered tree and its breakdown (`repick`).
- [x] Big trees freed on another thread; memory returned only at scan end
      (returning it locks the allocator for a while).
- [x] Scans run on their own thread pool: the window's parallel work (sorting
      the live table) no longer queues behind a scan (the 0.5–0.9 s stalls).
- [x] Skipped on the numbers: N1 background rebuild of the view tree (picks
      and filters are 1–2 frames at 4M), 2.6 live extension messages (live
      frames under 26 ms), 2.4 chart allocations (chart frames ~5 ms).
      "Measure as a parameter" moves to batch 3 (cleanup, not needed now).

## Batch 3: architecture and readability (after QA GO)

- [ ] SM-81 (S4, the user wants it in this batch): a move to another
      filesystem that can't remove a source in a read-only folder must say
      what failed (the copy was made, the original couldn't be removed
      because its folder can't be changed, and it's still in place), not
      "don't have permission to read or enter that folder". Status line as is.
- [ ] Measure (bytes or files) passed as a parameter instead of the global
      flag.
- [ ] `cfg!(test)` switches replaced by settings passed in at creation.
- [ ] Split scan.rs (tree, formatting, filesystem helpers, scanning).
- [ ] One path-to-node lookup (find_node, find_by_path, index_path_to).
- [ ] A `Stat` struct for file details (removes repeated 14-field Node
      literals and the 8-argument `LiveTree::close`).
- [ ] Document that slice looks are keyed by node address (valid per tree_gen).
- [ ] Break up the long UI functions (table_ui, toolbar_ui, chart_ui,
      category_bar_ui, App::ui).
- [ ] Library crate with explicit imports; DiskScanApp split into smaller
      state structs.

## Optional / declined

- Exact age shades while a category is picked during a scan (+~160 B per
  folder during scans). Not planned unless asked.
- Name/size/date filters and extension picks during a scan: would need a
  second copy of every file during the scan (hundreds of MB at 4M files)
  and scanner changes. Declined unless the user asks.
- CI: deferred (see Workflow).
