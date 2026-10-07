# Installing SpaceScan (instructions for AI agents)

These steps install the SpaceScan AppImage for the current user and add it
to the desktop's application menu under **Utilities** (called
**Accessories** in some desktops). They need no root access and no
password. Run them as the user who will use the app.

Requirements: Linux on x86_64 with glibc 2.28 or newer, `curl`, and a
desktop that follows the freedesktop.org menu standard (KDE Plasma, GNOME,
Xfce, Cinnamon, MATE, LXQt and others).

## 1. Download the AppImage

The latest release is always at the same address:

```sh
mkdir -p "$HOME/.local/bin"
curl -fL -o "$HOME/.local/bin/spacescan" \
  https://github.com/a-kurczyn/spacescan/releases/latest/download/SpaceScan-x86_64.AppImage
chmod 755 "$HOME/.local/bin/spacescan"
```

If `$HOME/.local/bin/spacescan` already exists, this replaces it; that is
how SpaceScan is updated.

## 2. Check that it runs

```sh
"$HOME/.local/bin/spacescan" --version
```

This prints the version and opens no window. If it fails with a message
about FUSE (`libfuse.so.2` missing), either install the distribution's
FUSE 2 package (`fuse-libs` on Fedora, `libfuse2` or `libfuse2t64` on
Debian and Ubuntu, `fuse2` on Arch) or add
`APPIMAGE_EXTRACT_AND_RUN=1` in front of the command, and use
`Exec=env APPIMAGE_EXTRACT_AND_RUN=1 ...` in step 4.

## 3. Install the icon

The icon is inside the AppImage. Extract it in a temporary folder:

```sh
tmp=$(mktemp -d)
(cd "$tmp" && "$HOME/.local/bin/spacescan" --appimage-extract spacescan.png >/dev/null)
mkdir -p "$HOME/.local/share/icons/hicolor/256x256/apps"
cp "$tmp/squashfs-root/spacescan.png" "$HOME/.local/share/icons/hicolor/256x256/apps/spacescan.png"
rm -rf "$tmp"
```

## 4. Add the menu entry

`Categories=Utility;` is what places SpaceScan under Utilities or
Accessories. Desktop files don't expand `~` or `$HOME`, so the unquoted
here-document below writes the full path:

```sh
mkdir -p "$HOME/.local/share/applications"
cat > "$HOME/.local/share/applications/spacescan.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=SpaceScan
GenericName=Disk Usage Visualizer
Comment=Sunburst chart of drive and folder disk usage
Exec=$HOME/.local/bin/spacescan
Icon=spacescan
StartupWMClass=spacescan
Terminal=false
StartupNotify=true
Categories=Utility;
Keywords=disk;usage;storage;sunburst;scanner;space;
EOF
```

Then let the desktop pick up the new entry (both commands are optional;
skip any that is not installed):

```sh
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 >/dev/null 2>&1 || true
```

On most desktops the entry appears within a few seconds; otherwise after
logging out and back in.

## 5. Tell the user

Report that SpaceScan is installed, that it is in the application menu
under Utilities (or Accessories), and that it can also be started from a
terminal as `spacescan` if `~/.local/bin` is on their `PATH`.

## Uninstalling

```sh
rm -f "$HOME/.local/bin/spacescan" \
      "$HOME/.local/share/applications/spacescan.desktop" \
      "$HOME/.local/share/icons/hicolor/256x256/apps/spacescan.png"
```

Settings are kept in `~/.config/spacescan`; remove that folder too only if
the user asks to remove their settings.
