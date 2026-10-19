Name:           spacescan
Version:        1.2.0
Release:        1%{?dist}
Summary:        Disk-usage explorer with a sunburst chart, a keyboard-driven table and reports

SourceLicense:  GPL-3.0-only
# SpaceScan's own license, then the crates built into the binary (from
# %%cargo_license_summary; update it when Cargo.lock changes).
License:        GPL-3.0-only AND (MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0 AND (MIT OR Apache-2.0) AND Unicode-3.0 AND (0BSD OR MIT OR Apache-2.0) AND Apache-2.0 AND Apache-2.0 AND MIT AND (Apache-2.0 OR GPL-2.0-only) AND (Apache-2.0 OR MIT) AND (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) AND BSD-2-Clause AND (BSD-2-Clause OR Apache-2.0 OR MIT) AND BSD-3-Clause AND (BSD-3-Clause OR Apache-2.0) AND (BSD-3-Clause OR MIT OR Apache-2.0) AND BSL-1.0 AND ISC AND MIT AND (MIT OR Apache-2.0) AND (MIT OR Apache-2.0 OR LGPL-2.1-or-later) AND (MIT OR Apache-2.0 OR Zlib) AND (MIT OR Zlib OR Apache-2.0) AND Unicode-3.0 AND (Unlicense OR MIT) AND Zlib AND (Zlib OR Apache-2.0 OR MIT)
URL:            https://github.com/a-kurczyn/spacescan
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
# The crates from Cargo.lock: `cargo vendor --versioned-dirs vendor` in the
# source folder, then `tar -cJf %%{name}-%%{version}-vendor.tar.xz vendor`.
Source1:        %{name}-%{version}-vendor.tar.xz

ExclusiveArch:  %{rust_arches}

BuildRequires:  cargo-rpm-macros >= 26
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# Loaded at run time (not linked), so rpm can't find them by itself.
%if 0%{?__isa_bits} == 64
%global soname_suffix ()(64bit)
%endif
Requires:       libX11.so.6%{?soname_suffix}
Requires:       libX11-xcb.so.1%{?soname_suffix}
Requires:       libXcursor.so.1%{?soname_suffix}
Requires:       libXi.so.6%{?soname_suffix}
Requires:       libxkbcommon.so.0%{?soname_suffix}
Requires:       libxkbcommon-x11.so.0%{?soname_suffix}
Requires:       libwayland-client.so.0%{?soname_suffix}
Requires:       libwayland-egl.so.1%{?soname_suffix}
Requires:       libEGL.so.1%{?soname_suffix}
Requires:       libdbus-1.so.3%{?soname_suffix}
Requires:       hicolor-icon-theme
# The folder picker.
Recommends:     xdg-desktop-portal

%description
SpaceScan scans a drive or folder and shows where the space goes, as a
sunburst chart and as a table you can drive from the keyboard. Files are
grouped by kind, with filters by name and category. It can delete, move to
the trash, copy and move files without touching other mounted drives, and
writes text, CSV and JSON reports from the command line.

%prep
%autosetup -n %{name}-%{version} -a1
%cargo_prep -v vendor

%build
%cargo_build
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies
%{cargo_vendor_manifest}

%install
install -Dpm 0755 target/rpm/%{name} %{buildroot}%{_bindir}/%{name}
install -Dpm 0644 packaging/%{name}.desktop %{buildroot}%{_datadir}/applications/%{name}.desktop
for size in 16 24 32 48 64 128 256; do
  install -Dpm 0644 packaging/icons/${size}x${size}/%{name}.png \
    %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/%{name}.png
done
install -Dpm 0644 packaging/%{name}.1 %{buildroot}%{_mandir}/man1/%{name}.1
install -Dpm 0644 packaging/io.github.a_kurczyn.%{name}.metainfo.xml %{buildroot}%{_metainfodir}/io.github.a_kurczyn.%{name}.metainfo.xml

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{name}.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/io.github.a_kurczyn.%{name}.metainfo.xml
%cargo_test

%files
%license LICENSE LICENSE.dependencies cargo-vendor.txt
# The About window opens THIRD-PARTY-LICENSES.html from here; docs/ holds
# the screenshots README.md shows.
%doc README.md CHANGELOG.md THIRD-PARTY-LICENSES.html docs
# Optional right-click entries for file managers; the user copies them in.
%doc packaging/filemanagers
%{_bindir}/%{name}
%{_mandir}/man1/%{name}.1*
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/*/apps/%{name}.png
%{_metainfodir}/io.github.a_kurczyn.%{name}.metainfo.xml

%changelog
* Mon Oct 19 2026 Alejandro Kurczyn <335237751+a-kurczyn@users.noreply.github.com> - 1.2.0-1
- Cell view: a Voronoi treemap whose cell areas match sizes, zoomable
- Sunburst animations; arrow keys work as in ncdu in every view
- Categories editor with 16 distinct colors, theme setting, -U update
* Sat Oct 17 2026 Alejandro Kurczyn <335237751+a-kurczyn@users.noreply.github.com> - 1.1.3-1
- The AppImage starts on systems without libxkbcommon-x11
- No scrollbar over the path bar in a narrow window
- Optional right-click "Scan with SpaceScan" for file managers

* Sun Oct 11 2026 Alejandro Kurczyn <335237751+a-kurczyn@users.noreply.github.com> - 1.1.2-1
- The window opens on software renderers with fewer capabilities
- Manual page

* Sun Oct 11 2026 Alejandro Kurczyn <335237751+a-kurczyn@users.noreply.github.com> - 1.1.1-1
- Icons in all common sizes; library licenses and README screenshots installed

* Sun Oct 11 2026 Alejandro Kurczyn <335237751+a-kurczyn@users.noreply.github.com> - 1.1.0-1
- First package
