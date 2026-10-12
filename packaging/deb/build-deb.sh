#!/bin/sh
# Builds the SpaceScan .deb from a built program and the source folder.
# Usage: build-deb.sh PROGRAM SOURCE_FOLDER OUTPUT_FOLDER [REVISION]
# PROGRAM should be the portable build (glibc 2.28) used for the AppImage,
# so the package works on Debian 10 and Ubuntu 20.04 or newer.
# Needs dpkg-deb; run it as a normal user.
set -eu

program=$1
src=$2
out=$3
revision=${4:-1}

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$src/Cargo.toml" | head -n 1)
date=$(git -C "$src" log -1 --format=%aD 2>/dev/null || date -R)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

install -Dm755 "$program" "$root/usr/bin/spacescan"
install -Dm644 "$src/packaging/spacescan.desktop" "$root/usr/share/applications/spacescan.desktop"
for size in 16 24 32 48 64 128 256; do
    install -Dm644 "$src/packaging/icons/${size}x${size}/spacescan.png" \
        "$root/usr/share/icons/hicolor/${size}x${size}/apps/spacescan.png"
done
install -Dm644 "$src/packaging/io.github.a_kurczyn.spacescan.metainfo.xml" \
    "$root/usr/share/metainfo/io.github.a_kurczyn.spacescan.metainfo.xml"

doc="$root/usr/share/doc/spacescan"
install -Dm644 "$src/packaging/deb/copyright" "$doc/copyright"
install -m644 "$src/README.md" "$src/CHANGELOG.md" "$src/THIRD-PARTY-LICENSES.html" "$doc/"
mkdir -p "$doc/docs"
install -m644 "$src"/docs/*.png "$doc/docs/"
printf '%s\n' \
    "spacescan ($version-$revision) unstable; urgency=medium" \
    "" \
    "  * SpaceScan $version; see CHANGELOG.md." \
    "" \
    " -- Alejandro Kurczyn <335237751+a-kurczyn@users.noreply.github.com>  $date" |
    gzip -9n > "$doc/changelog.Debian.gz"

mkdir -p "$root/DEBIAN"
size=$(du -sk --exclude=DEBIAN "$root" | cut -f1)
sed -e "s/@VERSION@/$version-$revision/" -e "s/@SIZE@/$size/" \
    "$src/packaging/deb/control" > "$root/DEBIAN/control"
(cd "$root" && find usr -type f -exec md5sum {} +) > "$root/DEBIAN/md5sums"

mkdir -p "$out"
dpkg-deb --root-owner-group -Zxz --build "$root" "$out/spacescan_${version}-${revision}_amd64.deb"
