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

## Batch 1: safety, quick efficiency wins, small cleanups (done, sent to QA)

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

## Batch 2: measure, then the bigger real-time work (after QA GO)

- [ ] N5 Frame-time benchmark (ignored test): chart, table and category bar
      on a big synthetic tree while sliders, zoom and filters change; reports
      median and worst frame times.
- [ ] Measure as a parameter instead of the global flag (groundwork for N1).
- [ ] N1 Applying a filter or pick off the UI thread (keep showing the old
      view until the new one is ready; latest wins). ~20 ms at 360k files,
      est. ~0.2 s at 4M.
- [ ] 2.6 Live extension totals kept in the live tree instead of one message
      per folder.
- [ ] 2.4 Chart: fewer per-frame allocations, all slices in one mesh; a
      layout cache only if the benchmark calls for it.

## Batch 3: architecture and readability (after QA GO)

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
