# Changelog

## 1.2.0 (not released yet)

### New
- **Map view**: a third view next to the table and the sunburst. The
  folder is split into cells whose areas match their sizes (a Voronoi
  treemap), colored by category and shaded by age from the newest files
  in the middle to the oldest at the border. Hover shows the same
  details as the sunburst; a click opens the folder under the pointer.
  Cells share the area by size, at every level, and even the smallest
  item gets a cell of at least 16 × 16 pixels, easy to see and click.
  Only when a folder holds more items than fit at that size (or more
  than 400) do the smallest share a grey "other" corner of 16 × 16
  pixels, which opens the next of them like a folder, as many times as
  needed. A strip beside the map, as wide as it needs, holds the
  folder's details and the category legend. Its button sits between
  the sunburst's and the table's. The map grows live during a scan,
  glides when it changes and zooms into and out of folders; the
  animations can be turned off in the settings. The map shows two levels
  of folders by default; Settings › Map levels shows up to twelve, the
  items inside folders drawn within their cells. Settings › Gap between
  map cells leaves up to 12 pixels of space between them (4 by
  default). Ctrl+wheel zooms the map in up to 8× around the pointer,
  showing the names of smaller cells, and dragging moves it; the space
  between cells and
  around the cells inside a folder stays the same on screen. A folder's
  map is worked out in about a fifth of a second even with 400 cells on
  a 4K screen, and folders seen before show at once.
- **Sunburst animations**: slices glide to their new angles when the
  order, a filter or the measure changes, and the chart zooms into a
  folder's slice when it opens and back out when it's left. Settings ›
  Behavior control › Animations turns them off for both charts.
- **Theme setting**: Auto (as before), Light or Dark.
- **Categories window** (Settings › Edit categories…): rename, add and
  remove categories, change their extensions and pick their colors
  from a list of 16 that suit light and dark themes and stay easy to
  tell apart, also with color blindness, or type any color as
  `#RRGGBB`. It warns about extensions in two categories and colors that
  are hard to tell apart, also for color-blind eyes when the color-blind
  palette is on.
- **`spacescan -U`** updates the AppImage to the latest release, checked
  against the release's checksums. SpaceScan never looks for updates on
  its own.

### Changed
- The arrow keys work the same in the table, the sunburst and the map,
  as in ncdu: ⬆ and ⬇ move to the previous and next item and stop at the
  first and last, ➡ opens the highlighted folder, ⬅ goes to the parent
  folder.
- Border thickness is set in whole screen pixels, 1 to 3.
- Settings are grouped by what they change: Unit of measure, Sunburst
  control, Treemap control, Rendering controls (colors and lines, both
  charts), Table control and Behavior control (apparent size,
  clipboard, log lines, categories, language).
- In the sunburst and the map, the highlighted item's path from the
  folder shown follows the path bar in yellow (its folders can be
  clicked); the corner stats and the map's strip no longer repeat it.

## 1.1.3 (2026-10-17)

### New
- **Right-click "Scan with SpaceScan"** for folders and drives in Dolphin,
  Nemo, Nautilus and Thunar. It is optional and not installed by default;
  the README tells how to add it.

### Fixed
- **AppImage on systems without `libxkbcommon-x11`** (minimal or
  headless X setups) quit at start with a panic. The AppImage now carries
  that library and uses it only when the system has none.
- **Path bar in a narrow window**: a long path no longer shows a
  scrollbar over the folders; the deepest folder stays in view and the
  mouse wheel still scrolls (the plain wheel too, not only Shift+wheel).
- **Narrow windows**: the window can't be made narrower than 460 px, where
  the toolbar's buttons and the path bar overlapped. In the chart view the
  top-left path line stops before the sort buttons instead of running under
  them.

## 1.1.2 (2026-10-11)

### New
- **Debian and Ubuntu package** (`.deb`, Debian 10 and Ubuntu 20.04 or
  newer), attached to each release.
- **Arch Linux package file** (PKGBUILD) in `packaging/aur`, to build and
  install with `makepkg`.
- **Manual page** (`man spacescan`) in the Fedora, Debian/Ubuntu and Arch
  packages.

### Fixed
- The window opens on computers whose graphics driver offers fewer
  capabilities, such as virtual machines without 3D acceleration; it used
  to stop at start with a graphics error.

## 1.1.1 (2026-10-11)

### Fixed
- About › Third-party licenses opens the notices installed with the
  Fedora package; without them, it opens the page for this version
  instead of the latest one.
- The Fedora package installs the icon in all common sizes, and the
  screenshots its README shows. The app-store screenshots fit the
  AppStream size limits.

## 1.1.0 (2026-10-11)

### New
- **Pick categories in the chart.** Click categories in the chart's legend
  to show only their files; click again to drop one. In the category bar,
  Ctrl+click picks several categories at once.
- **Start the app with a selection:** `--category NAME,NAME` shows only
  those categories' files, and `--filter TEXT` starts with a name filter,
  as typed in the Filters window.
- **Filtered reports:** `list`, `flat` and `exts` take `--filter TEXT`, and
  folders then count only the matching files.
- **Fedora package** (rpm, through COPR), with a menu entry under
  Utilities and the app's details for GNOME Software and KDE Discover.

### Improved
- Moving many files to the trash, or deleting them, is much faster when
  network or other drives are mounted, and a running count shows how far
  it has got. The window stays responsive throughout, and quitting stops
  cleanly between two items.

### Fixed
- The chart no longer shows a slice's details under an open dialog, menu
  or the legend.
- The AppImage includes the license notices of the libraries it's built
  with (About › Third-party licenses), and starts on systems that have
  only FUSE 3, such as Ubuntu 22.04 and Debian 12 or newer.

## 1.0.0 (2026-10-06)

First public release.
