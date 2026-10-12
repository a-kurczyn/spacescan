# Changelog

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
